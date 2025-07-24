//! The endpoint of a connection, which is the "brain" of the new layered protocol.
//!
//! It owns the reliability layer and the congestion control layer, and is responsible
//! for processing incoming and outgoing packets.
//!
//! 连接的端点，是新分层协议的“大脑”。
//!
//! 它拥有可靠性层和拥塞控制层，并负责处理传入和传出的包。

use crate::config::Config;
use crate::congestion::{vegas::Vegas, CongestionControl};
use crate::core::reliability::ReliabilityLayer;
use crate::error::{Error, Result};
use crate::packet::frame::Frame;
use crate::packet::header::{LongHeader, ShortHeader};
use crate::packet::sack::{decode_sack_ranges, encode_sack_ranges};
use crate::socket::SendCommand;
use bytes::Bytes;
use std::net::SocketAddr;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, info, warn};

/// Commands sent from the `Stream` handle to the `Endpoint` worker.
#[derive(Debug)]
pub enum StreamCommand {
    /// User data to be sent reliably.
    SendData(Bytes),
    /// Request to gracefully close the connection.
    Close,
}

/// The state of a connection.
/// 连接的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Connecting,
    Established,
    Closing,
    FinWait,
    Closed,
}

/// Represents one end of a reliable connection.
///
/// 代表一个可靠连接的一端。
pub struct Endpoint {
    remote_addr: SocketAddr,
    local_cid: u32,
    peer_cid: u32,
    state: State,
    start_time: Instant,
    reliability: ReliabilityLayer,
    congestion_control: Box<dyn CongestionControl>,
    /// The receive window of the peer, in packets.
    peer_recv_window: u32,
    config: Config,
    /// Tracks when the last packet was received from the peer.
    last_recv_time: Instant,
    /// Receives frames from the main socket task.
    receiver: mpsc::Receiver<Frame>,
    /// Sends commands to the central socket sender task.
    sender: mpsc::Sender<SendCommand>,
    /// Receives commands from the `Stream` handle.
    rx_from_stream: mpsc::Receiver<StreamCommand>,
    /// Sends reassembled, ordered data to the `Stream` handle.
    tx_to_stream: mpsc::Sender<Bytes>,
}

impl Endpoint {
    /// Creates a new `Endpoint` for the client-side.
    pub fn new_client(
        config: Config,
        remote_addr: SocketAddr,
        local_cid: u32,
        receiver: mpsc::Receiver<Frame>,
        sender: mpsc::Sender<SendCommand>,
    ) -> (Self, mpsc::Sender<StreamCommand>, mpsc::Receiver<Bytes>) {
        let (tx_to_endpoint, rx_from_stream) = mpsc::channel(128);
        let (tx_to_stream, rx_from_endpoint) = mpsc::channel(128);

        let reliability = ReliabilityLayer::new(config.clone());
        let congestion_control = Box::new(Vegas::new(config.clone()));

        let endpoint = Self {
            remote_addr,
            local_cid,
            peer_cid: 0, // Client doesn't know peer's CID initially
            state: State::Connecting,
            start_time: Instant::now(),
            reliability,
            congestion_control,
            peer_recv_window: 32, // Default initial value
            config,
            last_recv_time: Instant::now(),
            receiver,
            sender,
            rx_from_stream,
            tx_to_stream,
        };

        (endpoint, tx_to_endpoint, rx_from_endpoint)
    }

