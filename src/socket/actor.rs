//! The implementation of the transport-based `SocketActor`.
//!
//! 基于传输的 `SocketActor` 实现。

use super::{
    command::SocketActorCommand,
    routing::FrameRouter,
    transport::{BindableTransport, TransportManager},
};
use crate::{
    config::Config,
    core::{endpoint::Endpoint, stream::Stream},
    error::{Result},
    packet::frame::Frame,
};
use std::{net::SocketAddr, sync::Arc};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// Metadata associated with each connection managed by the `ReliableUdpSocket`.
///
/// 与每个由 `ReliableUdpSocket` 管理的连接相关联的元数据。
#[derive(Debug)]
pub(crate) struct ConnectionMeta {
    /// The channel sender to the connection's `Endpoint` task.
    /// 到连接 `Endpoint` 任务的通道发送端。
    pub(crate) sender: mpsc::Sender<(Frame, SocketAddr)>,
}

/// The actor that owns and manages the transport and all connection state.
///
/// This actor runs in a dedicated task and processes commands from the public
/// `ReliableUdpSocket` handle and incoming frames from the transport.
///
/// 拥有并管理传输和所有连接状态的actor。
///
/// 此actor在专用任务中运行，并处理来自公共 `ReliableUdpSocket` 句柄的命令和来自传输的传入帧。
pub(crate) struct TransportSocketActor<T: BindableTransport> {
    /// 传输管理器 - 负责所有底层传输操作
    /// Transport manager - responsible for all low-level transport operations
    pub(crate) transport_manager: TransportManager<T>,
    /// 帧路由管理器 - 负责帧路由和连接映射管理
    /// Frame router manager - responsible for frame routing and connection mapping
    pub(crate) frame_router: FrameRouter,
    pub(crate) config: Arc<Config>,
    pub(crate) accept_tx: mpsc::Sender<(Stream, SocketAddr)>,
    pub(crate) command_rx: mpsc::Receiver<SocketActorCommand>,
    pub(crate) command_tx: mpsc::Sender<SocketActorCommand>,
}

impl<T: BindableTransport> TransportSocketActor<T> {
    /// Runs the actor's main event loop.
    ///
    /// 运行 actor 的主事件循环。
    pub(crate) async fn run(&mut self) {
        let mut cleanup_interval =
            tokio::time::interval(self.config.connection.draining_cleanup_interval);

        loop {
            // 获取传输引用用于接收操作
            // Get transport reference for receive operations
            let transport = self.transport_manager.transport();
            
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
                // 2. Handle incoming frames from transport.
                // 2. 处理来自传输的传入帧。
                Ok(datagram) = transport.recv_frames() => {
                    debug!(
                        addr = %datagram.remote_addr,
                        frame_count = datagram.frames.len(),
                        "Received frame batch from transport"
                    );

                    if !datagram.frames.is_empty() {
                        // Check if the first frame indicates a new connection attempt.
                        // 检查第一帧是否表示新的连接尝试。
                        if let Frame::Syn { .. } = &datagram.frames[0] {
                            self.handle_new_connection(datagram.frames, datagram.remote_addr).await;
                        } else {
                            // Otherwise, dispatch frames individually to existing connections.
                            // 否则，将帧单独分派到现有连接。
                            for frame in datagram.frames {
                                self.dispatch_frame(frame, datagram.remote_addr).await;
                            }
                        }
                    }
                }
                // 3. Handle periodic cleanup of draining CIDs.
                // 3. 处理 draining CIDs 的定期清理。
                _ = cleanup_interval.tick() => {
                    self.frame_router.cleanup_draining_pool();
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
                while self.frame_router.connection_exists(local_cid)
                    || self.frame_router.is_draining(local_cid)
                {
                    local_cid = rand::random();
                }

                let (tx_to_endpoint, rx_from_socket) = mpsc::channel(128);

                let transport_tx = self.transport_manager.send_tx();

                let (mut endpoint, tx_to_stream_handle, rx_from_stream_handle) =
                    Endpoint::new_client(
                        config,
                        remote_addr,
                        local_cid,
                        rx_from_socket,
                        transport_tx,
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

                // Register the new connection with the frame router
                // 在帧路由器中注册新连接
                self.frame_router.register_connection(
                    local_cid,
                    remote_addr,
                    ConnectionMeta {
                        sender: tx_to_endpoint,
                    },
                );

                let stream = Stream::new(tx_to_stream_handle, rx_from_stream_handle);
                let _ = response_tx.send(Ok(stream));
            }
            SocketActorCommand::Rebind {
                new_local_addr,
                response_tx,
            } => {
                let result = self.transport_manager.rebind(new_local_addr).await.map(|_| ());
                let _ = response_tx.send(result);
            }
            SocketActorCommand::UpdateAddr { cid, new_addr } => {
                self.frame_router.update_connection_address(cid, new_addr);
            }
            SocketActorCommand::RemoveConnection { cid } => {
                self.frame_router.remove_connection_by_cid(cid);
            }
        }
        Ok(())
    }

    /// Dispatches a received frame to an appropriate (and existing) connection task.
    ///
    /// 将接收到的帧分派给一个合适的（已存在的）连接任务。
    async fn dispatch_frame(&mut self, frame: Frame, remote_addr: SocketAddr) {
        // 委托给帧路由管理器处理
        // Delegate to frame router manager for handling
        let _result = self.frame_router.dispatch_frame(frame, remote_addr).await;
        // RoutingResult is already logged appropriately in the frame router
        // RoutingResult已在帧路由器中适当记录
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
            // If a connection already exists for this address, the frame router will handle
            // the replacement automatically when we register the new connection.
            // 如果此地址已存在连接，帧路由器在我们注册新连接时会自动处理替换。

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
            while self.frame_router.connection_exists(local_cid)
                || self.frame_router.is_draining(local_cid)
            {
                local_cid = rand::random();
            }
            let (tx_to_endpoint, rx_from_socket) = mpsc::channel(128);

            let transport_tx = self.transport_manager.send_tx();

            let (mut endpoint, tx_to_stream_handle, rx_from_stream_handle) =
                Endpoint::new_server(
                    config.clone(),
                    remote_addr,
                    local_cid,
                    peer_cid,
                    rx_from_socket,
                    transport_tx,
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

            // Register the connection with the frame router
            // 在帧路由器中注册连接
            self.frame_router.register_connection(
                local_cid,
                remote_addr,
                ConnectionMeta {
                    sender: tx_to_endpoint,
                },
            );

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
                self.frame_router.remove_connection_by_cid(local_cid);
                // The spawned endpoint task will eventually time out and die on its own
                // because it will never receive a SYN-ACK confirmation from the user.
                // 生成的端点任务最终会超时并自行消亡，因为它永远不会收到用户的SYN-ACK确认。
            }
        } else {
            warn!("handle_new_connection called with a non-SYN frame as the first frame");
        }
    }


}