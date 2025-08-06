//! Constructors for the `Endpoint`.

use crate::core::endpoint::{Endpoint, StreamCommand};
use crate::socket::{Transport, TransportCommand};
use crate::{
    config::Config,
    socket::SocketActorCommand,
    error::Result,
    timer::{HybridTimerTaskHandle, task::types::SenderCallback},
};
use crate::core::reliability::{
    logic::congestion::{traits::CongestionController, vegas_controller::VegasController},
    coordination::flow_control_coordinator::FlowControlCoordinatorConfig,
};
use bytes::Bytes;
use std::net::SocketAddr;
use tokio::sync::mpsc;
use crate::core::endpoint::lifecycle::{ConnectionLifecycleManager, DefaultLifecycleManager};
use crate::core::endpoint::timing::{TimeoutEvent, TimingManager};
use crate::core::endpoint::types::{
    channels::ChannelManager,
    identity::ConnectionIdentity,
    state::ConnectionState,
    transport::TransportManager,
};


impl<T: Transport, C: CongestionController> Endpoint<T, C> {
    /// Creates a new `Endpoint` for the client-side.
    pub async fn new_client(
        config: Config,
        remote_addr: SocketAddr,
        local_cid: u32,
        receiver: mpsc::Receiver<(crate::packet::frame::Frame, SocketAddr)>,
        sender: mpsc::Sender<TransportCommand<T>>,
        command_tx: mpsc::Sender<SocketActorCommand>,
        initial_data: Option<Bytes>,
        timer_handle: HybridTimerTaskHandle<TimeoutEvent, SenderCallback<TimeoutEvent>>,
        congestion_controller: C,
        flow_control_config: FlowControlCoordinatorConfig,
    ) -> Result<(Self, mpsc::Sender<StreamCommand>, mpsc::Receiver<Vec<Bytes>>)> {
        let (tx_to_endpoint, rx_from_stream) = mpsc::channel(128);
        let (tx_to_stream, rx_from_endpoint) = mpsc::channel(128);
        
        // 创建定时器Actor用于批量定时器管理
        // Create timer actor for batch timer management
        let timer_actor = crate::timer::start_sender_timer_actor(timer_handle.clone(), None);
        let (mut timing, timer_event_rx) = TimingManager::new(local_cid, timer_handle);

        // 创建使用统一可靠性层的传输管理器
        // Create transport manager with unified reliability layer
        let mut transport = TransportManager::new(
            local_cid,
            timer_actor,
            timing.get_timeout_tx(),
            congestion_controller,
            flow_control_config,
            config.clone(),
        );
        
        // 注册初始的空闲超时定时器
        // Register initial idle timeout timer
        if let Err(e) = timing.register_idle_timeout(&config).await {
            return Err(crate::error::Error::TimerError(e.to_string()));
        }
        
        if let Some(data) = initial_data {
            // 立即写入0-RTT数据到发送缓冲区
            // Immediately write the 0-RTT data to send buffer
            transport.unified_reliability_mut().write_to_send_buffer(data);
        }

        let mut lifecycle_manager = DefaultLifecycleManager::new(
            ConnectionState::Connecting,
            config.clone(),
        );
        lifecycle_manager.initialize(local_cid, remote_addr)?;

        
        
        // 注册初始的空闲超时定时器
        // Register initial idle timeout timer
        if let Err(e) = timing.register_idle_timeout(&config).await {
            return Err(crate::error::Error::TimerError(e.to_string()));
        }
        
        // 为客户端连接注册连接超时定时器（30秒超时）
        // Register connection timeout timer for client connection (30 seconds timeout)
        let connection_timeout = tokio::time::Duration::from_secs(30);
        if let Err(e) = timing.register_connection_timeout(connection_timeout).await {
            tracing::warn!("Failed to register connection timeout for client: {}", e);
        }

        let endpoint = Self {
            identity: ConnectionIdentity::new(local_cid, 0, remote_addr),
            timing,
            transport,
            lifecycle_manager,
            config,
            channels: ChannelManager::new(
                receiver,
                sender,
                command_tx,
                rx_from_stream,
                Some(tx_to_stream),
                timer_event_rx, // 事件驱动架构的核心 - 定时器事件通道
            ),
        };

        Ok((endpoint, tx_to_endpoint, rx_from_endpoint))
    }