    /// Creates a new `Endpoint` for the server-side.
    pub fn new_server(
        config: Config,
        remote_addr: SocketAddr,
        local_cid: u32,
        peer_cid: u32,
        receiver: mpsc::Receiver<Frame>,
        sender: mpsc::Sender<SendCommand>,
    ) -> (Self, mpsc::Sender<StreamCommand>, mpsc::Receiver<Bytes>) {
        let (tx_to_endpoint, rx_from_stream) = mpsc::channel(128);
        let (tx_to_stream, rx_from_endpoint) = mpsc::channel(128);

        let reliability = ReliabilityLayer::new(config.clone());
        let congestion_control = Box::new(Vegas::new(config.clone()));

        let endpoint = Self {
            remote_addr,
            local_cid,
            peer_cid,
            state: State::Established, // Server starts as established after receiving SYN
            start_time: Instant::now(),
            reliability,
            congestion_control,
            peer_recv_window: 32, // Default initial value
            config,
            last_recv_time: Instant::now(),
            receiver,
            sender,
            rx_from_stream,
            tx_to_stream,
        };

        (endpoint, tx_to_endpoint, rx_from_endpoint)
    }

    /// Helper function to process a single incoming frame.
    async fn handle_frame(&mut self, frame: Frame) -> Result<bool> {
        debug!(cid = self.local_cid, ?frame, "Handling frame");
        self.last_recv_time = Instant::now();
        let mut ack_was_sent = false;
        match frame {
            Frame::Syn { header, payload } => {
                // This is the server-side logic for an incoming connection.
                if self.state == State::Connecting {
                    self.state = State::Established;
                    self.peer_cid = header.source_cid;
                    info!(
                        cid = self.local_cid,
                        peer_cid = self.peer_cid,
                        "Connection established (server-side)"
                    );

                    if !payload.is_empty() {
                        self.reliability.receive_push(0, payload);
                    }

                    // Respond with a SYN-ACK
                    let syn_ack_header = LongHeader {
                        command: crate::packet::command::Command::SynAck,
                        protocol_version: self.config.protocol_version,
                        destination_cid: self.peer_cid,
                        source_cid: self.local_cid,
                    };
                    let syn_ack_frame = Frame::SynAck {
                        header: syn_ack_header,
                    };
                    self.send_frames(vec![syn_ack_frame]).await?;
                }
            }
            Frame::SynAck { header } => {
                if self.state == State::Connecting {
                    self.state = State::Established;
                    self.peer_cid = header.source_cid;
                    info!(
                        cid = self.local_cid,
                        peer_cid = self.peer_cid,
                        "Connection established (client-side)"
                    );
                    // Acknowledge the SYN-ACK to let the server know we are ready.
                    // This also serves as a keep-alive.
                    self.send_standalone_ack().await?;
                    ack_was_sent = true;
                }
            }
            Frame::Push { header, payload } => {
                self.reliability
                    .receive_push(header.sequence_number, payload);
                // Immediately acknowledge the push to prevent sender from timing out.
                // This follows the "Immediate ACK" principle.
                self.send_standalone_ack().await?;
                ack_was_sent = true;
            }
            Frame::Ack { header, payload } => {
                self.peer_recv_window = header.recv_window_size as u32;

                let sack_ranges = decode_sack_ranges(payload);
                let (loss_detected, rtt_sample, frames_to_fast_retx) =
                    self.reliability.handle_ack(sack_ranges);

                if let Some(rtt) = rtt_sample {
                    self.congestion_control.on_ack(rtt);
                }

                if loss_detected {
                    self.congestion_control.on_packet_loss(Instant::now());
                }

                if !frames_to_fast_retx.is_empty() {
                    self.send_frames(frames_to_fast_retx).await?;
                }
            }
            Frame::Fin { header: _ } => {
                self.state = State::FinWait;
                self.send_standalone_ack().await?;
            }
            _ => {
                // TODO: Handle other frame types
            }
        }
        Ok(ack_was_sent)
    }

