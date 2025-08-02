//! Socket会话协调器 - 统一协调各层交互的中央控制器
//! Socket Session Coordinator - Central controller that coordinates inter-layer interactions
//!
//! 该模块提供了SocketSessionCoordinator，作为传输管理、帧路由管理和连接生命周期管理的
//! 统一协调器。它负责处理新连接的创建、握手协商、0-RTT数据处理，以及连接ID的生成和冲突检测。
//!
//! This module provides SocketSessionCoordinator as a unified coordinator for transport management,
//! frame routing management, and connection lifecycle management. It handles new connection creation,
//! handshake negotiation, 0-RTT data processing, and connection ID generation with conflict detection.

use crate::core::endpoint::timing::TimeoutEvent;
use crate::socket::event_loop::routing::FrameRouter;
use crate::socket::{
    command::SocketActorCommand,
    event_loop::ConnectionMeta,
    transport::{BindableTransport, TransportManager},
};
use crate::{
    config::Config,
    core::{endpoint::Endpoint, stream::Stream},
    error::{Error, Result},
    packet::frame::Frame,
    timer::{HybridTimerTaskHandle, task::types::SenderCallback},
};
use std::{net::SocketAddr, sync::Arc};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// 服务器端点创建结果类型别名
/// Type alias for server endpoint creation result
type ServerEndpointBundle<T> = (Endpoint<T>, mpsc::Sender<(Frame, SocketAddr)>, Stream);

/// Socket会话协调器 - 统一协调各层交互的中央控制器
/// Socket Session Coordinator - Central controller that coordinates inter-layer interactions
///
/// 该组件作为传输层、路由层和连接生命周期层之间的协调器，负责：
/// - 新连接的创建和协商
/// - 连接ID的生成和冲突检测
/// - 握手协商和0-RTT数据处理
/// - 连接状态的管理和清理
///
/// This component serves as a coordinator between transport layer, routing layer, and
/// connection lifecycle layer, responsible for:
/// - New connection creation and negotiation
/// - Connection ID generation and conflict detection
/// - Handshake negotiation and 0-RTT data processing
/// - Connection state management and cleanup
pub(crate) struct SocketSessionCoordinator<T: BindableTransport> {
    /// 传输管理器
    /// Transport manager
    transport_manager: TransportManager<T>,
    /// 帧路由管理器  
    /// Frame router manager
    frame_router: FrameRouter,
    /// 配置信息
    /// Configuration
    config: Arc<Config>,
    /// 向用户层发送新连接的通道
    /// Channel for sending new connections to user layer
    accept_tx: mpsc::Sender<(Stream, SocketAddr)>,
    /// 发送命令到上层的通道
    /// Channel for sending commands to upper layer
    command_tx: mpsc::Sender<SocketActorCommand>,
    /// 混合并行定时器任务句柄
    /// Hybrid parallel timer task handle
    timer_handle: HybridTimerTaskHandle<TimeoutEvent, SenderCallback<TimeoutEvent>>,
}

impl<T: BindableTransport> SocketSessionCoordinator<T> {
    /// 创建新的会话协调器
    /// Create a new session coordinator
    pub(crate) fn new(
        transport_manager: TransportManager<T>,
        frame_router: FrameRouter,
        config: Arc<Config>,
        accept_tx: mpsc::Sender<(Stream, SocketAddr)>,
        command_tx: mpsc::Sender<SocketActorCommand>,
        timer_handle: HybridTimerTaskHandle<TimeoutEvent, SenderCallback<TimeoutEvent>>,
    ) -> Self {
        Self {
            transport_manager,
            frame_router,
            config,
            accept_tx,
            command_tx,
            timer_handle,
        }
    }

    /// 获取传输管理器的引用
    /// Gets a reference to the transport manager
    pub(crate) fn transport_manager(&self) -> &TransportManager<T> {
        &self.transport_manager
    }

    /// 获取传输管理器的可变引用
    /// Gets a mutable reference to the transport manager
    pub(crate) fn transport_manager_mut(&mut self) -> &mut TransportManager<T> {
        &mut self.transport_manager
    }

    /// 获取帧路由器的可变引用
    /// Gets a mutable reference to the frame router
    pub(crate) fn frame_router_mut(&mut self) -> &mut FrameRouter {
        &mut self.frame_router
    }

