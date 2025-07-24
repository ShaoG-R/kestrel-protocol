//! 定义了单个可靠连接。
//! Defines a single reliable connection.

use crate::buffer::{ReceiveBuffer, SendBuffer};
use crate::congestion_control::CongestionController;
use crate::error::{Error, Result};
use crate::packet::frame::Frame;
use crate::packet::sack::{decode_sack_ranges, encode_sack_ranges};
use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;

const INITIAL_RTO: Duration = Duration::from_millis(1000);
const MIN_RTO: Duration = Duration::from_millis(500);
const FAST_RETX_THRESHOLD: u16 = 3;
const MAX_PAYLOAD_SIZE: usize = 1200; // A reasonable payload size for UDP
const ACK_THRESHOLD: u16 = 2; // Send an ACK for every 2 packets received

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
    /// The frames to send.
    /// 要发送的帧。
    pub frames: Vec<Frame>,
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
    /// We have sent a FIN and are waiting for it to be acknowledged.
    /// The peer may still be sending data.
    /// 我们发送了一个FIN，正在等待它的确认。
    /// 对端可能仍在发送数据。
    Closing,
    /// We have received a FIN from the peer and acknowledged it.
    /// We are waiting for the application to close the connection from our side.
    /// 我们收到了来自对端的FIN并已确认。
    /// 我们正在等待应用程序从我们这边关闭连接。
    FinWait,
    /// The connection is closed.
    /// 连接已关闭。
    Closed,
}

/// The internal state of a connection, separated to satisfy the borrow checker.
/// 连接的内含状态，分离出来是为了满足借用检查器。
struct ConnectionState {
    remote_addr: SocketAddr,
    connection_id: u32,
    start_time: Instant,
    state: State,
    send_buffer: SendBuffer,
    recv_buffer: ReceiveBuffer,
    sender: mpsc::Sender<SendCommand>,
    sequence_number_counter: u32,
    rto_estimator: RttEstimator,
    /// The receive window of the peer, in packets.
    /// 对端的接收窗口（以包为单位）。
    peer_recv_window: u16,
    /// The congestion controller.
    /// 拥塞控制器。
    congestion_controller: CongestionController,
    /// Counts the number of packets received that should trigger an ACK.
    /// 记录收到的、应当触发ACK的包的数量。
    ack_eliciting_packets_since_last_ack: u16,
    /// Waker for the reading task.
    /// 读取任务的 Waker。
    read_waker: Option<std::task::Waker>,
    /// Waker for the writing task.
    /// 写入任务的 Waker。
    write_waker: Option<std::task::Waker>,
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
        connection_id: u32,
        initial_state: State,
        sender: mpsc::Sender<SendCommand>,
    ) -> (Self, mpsc::Sender<Frame>) {
        // We can tune the channel size later.
        // 我们之后可以调整通道的大小。
        let (in_tx, in_rx) = mpsc::channel(128);
        let state = ConnectionState {
            remote_addr,
            connection_id,
            start_time: Instant::now(),
            state: initial_state,
            send_buffer: SendBuffer::new(),
            recv_buffer: ReceiveBuffer::new(),
            sender,
            sequence_number_counter: 0,
            rto_estimator: RttEstimator::new(),
            // Initialize with a small default window until we hear from the peer.
            peer_recv_window: 32,
            congestion_controller: CongestionController::new(),
            ack_eliciting_packets_since_last_ack: 0,
            read_waker: None,
            write_waker: None,
        };
        let connection = Self {
            state,
            receiver: in_rx,
        };
        (connection, in_tx)
    }

    /// Initiates a graceful shutdown of the connection.
    ///
    /// This sends a FIN packet and transitions the connection to the `Closing` state.
    /// The connection will be fully closed once the FIN is acknowledged and the
    /// peer has also closed its side.
    ///
    /// 发起连接的优雅关闭。
    ///
    /// 这会发送一个FIN包，并将连接转换到`Closing`状态。
    /// 一旦FIN被确认并且对端也关闭了它的连接，连接将被完全关闭。
    pub async fn close(&mut self) -> Result<()> {
        self.state.shutdown();
        Ok(())
    }

    /// Runs the connection's main loop to process incoming frames.
    ///
    /// 运行连接的主循环以处理传入的帧。
    pub async fn run(&mut self) -> Result<()> {
        let state = &mut self.state;
        let receiver = &mut self.receiver;
        let mut rto_timer = tokio::time::interval(state.rto_estimator.rto());

        loop {
            // By splitting the struct, the borrow checker now understands
            // that we are operating on disjoint parts.
            state.packetize_stream_data();

            tokio::select! {
                Some(frame) = receiver.recv() => {
                    state.handle_frame(frame).await?;
                }

                res = async {
                    let mut frames_to_send = Vec::new();
                    while state.can_send_more() {
                        if let Some(frame) = state.send_buffer.pop_next_frame() {
                            frames_to_send.push(frame);
                        } else {
                            break; // No more frames to send
                        }
                    }

                    if !frames_to_send.is_empty() {
                        let now = Instant::now();
                        for frame in &frames_to_send {
                            state.send_buffer.add_in_flight(frame.clone(), now);
                        }
                        state.send_frames(frames_to_send).await?;
                    }
                    let res: Result<()> = Ok(());
                    res
                }, if state.send_buffer.has_data_to_send() && state.can_send_more() => {
                    res?;
                }

                _ = rto_timer.tick() => {
                    state.check_for_retransmissions().await?;
                }

                else => {
                    break;
                }
            }

            if state.state == State::Closed {
                break;
            }
        }

        println!("Connection to {} closed.", state.remote_addr);
        state.state = State::Closed;
        Ok(())
    }
}