    /// Runs the endpoint's main event loop.
    pub async fn run(&mut self) -> Result<()> {
        if self.state == State::Connecting {
            debug!(cid = self.local_cid, "Sending initial SYN");
            self.send_initial_syn().await?;
        }

        loop {
            // Reassemble any ready packets and send them to the handle.
            if let Some(available_data) = self.reliability.reassemble() {
                debug!(
                    cid = self.local_cid,
                    bytes = available_data.len(),
                    "Sending reassembled data to user stream"
                );
                if self.tx_to_stream.send(available_data).await.is_err() {
                    // Stream handle is dropped, connection is dead.
                    self.state = State::Closed;
                }
            }

            // Packetize any available stream data.
            self.packetize_and_send().await?;

            let now = Instant::now();
            let next_wakeup = self
                .reliability
                .next_rto_deadline()
                .unwrap_or_else(|| now + self.config.idle_timeout);

            let sleep_future = tokio::time::sleep_until(next_wakeup.into());
            debug!(
                cid = self.local_cid,
                ?next_wakeup,
                "Waiting for event..."
            );

            tokio::select! {
                Some(mut frame) = self.receiver.recv() => {
                    debug!(cid = self.local_cid, "Received event: Network Frame");
                    let mut ack_was_sent_in_batch = false;
                    loop {
                        let ack_sent_this_frame = self.handle_frame(frame).await?;
                        ack_was_sent_in_batch |= ack_sent_this_frame;

                        // Try to drain any other frames that are immediately available
                        match self.receiver.try_recv() {
                            Ok(next_frame) => frame = next_frame,
                            Err(_) => break, // Channel is empty or closed
                        }
                    }

                    // Now that we've processed a batch of frames, check if we need to ACK,
                    // but only if we haven't already sent one for a PUSH in this batch.
                    debug!(cid = self.local_cid, "Finished processing batch of network frames");
                    if !ack_was_sent_in_batch && self.reliability.should_send_standalone_ack() {
                        debug!(cid = self.local_cid, "Sending standalone ACK after batch");
                        self.send_standalone_ack().await?;
                    }
                }

                // A command from the public-facing `Stream` handle.
                Some(cmd) = self.rx_from_stream.recv() => {
                    debug!(cid = self.local_cid, "Received event: User Command {:?}", cmd);
                    match cmd {
                        StreamCommand::SendData(data) => {
                            self.reliability.write_to_stream(&data);
                        }
                        StreamCommand::Close => {
                            self.shutdown();
                        }
                    }
                }

                _ = sleep_future => {
                    debug!(cid = self.local_cid, "Received event: RTO Timer Expired");
                    let (rto_occured, frames_to_resend) = self.reliability.check_for_retransmissions();
                    if rto_occured {
                        self.congestion_control.on_packet_loss(Instant::now());
                    }
                    if !frames_to_resend.is_empty() {
                        self.send_frames(frames_to_resend).await?;
                    }

                    // Check for idle timeout
                    if now.saturating_duration_since(self.last_recv_time) > self.config.idle_timeout {
                        info!(cid = self.local_cid, "Connection timed out due to inactivity.");
                        self.state = State::Closed;
                    }
                }

                else => {
                    break;
                }
            }
            if self.state == State::Closing && self.reliability.is_in_flight_empty() {
                info!(cid = self.local_cid, "All in-flight packets ACKed. Closing.");
                self.state = State::Closed;
            }

            if self.state == State::Closed {
                break;
            }
        }
        Ok(())
    }

    fn shutdown(&mut self) {
        if self.state == State::Connecting {
            self.state = State::Closed;
            return;
        }

        if self.state == State::Established || self.state == State::FinWait {
            info!(cid = self.local_cid, "Shutdown initiated. Changing state to Closing.");
            self.state = State::Closing;
        }
    }

    /// Checks if we are allowed to send more packets based on flow and congestion control.
    fn can_send_more(&self) -> bool {
        let in_flight_count = self.reliability.in_flight_count() as u32;
        let congestion_window = self.congestion_control.congestion_window();

        in_flight_count < self.peer_recv_window && in_flight_count < congestion_window
    }