    /// 生成唯一的连接ID，避免与现有连接和排水池中的连接冲突
    /// Generate a unique connection ID, avoiding conflicts with existing connections and draining pool
    ///
    /// 该方法会持续生成随机连接ID，直到找到一个不与现有连接或排水池中的连接冲突的ID。
    /// 这确保了每个连接都有唯一的标识符。
    ///
    /// This method continuously generates random connection IDs until it finds one that doesn't
    /// conflict with existing connections or connections in the draining pool.
    /// This ensures each connection has a unique identifier.
    fn generate_unique_connection_id(&self) -> u32 {
        loop {
            let candidate_cid = rand::random();
            if !self.frame_router.connection_exists(candidate_cid)
                && !self.frame_router.is_draining(candidate_cid)
            {
                debug!(cid = candidate_cid, "Generated unique connection ID");
                return candidate_cid;
            }
            debug!(
                cid = candidate_cid,
                "Connection ID collision detected, retrying"
            );
        }
    }

    /// 验证协议版本兼容性
    /// Validate protocol version compatibility
    ///
    /// 该方法检查接收到的SYN帧中的协议版本是否与服务器配置的版本兼容。
    /// 版本不匹配将导致连接被拒绝。
    ///
    /// This method checks if the protocol version in the received SYN frame is compatible
    /// with the server's configured version. Version mismatch will result in connection rejection.
    fn validate_protocol_version(&self, client_version: u8, remote_addr: SocketAddr) -> bool {
        if client_version != self.config.protocol_version {
            warn!(
                addr = %remote_addr,
                client_version = client_version,
                server_version = self.config.protocol_version,
                "Dropping SYN with incompatible protocol version"
            );
            false
        } else {
            true
        }
    }

    /// 处理0-RTT数据帧
    /// Handle 0-RTT data frames
    ///
    /// 该方法处理与SYN帧一起发送的0-RTT数据帧。这些帧会被转发给新创建的端点，
    /// 以便在连接建立过程中就能开始处理应用数据。
    ///
    /// This method handles 0-RTT data frames sent together with the SYN frame.
    /// These frames are forwarded to the newly created endpoint so that application
    /// data can start being processed during connection establishment.
    async fn handle_zero_rtt_frames(
        &self,
        frames: Vec<Frame>,
        tx_to_endpoint: &mpsc::Sender<(Frame, SocketAddr)>,
        remote_addr: SocketAddr,
    ) {
        debug!(
            addr = %remote_addr,
            frame_count = frames.len(),
            "Processing 0-RTT frames"
        );

        for frame in frames {
            if let Frame::Push { .. } = &frame {
                if let Err(e) = tx_to_endpoint.try_send((frame, remote_addr)) {
                    warn!(
                        addr = %remote_addr,
                        error = %e,
                        "Failed to send 0-RTT frame to new endpoint, channel might be full. Dropping frame"
                    );
                }
            } else {
                warn!(
                    addr = %remote_addr,
                    frame = ?frame,
                    "Received non-PUSH frame immediately after SYN in a 0-RTT packet. Ignoring"
                );
            }
        }
    }

    /// 创建新的服务器端连接端点
    /// Create a new server-side connection endpoint
    ///
    /// 该方法负责创建服务器端的连接端点，配置相关的通道和参数，
    /// 并返回端点实例及其关联的流句柄。
    ///
    /// This method is responsible for creating a server-side connection endpoint,
    /// configuring related channels and parameters, and returning the endpoint
    /// instance along with its associated stream handle.
    async fn create_server_endpoint(
        &self,
        local_cid: u32,
        peer_cid: u32,
        remote_addr: SocketAddr,
    ) -> Result<ServerEndpointBundle<T>> {
        let (tx_to_endpoint, rx_from_socket) = mpsc::channel(128);
        let transport_tx = self.transport_manager.send_tx();

        let (endpoint, tx_to_stream_handle, rx_from_stream_handle) = Endpoint::new_server(
            self.config.as_ref().clone(),
            remote_addr,
            local_cid,
            peer_cid,
            rx_from_socket,
            transport_tx,
            self.command_tx.clone(),
            self.timer_handle.clone(),
        )
        .await?;

        let stream = Stream::new(tx_to_stream_handle, rx_from_stream_handle);

        Ok((endpoint, tx_to_endpoint, stream))
    }

