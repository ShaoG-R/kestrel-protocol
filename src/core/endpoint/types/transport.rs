//! 传输层管理模块 - 管理端点的传输层状态
//! Transport Layer Management Module - Manages transport layer state for endpoints
//!
//! 该模块封装了与传输层相关的所有状态和逻辑，包括可靠性层、
//! 流量控制窗口等功能。
//!
//! This module encapsulates all transport layer related state and logic,
//! including reliability layer, flow control windows, etc.

use crate::core::reliability::ReliabilityLayer;

/// 传输层管理器
/// Transport layer manager
pub struct TransportManager {
    /// 可靠性层，负责ARQ、重传、拥塞控制等
    /// Reliability layer responsible for ARQ, retransmission, congestion control, etc.
    reliability: ReliabilityLayer,
    
    /// 对端接收窗口大小
    /// Peer receive window size
    peer_recv_window: u32,
}

impl TransportManager {
    /// 创建新的传输层管理器
    /// Create new transport layer manager
    pub fn new(reliability: ReliabilityLayer) -> Self {
        Self {
            reliability,
            peer_recv_window: 32, // 默认窗口大小
        }
    }

    /// 创建带指定窗口大小的传输层管理器
    /// Create transport layer manager with specified window size
    pub fn with_peer_window(reliability: ReliabilityLayer, peer_recv_window: u32) -> Self {
        Self {
            reliability,
            peer_recv_window,
        }
    }

    /// 获取可靠性层的引用
    /// Get reference to reliability layer
    pub fn reliability(&self) -> &ReliabilityLayer {
        &self.reliability
    }

    /// 获取可靠性层的可变引用
    /// Get mutable reference to reliability layer
    pub fn reliability_mut(&mut self) -> &mut ReliabilityLayer {
        &mut self.reliability
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
    }

    /// 检查发送缓冲区是否为空
    /// Check if send buffer is empty
    pub fn is_send_buffer_empty(&self) -> bool {
        self.reliability.is_send_buffer_empty()
    }

    /// 检查接收缓冲区是否为空
    /// Check if receive buffer is empty
    pub fn is_recv_buffer_empty(&self) -> bool {
        self.reliability.is_recv_buffer_empty()
    }

    /// 检查在途数据包是否为空
    /// Check if in-flight packets are empty
    pub fn is_in_flight_empty(&self) -> bool {
        self.reliability.is_in_flight_empty()
    }

    /// 获取当前拥塞窗口大小
    /// Get current congestion window size
    pub fn congestion_window(&self) -> u32 {
        self.reliability.congestion_window()
    }

    /// 获取当前平滑往返时间
    /// Get current smoothed round trip time
    pub fn smoothed_rtt(&self) -> Option<std::time::Duration> {
        self.reliability.smoothed_rtt()
    }

    /// 获取当前RTT变化
    /// Get current RTT variation
    pub fn rtt_var(&self) -> Option<std::time::Duration> {
        self.reliability.rtt_var()
    }

    /// 获取下一个RTO截止时间
    /// Get next RTO deadline
    pub fn next_rto_deadline(&self) -> Option<tokio::time::Instant> {
        self.reliability.next_rto_deadline()
    }

    /// 清理在途数据包
    /// Clear in-flight packets
    pub fn clear_in_flight_packets(&mut self) {
        self.reliability.clear_in_flight_packets();
    }

    /// 重装和发送数据
    /// Reassemble and send data
    pub fn reassemble(&mut self) -> (Option<Vec<bytes::Bytes>>, bool) {
        self.reliability.reassemble()
    }

    /// 获取传输层统计信息
    /// Get transport layer statistics
    pub fn transport_stats(&self) -> TransportStats {
        TransportStats {
            peer_recv_window: self.peer_recv_window,
            congestion_window: self.congestion_window(),
            smoothed_rtt: self.smoothed_rtt(),
            rtt_var: self.rtt_var(),
            send_buffer_empty: self.is_send_buffer_empty(),
            recv_buffer_empty: self.is_recv_buffer_empty(),
            in_flight_empty: self.is_in_flight_empty(),
        }
    }
}

impl std::fmt::Debug for TransportManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransportManager")
            .field("peer_recv_window", &self.peer_recv_window)
            .field("congestion_window", &self.congestion_window())
            .field("smoothed_rtt", &self.smoothed_rtt())
            .field("send_buffer_empty", &self.is_send_buffer_empty())
            .field("recv_buffer_empty", &self.is_recv_buffer_empty())
            .field("in_flight_empty", &self.is_in_flight_empty())
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
    
    /// 当前拥塞窗口大小
    /// Current congestion window size
    pub congestion_window: u32,
    
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
            "TransportStats {{ peer_recv_window: {}, congestion_window: {}, smoothed_rtt: {:?}, rtt_var: {:?}, buffers_empty: {{send: {}, recv: {}, in_flight: {}}} }}",
            self.peer_recv_window,
            self.congestion_window,
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
    use crate::{config::Config, core::reliability::congestion::vegas::Vegas};

    fn create_test_reliability() -> ReliabilityLayer {
        let config = Config::default();
        let congestion_control = Box::new(Vegas::new(config.clone()));
        let connection_id = 1; // Test connection ID
        let timer_handle = crate::timer::start_hybrid_timer_task::<crate::core::endpoint::timing::TimeoutEvent>();
        let timer_actor = crate::timer::start_timer_actor(timer_handle, None);
        ReliabilityLayer::new(config, congestion_control, connection_id, timer_actor)
    }

    #[test]
    fn test_transport_manager_creation() {
        let reliability = create_test_reliability();
        let manager = TransportManager::new(reliability);
        
        assert_eq!(manager.peer_recv_window(), 32);
        assert!(manager.is_send_buffer_empty());
        assert!(manager.is_recv_buffer_empty());
        assert!(manager.is_in_flight_empty());
    }

    #[test]
    fn test_transport_manager_with_window() {
        let reliability = create_test_reliability();
        let manager = TransportManager::with_peer_window(reliability, 64);
        
        assert_eq!(manager.peer_recv_window(), 64);
    }

    #[test]
    fn test_peer_window_operations() {
        let reliability = create_test_reliability();
        let mut manager = TransportManager::new(reliability);
        
        assert_eq!(manager.peer_recv_window(), 32);
        
        manager.set_peer_recv_window(128);
        assert_eq!(manager.peer_recv_window(), 128);
    }

    #[test]
    fn test_transport_stats() {
        let reliability = create_test_reliability();
        let manager = TransportManager::new(reliability);
        
        let stats = manager.transport_stats();
        assert_eq!(stats.peer_recv_window, 32);
        assert!(stats.send_buffer_empty);
        assert!(stats.recv_buffer_empty);
        assert!(stats.in_flight_empty);
        
        let stats_string = stats.stats_string();
        assert!(stats_string.contains("TransportStats"));
        assert!(stats_string.contains("peer_recv_window: 32"));
    }

    #[test]
    fn test_transport_manager_debug() {
        let reliability = create_test_reliability();
        let manager = TransportManager::new(reliability);
        
        let debug_string = format!("{:?}", manager);
        assert!(debug_string.contains("TransportManager"));
        assert!(debug_string.contains("peer_recv_window"));
        assert!(debug_string.contains("congestion_window"));
    }
}