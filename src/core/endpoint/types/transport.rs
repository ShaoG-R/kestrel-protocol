//! 传输层管理模块 - 管理端点的传输层状态
//! Transport Layer Management Module - Manages transport layer state for endpoints
//!
//! 该模块封装了与传输层相关的所有状态和逻辑，包括可靠性层、
//! 流量控制窗口等功能。现已重构为使用统一可靠性层架构。
//!
//! This module encapsulates all transport layer related state and logic,
//! including reliability layer, flow control windows, etc. Now refactored
//! to use the unified reliability layer architecture.

use crate::{
    config::Config,
    core::{
        endpoint::timing::TimeoutEvent,
        reliability::{
            UnifiedReliabilityLayer,
            coordination::flow_control_coordinator::FlowControlCoordinatorConfig,
        },
    },
    timer::{
        actor::SenderTimerActorHandle,
        event::{ConnectionId, TimerEventData},
    },
};
use crate::core::reliability::logic::congestion::{
    traits::{CongestionController, CongestionStats},
    vegas_controller::VegasController,
};
use tokio::sync::mpsc;

/// 传输层管理器 - 使用统一可靠性层架构
/// Transport layer manager - using unified reliability layer architecture  
pub struct TransportManager<C: CongestionController = VegasController> {
    /// 统一可靠性层，负责ARQ、重传、拥塞控制等
    /// Unified reliability layer responsible for ARQ, retransmission, congestion control, etc.
    unified_reliability: UnifiedReliabilityLayer<C>,
    
    /// 对端接收窗口大小（已集成到统一层中，保留用于向后兼容）
    /// Peer receive window size (integrated into unified layer, kept for backward compatibility)
    peer_recv_window: u32,
}

impl TransportManager<VegasController> {
    /// 创建使用Vegas拥塞控制的传输层管理器（便利方法）
    /// Create transport layer manager with Vegas congestion control (convenience method)
    pub fn new_with_vegas(
        connection_id: ConnectionId,
        timer_actor: SenderTimerActorHandle,
        timeout_tx: mpsc::Sender<TimerEventData<TimeoutEvent>>,
        config: Config,
    ) -> Self {
        let congestion_controller = VegasController::new(config.clone());
        let flow_control_config = FlowControlCoordinatorConfig::default();
        Self::new(connection_id, timer_actor, timeout_tx, congestion_controller, flow_control_config, config)
    }

    /// 创建带指定窗口大小的Vegas传输层管理器（便利方法）
    /// Create Vegas transport layer manager with specified window size (convenience method)
    pub fn new_vegas_with_peer_window(
        connection_id: ConnectionId,
        timer_actor: SenderTimerActorHandle,
        timeout_tx: mpsc::Sender<TimerEventData<TimeoutEvent>>,
        config: Config,
        peer_recv_window: u32,
    ) -> Self {
        let congestion_controller = VegasController::new(config.clone());
        let flow_control_config = FlowControlCoordinatorConfig::default();
        Self::with_peer_window(connection_id, timer_actor, timeout_tx, congestion_controller, flow_control_config, config, peer_recv_window)
    }
}

impl<C: CongestionController> TransportManager<C> {
    /// 创建新的传输层管理器（使用统一可靠性层）
    /// Create new transport layer manager (using unified reliability layer)
    pub fn new(
        connection_id: ConnectionId,
        timer_actor: SenderTimerActorHandle,
        timeout_tx: mpsc::Sender<TimerEventData<TimeoutEvent>>,
        congestion_controller: C,
        flow_control_config: FlowControlCoordinatorConfig,
        config: Config,
    ) -> Self {
        let unified_reliability = UnifiedReliabilityLayer::new(
            connection_id,
            timer_actor,
            timeout_tx,
            congestion_controller,
            flow_control_config,
            config,
        );
        
        Self {
            unified_reliability,
            peer_recv_window: 32, // 默认窗口大小
        }
    }

    /// 创建带指定窗口大小的传输层管理器（使用统一可靠性层）
    /// Create transport layer manager with specified window size (using unified reliability layer)
    pub fn with_peer_window(
        connection_id: ConnectionId,
        timer_actor: SenderTimerActorHandle,
        timeout_tx: mpsc::Sender<TimerEventData<TimeoutEvent>>,
        congestion_controller: C,
        flow_control_config: FlowControlCoordinatorConfig,
        config: Config,
        peer_recv_window: u32,
    ) -> Self {
        let mut unified_reliability = UnifiedReliabilityLayer::new(
            connection_id,
            timer_actor,
            timeout_tx,
            congestion_controller,
            flow_control_config,
            config,
        );
        
        // 更新对端接收窗口大小到统一层
        unified_reliability.update_peer_receive_window(peer_recv_window);
        
        Self {
            unified_reliability,
            peer_recv_window,
        }
    }

