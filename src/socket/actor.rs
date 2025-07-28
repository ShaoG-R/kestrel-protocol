//! The implementation of the central `SocketActor`.
//!
//! `SocketActor` 核心实现。

use super::{
    command::{SenderTaskCommand, SocketActorCommand},
    draining::DrainingPool,
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
    pub(crate) draining_pool: DrainingPool,
    pub(crate) config: Arc<Config>,
    pub(crate) send_tx: mpsc::Sender<SenderTaskCommand<S>>,
    pub(crate) accept_tx: mpsc::Sender<(Stream, SocketAddr)>,
    pub(crate) command_rx: mpsc::Receiver<SocketActorCommand>,
    pub(crate) command_tx: mpsc::Sender<SocketActorCommand>,
}

impl<S: BindableUdpSocket> SocketActor<S> {
    /// Runs the actor's main event loop.
    ///
    /// 运行 actor 的主事件循环。
    pub(crate) async fn run(&mut self) {
        let mut recv_buf = [0u8; 2048]; // Max UDP packet size. 最大UDP包大小。
        let mut cleanup_interval =
            tokio::time::interval(self.config.connection.draining_cleanup_interval);

        loop {
            tokio::select! {
                // 1. Handle incoming actor commands.
                // 1. 处理传入的 actor 命令。
                Some(command) = self.command_rx.recv() => {
                    if self.handle_actor_command(command).await.is_err() {
                        // Error during command handling, possibly fatal.
                        // 命令处理期间出错，可能是致命的。
                        break;
                    }
                }
                // 2. Handle incoming UDP packets.
                // 2. 处理传入的UDP数据包。
                Ok((len, remote_addr)) = self.socket.recv_from(&mut recv_buf) => {
                    debug!(len, addr = %remote_addr, "Received UDP datagram");

                    // Immediately copy the received data into an owned buffer. This is the
                    // definitive fix for the race condition. By creating an owned copy,
                    // we ensure that the buffer being processed by this task cannot be
                    // overwritten by a subsequent `recv_from` call in the `select!` loop
                    // when this task `await`s. The `Frame`s decoded below will hold
                    // slices pointing to `datagram_buf`, which is safe.
                    //
                    // 立即将接收到的数据复制到一个拥有的缓冲区中。这是对竞争条件的最终修复。
                    // 通过创建拥有的副本，我们确保此任务正在处理的缓冲区不会在 `select!`
                    // 循环中被后续的 `recv_from` 调用覆盖，当此任务 `await` 时。
                    // 下面解码的 `Frame` 将持有指向 `datagram_buf` 的切片，这是安全的。
                    let datagram_buf = recv_buf[..len].to_vec();

                    // Decode all frames from the datagram first before dispatching any.
                    // 在分发之前，首先从数据报中解码所有帧。
                    let mut frames = Vec::new();
                    let mut cursor = &datagram_buf[..];
                    while !cursor.is_empty() {
                        let frame = match Frame::decode(&mut cursor) {
                            Some(frame) => frame,
                            None => {
                                warn!(addr = %remote_addr, "Received an invalid or partially decoded packet");
                                break; // Stop processing this datagram. 停止处理此数据报。
                            }
                        };
                        frames.push(frame);
                    }

                    // Now that all frames are safely decoded, dispatch them.
                    // 现在所有帧都已安全解码，开始分发它们。
                    if !frames.is_empty() {
                        // Check if the first frame indicates a new connection attempt.
                        // 检查第一帧是否表示新的连接尝试。
                        if let Frame::Syn { .. } = &frames[0] {
                            self.handle_new_connection(frames, remote_addr).await;
                        } else {
                            // Otherwise, dispatch frames individually to existing connections.
                            // 否则，将帧单独分派到现有连接。
                            for frame in frames {
                                self.dispatch_frame(frame, remote_addr).await;
                            }
                        }
                    }
                }
                // 3. Handle periodic cleanup of draining CIDs.
                // 3. 处理 draining CIDs 的定期清理。
                _ = cleanup_interval.tick() => {
                    self.draining_pool.cleanup();
                }
                else => break,
            }
        }
    }

    /// Handles a command sent to the actor.
    ///
    /// 处理发送给 actor 的命令。
    async fn handle_actor_command(&mut self, command: SocketActorCommand) -> Result<()> {
        match command {
            SocketActorCommand::Connect {
                remote_addr,
                config,
                initial_data,
                response_tx,
            } => {
                let mut local_cid = rand::random();
                while self.connections.contains_key(&local_cid)
                    || self.draining_pool.contains(&local_cid)
                {
                    local_cid = rand::random();
                }

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
                        // Connection timeouts are expected in certain scenarios (e.g., connection replacement),
                        // so we log them at a lower level to reduce noise.
                        // 连接超时在某些场景下是预期的（例如连接替换），所以我们以较低级别记录以减少噪音。
                        match e {
                            crate::error::Error::ConnectionTimeout => {
                                debug!(addr = %remote_addr, cid = %local_cid, "Endpoint closed due to timeout: {}", e);
                            }
                            _ => {
                                error!(addr = %remote_addr, cid = %local_cid, "Endpoint closed with error: {}", e);
                            }
                        }
                    }
                });

