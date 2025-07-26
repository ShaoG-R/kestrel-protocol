//! Constructors for the `Endpoint`.

use super::{
    state::ConnectionState,
    {Endpoint, StreamCommand},
};
use crate::{
    config::Config,
    congestion::vegas::Vegas,
    core::reliability::ReliabilityLayer,
    socket::{AsyncUdpSocket, SenderTaskCommand, SocketActorCommand},
};
use bytes::Bytes;
use std::net::SocketAddr;
use tokio::{
    sync::mpsc,
    time::Instant,
};

impl<S: AsyncUdpSocket> Endpoint<S> {
    /// Creates a new `Endpoint` for the client-side.
    pub fn new_client(
        config: Config,
        remote_addr: SocketAddr,
        local_cid: u32,
        receiver: mpsc::Receiver<(crate::packet::frame::Frame, SocketAddr)>,
        sender: mpsc::Sender<SenderTaskCommand<S>>,
        command_tx: mpsc::Sender<SocketActorCommand>,
        initial_data: Option<Bytes>,
    ) -> (Self, mpsc::Sender<StreamCommand>, mpsc::Receiver<Vec<Bytes>>) {
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
            state: ConnectionState::Connecting,
            start_time: now,
            reliability,
            peer_recv_window: 32,
            config,
            last_recv_time: now,
            receiver,
            sender,
            command_tx,
            rx_from_stream,
            tx_to_stream: Some(tx_to_stream),
            fin_pending_eof: false,
        };

        (endpoint, tx_to_endpoint, rx_from_endpoint)
    }

    /// Creates a new `Endpoint` for the server-side.
    pub fn new_server(
        config: Config,
        remote_addr: SocketAddr,
        local_cid: u32,
        peer_cid: u32,
        receiver: mpsc::Receiver<(crate::packet::frame::Frame, SocketAddr)>,
        sender: mpsc::Sender<SenderTaskCommand<S>>,
        command_tx: mpsc::Sender<SocketActorCommand>,
    ) -> (Self, mpsc::Sender<StreamCommand>, mpsc::Receiver<Vec<Bytes>>) {
        let (tx_to_endpoint, rx_from_stream) = mpsc::channel(128);
        let (tx_to_stream, rx_from_endpoint) = mpsc::channel(128);

        let congestion_control = Box::new(Vegas::new(config.clone()));
        let reliability = ReliabilityLayer::new(config.clone(), congestion_control);
        let now = Instant::now();

        let endpoint = Self {
            remote_addr,
            local_cid,
            peer_cid,
            state: ConnectionState::SynReceived,
            start_time: now,
            reliability,
            peer_recv_window: 32,
            config,
            last_recv_time: now,
            receiver,
            sender,
            command_tx,
            rx_from_stream,
            tx_to_stream: Some(tx_to_stream),
            fin_pending_eof: false,
        };

        (endpoint, tx_to_endpoint, rx_from_endpoint)
    }
} 