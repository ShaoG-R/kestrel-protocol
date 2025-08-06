//! 流量控制协调器 - 统一协调拥塞控制和流量控制
//! Flow Control Coordinator - Unified coordination of congestion and flow control
//!
//! 职责：
//! - 协调任意拥塞控制器和流量控制
//! - 管理发送速率和窗口大小
//! - 处理RTT更新和丢包响应
//! - 提供统一的流量控制接口

use crate::core::reliability::{
        coordination::traits::{
            ConfigurableFlowControlCoordinator, FlowControlCoordinator as FlowControlCoordinatorTrait, FlowControlDecision, FlowControlState, FlowControlStats as FlowControlStatsTrait
        }, logic::congestion::{
            vegas_controller::VegasController, CongestionController, CongestionStats
        }
    };
use std::time::Duration;
use tokio::time::Instant;
use tracing::{debug, info, trace, warn};

/// 流量控制协调器配置
/// Flow control coordinator configuration
#[derive(Debug, Clone)]
pub struct FlowControlCoordinatorConfig {
    /// 初始对端接收窗口
    /// Initial peer receive window
    pub initial_peer_receive_window: u32,
    
    /// 默认发送间隔阈值（毫秒）
    /// Default send interval threshold (milliseconds)
    pub default_send_interval_threshold_ms: u64,
    
    /// 高RTT阈值（毫秒）
    /// High RTT threshold (milliseconds)
    pub high_rtt_threshold_ms: u64,
    
    /// 丢包后延迟时间（毫秒）
    /// Delay after packet loss (milliseconds)
    pub packet_loss_delay_ms: u64,
    
    /// 是否启用自适应间隔
    /// Whether adaptive interval is enabled
    pub enable_adaptive_interval: bool,
}

impl Default for FlowControlCoordinatorConfig {
    fn default() -> Self {
        Self {
            initial_peer_receive_window: u32::MAX,
            default_send_interval_threshold_ms: 100,
            high_rtt_threshold_ms: 100,
            packet_loss_delay_ms: 10,
            enable_adaptive_interval: true,
        }
    }
}

/// 泛型流量控制协调器
/// Generic flow control coordinator
#[derive(Debug)]
pub struct FlowControlCoordinator<C: CongestionController> {
    /// 拥塞控制器
    /// Congestion controller
    congestion_controller: C,
    
    /// 对端接收窗口
    /// Peer receive window
    peer_receive_window: u32,
    
    /// 在途数据包数量
    /// In-flight packet count
    in_flight_count: u32,
    
    /// 最大发送速率（可选）
    /// Maximum send rate (optional)
    max_send_rate: Option<u32>,
    
    /// 上次发送时间
    /// Last send time
    last_send_time: Option<Instant>,
    
    /// 配置
    /// Configuration
    config: FlowControlCoordinatorConfig,
}

impl<C: CongestionController> FlowControlCoordinator<C> {
    /// 创建新的流量控制协调器
    /// Create new flow control coordinator
    pub fn new(congestion_controller: C, config: FlowControlCoordinatorConfig) -> Self {
        Self {
            congestion_controller,
            peer_receive_window: config.initial_peer_receive_window,
            in_flight_count: 0,
            max_send_rate: None,
            last_send_time: None,
            config,
        }
    }
    
    /// 处理ACK并更新流量控制状态
    /// Handle ACK and update flow control state
    pub fn handle_ack(&mut self, rtt: Duration, now: Instant) -> FlowControlDecision {
        // 更新拥塞控制器
        let congestion_decision = self.congestion_controller.handle_ack(rtt);
        
        // 计算新的发送许可
        let send_permit = self.calculate_send_permit();
        
        // 确定发送间隔建议
        let suggested_send_interval = self.calculate_send_interval(rtt);
        
        // 判断是否应该延迟发送
        let should_delay_send = self.should_delay_send(now);
        
        if congestion_decision.significant_change {
            debug!(
                old_cwnd = congestion_decision.new_congestion_window.saturating_sub(1),
                new_cwnd = congestion_decision.new_congestion_window,
                new_state = ?congestion_decision.new_state,
                send_permit = send_permit,
                algorithm = self.congestion_controller.algorithm_name(),
                "Flow control state updated after ACK"
            );
        }
        
        FlowControlDecision {
            congestion_decision,
            send_permit,
            suggested_send_interval,
            should_delay_send,
        }
    }
    
