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
    /// Creates a new `Endpoint` and the channel handles for its `Stream`.
    pub fn new(
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
            state: State::Connecting,
            start_time: Instant::now(),
            reliability,
            congestion_control,
            peer_recv_window: 32, // Default initial value
            config,
            receiver,
            sender,
            rx_from_stream,
            tx_to_stream,
        };

        (endpoint, tx_to_endpoint, rx_from_endpoint)
    }

    /// Helper function to process a single incoming frame.
    async fn handle_frame(&mut self, frame: Frame) -> Result<()> {
        match frame {
            Frame::Syn { header, payload } => {
                // This is the server-side logic for an incoming connection.
                if self.state == State::Connecting {
                    self.state = State::Established;
                    self.peer_cid = header.source_cid;

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
                }
            }
            Frame::Push { header, payload } => {
                self.reliability.receive_push(header.sequence_number, payload);
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
        Ok(())
    }

    /// Runs the endpoint's main event loop.
    pub async fn run(&mut self) -> Result<()> {
        if self.state == State::Connecting {
            self.send_initial_syn().await?;
        }

        loop {
            // Reassemble any ready packets and send them to the handle.
            if let Some(available_data) = self.reliability.reassemble() {
                if self.tx_to_stream.send(available_data).await.is_err() {
                    // Stream handle is dropped, connection is dead.
                    self.state = State::Closed;
                }
            }

            // Packetize any available stream data.
            self.packetize_and_send().await?;

            let rto_deadline = self.reliability.next_rto_deadline();
            let sleep_future = async {
                if let Some(deadline) = rto_deadline {
                    tokio::time::sleep_until(deadline.into()).await;
                } else {
                    // No packets in flight, so sleep indefinitely until another event.
                    std::future::pending::<()>().await;
                }
            };

            tokio::select! {
                Some(mut frame) = self.receiver.recv() => {
                    loop {
                        self.handle_frame(frame).await?;
                        // Try to drain any other frames that are immediately available
                        match self.receiver.try_recv() {
                            Ok(next_frame) => frame = next_frame,
                            Err(_) => break, // Channel is empty or closed
                        }
                    }

                    // Now that we've processed a batch of frames, check if we need to ACK.
                    if self.reliability.should_send_standalone_ack() {
                        self.send_standalone_ack().await?;
                    }
                }

                // A command from the public-facing `Stream` handle.
                Some(cmd) = self.rx_from_stream.recv() => {
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
                    let (rto_occured, frames_to_resend) = self.reliability.check_for_retransmissions();
                    if rto_occured {
                        self.congestion_control.on_packet_loss(Instant::now());
                    }
                    if !frames_to_resend.is_empty() {
                        self.send_frames(frames_to_resend).await?;
                    }
                }

                else => {
                    break;
                }
            }
            if self.state == State::Closing && self.reliability.is_in_flight_empty() {
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
            return Err(Error::ChannelClosed);
        }
        Ok(())
    }
} 