//! Constructors for the `Endpoint`.

use crate::core::endpoint::{Endpoint, StreamCommand};
use crate::socket::{Transport, TransportCommand};
use crate::{
    config::Config,
    core::reliability::ReliabilityLayer,
    socket::SocketActorCommand,
    error::Result,
};
use bytes::Bytes;
use std::net::SocketAddr;
use tokio::sync::mpsc;
use crate::core::endpoint::lifecycle::{ConnectionLifecycleManager, DefaultLifecycleManager};
use crate::core::endpoint::timing::TimingManager;
use crate::core::endpoint::types::{
    channels::ChannelManager,
    identity::ConnectionIdentity,
    state::ConnectionState,
    transport::TransportManager,
};
use crate::core::reliability::congestion::vegas::Vegas;

impl<T: Transport> Endpoint<T> {
    /// Creates a new `Endpoint` for the client-side.
    pub fn new_client(
        config: Config,
        remote_addr: SocketAddr,
        local_cid: u32,
        receiver: mpsc::Receiver<(crate::packet::frame::Frame, SocketAddr)>,
        sender: mpsc::Sender<TransportCommand<T>>,
        command_tx: mpsc::Sender<SocketActorCommand>,
        initial_data: Option<Bytes>,
    ) -> Result<(Self, mpsc::Sender<StreamCommand>, mpsc::Receiver<Vec<Bytes>>)> {
        let (tx_to_endpoint, rx_from_stream) = mpsc::channel(128);
        let (tx_to_stream, rx_from_endpoint) = mpsc::channel(128);

        let congestion_control = Box::new(Vegas::new(config.clone()));
        let mut reliability = ReliabilityLayer::new(config.clone(), congestion_control);
        if let Some(data) = initial_data {
            // Immediately write the 0-RTT data to the stream buffer.
            reliability.write_to_stream(data);
        }

        let mut lifecycle_manager = DefaultLifecycleManager::new(
            ConnectionState::Connecting,
            config.clone(),
        );
        lifecycle_manager.initialize(local_cid, remote_addr)?;

        let endpoint = Self {
            identity: ConnectionIdentity::new(local_cid, 0, remote_addr),
            timing: TimingManager::new(),
            transport: TransportManager::new(reliability),
            lifecycle_manager,
            config,
            channels: ChannelManager::new(
                receiver,
                sender,
                command_tx,
                rx_from_stream,
                Some(tx_to_stream),
            ),
        };

        Ok((endpoint, tx_to_endpoint, rx_from_endpoint))
    }

    /// Creates a new `Endpoint` for the server-side.
    pub fn new_server(
        config: Config,
        remote_addr: SocketAddr,
        local_cid: u32,
        peer_cid: u32,
        receiver: mpsc::Receiver<(crate::packet::frame::Frame, SocketAddr)>,
        sender: mpsc::Sender<TransportCommand<T>>,
        command_tx: mpsc::Sender<SocketActorCommand>,
    ) -> Result<(Self, mpsc::Sender<StreamCommand>, mpsc::Receiver<Vec<Bytes>>)> {
        let (tx_to_endpoint, rx_from_stream) = mpsc::channel(128);
        let (tx_to_stream, rx_from_endpoint) = mpsc::channel(128);

        let congestion_control = Box::new(Vegas::new(config.clone()));
        let reliability = ReliabilityLayer::new(config.clone(), congestion_control);

        let mut lifecycle_manager = DefaultLifecycleManager::new(
            ConnectionState::SynReceived,
            config.clone(),
        );
        lifecycle_manager.initialize(local_cid, remote_addr)?;

        let endpoint = Self {
            identity: ConnectionIdentity::new(local_cid, peer_cid, remote_addr),
            timing: TimingManager::new(),
            transport: TransportManager::new(reliability),
            lifecycle_manager,
            config,
            channels: ChannelManager::new(
                receiver,
                sender,
                command_tx,
                rx_from_stream,
                Some(tx_to_stream),
            ),
        };

        Ok((endpoint, tx_to_endpoint, rx_from_endpoint))
    }
} 