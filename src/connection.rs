//! 定义了单个可靠连接。
//! Defines a single reliable connection.

use crate::buffer::{ReceiveBuffer, SendBuffer};
use crate::packet::frame::Frame;
use std::net::SocketAddr;
use tokio::sync::mpsc;

/// A command for the central socket sender task.
/// 用于中央套接字发送任务的命令。
#[derive(Debug)]
pub struct SendCommand {
    /// The destination address.
    /// 目标地址。
    pub remote_addr: SocketAddr,
    /// The frame to send.
    /// 要发送的帧。
    pub frame: Frame,
}

/// The state of a connection.
/// 连接的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// The connection is being established.
    /// 正在建立连接。
    Connecting,
    /// The connection is established and ready for data transfer.
    /// 连接已建立，可以传输数据。
    Established,
    /// The connection is closing.
    /// 正在关闭连接。
    Closing,
    /// The connection is closed.
    /// 连接已关闭。
    Closed,
}

/// The internal state of a connection, separated to satisfy the borrow checker.
/// 连接的内含状态，分离出来是为了满足借用检查器。
struct ConnectionState {
    remote_addr: SocketAddr,
    state: State,
    send_buffer: SendBuffer,
    recv_buffer: ReceiveBuffer,
    sender: mpsc::Sender<SendCommand>,
}

/// Represents a single reliable connection.
///
/// Each connection is managed by its own asynchronous task.
///
/// 代表一个单一的可靠连接。
///
/// 每个连接都由其自己的异步任务管理。
pub struct Connection {
    /// The actual state of the connection.
    /// 连接的实际状态。
    state: ConnectionState,

    /// Receives frames from the main socket task.
    /// 从主套接字任务接收帧。
    receiver: mpsc::Receiver<Frame>,
}

impl Connection {
    /// Creates a new `Connection`.
    ///
    /// This also returns a sender for the main socket task to send frames to this connection.
    ///
    /// 创建一个新的 `Connection`。
    ///
    /// 这也会返回一个发送端，供主套接字任务向此连接发送帧。
    pub fn new(
        remote_addr: SocketAddr,
        initial_state: State,
        sender: mpsc::Sender<SendCommand>,
    ) -> (Self, mpsc::Sender<Frame>) {
        // We can tune the channel size later.
        // 我们之后可以调整通道的大小。
        let (in_tx, in_rx) = mpsc::channel(128);
        let state = ConnectionState {
            remote_addr,
            state: initial_state,
            send_buffer: SendBuffer::new(),
            recv_buffer: ReceiveBuffer::new(),
            sender,
        };
        let connection = Self { state, receiver: in_rx };
        (connection, in_tx)
    }

    /// Writes a slice of bytes to the connection.
    ///
    /// This will segment the data into one or more PUSH frames and queue them for sending.
    ///
    /// 向连接写入一个字节切片。
    ///
    /// 这会将数据分段成一个或多个PUSH帧，并将它们排队等待发送。
    pub async fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // TODO: This is a simplification. We should handle:
        // 1. A buf that is larger than a single packet's payload capacity.
        // 2. Generating correct sequence numbers.
        // 3. Locking and synchronization if `write` can be called concurrently.

        let payload = bytes::Bytes::copy_from_slice(buf);

        // Create a PUSH frame (with a dummy header for now)
        let push_header = crate::packet::header::ShortHeader {
            command: crate::packet::command::Command::Push,
            connection_id: 0, // TODO: Use the actual connection ID
            recv_window_size: 0, // TODO: Update with real value
            timestamp: 0, // TODO: Update with real value
            sequence_number: 0, // TODO: Get from a sequence number generator
            recv_next_sequence: 0, // TODO: Update with real value
        };

        let frame = Frame::Push {
            header: push_header,
            payload,
        };

        self.state.send_buffer.queue_frame(frame);

        // For now, we just pretend we wrote everything.
        Ok(buf.len())
    }

    /// Runs the connection's main loop to process incoming frames.
    ///
    /// 运行连接的主循环以处理传入的帧。
    pub async fn run(&mut self) {
        let state = &mut self.state;
        let receiver = &mut self.receiver;

        loop {
            // By splitting the struct, the borrow checker now understands
            // that we are operating on disjoint parts.
            tokio::select! {
                Some(frame) = receiver.recv() => {
                    state.handle_frame(frame).await;
                }

                _ = async {
                    if let Some(frame) = state.send_buffer.pop_next_frame() {
                        state.send_frame(frame).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                }, if !state.send_buffer.is_empty() => {}

                else => {
                    break;
                }
            }
        }

        println!("Connection to {} closed.", state.remote_addr);
        state.state = State::Closed;
    }
}

impl ConnectionState {
    /// Handles a single incoming frame based on the current state.
    /// 根据当前状态处理单个传入的帧。
    async fn handle_frame(&mut self, frame: Frame) {
        println!(
            "Connection to {} in state {:?} received frame: {:?}",
            self.remote_addr, self.state, frame
        );

        match self.state {
            State::Connecting => {
                if let Frame::Syn { header } = frame {
                    // Received a SYN, move to established and send SYN-ACK
                    // 收到SYN，转换到Established状态并发送SYN-ACK
                    println!("Transitioning to Established state.");
                    self.state = State::Established;

                    // Respond with a SYN-ACK
                    // 用SYN-ACK回应
                    let syn_ack_header = crate::packet::header::LongHeader {
                        command: crate::packet::command::Command::SynAck,
                        protocol_version: 0, // Or some constant
                        connection_id: header.connection_id,
                    };

                    let syn_ack_frame = Frame::SynAck {
                        header: syn_ack_header,
                    };

                    self.send_frame(syn_ack_frame).await;
                }
            }
            State::Established => {
                match frame {
                    Frame::Push { header, payload } => {
                        // TODO: Pass the data to the receive buffer for reordering and delivery.
                        // 将数据传递给接收缓冲区进行重排和交付。
                        println!("Received PUSH with {} bytes of payload.", payload.len());
                    }
                    Frame::Ack { header, payload } => {
                        // TODO: Process the acknowledgment information in the send buffer.
                        // 在发送缓冲区中处理确认信息。
                        println!("Received ACK.");
                    }
                    Frame::Fin { header } => {
                        // TODO: Handle connection termination.
                        println!("Received FIN, start closing connection.");
                        self.state = State::Closing;
                        // We should respond with a FIN-ACK.
                    }
                    _ => {
                        // Ignore other unexpected packets in this state.
                        println!("Ignoring unexpected frame in Established state: {:?}", frame);
                    }
                }
            }
            State::Closing => {
                // TODO: Handle FINs, ACKs during closing phase.
            }
            State::Closed => {
                // Should not receive frames in closed state.
            }
        }
    }

    /// Sends a frame to the central sender task.
    /// 发送一个帧到中央发送任务。
    async fn send_frame(&self, frame: Frame) {
        let cmd = SendCommand {
            remote_addr: self.remote_addr,
            frame,
        };
        if self.sender.send(cmd).await.is_err() {
            eprintln!(
                "Failed to send frame to {}: sender task is dead.",
                self.remote_addr
            );
        }
    }
}