    /// 获取统一可靠性层的引用
    /// Get reference to unified reliability layer
    pub fn unified_reliability(&self) -> &UnifiedReliabilityLayer<C> {
        &self.unified_reliability
    }

    /// 获取统一可靠性层的可变引用
    /// Get mutable reference to unified reliability layer
    pub fn unified_reliability_mut(&mut self) -> &mut UnifiedReliabilityLayer<C> {
        &mut self.unified_reliability
    }

    /// 获取对端接收窗口大小
    /// Get peer receive window size
    pub fn peer_recv_window(&self) -> u32 {
        self.peer_recv_window
    }

    /// 设置对端接收窗口大小
    /// Set peer receive window size
    pub fn set_peer_recv_window(&mut self, window_size: u32) {
        self.peer_recv_window = window_size;
        // 同步更新到统一可靠性层
        // Synchronously update to unified reliability layer
        self.unified_reliability.update_peer_receive_window(window_size);
    }

    /// 检查发送缓冲区是否为空
    /// Check if send buffer is empty
    pub fn is_send_buffer_empty(&self) -> bool {
        self.unified_reliability.is_send_buffer_empty()
    }

    /// 检查接收缓冲区是否为空
    /// Check if receive buffer is empty (统一层没有此方法，使用接收缓冲区统计信息判断)
    /// Check if receive buffer is empty (unified layer doesn't have this method, use receive buffer stats)
    pub fn is_recv_buffer_empty(&self) -> bool {
        // 通过统计信息判断接收缓冲区是否为空
        self.unified_reliability.get_statistics().receive_buffer_utilization == 0.0
    }

    /// 检查在途数据包是否为空
    /// Check if in-flight packets are empty
    pub fn is_in_flight_empty(&self) -> bool {
        self.unified_reliability.is_in_flight_empty()
    }

    /// 获取当前拥塞窗口大小
    /// Get current congestion window size
    pub fn congestion_window(&self) -> u32 {
        self.unified_reliability.get_statistics().congestion_stats.congestion_window()
    }

    /// 获取当前平滑往返时间（统一层尚未提供此接口，返回None）
    /// Get current smoothed round trip time (unified layer doesn't provide this interface yet, return None)
    pub fn smoothed_rtt(&self) -> Option<std::time::Duration> {
        // TODO: 统一可靠性层需要添加RTT信息访问接口
        // TODO: Unified reliability layer needs to add RTT information access interface
        None
    }

    /// 获取当前RTT变化（统一层尚未提供此接口，返回None）
    /// Get current RTT variation (unified layer doesn't provide this interface yet, return None)  
    pub fn rtt_var(&self) -> Option<std::time::Duration> {
        // TODO: 统一可靠性层需要添加RTT变化信息访问接口
        // TODO: Unified reliability layer needs to add RTT variation information access interface
        None
    }

    /// 清理在途数据包（使用异步方法）
    /// Clear in-flight packets (using async method)
    pub async fn clear_in_flight_packets(&mut self) {
        self.unified_reliability.cleanup_all_retransmission_timers().await;
    }

    /// 重装和发送数据
    /// Reassemble and send data  
    pub fn reassemble(&mut self) -> (Option<Vec<bytes::Bytes>>, bool) {
        let result = self.unified_reliability.reassemble_data();
        (result.data, result.fin_seen)
    }

    /// 获取传输层统计信息
    /// Get transport layer statistics
    pub fn transport_stats(&self) -> TransportStats {
        TransportStats {
            peer_recv_window: self.peer_recv_window,
            smoothed_rtt: self.smoothed_rtt(),
            rtt_var: self.rtt_var(),
            send_buffer_empty: self.is_send_buffer_empty(),
            recv_buffer_empty: self.is_recv_buffer_empty(),
            in_flight_empty: self.is_in_flight_empty(),
        }
    }
}

impl<C: CongestionController> std::fmt::Debug for TransportManager<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let unified_stats = self.unified_reliability.get_statistics();
        f.debug_struct("TransportManager")
            .field("peer_recv_window", &self.peer_recv_window)
            .field("congestion_stats", &unified_stats.congestion_stats)
            .field("smoothed_rtt", &self.smoothed_rtt())
            .field("send_buffer_empty", &self.is_send_buffer_empty())
            .field("recv_buffer_empty", &self.is_recv_buffer_empty())
            .field("in_flight_empty", &self.is_in_flight_empty())
            .field("in_flight_count", &unified_stats.total_in_flight)
            .field("active_timers", &unified_stats.active_timers)
            .field("send_buffer_utilization", &unified_stats.send_buffer_utilization)
            .field("receive_buffer_utilization", &unified_stats.receive_buffer_utilization)
            .finish()
    }
}