    /// 注册新连接到帧路由器
    /// Register new connection to frame router
    ///
    /// 该方法将新建立的连接注册到帧路由器中，以便后续的帧能够
    /// 正确路由到相应的连接端点。
    ///
    /// This method registers the newly established connection to the frame router
    /// so that subsequent frames can be correctly routed to the corresponding
    /// connection endpoint.
    async fn register_connection_to_router(
        &mut self,
        local_cid: u32,
        remote_addr: SocketAddr,
        tx_to_endpoint: mpsc::Sender<(Frame, SocketAddr)>,
    ) {
        self.frame_router
            .register_connection(
                local_cid,
                remote_addr,
                ConnectionMeta {
                    sender: tx_to_endpoint,
                },
            )
            .await;

        debug!(
            cid = local_cid,
            addr = %remote_addr,
            "Connection registered to frame router"
        );
    }

    /// 将新连接发送给用户应用
    /// Send new connection to user application
    ///
    /// 该方法尝试将新建立的连接流发送给等待的用户应用。
    /// 如果发送失败（如监听器通道已满），则会清理相关状态。
    ///
    /// This method attempts to send the newly established connection stream
    /// to the waiting user application. If sending fails (e.g., listener
    /// channel is full), it will clean up the related state.
    async fn send_connection_to_user(
        &mut self,
        stream: Stream,
        remote_addr: SocketAddr,
        local_cid: u32,
    ) -> Result<()> {
        if let Err(e) = self.accept_tx.try_send((stream, remote_addr)) {
            warn!(
                addr = %remote_addr,
                cid = local_cid,
                error = %e,
                "Listener channel full or closed, dropping new connection"
            );

            // Clean up the state we just added for this failed connection attempt.
            // 清理我们刚刚为这个失败的连接尝试添加的状态。
            self.frame_router.remove_connection_by_cid(local_cid);

            return Err(Error::ChannelClosed);
        }

        info!(
            addr = %remote_addr,
            cid = local_cid,
            "New connection successfully sent to user application"
        );

        Ok(())
    }

    /// 处理新的连接尝试，包含完整的连接生命周期管理
    /// Handle new connection attempts with complete connection lifecycle management
    ///
    /// 这是连接生命周期管理的核心方法，负责：
    /// 1. 验证SYN帧和协议版本
    /// 2. 生成唯一的连接ID
    /// 3. 创建和配置连接端点
    /// 4. 处理0-RTT数据
    /// 5. 注册连接并发送给用户应用
    ///
    /// This is the core method for connection lifecycle management, responsible for:
    /// 1. Validating SYN frame and protocol version
    /// 2. Generating unique connection ID
    /// 3. Creating and configuring connection endpoint
    /// 4. Handling 0-RTT data
    /// 5. Registering connection and sending to user application
    pub(crate) async fn handle_new_connection(
        &mut self,
        mut frames: Vec<Frame>,
        remote_addr: SocketAddr,
    ) -> Result<()> {
        // 验证帧列表不为空，且第一帧为SYN
        // Validate frame list is not empty and first frame is SYN
        if frames.is_empty() {
            warn!(addr = %remote_addr, "Received empty frame list for new connection");
            return Ok(());
        }

        let first_frame = frames.remove(0);
        let Frame::Syn { header } = first_frame else {
            warn!(
                addr = %remote_addr,
                "handle_new_connection called with a non-SYN frame as the first frame"
            );
            return Ok(());
        };

        // 验证协议版本兼容性
        // Validate protocol version compatibility
        if !self.validate_protocol_version(header.protocol_version, remote_addr) {
            return Ok(());
        }

        info!(addr = %remote_addr, "Accepting new connection attempt");

        // 提取对端连接ID并生成本地连接ID
        // Extract peer connection ID and generate local connection ID
        let peer_cid = header.source_cid;
        let local_cid = self.generate_unique_connection_id();

        // 创建服务器端连接端点
        // Create server-side connection endpoint
        let (mut endpoint, tx_to_endpoint, stream) = self
            .create_server_endpoint(local_cid, peer_cid, remote_addr)
            .await?;

        // 处理0-RTT数据帧（如果有的话）
        // Handle 0-RTT data frames (if any)
        if !frames.is_empty() {
            self.handle_zero_rtt_frames(frames, &tx_to_endpoint, remote_addr)
                .await;
        }

        // 生成端点任务，在独立的异步任务中运行
        // Spawn endpoint task to run in a separate async task
        tokio::spawn(async move {
            info!(
                addr = %remote_addr,
                cid = %local_cid,
                "Spawning new endpoint task for inbound connection"
            );

            if let Err(e) = endpoint.run().await {
                // Connection timeouts are expected in certain scenarios (e.g., connection replacement),
                // so we log them at a lower level to reduce noise.
                // 连接超时在某些场景下是预期的（例如连接替换），所以我们以较低级别记录以减少噪音。
                match e {
                    crate::error::Error::ConnectionTimeout => {
                        debug!(
                            addr = %remote_addr,
                            cid = %local_cid,
                            "Endpoint closed due to timeout: {}", e
                        );
                    }
                    _ => {
                        error!(
                            addr = %remote_addr,
                            cid = %local_cid,
                            "Endpoint closed with error: {}", e
                        );
                    }
                }
            }
        });

        // 注册连接到帧路由器
        // Register connection to frame router
        self.register_connection_to_router(local_cid, remote_addr, tx_to_endpoint)
            .await;

        // 将新连接发送给用户应用
        // Send new connection to user application
        self.send_connection_to_user(stream, remote_addr, local_cid)
            .await?;

        Ok(())
    }