    /// 处理丢包事件
    /// Handle packet loss event
    pub fn handle_packet_loss(&mut self, now: Instant) -> FlowControlDecision {
        // 更新拥塞控制器
        let congestion_decision = self.congestion_controller.handle_packet_loss(now);
        
        // 重新计算发送许可
        let send_permit = self.calculate_send_permit();
        
        // 丢包后建议延迟发送
        let suggested_send_interval = Some(Duration::from_millis(self.config.packet_loss_delay_ms));
        let should_delay_send = true;
        
        warn!(
            new_cwnd = congestion_decision.new_congestion_window,
            new_ssthresh = congestion_decision.new_slow_start_threshold,
            send_permit = send_permit,
            algorithm = self.congestion_controller.algorithm_name(),
            "Flow control adjusted after packet loss"
        );
        
        FlowControlDecision {
            congestion_decision,
            send_permit,
            suggested_send_interval,
            should_delay_send,
        }
    }
    
    /// 更新对端接收窗口
    /// Update peer receive window
    pub fn update_peer_receive_window(&mut self, window_size: u32) {
        let old_window = self.peer_receive_window;
        self.peer_receive_window = window_size;
        
        if old_window != window_size {
            trace!(
                old_window = old_window,
                new_window = window_size,
                "Peer receive window updated"
            );
        }
    }
    
    /// 更新在途数据包数量
    /// Update in-flight packet count
    pub fn update_in_flight_count(&mut self, count: u32) {
        let old_count = self.in_flight_count;
        self.in_flight_count = count;
        
        if old_count != count {
            trace!(
                old_count = old_count,
                new_count = count,
                "In-flight packet count updated"
            );
        }
    }
    
    /// 设置最大发送速率
    /// Set maximum send rate
    pub fn set_max_send_rate(&mut self, rate_packets_per_second: Option<u32>) {
        self.max_send_rate = rate_packets_per_second;
        
        if let Some(rate) = rate_packets_per_second {
            debug!(rate = rate, "Maximum send rate set");
        } else {
            debug!("Maximum send rate removed");
        }
    }
    
    /// 记录发送时间
    /// Record send time
    pub fn record_send_time(&mut self, now: Instant) {
        self.last_send_time = Some(now);
    }
    
    /// 获取当前流量控制状态
    /// Get current flow control state
    pub fn get_flow_control_state(&self) -> FlowControlState {
        FlowControlState {
            congestion_window: self.congestion_controller.congestion_window(),
            peer_receive_window: self.peer_receive_window,
            in_flight_count: self.in_flight_count,
            send_permit: self.calculate_send_permit(),
            congestion_state: self.congestion_controller.current_state(),
            min_rtt: self.congestion_controller.min_rtt(),
            last_rtt: self.congestion_controller.last_rtt(),
        }
    }
    
    /// 重置流量控制状态
    /// Reset flow control state
    pub fn reset(&mut self) {
        self.congestion_controller.reset();
        self.peer_receive_window = self.config.initial_peer_receive_window;
        self.in_flight_count = 0;
        self.max_send_rate = None;
        self.last_send_time = None;
        
        info!(
            algorithm = self.congestion_controller.algorithm_name(),
            "Flow control coordinator reset"
        );
    }
    
    /// 计算发送许可
    /// Calculate send permit
    fn calculate_send_permit(&self) -> u32 {
        let congestion_permit = self.congestion_controller.congestion_window()
            .saturating_sub(self.in_flight_count);
        let flow_permit = self.peer_receive_window
            .saturating_sub(self.in_flight_count);
        
        let permit = std::cmp::min(congestion_permit, flow_permit);
        
        trace!(
            congestion_permit = congestion_permit,
            flow_permit = flow_permit,
            final_permit = permit,
            "Send permit calculated"
        );
        
        permit
    }
    
    /// 计算建议的发送间隔
    /// Calculate suggested send interval
    fn calculate_send_interval(&self, current_rtt: Duration) -> Option<Duration> {
        if let Some(max_rate) = self.max_send_rate {
            if max_rate > 0 {
                // 基于最大发送速率计算间隔，避免除零错误
                let interval = Duration::from_secs(1) / max_rate;
                return Some(interval);
            } else {
                // 如果速率为0，表示禁止发送
                return Some(Duration::from_secs(3600)); // 1小时延迟，实际上禁止发送
            }
        }
        
        // 基于RTT的自适应间隔（可选）
        if self.config.enable_adaptive_interval {
            let threshold = Duration::from_millis(self.config.high_rtt_threshold_ms);
            if current_rtt > threshold {
                // 高RTT时适当增加发送间隔
                Some(current_rtt / 10)
            } else {
                None
            }
        } else {
            None
        }
    }
    
