//! 定义了单个可靠连接。
//! Defines a single reliable connection.

use crate::buffer::{ReceiveBuffer, SendBuffer};
use crate::packet::frame::Frame;
use crate::packet::sack::{decode_sack_ranges, encode_sack_ranges};
use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const INITIAL_RTO: Duration = Duration::from_millis(1000);
const MIN_RTO: Duration = Duration::from_millis(500);

/// An estimator for the round-trip time (RTT).
///
/// This uses a simple version of the Jacobson/Karels algorithm.
///
/// RTT 估算器。
///
/// 使用一个简化版的 Jacobson/Karels 算法。
#[derive(Debug)]
struct RttEstimator {
    srtt: Duration,
    rttvar: Duration,
    rto: Duration,
}

impl RttEstimator {
    fn new() -> Self {
        Self {
            srtt: Duration::from_secs(0),
            rttvar: Duration::from_secs(0),
            rto: INITIAL_RTO,
        }
    }

    fn rto(&self) -> Duration {
        self.rto
    }

    /// Updates the RTO based on a new RTT sample.
    /// 根据一个新的RTT样本更新RTO。
    fn update(&mut self, rtt: Duration) {
        if self.srtt == Duration::from_secs(0) {
            // First sample
            self.srtt = rtt;
            self.rttvar = rtt / 2;
        } else {
            // Subsequent samples
            let delta = if self.srtt > rtt {
                self.srtt - rtt
            } else {
                rtt - self.srtt
            };
            self.rttvar = (3 * self.rttvar + delta) / 4;
            self.srtt = (7 * self.srtt + rtt) / 8;
        }
        self.rto = (self.srtt + 4 * self.rttvar).max(MIN_RTO);
    }
}

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
    sequence_number_counter: u32,
    rto_estimator: RttEstimator,
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
            sequence_number_counter: 0,
            rto_estimator: RttEstimator::new(),
        };
        let connection = Self {
            state,
            receiver: in_rx,
        };
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
            sequence_number: self.state.sequence_number_counter,
            recv_next_sequence: 0, // TODO: Update with real value
        };
        self.state.sequence_number_counter += 1;

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
        let mut rto_timer = tokio::time::interval(state.rto_estimator.rto());

        loop {
            // By splitting the struct, the borrow checker now understands
            // that we are operating on disjoint parts.
            tokio::select! {
                Some(frame) = receiver.recv() => {
                    state.handle_frame(frame).await;
                }

                _ = async {
                    if let Some(frame) = state.send_buffer.pop_next_frame() {
                        let now = Instant::now();
                        state.send_buffer.add_in_flight(frame.clone(), now);
                        state.send_frame(frame).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                }, if !state.send_buffer.is_empty() => {}

                _ = rto_timer.tick() => {
                    state.check_for_retransmissions().await;
                }

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
                        // Pass the data to the receive buffer for reordering and delivery.
                        // 将数据传递给接收缓冲区进行重排和交付。
                        println!(
                            "Received PUSH with seq={} and {} bytes of payload.",
                            header.sequence_number,
                            payload.len()
                        );
                        self.recv_buffer.receive(header.sequence_number, payload);
                    }
                    Frame::Ack { header, payload } => {
                        let sack_ranges = decode_sack_ranges(payload);
                        println!("Received ACK with SACK ranges: {:?}", sack_ranges);

                        let mut acked_seq_numbers = BTreeSet::new();
                        for range in sack_ranges {
                            for seq in range.start..=range.end {
                                acked_seq_numbers.insert(seq);
                            }
                        }

                        let now = Instant::now();
                        self.send_buffer.in_flight.retain(|packet| {
                            if let Some(seq) = packet.frame.sequence_number() {
                                if acked_seq_numbers.contains(&seq) {
                                    // This packet is acknowledged. Update RTO and remove it.
                                    let rtt = now.duration_since(packet.last_sent_at);
                                    self.rto_estimator.update(rtt);
                                    println!(
                                        "Packet with seq={} acked. RTT: {:?}, New RTO: {:?}",
                                        seq,
                                        rtt,
                                        self.rto_estimator.rto()
                                    );
                                    return false; // Remove from in_flight
                                }
                            }
                            true // Keep in in_flight
                        });
                    }
                    Frame::Fin { header } => {
                        println!("Received FIN, closing connection and sending ACK.");
                        self.state = State::Closing;
                        // Respond with an ACK to their FIN. This is part of the 4-way handshake.
                        let mut ack_payload = bytes::BytesMut::new();
                        let sack_ranges = self.recv_buffer.get_sack_ranges();
                        encode_sack_ranges(&sack_ranges, &mut ack_payload);

                        let ack_header = crate::packet::header::ShortHeader {
                            command: crate::packet::command::Command::Ack,
                            connection_id: header.connection_id,
                            recv_window_size: 0, // TODO
                            timestamp: 0,        // TODO
                            sequence_number: self.sequence_number_counter,
                            recv_next_sequence: self.recv_buffer.next_sequence(),
                        };
                        self.sequence_number_counter += 1;
                        let ack_frame = Frame::Ack {
                            header: ack_header,
                            payload: ack_payload.freeze(),
                        };
                        self.send_frame(ack_frame).await;
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

    /// Checks for any in-flight packets that have timed out and retransmits them.
    /// 检查是否有任何在途数据包已超时并进行重传。
    async fn check_for_retransmissions(&mut self) {
        let rto = self.rto_estimator.rto();
        let now = Instant::now();
        let mut frames_to_resend = Vec::new();

        for packet in self.send_buffer.iter_in_flight_mut() {
            if now.duration_since(packet.last_sent_at) > rto {
                println!(
                    "Retransmitting packet with seq={:?}",
                    packet.frame.sequence_number()
                );
                // Collect the frame to resend it later.
                frames_to_resend.push(packet.frame.clone());
                // Update the time it was sent.
                packet.last_sent_at = now;
            }
        }

        for frame in frames_to_resend {
            self.send_frame(frame).await;
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