    /// Takes data from the reliability layer's stream buffer, packetizes it,
    /// and sends it. Also sends FIN packet if the state is Closing.
    async fn packetize_and_send(&mut self) -> Result<()> {
        let mut frames_to_send = Vec::new();

        // Packetize PUSH frames until the window is full.
        while self.can_send_more() {
            let now = Instant::now();
            let (_, next_seq_to_ack, our_recv_window) = self.reliability.get_ack_info();
            let mut frames = self.reliability.packetize_stream_data(
                self.peer_cid,
                our_recv_window,
                next_seq_to_ack,
                now,
            );
            if frames.is_empty() {
                break;
            }
            frames_to_send.append(&mut frames);
        }

        // If we are closing and there isn't a FIN already in flight, send one.
        if self.state == State::Closing && !self.reliability.has_fin_in_flight() {
            debug!(cid = self.local_cid, "Packetizing FIN frame");
            let fin_header = ShortHeader {
                command: crate::packet::command::Command::Fin,
                connection_id: self.peer_cid,
                recv_window_size: 0, // Not important in FIN
                timestamp: self.start_time.elapsed().as_millis() as u32,
                sequence_number: self.reliability.next_sequence_number(),
                recv_next_sequence: 0, // Not important in FIN
            };
            let fin_frame = Frame::Fin { header: fin_header };
            self.reliability.add_fin_to_in_flight(fin_frame.clone());
            frames_to_send.push(fin_frame);
        }

        if !frames_to_send.is_empty() {
            debug!(
                cid = self.local_cid,
                num_frames = frames_to_send.len(),
                "Sending packetized frames"
            );
            self.send_frames(frames_to_send).await?;
        }
        Ok(())
    }

    /// Sends the initial SYN packet to start the connection.
    /// This may include 0-RTT data.
    async fn send_initial_syn(&mut self) -> Result<()> {
        // TODO: This doesn't actually pull 0-RTT data yet.
        // We need to call a packetize method on reliability layer first.
        let syn_header = LongHeader {
            command: crate::packet::command::Command::Syn,
            protocol_version: self.config.protocol_version,
            destination_cid: self.peer_cid, // Initially 0 for server
            source_cid: self.local_cid,
        };
        let frame = Frame::Syn {
            header: syn_header,
            payload: bytes::Bytes::new(), // Placeholder for 0-RTT data
        };

        // We don't use send_frames here because SYN packets don't get added
        // to the reliability layer's in-flight queue in the same way.
        // They are implicitly acknowledged by the SYN-ACK.
        let cmd = SendCommand {
            remote_addr: self.remote_addr,
            frames: vec![frame],
        };
        self.sender.send(cmd).await.map_err(|_| Error::ChannelClosed)
    }

    /// Creates and sends an ACK frame that is not piggybacked on any other data.
    async fn send_standalone_ack(&mut self) -> Result<()> {
        let (sack_ranges, recv_next, window_size) = self.reliability.get_ack_info();
        if sack_ranges.is_empty() {
            return Ok(());
        }

        let mut ack_payload = bytes::BytesMut::new();
        encode_sack_ranges(&sack_ranges, &mut ack_payload);

        let ack_header = ShortHeader {
            command: crate::packet::command::Command::Ack,
            connection_id: self.peer_cid,
            recv_window_size: window_size,
            timestamp: self.start_time.elapsed().as_millis() as u32,
            sequence_number: self.reliability.next_sequence_number(),
            recv_next_sequence: recv_next,
        };

        let ack_frame = Frame::Ack {
            header: ack_header,
            payload: ack_payload.freeze(),
        };

        self.send_frames(vec![ack_frame]).await?;
        self.reliability.on_ack_sent();
        Ok(())
    }

    /// Sends a vector of frames to the central sender task.
    async fn send_frames(&self, frames: Vec<Frame>) -> Result<()> {
        if frames.is_empty() {
            return Ok(());
        }
        let cmd = SendCommand {
            remote_addr: self.remote_addr,
            frames,
        };
        if self.sender.send(cmd).await.is_err() {
            // This indicates the socket's sender task has shut down,
            // which is a fatal error for this connection.
            warn!(cid = self.local_cid, "Failed to send command to socket sender task");
            return Err(Error::ChannelClosed);
        }
        Ok(())
    }
} 