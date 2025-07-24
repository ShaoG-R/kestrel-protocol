//! 定义了单个可靠连接。
//! Defines a single reliable connection.

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

/// Represents a single reliable connection.
///
/// Each connection is managed by its own asynchronous task.
///
/// 代表一个单一的可靠连接。
///
/// 每个连接都由其自己的异步任务管理。
pub struct Connection {
    /// The remote address of the connection.
    /// 连接的远程地址。
    remote_addr: SocketAddr,

    /// The current state of the connection.
    /// 连接的当前状态。
    state: State,

    /// Receives frames from the main socket task.
    /// 从主套接字任务接收帧。
    receiver: mpsc::Receiver<Frame>,

    /// Sends frames to the central socket sender task.
    /// 向中央套接字发送任务发送帧。
    sender: mpsc::Sender<SendCommand>,
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
        let connection = Self {
            remote_addr,
            state: initial_state,
            receiver: in_rx,
            sender,
        };
        (connection, in_tx)
    }

    /// Runs the connection's main loop to process incoming frames.
    ///
    /// 运行连接的主循环以处理传入的帧。
    pub async fn run(&mut self) {
        while let Some(frame) = self.receiver.recv().await {
            self.handle_frame(frame).await;
        }

        // When the channel is closed, the loop exits and the connection task ends.
        // 当通道关闭时，循环退出，连接任务结束。
        println!("Connection to {} closed.", self.remote_addr);
        self.state = State::Closed;
    }

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
                // TODO: Handle data, acks, etc.
            }
            State::Closing => {
                // TODO: Handle FINs, etc.
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
