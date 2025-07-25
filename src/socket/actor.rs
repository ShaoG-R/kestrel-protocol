//! The implementation of the central `SocketActor`.

use super::{
    command::{SenderTaskCommand, SocketActorCommand},
    traits::BindableUdpSocket,
};
use crate::{
    config::Config,
    core::{endpoint::Endpoint, stream::Stream},
    error::{Error, Result},
    packet::frame::Frame,
};
use std::{collections::HashMap, net::SocketAddr, sync::Arc};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// Metadata associated with each connection managed by the `ReliableUdpSocket`.
///
/// 与每个由 `ReliableUdpSocket` 管理的连接相关联的元数据。
pub(crate) struct ConnectionMeta {
    /// The channel sender to the connection's `Endpoint` task.
    /// 到连接 `Endpoint` 任务的通道发送端。
    pub(crate) sender: mpsc::Sender<(Frame, SocketAddr)>,
}

/// The actor that owns and manages the UDP socket and all connection state.
///
/// This actor runs in a dedicated task and processes commands from the public
/// `ReliableUdpSocket` handle and incoming UDP packets.
///
/// 拥有并管理UDP套接字和所有连接状态的actor。
///
/// 此actor在专用任务中运行，并处理来自公共 `ReliableUdpSocket` 句柄的命令和传入的UDP数据包。
pub(crate) struct SocketActor<S: BindableUdpSocket> {
    pub(crate) socket: Arc<S>,
    pub(crate) connections: HashMap<u32, ConnectionMeta>,
    pub(crate) addr_to_cid: HashMap<SocketAddr, u32>,
    pub(crate) send_tx: mpsc::Sender<SenderTaskCommand<S>>,
    pub(crate) accept_tx: mpsc::Sender<(Stream, SocketAddr)>,
    pub(crate) command_rx: mpsc::Receiver<SocketActorCommand>,
    pub(crate) command_tx: mpsc::Sender<SocketActorCommand>,
}

impl<S: BindableUdpSocket> SocketActor<S> {
    /// Runs the actor's main event loop.
    pub(crate) async fn run(&mut self) {
        let mut recv_buf = [0u8; 2048]; // Max UDP packet size

        loop {
            tokio::select! {
                // 1. Handle incoming actor commands
                Some(command) = self.command_rx.recv() => {
                    if self.handle_actor_command(command).await.is_err() {
                        // Error during command handling, possibly fatal.
                        break;
                    }
                }
                // 2. Handle incoming UDP packets
                Ok((len, remote_addr)) = self.socket.recv_from(&mut recv_buf) => {
                    debug!(len, addr = %remote_addr, "Received UDP datagram");
                    let mut cursor = &recv_buf[..len];
                    while !cursor.is_empty() {
                        let frame = match Frame::decode(&mut cursor) {
                            Some(frame) => frame,
                            None => {
                                warn!(addr = %remote_addr, "Received an invalid or partially decoded packet");
                                break; // Stop processing this datagram
                            }
                        };
                        self.dispatch_frame(frame, remote_addr).await;
                    }
                }
                else => break,
            }
        }
    }

    /// Handles a command sent to the actor.
    async fn handle_actor_command(&mut self, command: SocketActorCommand) -> Result<()> {
        match command {
            SocketActorCommand::Connect {
                remote_addr,
                config,
                initial_data,
                response_tx,
            } => {
                let local_cid = rand::random();

                let (tx_to_endpoint, rx_from_socket) = mpsc::channel(128);

                let (mut endpoint, tx_to_stream_handle, rx_from_stream_handle) =
                    Endpoint::new_client(
                        config,
                        remote_addr,
                        local_cid,
                        rx_from_socket,
                        self.send_tx.clone(),
                        self.command_tx.clone(),
                        initial_data,
                    );

                tokio::spawn(async move {
                    info!(addr = %remote_addr, cid = %local_cid, "Spawning new endpoint task for outbound connection");
                    if let Err(e) = endpoint.run().await {
                        error!(addr = %remote_addr, cid = %local_cid, "Endpoint closed with error: {}", e);
                    }
                });

                // The key in the `connections` map is always OUR endpoint's CID.
                self.connections.insert(
                    local_cid,
                    ConnectionMeta {
                        sender: tx_to_endpoint,
                    },
                );
                // For outgoing connections, we initially map the remote address to our own CID
                // until the handshake completes and we learn the peer's CID.
                self.addr_to_cid.insert(remote_addr, local_cid);

                let stream = Stream::new(tx_to_stream_handle, rx_from_stream_handle);
                let _ = response_tx.send(Ok(stream));
            }
            SocketActorCommand::Rebind {
                new_local_addr,
                response_tx,
            } => {
                let result = async {
                    let new_socket = Arc::new(S::bind(new_local_addr).await?);
                    self.send_tx
                        .send(SenderTaskCommand::SwapSocket(new_socket.clone()))
                        .await
                        .map_err(|_| Error::ChannelClosed)?;
                    self.socket = new_socket;
                    info!(addr = ?new_local_addr, "Socket rebound to new local address");
                    Ok(())
                }
                .await;
                let _ = response_tx.send(result);
            }
            SocketActorCommand::UpdateAddr { cid, new_addr } => {
                let mut old_addr = None;
                for (addr, &c) in self.addr_to_cid.iter() {
                    if c == cid {
                        old_addr = Some(*addr);
                        break;
                    }
                }
                if let Some(addr) = old_addr {
                    self.addr_to_cid.remove(&addr);
                }
                self.addr_to_cid.insert(new_addr, cid);
            }
        }
        Ok(())
    }

