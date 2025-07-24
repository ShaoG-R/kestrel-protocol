//! The endpoint of a connection, which is the "brain" of the new layered protocol.
//!
//! It owns the reliability layer and is responsible for processing incoming and
//! outgoing packets, managing connection state, and orchestrating the different
//! protocol layers.
//!
//! 连接的端点，是新分层协议的“大脑”。
//!
//! 它拥有可靠性层，并负责处理传入和传出的包，管理连接状态，以及协调不同的协议层。

use crate::{
    config::Config,
    congestion::vegas::Vegas,
    core::reliability::ReliabilityLayer,
    error::{Error, Result},
    packet::{
        frame::Frame,
        header::{LongHeader, ShortHeader},
        sack::{decode_sack_ranges, encode_sack_ranges},
    },
    socket::SendCommand,
};
use bytes::{Bytes, BytesMut};
use std::net::SocketAddr;
use tokio::{
    sync::mpsc,
    time::{Instant, sleep_until},
};
use tracing::info;

/// Commands sent from the `Stream` handle to the `Endpoint` worker.
#[derive(Debug)]
pub enum StreamCommand {
    SendData(Bytes),
    Close,
}

/// The state of a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Connecting,
    SynReceived,
    Established,
    Closing,
    FinWait,
    Closed,
}