impl AsyncRead for Connection {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let state = &mut self.get_mut().state;

        // Try to read from the reassembled buffer first.
        let bytes_read = state.recv_buffer.read_from_stream(buf.initialize_unfilled());
        if bytes_read > 0 {
            buf.advance(bytes_read);
            return Poll::Ready(Ok(()));
        }

        // If no data is available, check connection state.
        if state.state == State::Closed || state.state == State::FinWait {
            // If the connection is closed and the buffer is empty, it's EOF.
            return Poll::Ready(Ok(()));
        }

        // If no data is available and the connection is still open,
        // store the waker and return Pending.
        state.read_waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

impl AsyncWrite for Connection {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let state = &mut self.get_mut().state;

        if state.state == State::Closed || state.state == State::Closing {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "Connection is closing or closed",
            )));
        }

        let bytes_written = state.send_buffer.write_to_stream(buf);

        if bytes_written == 0 {
            // Buffer is full, store waker and return pending.
            state.write_waker = Some(cx.waker().clone());
            return Poll::Pending;
        }

        Poll::Ready(Ok(bytes_written))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        // Since packetization and sending happens in the background `run` task,
        // a flush can be considered successful if the data has been moved to the
        // internal buffer. For a more "correct" flush, we would need to wait
        // until the send_buffer is empty, which requires another waker mechanism.
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let state = &mut self.get_mut().state;

        if state.state == State::Established {
            state.shutdown();
        }

        if state.send_buffer.has_data_to_send() {
            // Still data to send, wait.
            state.write_waker = Some(cx.waker().clone());
            Poll::Pending
        } else {
            // Buffer is empty, shutdown is complete.
            Poll::Ready(Ok(()))
        }
    }
}

impl ConnectionState {
    /// Turns data from the stream buffer into PUSH packets.
    /// 将流缓冲区的数据打包成PUSH包。
    fn packetize_stream_data(&mut self) {
        // In connecting state, the first data packet becomes a SYN with payload.
        if self.state == State::Connecting && self.sequence_number_counter == 0 {
            if let Some(chunk) = self.send_buffer.create_chunk(MAX_PAYLOAD_SIZE) {
                println!("Sending 0-RTT data in SYN packet.");
                let syn_header = crate::packet::header::LongHeader {
                    command: crate::packet::command::Command::Syn,
                    protocol_version: 0,
                    connection_id: self.connection_id,
                };
                let frame = Frame::Syn {
                    header: syn_header,
                    payload: chunk,
                };
                self.send_buffer.queue_frame(frame);
                // We don't increment sequence_number_counter for SYN with data yet,
                // as it's implicitly sequence 0. The next PUSH will be 1.
            }
            return; // Only send one SYN
        }

        while let Some(chunk) = self.send_buffer.create_chunk(MAX_PAYLOAD_SIZE) {
            let push_header = crate::packet::header::ShortHeader {
                command: crate::packet::command::Command::Push,
                connection_id: self.connection_id,
                recv_window_size: self.recv_buffer.window_size(),
                timestamp: self.start_time.elapsed().as_millis() as u32,
                sequence_number: self.sequence_number_counter,
                recv_next_sequence: self.recv_buffer.next_sequence(),
            };
            self.sequence_number_counter += 1;

            let frame = Frame::Push {
                header: push_header,
                payload: chunk,
            };

            self.send_buffer.queue_frame(frame);
        }
    }

