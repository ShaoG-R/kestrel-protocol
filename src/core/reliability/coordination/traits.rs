//! 流控协调器抽象接口
//! Flow Control Coordinator Abstract Interfaces
//!
//! 职责：
//! - 定义通用的流控协调器trait
//! - 提供可扩展的流控策略接口
//! - 支持不同拥塞控制算法的统一集成

use crate::core::reliability::logic::congestion::{CongestionController, CongestionDecision, CongestionState};
use std::time::Duration;
use tokio::time::Instant;

/// 流量控制决策
/// Flow control decision
#[derive(Debug, Clone)]
pub struct FlowControlDecision {
    /// 拥塞控制决策
    /// Congestion control decision
    pub congestion_decision: CongestionDecision,
    
    /// 新的发送许可
    /// New send permit
    pub send_permit: u32,
    
    /// 建议的发送间隔
    /// Suggested send interval
    pub suggested_send_interval: Option<Duration>,
    
    /// 是否应该延迟发送
    /// Whether sending should be delayed
    pub should_delay_send: bool,
}

/// 流量控制状态
/// Flow control state
#[derive(Debug, Clone)]
pub struct FlowControlState {
    /// 当前拥塞窗口
    /// Current congestion window
    pub congestion_window: u32,
    
    /// 对端接收窗口
    /// Peer receive window
    pub peer_receive_window: u32,
    
    /// 在途数据包数量
    /// In-flight packet count
    pub in_flight_count: u32,
    
    /// 发送许可
    /// Send permit
    pub send_permit: u32,
    
    /// 拥塞控制状态
    /// Congestion control state
    pub congestion_state: CongestionState,
    
    /// 最小RTT
    /// Minimum RTT
    pub min_rtt: Duration,
    
    /// 最后RTT
    /// Last RTT
    pub last_rtt: Duration,
}

/// 流控统计信息
/// Flow control statistics
pub trait FlowControlStats: std::fmt::Debug + std::fmt::Display + Clone {
    /// 获取发送许可
    /// Get send permit
    fn send_permit(&self) -> u32;
    
    /// 获取对端接收窗口
    /// Get peer receive window
    fn peer_receive_window(&self) -> u32;
    
    /// 获取在途数据包数量
    /// Get in-flight packet count
    fn in_flight_count(&self) -> u32;
    
    /// 是否有速率限制
    /// Whether rate limit is enabled
    fn has_rate_limit(&self) -> bool;
    
    /// 获取最大发送速率
    /// Get maximum send rate
    fn max_send_rate(&self) -> Option<u32>;
}

/// 流控协调器核心trait
/// Core flow control coordinator trait
pub trait FlowControlCoordinator<C: CongestionController>: std::fmt::Debug + Send + Sync {
    /// 统计信息类型
    /// Statistics type
    type Stats: FlowControlStats;
    
    /// 处理ACK并更新流量控制状态
    /// Handle ACK and update flow control state
    fn handle_ack(&mut self, rtt: Duration, now: Instant) -> FlowControlDecision;
    
    /// 处理丢包事件
    /// Handle packet loss event
    fn handle_packet_loss(&mut self, now: Instant) -> FlowControlDecision;
    
    /// 更新对端接收窗口
    /// Update peer receive window
    fn update_peer_receive_window(&mut self, window_size: u32);
    
    /// 更新在途数据包数量
    /// Update in-flight packet count
    fn update_in_flight_count(&mut self, count: u32);
    
    /// 设置最大发送速率
    /// Set maximum send rate
    fn set_max_send_rate(&mut self, rate_packets_per_second: Option<u32>);
    
    /// 记录发送时间
    /// Record send time
    fn record_send_time(&mut self, now: Instant);
    
    /// 获取当前流量控制状态
    /// Get current flow control state
    fn get_flow_control_state(&self) -> FlowControlState;
    
    /// 重置流量控制状态
    /// Reset flow control state
    fn reset(&mut self);
    
    /// 获取统计信息
    /// Get statistics
    fn get_statistics(&self) -> Self::Stats;
    
    /// 获取拥塞控制器的引用
    /// Get reference to congestion controller
    fn congestion_controller(&self) -> &C;
    
    /// 获取拥塞控制器的可变引用
    /// Get mutable reference to congestion controller
    fn congestion_controller_mut(&mut self) -> &mut C;
}

/// 可配置的流控协调器trait
/// Configurable flow control coordinator trait
pub trait ConfigurableFlowControlCoordinator<C: CongestionController>: FlowControlCoordinator<C> {
    /// 配置类型
    /// Configuration type
    type Config: Clone;
    
    /// 使用指定的拥塞控制器和配置创建新实例
    /// Create new instance with specified congestion controller and configuration
    fn new(congestion_controller: C, config: Self::Config) -> Self;
    
    /// 更新配置
    /// Update configuration
    fn update_config(&mut self, config: Self::Config);
    
    /// 获取当前配置
    /// Get current configuration
    fn get_config(&self) -> &Self::Config;
}

/// 流控协调器工厂trait
/// Flow control coordinator factory trait
pub trait FlowControlCoordinatorFactory<C: CongestionController, FC: FlowControlCoordinator<C>> {
    /// 配置类型
    /// Configuration type
    type Config: Clone;
    
    /// 创建流控协调器实例
    /// Create flow control coordinator instance
    fn create(&self, congestion_controller: C, config: Self::Config) -> FC;
    
    /// 获取支持的策略名称
    /// Get supported strategy name
    fn strategy_name(&self) -> &'static str;
}

/// 高级流控功能trait
/// Advanced flow control features trait
pub trait AdvancedFlowControlCoordinator<C: CongestionController>: FlowControlCoordinator<C> {
    /// 处理网络状态变化
    /// Handle network condition change
    fn handle_network_condition_change(&mut self, condition: NetworkCondition) -> FlowControlDecision;
    
    /// 获取当前网络状态评估
    /// Get current network condition assessment
    fn assess_network_condition(&self) -> NetworkCondition;
    
    /// 设置自适应参数
    /// Set adaptive parameters
    fn set_adaptive_parameters(&mut self, params: AdaptiveParameters);
    
    /// 是否支持自适应调整
    /// Whether adaptive adjustment is supported
    fn supports_adaptive_adjustment(&self) -> bool {
        false
    }
}

/// 网络状态评估
/// Network condition assessment
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkCondition {
    /// 良好
    /// Good
    Good,
    /// 一般
    /// Fair
    Fair,
    /// 拥塞
    /// Congested
    Congested,
    /// 不稳定
    /// Unstable
    Unstable,
}

/// 自适应参数
/// Adaptive parameters
#[derive(Debug, Clone)]
pub struct AdaptiveParameters {
    /// 拥塞敏感度
    /// Congestion sensitivity
    pub congestion_sensitivity: f32,
    
    /// 响应速度
    /// Response speed
    pub response_speed: f32,
    
    /// 稳定性偏好
    /// Stability preference
    pub stability_preference: f32,
}