/// Represents one end of a reliable connection.
pub struct Endpoint {
    remote_addr: SocketAddr,
    local_cid: u32,
    peer_cid: u32,
    state: State,
    start_time: Instant,
    reliability: ReliabilityLayer,
    peer_recv_window: u32,
    config: Config,
    last_recv_time: Instant,
    receiver: mpsc::Receiver<Frame>,
    sender: mpsc::Sender<SendCommand>,
    rx_from_stream: mpsc::Receiver<StreamCommand>,
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
        initial_data: Option<Bytes>,
    ) -> (Self, mpsc::Sender<StreamCommand>, mpsc::Receiver<Bytes>) {
        let (tx_to_endpoint, rx_from_stream) = mpsc::channel(128);
        let (tx_to_stream, rx_from_endpoint) = mpsc::channel(128);

        let congestion_control = Box::new(Vegas::new(config.clone()));
        let mut reliability = ReliabilityLayer::new(config.clone(), congestion_control);
        if let Some(data) = initial_data {
            // Immediately write the 0-RTT data to the stream buffer.
            reliability.write_to_stream(&data);
        }
        let now = Instant::now();

        let endpoint = Self {
            remote_addr,
            local_cid,
            peer_cid: 0,
            state: State::Connecting,
            start_time: now,
            reliability,
            peer_recv_window: 32,
            config,
            last_recv_time: now,
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

        let congestion_control = Box::new(Vegas::new(config.clone()));
        let reliability = ReliabilityLayer::new(config.clone(), congestion_control);
        let now = Instant::now();

        let endpoint = Self {
            remote_addr,
            local_cid,
            peer_cid,
            state: State::SynReceived,
            start_time: now,
            reliability,
            peer_recv_window: 32,
            config,
            last_recv_time: now,
            receiver,
            sender,
            rx_from_stream,
            tx_to_stream,
        };

        (endpoint, tx_to_endpoint, rx_from_endpoint)
    }

    /// Runs the endpoint's main event loop.
    pub async fn run(&mut self) -> Result<()> {
        if self.state == State::Connecting {
            self.send_initial_syn().await?;
        }

        loop {
            // In SynReceived, we don't set a timeout. We wait for the user to accept.
            let next_wakeup = if self.state == State::SynReceived {
                Instant::now() + self.config.idle_timeout // Effectively, sleep forever until a message
            } else {
                self.reliability
                    .next_rto_deadline()
                    .unwrap_or_else(|| Instant::now() + self.config.idle_timeout)
            };

            tokio::select! {
                biased; // Prioritize incoming packets and user commands

                // 1. Handle frames from the network
                Some(frame) = self.receiver.recv() => {
                    self.handle_frame(frame).await?;
                    // After handling one frame, try to drain any other pending frames
                    // to process them in a batch.
                    while let Ok(frame) = self.receiver.try_recv() {
                        self.handle_frame(frame).await?;
                    }
                }

                // 2. Handle commands from the user stream
                Some(cmd) = self.rx_from_stream.recv() => {
                    self.handle_stream_command(cmd).await?;
                }

                // 3. Handle timeouts
                _ = sleep_until(next_wakeup) => {
                    self.handle_timeout(Instant::now()).await?;
                }

                // 4. Stop if all channels are closed
                else => break,
            }

            // After handling all immediate events, perform follow-up actions.

            // 5. Reassemble data and send to the user stream
            if let Some(data) = self.reliability.reassemble() {
                if self.tx_to_stream.send(data).await.is_err() {
                    self.state = State::Closed; // Handle dropped
                }
            }

            // 6. Packetize and send any pending user data, but only if established.
            if self.state == State::Established {
                self.packetize_and_send().await?;
            }
            
            // 7. Check if we need to close the connection
            if self.should_close() {
                break;
            }
        }
        Ok(())
    }
    
    async fn handle_frame(&mut self, frame: Frame) -> Result<()> {
        self.last_recv_time = Instant::now();
        match frame {
            Frame::Syn { .. } => {
                // This is now primarily handled by the Socket creating the Endpoint.
                // If we receive another SYN, it might be a retransmission from the client
                // because it hasn't received our SYN-ACK yet.
                if self.state == State::SynReceived {
                    info!(cid = self.local_cid, "Received duplicate SYN, ignoring.");
                    // If we have already been triggered to send a SYN-ACK (i.e., data is in the
                    // send buffer), we can resend it.
                    if !self.reliability.is_send_buffer_empty() {
                        self.send_syn_ack().await?;
                    }
                }
            }
            Frame::SynAck { header, payload } => {
                if self.state == State::Connecting {
                    self.state = State::Established;
                    self.peer_cid = header.source_cid;
                    info!(cid = self.local_cid, "Connection established (client-side)");
                    if !payload.is_empty() {
                        self.reliability.receive_push(0, payload);
                    }

                    // Acknowledge the SYN-ACK, potentially with piggybacked data.
                    if !self.reliability.is_send_buffer_empty() {
                        self.packetize_and_send().await?;
                    } else {
                        self.send_standalone_ack().await?;
                    }
                }
            }
            Frame::Push { header, payload } => {
                self.reliability.receive_push(header.sequence_number, payload);
                // Always send an ACK for a PUSH frame to ensure timely delivery.
                self.send_standalone_ack().await?;
            }
            Frame::Ack { header, payload } => {
                self.peer_recv_window = header.recv_window_size as u32;
                let sack_ranges = decode_sack_ranges(payload);
                let frames_to_retx = self
                    .reliability
                    .handle_ack(header.recv_next_sequence, sack_ranges, Instant::now());
                if !frames_to_retx.is_empty() {
                    self.send_frames(frames_to_retx).await?;
                }
            }
            Frame::Fin { header, .. } => {
                self.reliability.receive_fin(header.sequence_number);
                self.send_standalone_ack().await?;
                // The other side has closed their writing end. We can no longer receive
                // any more data. We signal this to the user stream by closing the channel.
                // The Endpoint will shut down shortly after.
                self.tx_to_stream.closed().await;
                self.state = State::Closed;
            }
            _ => {}
        }
        Ok(())
    }

    async fn handle_stream_command(&mut self, cmd: StreamCommand) -> Result<()> {
        match cmd {
            StreamCommand::SendData(data) => {
                self.reliability.write_to_stream(&data);
                // If this is the first data sent on a server-side connection,
                // it triggers the SYN-ACK and establishes the connection.
                if self.state == State::SynReceived {
                    self.state = State::Established;
                    info!(cid = self.local_cid, "Connection accepted by user, sending SYN-ACK.");
                    self.send_syn_ack().await?;
                }
            }
            StreamCommand::Close => {
                self.shutdown();
                // Immediately attempt to send a FIN packet.
                self.packetize_and_send().await?;
            }
        }
        Ok(())
    }

    async fn handle_timeout(&mut self, now: Instant) -> Result<()> {
        let frames_to_resend = self.reliability.check_for_retransmissions(now);
        if !frames_to_resend.is_empty() {
            self.send_frames(frames_to_resend).await?;
        }

        if now.saturating_duration_since(self.last_recv_time) > self.config.idle_timeout {
            info!(cid = self.local_cid, "Connection timed out");
            self.state = State::Closed;
        }
        Ok(())
    }
    
    fn should_close(&mut self) -> bool {
        if self.state == State::Closing && self.reliability.is_in_flight_empty() {
            info!(cid = self.local_cid, "All data ACKed, closing now.");
            self.state = State::Closed;
        }
        self.state == State::Closed
    }

    fn shutdown(&mut self) {
        if self.state == State::Established || self.state == State::FinWait {
            self.state = State::Closing;
        }
    }

    async fn packetize_and_send(&mut self) -> Result<()> {
        let mut frames_to_send = self
            .reliability
            .packetize_stream_data(self.peer_cid, Instant::now(), self.start_time);

        if self.state == State::Closing && !self.reliability.has_fin_in_flight() {
            let fin_frame = self.create_fin_frame();
            self.reliability.add_fin_to_in_flight(fin_frame.clone(), Instant::now());
            frames_to_send.push(fin_frame);
        }

        if !frames_to_send.is_empty() {
            self.send_frames(frames_to_send).await?;
        }
        Ok(())
    }

    async fn send_initial_syn(&mut self) -> Result<()> {
        let initial_payload = self.reliability.take_stream_buffer();

        // The SYN packet itself will be retransmitted on timeout if not acknowledged.
        // The reliability of the payload is tied to the SYN packet.
        // We don't need to add it to the in-flight queue separately, as that's for
        // sequence-numbered PUSH frames.

        let syn_header = LongHeader {
            command: crate::packet::command::Command::Syn,
            protocol_version: self.config.protocol_version,
            destination_cid: 0, // Server's CID is unknown
            source_cid: self.local_cid,
        };
        let frame = Frame::Syn {
            header: syn_header,
            payload: initial_payload,
        };
        self.send_frames(vec![frame]).await
    }

    async fn send_syn_ack(&mut self) -> Result<()> {
        let payload = self.reliability.take_stream_buffer();
        let syn_ack_header = LongHeader {
            command: crate::packet::command::Command::SynAck,
            protocol_version: self.config.protocol_version,
            destination_cid: self.peer_cid,
            source_cid: self.local_cid,
        };
        let frame = Frame::SynAck {
            header: syn_ack_header,
            payload,
        };
        self.send_frames(vec![frame]).await
    }

    async fn send_standalone_ack(&mut self) -> Result<()> {
        let (sack_ranges, recv_next, window_size) = self.reliability.get_ack_info();
        if !self.reliability.is_ack_pending() {
            return Ok(());
        }

        let mut ack_payload = BytesMut::with_capacity(sack_ranges.len() * 8);
        encode_sack_ranges(&sack_ranges, &mut ack_payload);

        let ack_header = ShortHeader {
            command: crate::packet::command::Command::Ack,
            connection_id: self.peer_cid,
            recv_window_size: window_size,
            timestamp: Instant::now().duration_since(self.start_time).as_millis() as u32,
            sequence_number: 0, // ACK frames do not have a sequence number
            recv_next_sequence: recv_next,
        };

        let frame = Frame::Ack { header: ack_header, payload: ack_payload.freeze() };
        self.send_frames(vec![frame]).await?;
        self.reliability.on_ack_sent();
        Ok(())
    }

    fn create_fin_frame(&mut self) -> Frame {
        let fin_header = ShortHeader {
            command: crate::packet::command::Command::Fin,
            connection_id: self.peer_cid,
            recv_window_size: 0,
            timestamp: Instant::now().duration_since(self.start_time).as_millis() as u32,
            sequence_number: self.reliability.next_sequence_number(),
            recv_next_sequence: 0,
        };
        Frame::Fin { header: fin_header }
    }

    async fn send_frames(&self, frames: Vec<Frame>) -> Result<()> {
        if frames.is_empty() {
            return Ok(());
        }
        let cmd = SendCommand {
            remote_addr: self.remote_addr,
            frames,
        };
        self.sender.send(cmd).await.map_err(|_| Error::ChannelClosed)
    }
}