    /// Handles a single incoming frame based on the current state.
    /// 根据当前状态处理单个传入的帧。
    async fn handle_frame(&mut self, frame: Frame) -> Result<()> {
        println!(
            "Connection to {} in state {:?} received frame: {:?}",
            self.remote_addr, self.state, frame
        );

        match self.state {
            State::Connecting => match frame {
                Frame::Syn { header: _, payload } => {
                    // This is the server-side logic for an incoming connection.
                    // 这是服务器端处理新连接的逻辑。
                    println!("Transitioning to Established state.");
                    self.state = State::Established;

                    if !payload.is_empty() {
                        // This is 0-RTT data.
                        println!("Received 0-RTT data ({} bytes).", payload.len());
                        self.recv_buffer.receive(0, payload);
                        if self.recv_buffer.reassemble() {
                            if let Some(waker) = self.read_waker.take() {
                                waker.wake();
                            }
                        }
                    }

                    // Respond with a SYN-ACK
                    let syn_ack_header = crate::packet::header::LongHeader {
                        command: crate::packet::command::Command::SynAck,
                        protocol_version: 0,
                        connection_id: self.connection_id,
                    };
                    let syn_ack_frame = Frame::SynAck {
                        header: syn_ack_header,
                    };
                    self.send_frames(vec![syn_ack_frame]).await?;
                }
                Frame::SynAck { header: _ } => {
                    // This is the client-side logic when receiving the server's ack.
                    // 这是客户端收到服务器ack时的逻辑。
                    println!("Received SYN-ACK, connection established.");
                    self.state = State::Established;
                    // Our SYN is implicitly acknowledged. The data sent in it can be
                    // considered "in-flight" but since there's no sequence number,
                    // we don't track it for retransmission. We assume it arrived.
                }
                _ => {
                    // Ignore other packets while connecting
                }
            },
            State::Established => match frame {
                Frame::Push { header, payload } => {
                    // Pass the data to the receive buffer for reordering and delivery.
                    // 将数据传递给接收缓冲区进行重排和交付。
                    println!(
                        "Received PUSH with seq={} and {} bytes of payload.",
                        header.sequence_number,
                        payload.len()
                    );
                    self.recv_buffer.receive(header.sequence_number, payload);
                    if self.recv_buffer.reassemble() {
                        // New data is available for reading, wake up the reader task.
                        if let Some(waker) = self.read_waker.take() {
                            waker.wake();
                        }
                    }

                    // Immediate ACK logic with threshold.
                    // 带阈值的即时ACK逻辑。
                    self.ack_eliciting_packets_since_last_ack += 1;
                    if self.ack_eliciting_packets_since_last_ack >= ACK_THRESHOLD
                        && !self.recv_buffer.get_sack_ranges().is_empty()
                    {
                        self.send_ack().await?;
                    }
                }
                Frame::Ack { header, payload } => {
                    self.handle_ack(header, payload).await?;
                }
                Frame::Fin { header } => {
                    println!("Received FIN, entering FIN_WAIT and sending ACK.");
                    // Acknowledge their FIN.
                    self.send_ack_for_fin(&header).await?;
                    // Transition to FinWait, waiting for our side to close.
                    self.state = State::FinWait;
                }
                _ => {
                    // Ignore other unexpected packets in this state.
                    println!("Ignoring unexpected frame in Established state: {:?}", frame);
                }
            },
            State::Closing => match frame {
                Frame::Ack { header, payload } => {
                    self.handle_ack(header, payload).await?;
                    // The peer might also send a FIN in this state.
                }
                Frame::Fin { header } => {
                    println!("Received FIN while Closing, sending ACK.");
                    self.send_ack_for_fin(&header).await?;
                    // If our FIN is acknowledged and we have received and acknowledged their FIN,
                    // we can move to closed. This check happens implicitly as the ACK processing
                    // will clear the in-flight buffer. We might need a more explicit check.
                    // For now, let's assume if we get a FIN here, we can close.
                    if self.state == State::Closing && self.send_buffer.in_flight.is_empty() {
                         self.state = State::Closed;
                    }
                }
                _ => {
                    println!("Ignoring unexpected frame in Closing state: {:?}", frame);
                }
            },
            State::FinWait => match frame {
                // Application has called close(), now we send our FIN.
                // We shouldn't be receiving data frames here, but handle ACKs.
                Frame::Ack { header, payload } => {
                    self.handle_ack(header, payload).await?;
                }
                _ => {
                    println!("Ignoring unexpected frame in FinWait state: {:?}", frame);
                }
            },
            State::Closed => {
                // Should not receive frames in closed state.
            }
        }
        Ok(())
    }