                // The key in the `connections` map is always OUR endpoint's CID.
                // `connections` 映射中的键始终是我们端点的 CID。
                self.connections.insert(
                    local_cid,
                    ConnectionMeta {
                        sender: tx_to_endpoint,
                    },
                );
                // For outgoing connections, we initially map the remote address to our own CID
                // until the handshake completes and we learn the peer's CID.
                // 对于出站连接，我们最初将远程地址映射到我们自己的CID，
                // 直到握手完成并获知对端的CID。
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
            SocketActorCommand::RemoveConnection { cid } => {
                self.remove_connection_by_cid(cid);
            }
        }
        Ok(())
    }

    /// Dispatches a received frame to an appropriate (and existing) connection task.
    ///
    /// 将接收到的帧分派给一个合适的（已存在的）连接任务。
    async fn dispatch_frame(&mut self, frame: Frame, remote_addr: SocketAddr) {
        let cid = frame.destination_cid();

        // 1. Try to route to an established connection via its destination CID.
        //    This is the primary routing mechanism and supports connection migration.
        //    CIDs are non-zero for established connections.
        // 1. 尝试通过目标CID路由到已建立的连接。
        //    这是主要的路由机制，并支持连接迁移。
        //    已建立连接的CID不为零。
        if cid != 0 {
            if let Some(meta) = self.connections.get(&cid) {
                if meta.sender.send((frame, remote_addr)).await.is_err() {
                    debug!(addr = %remote_addr, cid = %cid, "Endpoint (CID lookup) died. Removing.");
                    self.remove_connection_by_cid(cid);
                }
                return;
            }
        }

        // 2. Handle packets for connections that are still in handshake (e.g. retransmitted SYN)
        //    by looking up the remote address.
        // 2. 通过查找远程地址来处理仍处于握手状态的连接的数据包（例如重传的SYN）。
        if let Some(&existing_cid) = self.addr_to_cid.get(&remote_addr) {
            if let Some(meta) = self.connections.get(&existing_cid) {
                if meta.sender.send((frame, remote_addr)).await.is_err() {
                    debug!(addr = %remote_addr, cid = %existing_cid, "Endpoint (addr lookup) died. Removing.");
                    self.remove_connection_by_cid(existing_cid);
                }
                return;
            }
        }

        // 3. If we get here, it's an unroutable, non-SYN packet.
        //    We also check the draining CIDs to provide better logging for why a packet might be dropped.
        // 3. 如果执行到这里，说明这是一个不可路由的、非SYN的数据包。
        //    我们同时会检查draining CIDs，以便为数据包为何被丢弃提供更好的日志。
        if self.draining_pool.contains(&cid) {
            debug!(
                "Ignoring packet for draining connection from {} with CID {}: {:?}",
                remote_addr, cid, frame
            );
        } else {
            debug!(
                "Ignoring non-SYN packet from unknown source {} with unroutable CID {}: {:?}",
                remote_addr, cid, frame
            );
        }
    }

    /// Handles a new connection attempt, which may include 0-RTT data frames.
    ///
    /// 处理新的连接尝试，其中可能包含0-RTT数据帧。
    async fn handle_new_connection(
        &mut self,
        mut frames: Vec<Frame>,
        remote_addr: SocketAddr,
    ) {
        // The first frame must be a SYN.
        // 第一帧必须是 SYN。
        if frames.is_empty() {
            return;
        }
        let first_frame = frames.remove(0);

        if let Frame::Syn { header } = first_frame {
            // If a connection already exists for this address, the new SYN indicates
            // that the client has abandoned the old one and wants to start fresh.
            // We honor this by tearing down the old state before creating the new one.
            // 如果此地址已存在连接，则新的SYN表示客户端已放弃旧连接并希望重新开始。
            // 我们通过在创建新连接之前拆除旧状态来满足此请求。
            if let Some(&old_cid) = self.addr_to_cid.get(&remote_addr) {
                info!(
                    addr = %remote_addr,
                    old_cid = old_cid,
                    "Received new SYN from an address with a lingering connection. Replacing it."
                );
                self.remove_connection_by_cid(old_cid);
            }

            let config = self.config.as_ref();
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
            let mut local_cid = rand::random(); // This is OUR CID for the connection. 这是我们用于此连接的CID。
            while self.connections.contains_key(&local_cid)
                || self.draining_pool.contains(&local_cid)
            {
                local_cid = rand::random();
            }
            let (tx_to_endpoint, rx_from_socket) = mpsc::channel(128);

            let (mut endpoint, tx_to_stream_handle, rx_from_stream_handle) =
                Endpoint::new_server(
                    config.clone(),
                    remote_addr,
                    local_cid,
                    peer_cid,
                    rx_from_socket,
                    self.send_tx.clone(),
                    self.command_tx.clone(),
                );

            // Before spawning the endpoint, send any 0-RTT PUSH frames to it.
            // This ensures the frames are in its queue before it even starts running.
            // 在生成端点之前，向其发送任何0-RTT的PUSH帧。
            // 这可以确保这些帧在端点开始运行之前就已在其队列中。
            for frame in frames {
                if let Frame::Push { .. } = &frame {
                    if tx_to_endpoint.try_send((frame, remote_addr)).is_err() {
                        warn!(addr = %remote_addr, "Failed to send 0-RTT frame to new endpoint, channel might be full. Dropping frame.");
                    }
                } else {
                    warn!(addr = %remote_addr, "Received non-PUSH frame immediately after SYN in a 0-RTT packet. Ignoring: {:?}", frame);
                }
            }

            tokio::spawn(async move {
                info!(addr = %remote_addr, cid = %local_cid, "Spawning new endpoint task for inbound connection");
                if let Err(e) = endpoint.run().await {
                    // Connection timeouts are expected in certain scenarios (e.g., connection replacement),
                    // so we log them at a lower level to reduce noise.
                    // 连接超时在某些场景下是预期的（例如连接替换），所以我们以较低级别记录以减少噪音。
                    match e {
                        crate::error::Error::ConnectionTimeout => {
                            debug!(addr = %remote_addr, cid = %local_cid, "Endpoint closed due to timeout: {}", e);
                        }
                        _ => {
                            error!(addr = %remote_addr, cid = %local_cid, "Endpoint closed with error: {}", e);
                        }
                    }
                }
            });

            // Add to connections map using OUR local_cid.
            // 使用我们的 local_cid 添加到 connections 映射。
            self.connections.insert(
                local_cid,
                ConnectionMeta {
                    sender: tx_to_endpoint,
                },
            );
            // Add to addr->cid map to find this connection for retransmitted SYNs.
            // 添加到 addr->cid 映射，以便为重传的 SYN 找到此连接。
            self.addr_to_cid.insert(remote_addr, local_cid);

            let stream = Stream::new(tx_to_stream_handle, rx_from_stream_handle);
            // Use `try_send` to avoid blocking the actor if the listener is slow or the
            // channel is full. This is crucial for preventing deadlocks.
            // 使用 `try_send` 避免在监听器慢或通道满时阻塞 actor。
            // 这对于防止死锁至关重要。
            if self.accept_tx.try_send((stream, remote_addr)).is_err() {
                warn!(
                    "Listener channel full or closed, dropping new connection from {}",
                    remote_addr
                );
                // Clean up the state we just added for this failed connection attempt.
                // 清理我们刚刚为这个失败的连接尝试添加的状态。
                self.connections.remove(&local_cid);
                self.addr_to_cid.remove(&remote_addr);
                // The spawned endpoint task will eventually time out and die on its own
                // because it will never receive a SYN-ACK confirmation from the user.
                // 生成的端点任务最终会超时并自行消亡，因为它永远不会收到用户的SYN-ACK确认。
            }
        } else {
            warn!("handle_new_connection called with a non-SYN frame as the first frame");
        }
    }

    /// Removes a connection and its associated state from the actor.
    ///
    /// This is the single authoritative place for connection cleanup.
    /// It removes the connection from the main CID map and also cleans up
    /// the temporary address-to-CID mapping used during handshakes.
    ///
    /// 移除一个连接及其关联的状态。
    ///
    /// 这是连接清理的唯一权威位置。它会从主CID映射中移除连接，
    /// 并清理握手期间使用的临时地址到CID的映射。
    fn remove_connection_by_cid(&mut self, cid: u32) {
        if self.connections.remove(&cid).is_none() {
            return; // Already removed, nothing to do. 已被移除，无需任何操作。
        }

        // Find and remove the corresponding address mapping.
        // 查找并移除对应的地址映射。
        self.addr_to_cid.retain(|_addr, c| *c != cid);
        
        // Instead of forgetting the CID, move it to the draining state.
        // 不直接丢弃CID，而是将其移至draining状态。
        self.draining_pool.insert(cid);

        info!(cid = %cid, "Cleaned up connection state. CID is now in draining state.");
    }
} 