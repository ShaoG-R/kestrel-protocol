//! 拥塞控制器抽象接口
//! Congestion Controller Abstract Interfaces
//!
//! 职责：
//! - 定义通用的拥塞控制器trait
//! - 提供可扩展的拥塞控制算法接口
//! - 支持不同拥塞控制算法的统一管理

use std::time::Duration;
use tokio::time::Instant;

/// 拥塞控制状态
/// Congestion control state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CongestionState {
    /// 慢启动阶段
    /// Slow start phase
    SlowStart,
    /// 拥塞避免阶段
    /// Congestion avoidance phase
    CongestionAvoidance,
    /// 快速恢复阶段
    /// Fast recovery phase
    FastRecovery,
}

/// 拥塞控制决策结果
/// Congestion control decision result
#[derive(Debug, Clone)]
pub struct CongestionDecision {
    /// 新的拥塞窗口大小
    /// New congestion window size
    pub new_congestion_window: u32,
    
    /// 新的慢启动阈值
    /// New slow start threshold
    pub new_slow_start_threshold: u32,
    
    /// 新的状态
    /// New state
    pub new_state: CongestionState,
    
    /// 是否发生了显著变化
    /// Whether significant change occurred
    pub significant_change: bool,
    
    /// 建议的发送速率（包/秒）
    /// Suggested send rate (packets/second)
    pub suggested_send_rate: Option<u32>,
}

/// 拥塞控制统计信息
/// Congestion control statistics
pub trait CongestionStats: std::fmt::Debug + std::fmt::Display + Clone {
    /// 获取当前拥塞窗口
    /// Get current congestion window
    fn congestion_window(&self) -> u32;
    
    /// 获取慢启动阈值
    /// Get slow start threshold
    fn slow_start_threshold(&self) -> u32;
    
    /// 获取当前状态
    /// Get current state
    fn state(&self) -> CongestionState;
    
    /// 获取最小RTT
    /// Get minimum RTT
    fn min_rtt(&self) -> Duration;
    
    /// 获取最后RTT
    /// Get last RTT
    fn last_rtt(&self) -> Duration;
}

/// 拥塞控制器核心trait
/// Core congestion controller trait
pub trait CongestionController: std::fmt::Debug + Send + Sync {
    /// 统计信息类型
    /// Statistics type
    type Stats: CongestionStats;
    
    /// 处理ACK并做出拥塞控制决策
    /// Handle ACK and make congestion control decision
    fn handle_ack(&mut self, rtt: Duration) -> CongestionDecision;
    
    /// 处理丢包事件
    /// Handle packet loss event
    fn handle_packet_loss(&mut self, now: Instant) -> CongestionDecision;
    
    /// 获取当前拥塞窗口
    /// Get current congestion window
    fn congestion_window(&self) -> u32;
    
    /// 获取当前状态
    /// Get current state
    fn current_state(&self) -> CongestionState;
    
    /// 获取慢启动阈值
    /// Get slow start threshold
    fn slow_start_threshold(&self) -> u32;
    
    /// 获取最小RTT
    /// Get minimum RTT
    fn min_rtt(&self) -> Duration;
    
    /// 获取最后RTT
    /// Get last RTT
    fn last_rtt(&self) -> Duration;
    
    /// 重置控制器状态
    /// Reset controller state
    fn reset(&mut self);
    
    /// 获取统计信息
    /// Get statistics
    fn get_statistics(&self) -> Self::Stats;
    
    /// 获取算法名称
    /// Get algorithm name
    fn algorithm_name(&self) -> &'static str;
}

/// 可配置的拥塞控制器trait
/// Configurable congestion controller trait
pub trait ConfigurableCongestionController: CongestionController {
    /// 配置类型
    /// Configuration type
    type Config: Clone;
    
    /// 使用配置创建新实例
    /// Create new instance with configuration
    fn new(config: Self::Config) -> Self;
    
    /// 更新配置
    /// Update configuration
    fn update_config(&mut self, config: Self::Config);
    
    /// 获取当前配置
    /// Get current configuration
    fn get_config(&self) -> &Self::Config;
}

/// 拥塞控制器工厂trait
/// Congestion controller factory trait
pub trait CongestionControllerFactory<C: CongestionController> {
    /// 配置类型
    /// Configuration type
    type Config: Clone;
    
    /// 创建拥塞控制器实例
    /// Create congestion controller instance
    fn create(&self, config: Self::Config) -> C;
    
    /// 获取支持的算法名称
    /// Get supported algorithm name
    fn algorithm_name(&self) -> &'static str;
}

/// 高级拥塞控制功能trait
/// Advanced congestion control features trait
pub trait AdvancedCongestionController: CongestionController {
    /// 处理显式拥塞通知
    /// Handle explicit congestion notification
    fn handle_ecn(&mut self, now: Instant) -> CongestionDecision;
    
    /// 处理带宽估计更新
    /// Handle bandwidth estimate update
    fn handle_bandwidth_update(&mut self, bandwidth_bps: u64) -> CongestionDecision;
    
    /// 获取当前带宽估计
    /// Get current bandwidth estimate
    fn estimated_bandwidth(&self) -> Option<u64>;
    
    /// 是否支持带宽感知
    /// Whether bandwidth-aware is supported
    fn is_bandwidth_aware(&self) -> bool {
        false
    }
    
    /// 是否支持ECN
    /// Whether ECN is supported
    fn supports_ecn(&self) -> bool {
        false
    }
}