    /// Handles an incoming ACK frame, processing SACK ranges and fast retransmissions.
    /// 处理传入的ACK帧，处理SACK范围和快速重传。
    async fn handle_ack(
        &mut self,
        header: crate::packet::header::ShortHeader,
        payload: bytes::Bytes,
    ) -> Result<()> {
        self.peer_recv_window = header.recv_window_size;

        let sack_ranges = decode_sack_ranges(payload);
        println!("Received ACK with SACK ranges: {:?}", sack_ranges);

        if sack_ranges.is_empty() {
            return Ok(()); // Nothing to do
        }

        let mut acked_seq_numbers = BTreeSet::new();
        for range in sack_ranges {
            for seq in range.start..=range.end {
                acked_seq_numbers.insert(seq);
            }
        }

        let highest_acked_seq = *acked_seq_numbers.iter().next_back().unwrap();

        let now = Instant::now();
        let mut frames_to_fast_retx = Vec::new();

        let previously_in_flight = self.send_buffer.in_flight.len();

        self.send_buffer.in_flight.retain(|packet| {
            if let Some(seq) = packet.frame.sequence_number() {
                if acked_seq_numbers.contains(&seq) {
                    // This packet is acknowledged. Update RTO and remove it.
                    let rtt = now.duration_since(packet.last_sent_at);
                    self.rto_estimator.update(rtt);
                    self.congestion_controller.on_ack(rtt);
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

        // If packets were acknowledged, the send window may have opened up.
        // Wake the writer task if it's waiting.
        if self.send_buffer.in_flight.len() < previously_in_flight {
            if let Some(waker) = self.write_waker.take() {
                waker.wake();
            }
        }

        // After removing acknowledged packets, check for fast retransmissions
        for packet in self.send_buffer.iter_in_flight_mut() {
            if let Some(seq) = packet.frame.sequence_number() {
                if seq < highest_acked_seq {
                    packet.fast_retx_count += 1;
                    if packet.fast_retx_count >= FAST_RETX_THRESHOLD {
                        println!("Fast retransmitting packet with seq={}", seq);
                        frames_to_fast_retx.push(packet.frame.clone());
                        packet.last_sent_at = now;
                        packet.fast_retx_count = 0; // Reset count
                        self.congestion_controller.on_packet_loss();
                    }
                }
            }
        }

        if !frames_to_fast_retx.is_empty() {
            self.send_frames(frames_to_fast_retx).await?;
        }

        // This is the primary way a connection moves to the Closed state.
        // When we are in the 'Closing' state (we have sent a FIN), and the in-flight
        // buffer becomes empty, it means our FIN packet has been acknowledged by the peer.
        // At this point, our side of the connection is done.
        //
        // 这是连接进入Closed状态的主要途径。
        // 当我们处于'Closing'状态（我们已经发送了一个FIN），并且在途缓冲区变空时，
        // 这意味着我们的FIN包已经被对端确认。此时，我们这边的连接就完成了。
        if self.state == State::Closing && self.send_buffer.in_flight.is_empty() {
            println!("Our FIN has been acknowledged. Closing connection.");
            self.state = State::Closed;
        }
        Ok(())
    }

    /// Initiates connection shutdown.
    /// 发起连接关闭。
    fn shutdown(&mut self) {
        if self.state == State::Connecting {
            self.state = State::Closed;
            if let Some(waker) = self.write_waker.take() {
                waker.wake();
            }
            if let Some(waker) = self.read_waker.take() {
                waker.wake();
            }
            return;
        }

        if self.state == State::Established || self.state == State::FinWait {
            let should_send_fin;
            if self.state == State::Established {
                self.state = State::Closing;
                should_send_fin = true;
            } else {
                // We were in FinWait, meaning we already received a FIN.
                // Now that our side is also closing, the connection is fully closed.
                // This assumes our FIN will be sent and eventually ACKed.
                // The final transition to Closed happens when our FIN is acknowledged.
                self.state = State::Closing;
                should_send_fin = true;
            }

            if should_send_fin {
                let fin_header = crate::packet::header::ShortHeader {
                    command: crate::packet::command::Command::Fin,
                    connection_id: self.connection_id,
                    recv_window_size: self.recv_buffer.window_size(),
                    timestamp: self.start_time.elapsed().as_millis() as u32,
                    sequence_number: self.sequence_number_counter,
                    recv_next_sequence: self.recv_buffer.next_sequence(),
                };
                self.sequence_number_counter += 1;

                println!("Sending FIN with seq={}", fin_header.sequence_number);
                let fin_frame = Frame::Fin { header: fin_header };

                self.send_buffer.queue_frame(fin_frame);
                // The main loop will pick it up and send it.
            }
        }
    }

    /// Sends an ACK in response to a FIN.
    /// 发送一个ACK来响应FIN。
    async fn send_ack_for_fin(
        &mut self,
        _fin_header: &crate::packet::header::ShortHeader,
    ) -> Result<()> {
        let ack_frame = self.create_ack_frame();
        self.send_frames(vec![ack_frame]).await?;
        self.ack_eliciting_packets_since_last_ack = 0;
        Ok(())
    }

    /// Creates and sends an ACK frame.
    /// 创建并发送一个ACK帧。
    async fn send_ack(&mut self) -> Result<()> {
        let ack_frame = self.create_ack_frame();
        self.send_frames(vec![ack_frame]).await?;
        self.ack_eliciting_packets_since_last_ack = 0;
        Ok(())
    }

    /// Creates an ACK frame with the current SACK ranges.
    /// 使用当前的SACK范围创建一个ACK帧。
    fn create_ack_frame(&mut self) -> Frame {
        let mut ack_payload = bytes::BytesMut::new();
        let sack_ranges = self.recv_buffer.get_sack_ranges();
        encode_sack_ranges(&sack_ranges, &mut ack_payload);

        let ack_header = crate::packet::header::ShortHeader {
            command: crate::packet::command::Command::Ack,
            connection_id: self.connection_id,
            recv_window_size: self.recv_buffer.window_size(),
            timestamp: self.start_time.elapsed().as_millis() as u32,
            sequence_number: self.sequence_number_counter,
            recv_next_sequence: self.recv_buffer.next_sequence(),
        };
        self.sequence_number_counter += 1;
        Frame::Ack {
            header: ack_header,
            payload: ack_payload.freeze(),
        }
    }


    /// Checks if we are allowed to send more packets based on flow and congestion control.
    /// 根据流量和拥塞控制检查我们是否被允许发送更多的包。
    fn can_send_more(&self) -> bool {
        let in_flight_count = self.send_buffer.in_flight.len() as u32;
        let peer_recv_window = self.peer_recv_window as u32;
        let congestion_window = self.congestion_controller.congestion_window();

        // The number of packets in flight must be less than both the peer's receive
        // window and our own congestion window.
        in_flight_count < peer_recv_window && in_flight_count < congestion_window
    }

    /// Checks for any in-flight packets that have timed out and retransmits them.
    /// 检查是否有任何在途数据包已超时并进行重传。
    async fn check_for_retransmissions(&mut self) -> Result<()> {
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
                self.congestion_controller.on_packet_loss();
            }
        }

        if !frames_to_resend.is_empty() {
            self.send_frames(frames_to_resend).await?;
        }
        Ok(())
    }

    /// Sends a frame to the central sender task.
    /// 发送一个帧到中央发送任务。
    async fn send_frames(&self, frames: Vec<Frame>) -> Result<()> {
        if frames.is_empty() {
            return Ok(());
        }
        let cmd = SendCommand {
            remote_addr: self.remote_addr,
            frames,
        };
        if self.sender.send(cmd).await.is_err() {
            return Err(Error::ChannelClosed);
        }
        Ok(())
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::command::Command;
    use crate::testing::TestHarness;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn test_simple_write_and_ack() {
        let mut harness = TestHarness::new_with_state(State::Established);
        let test_data = b"hello from test";

        // Write data to the connection using the AsyncWrite trait
        harness
            .connection
            .write_all(test_data)
            .await
            .expect("Write failed");

        // Tick the connection to packetize and send the data
        harness.tick().await;

        // The connection should have sent a PUSH frame
        let sent_frames = harness
            .recv_from_connection()
            .await
            .expect("Did not receive frames from connection");
        assert_eq!(sent_frames.len(), 1);
        let push_frame = &sent_frames[0];

        let seq_num = if let Frame::Push { header, payload } = push_frame {
            assert_eq!(payload.as_ref(), test_data);
            header.sequence_number
        } else {
            panic!("Expected a PUSH frame, got {:?}", push_frame);
        };

        // The packet should now be in-flight
        assert_eq!(harness.connection.state.send_buffer.in_flight.len(), 1);

        // Now, simulate the peer sending an ACK for this packet
        let ack_header = crate::packet::header::ShortHeader {
            command: Command::Ack,
            connection_id: 1,
            recv_window_size: 100,
            timestamp: 0,
            sequence_number: 100, // Peer's sequence number, doesn't matter much here
            recv_next_sequence: 1,
        };
        let sack_ranges = vec![crate::packet::sack::SackRange {
            start: seq_num,
            end: seq_num,
        }];
        let mut ack_payload = bytes::BytesMut::new();
        crate::packet::sack::encode_sack_ranges(&sack_ranges, &mut ack_payload);

        let ack_frame = Frame::Ack {
            header: ack_header,
            payload: ack_payload.freeze(),
        };

        harness.send_to_connection(ack_frame).await;

        // Tick the connection to process the ACK
        harness.tick().await;

        // The in-flight buffer should now be empty
        assert!(harness.connection.state.send_buffer.in_flight.is_empty());
    }

    #[tokio::test]
    async fn test_0rtt_write() {
        let mut harness = TestHarness::new_with_state(State::Connecting);
        let test_data = b"0-rtt data";

        // Write data before the connection is established
        harness
            .connection
            .write_all(test_data)
            .await
            .expect("0-RTT write failed");

        // Tick the connection to packetize and send the data
        harness.tick().await;

        // The connection should have sent a SYN frame with the data
        let sent_frames = harness
            .recv_from_connection()
            .await
            .expect("Did not receive frames from connection");
        assert_eq!(sent_frames.len(), 1);
        let syn_frame = &sent_frames[0];

        if let Frame::Syn { header, payload } = syn_frame {
            assert_eq!(payload.as_ref(), test_data);
            assert_eq!(header.command, Command::Syn);
        } else {
            panic!("Expected a SYN frame, got {:?}", syn_frame);
        };

        // Now, simulate the peer sending a SYN-ACK
        let syn_ack_header = crate::packet::header::LongHeader {
            command: Command::SynAck,
            protocol_version: 0,
            connection_id: 1,
        };
        let syn_ack_frame = Frame::SynAck {
            header: syn_ack_header,
        };
        harness.send_to_connection(syn_ack_frame).await;

        // Tick the connection to process the SYN-ACK
        harness.tick().await;

        // The connection should now be in the Established state
        assert_eq!(harness.connection.state.state, State::Established);
    }

    #[tokio::test]
    async fn test_rto_retransmission() {
        let mut harness = TestHarness::new_with_state(State::Established);
        let test_data = b"rto test data";

        harness
            .connection
            .write_all(test_data)
            .await
            .expect("Write failed");
        harness.tick().await;

        // The connection should have sent a PUSH frame. We receive it but don't ACK it.
        let sent_frames = harness
            .recv_from_connection()
            .await
            .expect("Did not receive frames from connection");
        assert_eq!(sent_frames.len(), 1);
        let original_seq_num = sent_frames[0].sequence_number().unwrap();
        assert_eq!(harness.connection.state.send_buffer.in_flight.len(), 1);

        // Advance time past the initial RTO
        let initial_rto = harness.connection.state.rto_estimator.rto();
        harness.advance_time(initial_rto + Duration::from_millis(100)).await;

        // The connection should have retransmitted the packet.
        let retransmitted_frames = harness
            .recv_from_connection()
            .await
            .expect("Did not receive retransmitted frames");
        assert_eq!(retransmitted_frames.len(), 1);
        let retransmitted_seq_num = retransmitted_frames[0].sequence_number().unwrap();

        assert_eq!(
            original_seq_num, retransmitted_seq_num,
            "The sequence number of the retransmitted packet should be the same"
        );
        // The packet is still in-flight
        assert_eq!(harness.connection.state.send_buffer.in_flight.len(), 1);
    }
}