    /// Dispatches a received frame to the appropriate connection task.
    async fn dispatch_frame(&mut self, frame: Frame, remote_addr: SocketAddr) {
        let cid = frame.destination_cid();

        // 1. Try to route to an established connection via its destination CID.
        // This is the primary routing mechanism and supports connection migration.
        // CIDs are non-zero for established connections.
        if cid != 0 {
            if let Some(meta) = self.connections.get(&cid) {
                if meta.sender.send((frame, remote_addr)).await.is_err() {
                    debug!(addr = %remote_addr, cid = %cid, "Endpoint (looked up by CID) died. Removing connection.");
                    self.connections.remove(&cid);
                }
                return;
            }
        }

        // 2. Handle packets for connections that are still in handshake (e.g. retransmitted SYN)
        // by looking up the remote address.
        if let Some(&existing_cid) = self.addr_to_cid.get(&remote_addr) {
            if let Some(meta) = self.connections.get(&existing_cid) {
                if meta.sender.send((frame, remote_addr)).await.is_err() {
                     debug!(addr = %remote_addr, cid = %existing_cid, "Endpoint (looked up by addr) died. Removing connection.");
                     self.connections.remove(&existing_cid);
                     self.addr_to_cid.remove(&remote_addr);
                }
                return;
            }
        }

        // 3. If the packet could not be routed, it must be a SYN for a new connection.
        if let Frame::Syn { header, .. } = &frame {
            let config = Config::default();
            if header.protocol_version != config.protocol_version {
                warn!(
                    addr = %remote_addr,
                    client_version = header.protocol_version,
                    server_version = config.protocol_version,
                    "Dropping SYN with incompatible protocol version."
                );
                return;
            }

            info!(addr = %remote_addr, "Accepting new connection attempt.");
            let peer_cid = header.source_cid;
            let local_cid = rand::random(); // This is OUR CID for the connection.
            let (tx_to_endpoint, rx_from_socket) = mpsc::channel(128);

            let (mut endpoint, tx_to_stream_handle, rx_from_stream_handle) =
                Endpoint::new_server(
                    config,
                    remote_addr,
                    local_cid,
                    peer_cid,
                    rx_from_socket,
                    self.send_tx.clone(),
                    self.command_tx.clone(),
                );
            
            tokio::spawn(async move {
                info!(addr = %remote_addr, cid = %local_cid, "Spawning new endpoint task for inbound connection");
                if let Err(e) = endpoint.run().await {
                    error!(addr = %remote_addr, cid = %local_cid, "Endpoint closed with error: {}", e);
                }
            });

            // Add to connections map using OUR local_cid.
            self.connections.insert(
                local_cid,
                ConnectionMeta {
                    sender: tx_to_endpoint.clone(),
                },
            );
            // Add to addr->cid map to find this connection for retransmitted SYNs.
            self.addr_to_cid.insert(remote_addr, local_cid);

            let stream = Stream::new(tx_to_stream_handle, rx_from_stream_handle);
            // Use `try_send` to avoid blocking the actor if the listener is slow or the
            // channel is full. This is crucial for preventing deadlocks.
            if self.accept_tx.try_send((stream, remote_addr)).is_err() {
                warn!(
                    "Listener channel full or closed, dropping new connection from {}",
                    remote_addr
                );
                // Clean up the state we just added for this failed connection attempt.
                self.connections.remove(&local_cid);
                self.addr_to_cid.remove(&remote_addr);
                // The spawned endpoint task will eventually time out and die on its own
                // because it will never receive a SYN-ACK confirmation from the user.
            }
        } else {
            debug!(
                "Ignoring non-SYN packet from unknown source {} with unroutable CID {}: {:?}",
                remote_addr, cid, frame
            );
        }
    }
} 