    /// Creates a new `Endpoint` for the server-side.
    pub async fn new_server(
        config: Config,
        remote_addr: SocketAddr,
        local_cid: u32,
        peer_cid: u32,
        receiver: mpsc::Receiver<(crate::packet::frame::Frame, SocketAddr)>,
        sender: mpsc::Sender<TransportCommand<T>>,
        command_tx: mpsc::Sender<SocketActorCommand>,
        timer_handle: HybridTimerTaskHandle<TimeoutEvent, SenderCallback<TimeoutEvent>>,
        congestion_controller: C,
        flow_control_config: FlowControlCoordinatorConfig,
    ) -> Result<(Self, mpsc::Sender<StreamCommand>, mpsc::Receiver<Vec<Bytes>>)> {
        let (tx_to_endpoint, rx_from_stream) = mpsc::channel(128);
        let (tx_to_stream, rx_from_endpoint) = mpsc::channel(128);
        
        // 创建定时器Actor用于批量定时器管理
        // Create timer actor for batch timer management
        let timer_actor = crate::timer::start_sender_timer_actor(timer_handle.clone(), None);
        
        let mut lifecycle_manager = DefaultLifecycleManager::new(
            ConnectionState::SynReceived,
            config.clone(),
        );
        lifecycle_manager.initialize(local_cid, remote_addr)?;

        let (mut timing, timer_event_rx) = TimingManager::new(local_cid, timer_handle);
        
        // 创建使用统一可靠性层的传输管理器
        // Create transport manager with unified reliability layer
        let transport = TransportManager::new(
            local_cid,
            timer_actor,
            timing.get_timeout_tx(),
            congestion_controller,
            flow_control_config,
            config.clone(),
        );

        // 注册初始的空闲超时定时器
        // Register initial idle timeout timer
        if let Err(e) = timing.register_idle_timeout(&config).await {
            return Err(crate::error::Error::TimerError(e.to_string()));
        }

        let endpoint = Self {
            identity: ConnectionIdentity::new(local_cid, peer_cid, remote_addr),
            timing,
            transport,
            lifecycle_manager,
            config,
            channels: ChannelManager::new(
                receiver,
                sender,
                command_tx,
                rx_from_stream,
                Some(tx_to_stream),
                timer_event_rx, // 事件驱动架构的核心 - 定时器事件通道
            ),
        };

        Ok((endpoint, tx_to_endpoint, rx_from_endpoint))
    }
}

// 便利构造函数，使用默认的Vegas拥塞控制器
// Convenience constructors using default Vegas congestion controller
impl<T: Transport> Endpoint<T, VegasController> {
    /// Creates a new `Endpoint` for the client-side with Vegas congestion control.
    pub async fn new_client_with_vegas(
        config: Config,
        remote_addr: SocketAddr,
        local_cid: u32,
        receiver: mpsc::Receiver<(crate::packet::frame::Frame, SocketAddr)>,
        sender: mpsc::Sender<TransportCommand<T>>,
        command_tx: mpsc::Sender<SocketActorCommand>,
        initial_data: Option<Bytes>,
        timer_handle: HybridTimerTaskHandle<TimeoutEvent, SenderCallback<TimeoutEvent>>,
    ) -> Result<(Self, mpsc::Sender<StreamCommand>, mpsc::Receiver<Vec<Bytes>>)> {
        let congestion_controller = VegasController::new(config.clone());
        let flow_control_config = FlowControlCoordinatorConfig::default();
        
        Self::new_client(
            config,
            remote_addr,
            local_cid,
            receiver,
            sender,
            command_tx,
            initial_data,
            timer_handle,
            congestion_controller,
            flow_control_config,
        ).await
    }

    /// Creates a new `Endpoint` for the server-side with Vegas congestion control.
    pub async fn new_server_with_vegas(
        config: Config,
        remote_addr: SocketAddr,
        local_cid: u32,
        peer_cid: u32,
        receiver: mpsc::Receiver<(crate::packet::frame::Frame, SocketAddr)>,
        sender: mpsc::Sender<TransportCommand<T>>,
        command_tx: mpsc::Sender<SocketActorCommand>,
        timer_handle: HybridTimerTaskHandle<TimeoutEvent, SenderCallback<TimeoutEvent>>,
    ) -> Result<(Self, mpsc::Sender<StreamCommand>, mpsc::Receiver<Vec<Bytes>>)> {
        let congestion_controller = VegasController::new(config.clone());
        let flow_control_config = FlowControlCoordinatorConfig::default();
        
        Self::new_server(
            config,
            remote_addr,
            local_cid,
            peer_cid,
            receiver,
            sender,
            command_tx,
            timer_handle,
            congestion_controller,
            flow_control_config,
        ).await
    }
} 