/// 传输层统计信息
/// Transport layer statistics
#[derive(Debug, Clone)]
pub struct TransportStats {
    /// 对端接收窗口大小
    /// Peer receive window size
    pub peer_recv_window: u32,
    
    /// 平滑往返时间
    /// Smoothed round trip time
    pub smoothed_rtt: Option<std::time::Duration>,
    
    /// RTT变化
    /// RTT variation
    pub rtt_var: Option<std::time::Duration>,
    
    /// 发送缓冲区是否为空
    /// Whether send buffer is empty
    pub send_buffer_empty: bool,
    
    /// 接收缓冲区是否为空
    /// Whether receive buffer is empty
    pub recv_buffer_empty: bool,
    
    /// 在途数据包是否为空
    /// Whether in-flight packets are empty
    pub in_flight_empty: bool,
}

impl TransportStats {
    /// 获取统计信息的字符串表示
    /// Get string representation of statistics
    pub fn stats_string(&self) -> String {
        format!(
            "TransportStats {{ peer_recv_window: {}, smoothed_rtt: {:?}, rtt_var: {:?}, buffers_empty: {{send: {}, recv: {}, in_flight: {}}} }}",
            self.peer_recv_window,
            self.smoothed_rtt,
            self.rtt_var,
            self.send_buffer_empty,
            self.recv_buffer_empty,
            self.in_flight_empty
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn create_test_transport_manager() -> TransportManager<VegasController> {
        let config = Config::default();
        let connection_id = 1; // Test connection ID
        let timer_handle = crate::timer::start_hybrid_timer_task::<crate::core::endpoint::timing::TimeoutEvent, crate::timer::task::types::SenderCallback<crate::core::endpoint::timing::TimeoutEvent>>();
        let timer_actor = crate::timer::start_sender_timer_actor(timer_handle, None);
        let (tx_to_endpoint, _rx_from_stream) = tokio::sync::mpsc::channel(128);
        TransportManager::new_with_vegas(connection_id, timer_actor, tx_to_endpoint, config)
    }

    #[tokio::test]
    async fn test_transport_manager_creation() {
        let manager = create_test_transport_manager().await;
        
        assert_eq!(manager.peer_recv_window(), 32);
        assert!(manager.is_send_buffer_empty());
        assert!(manager.is_recv_buffer_empty());
        assert!(manager.is_in_flight_empty());
    }

    #[tokio::test]
    async fn test_transport_manager_with_window() {
        let config = Config::default();
        let connection_id = 1;
        let timer_handle = crate::timer::start_hybrid_timer_task::<crate::core::endpoint::timing::TimeoutEvent, crate::timer::task::types::SenderCallback<crate::core::endpoint::timing::TimeoutEvent>>();
        let timer_actor = crate::timer::start_sender_timer_actor(timer_handle, None);
        let (tx_to_endpoint, _rx_from_stream) = tokio::sync::mpsc::channel(128);
        
        let manager = TransportManager::new_vegas_with_peer_window(connection_id, timer_actor, tx_to_endpoint, config, 64);
        
        assert_eq!(manager.peer_recv_window(), 64);
    }

    #[tokio::test]
    async fn test_peer_window_operations() {
        let mut manager = create_test_transport_manager().await;
        
        assert_eq!(manager.peer_recv_window(), 32);
        
        manager.set_peer_recv_window(128);
        assert_eq!(manager.peer_recv_window(), 128);
    }

    #[tokio::test]
    async fn test_transport_stats() {
        let manager = create_test_transport_manager().await;
        
        let stats = manager.transport_stats();
        assert_eq!(stats.peer_recv_window, 32);
        assert!(stats.send_buffer_empty);
        assert!(stats.recv_buffer_empty);
        assert!(stats.in_flight_empty);
        
        let stats_string = stats.stats_string();
        assert!(stats_string.contains("TransportStats"));
        assert!(stats_string.contains("peer_recv_window: 32"));
    }

    #[tokio::test]
    async fn test_transport_manager_debug() {
        let manager = create_test_transport_manager().await;
        
        let debug_string = format!("{:?}", manager);
        assert!(debug_string.contains("TransportManager"));
        assert!(debug_string.contains("peer_recv_window"));
        assert!(debug_string.contains("congestion_stats"));
    }
}