    /// 创建客户端连接
    /// Create client connection
    ///
    /// 该方法为客户端连接创建端点，生成唯一的连接ID，并设置相关的通道和配置。
    ///
    /// This method creates an endpoint for client connection, generates a unique connection ID,
    /// and sets up related channels and configuration.
    pub(crate) async fn create_client_connection(
        &mut self,
        remote_addr: SocketAddr,
        config: Config,
        initial_data: Option<bytes::Bytes>,
    ) -> Result<Stream> {
        // 生成唯一的本地连接ID
        // Generate unique local connection ID
        let local_cid = self.generate_unique_connection_id();

        let (tx_to_endpoint, rx_from_socket) = mpsc::channel(128);
        let transport_tx = self.transport_manager.send_tx();

        let (mut endpoint, tx_to_stream_handle, rx_from_stream_handle) = Endpoint::new_client(
            config,
            remote_addr,
            local_cid,
            rx_from_socket,
            transport_tx,
            self.command_tx.clone(),
            initial_data,
            self.timer_handle.clone(),
        )
        .await?;

        // 生成端点任务
        // Spawn endpoint task
        tokio::spawn(async move {
            info!(
                addr = %remote_addr,
                cid = %local_cid,
                "Spawning new endpoint task for outbound connection"
            );

            if let Err(e) = endpoint.run().await {
                match e {
                    crate::error::Error::ConnectionTimeout => {
                        debug!(
                            addr = %remote_addr,
                            cid = %local_cid,
                            "Endpoint closed due to timeout: {}", e
                        );
                    }
                    _ => {
                        error!(
                            addr = %remote_addr,
                            cid = %local_cid,
                            "Endpoint closed with error: {}", e
                        );
                    }
                }
            }
        });

        // 注册连接到帧路由器
        // Register connection to frame router
        self.register_connection_to_router(local_cid, remote_addr, tx_to_endpoint)
            .await;

        let stream = Stream::new(tx_to_stream_handle, rx_from_stream_handle);
        Ok(stream)
    }

    /// 分发帧到现有连接
    /// Dispatch frame to existing connections
    ///
    /// 该方法将接收到的帧分发给相应的现有连接。这是帧路由器功能的封装。
    ///
    /// This method dispatches received frames to corresponding existing connections.
    /// This is a wrapper around frame router functionality.
    pub(crate) async fn dispatch_frame(&mut self, frame: Frame, remote_addr: SocketAddr) {
        let _result = self.frame_router.dispatch_frame(frame, remote_addr).await;
        // RoutingResult is already logged appropriately in the frame router
        // RoutingResult已在帧路由器中适当记录
    }
}

impl<T: BindableTransport> std::fmt::Debug for SocketSessionCoordinator<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SocketSessionCoordinator")
            .field("config", &self.config)
            .field("frame_router", &self.frame_router)
            .finish_non_exhaustive()
    }
}