    /// 判断是否应该延迟发送
    /// Determine if sending should be delayed
    fn should_delay_send(&self, now: Instant) -> bool {
        // 拥塞窗口为0时应该延迟
        if self.congestion_controller.congestion_window() == 0 {
            return true;
        }
        
        // 检查速率限制
        if let Some(last_send) = self.last_send_time {
            if let Some(max_rate) = self.max_send_rate {
                if max_rate > 0 {
                    let min_interval = Duration::from_secs(1) / max_rate;
                    let elapsed = now.duration_since(last_send);
                    return elapsed < min_interval;
                } else {
                    // 速率为0，禁止发送
                    return true;
                }
            }
        }
        
        false
    }
    
    /// 获取统计信息
    /// Get statistics
    pub fn get_statistics(&self) -> FlowControlStats<C::Stats> {
        let congestion_stats = self.congestion_controller.get_statistics();
        let state = self.get_flow_control_state();
        
        FlowControlStats {
            congestion_stats,
            peer_receive_window: self.peer_receive_window,
            in_flight_count: self.in_flight_count,
            send_permit: state.send_permit,
            max_send_rate: self.max_send_rate,
            has_rate_limit: self.max_send_rate.is_some(),
        }
    }
    
    /// 获取拥塞控制器的引用
    /// Get reference to congestion controller
    pub fn congestion_controller(&self) -> &C {
        &self.congestion_controller
    }
    
    /// 获取拥塞控制器的可变引用
    /// Get mutable reference to congestion controller
    pub fn congestion_controller_mut(&mut self) -> &mut C {
        &mut self.congestion_controller
    }
}

/// 流量控制统计信息
/// Flow control statistics
#[derive(Debug, Clone)]
pub struct FlowControlStats<S: CongestionStats> {
    pub congestion_stats: S,
    pub peer_receive_window: u32,
    pub in_flight_count: u32,
    pub send_permit: u32,
    pub max_send_rate: Option<u32>,
    pub has_rate_limit: bool,
}

impl<S: CongestionStats> FlowControlStatsTrait for FlowControlStats<S> {
    fn send_permit(&self) -> u32 {
        self.send_permit
    }
    
    fn peer_receive_window(&self) -> u32 {
        self.peer_receive_window
    }
    
    fn in_flight_count(&self) -> u32 {
        self.in_flight_count
    }
    
    fn has_rate_limit(&self) -> bool {
        self.has_rate_limit
    }
    
    fn max_send_rate(&self) -> Option<u32> {
        self.max_send_rate
    }
}

impl<S: CongestionStats> std::fmt::Display for FlowControlStats<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "FlowControl[permit:{}, peer_window:{}, in_flight:{}, rate_limit:{}, {}]",
            self.send_permit,
            self.peer_receive_window,
            self.in_flight_count,
            if self.has_rate_limit { "enabled" } else { "disabled" },
            self.congestion_stats
        )
    }
}

// 实现FlowControlCoordinatorTrait
impl<C: CongestionController> FlowControlCoordinatorTrait<C> for FlowControlCoordinator<C> {
    type Stats = FlowControlStats<C::Stats>;
    
    fn handle_ack(&mut self, rtt: Duration, now: Instant) -> FlowControlDecision {
        self.handle_ack(rtt, now)
    }
    
    fn handle_packet_loss(&mut self, now: Instant) -> FlowControlDecision {
        self.handle_packet_loss(now)
    }
    
    fn update_peer_receive_window(&mut self, window_size: u32) {
        self.update_peer_receive_window(window_size)
    }
    
    fn update_in_flight_count(&mut self, count: u32) {
        self.update_in_flight_count(count)
    }
    
    fn set_max_send_rate(&mut self, rate_packets_per_second: Option<u32>) {
        self.set_max_send_rate(rate_packets_per_second)
    }
    
    fn record_send_time(&mut self, now: Instant) {
        self.record_send_time(now)
    }
    
    fn get_flow_control_state(&self) -> FlowControlState {
        self.get_flow_control_state()
    }
    
    fn reset(&mut self) {
        self.reset()
    }
    
    fn get_statistics(&self) -> Self::Stats {
        self.get_statistics()
    }
    
    fn congestion_controller(&self) -> &C {
        self.congestion_controller()
    }
    
    fn congestion_controller_mut(&mut self) -> &mut C {
        self.congestion_controller_mut()
    }
}

impl<C: CongestionController> ConfigurableFlowControlCoordinator<C> for FlowControlCoordinator<C> {
    type Config = FlowControlCoordinatorConfig;
    
    fn new(congestion_controller: C, config: Self::Config) -> Self {
        Self::new(congestion_controller, config)
    }
    
    fn update_config(&mut self, config: Self::Config) {
        self.peer_receive_window = config.initial_peer_receive_window;
        self.config = config;
    }
    
    fn get_config(&self) -> &Self::Config {
        &self.config
    }
}

// 为了向后兼容，提供Vegas专用的类型别名
pub type VegasFlowControlCoordinator = FlowControlCoordinator<VegasController>;