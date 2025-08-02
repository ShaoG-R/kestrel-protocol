//! 流量控制协调器 - 统一协调拥塞控制和流量控制
//! Flow Control Coordinator - Unified coordination of congestion and flow control
//!
//! 职责：
//! - 协调Vegas拥塞控制器和流量控制
//! - 管理发送速率和窗口大小
//! - 处理RTT更新和丢包响应
//! - 提供统一的流量控制接口

use super::super::logic::vegas_controller::{VegasController, CongestionDecision, CongestionState, VegasStats};
use crate::config::Config;
use std::time::Duration;
use tokio::time::Instant;
use tracing::{debug, info, trace, warn};

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

/// 流量控制协调器
/// Flow control coordinator
#[derive(Debug)]
pub struct FlowControlCoordinator {
    /// Vegas拥塞控制器
    /// Vegas congestion controller
    vegas_controller: VegasController,
    
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
    
    /// 配置参数
    /// Configuration parameters
    config: Config,
}

impl FlowControlCoordinator {
    /// 创建新的流量控制协调器
    /// Create new flow control coordinator
    pub fn new(config: Config) -> Self {
        Self {
            vegas_controller: VegasController::new(config.clone()),
            peer_receive_window: u32::MAX, // 初始假设无限制
            in_flight_count: 0,
            max_send_rate: None,
            last_send_time: None,
            config,
        }
    }
    
    /// 处理ACK并更新流量控制状态
    /// Handle ACK and update flow control state
    pub fn handle_ack(&mut self, rtt: Duration, now: Instant) -> FlowControlDecision {
        // 更新Vegas拥塞控制器
        let congestion_decision = self.vegas_controller.handle_ack(rtt);
        
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
        // 更新Vegas拥塞控制器
        let congestion_decision = self.vegas_controller.handle_packet_loss(now);
        
        // 重新计算发送许可
        let send_permit = self.calculate_send_permit();
        
        // 丢包后建议延迟发送
        let suggested_send_interval = Some(Duration::from_millis(10)); // 短暂延迟
        let should_delay_send = true;
        
        warn!(
            new_cwnd = congestion_decision.new_congestion_window,
            new_ssthresh = congestion_decision.new_slow_start_threshold,
            send_permit = send_permit,
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
            congestion_window: self.vegas_controller.congestion_window(),
            peer_receive_window: self.peer_receive_window,
            in_flight_count: self.in_flight_count,
            send_permit: self.calculate_send_permit(),
            congestion_state: self.vegas_controller.current_state(),
            min_rtt: self.vegas_controller.min_rtt(),
            last_rtt: self.vegas_controller.last_rtt(),
        }
    }
    
    /// 重置流量控制状态
    /// Reset flow control state
    pub fn reset(&mut self) {
        self.vegas_controller.reset();
        self.peer_receive_window = u32::MAX;
        self.in_flight_count = 0;
        self.last_send_time = None;
        
        info!("Flow control coordinator reset");
    }
    
    /// 计算发送许可
    /// Calculate send permit
    fn calculate_send_permit(&self) -> u32 {
        let congestion_permit = self.vegas_controller.congestion_window()
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
            // 基于最大发送速率计算间隔
            let interval = Duration::from_secs(1) / max_rate;
            return Some(interval);
        }
        
        // 基于RTT的自适应间隔（可选）
        if current_rtt > Duration::from_millis(100) {
            // 高RTT时适当增加发送间隔
            Some(current_rtt / 10)
        } else {
            None
        }
    }
    
    /// 判断是否应该延迟发送
    /// Determine if sending should be delayed
    fn should_delay_send(&self, now: Instant) -> bool {
        if let Some(last_send) = self.last_send_time {
            if let Some(max_rate) = self.max_send_rate {
                let min_interval = Duration::from_secs(1) / max_rate;
                let elapsed = now.duration_since(last_send);
                return elapsed < min_interval;
            }
        }
        
        // 拥塞窗口为0时应该延迟
        self.vegas_controller.congestion_window() == 0
    }
    
    /// 获取统计信息
    /// Get statistics
    pub fn get_statistics(&self) -> FlowControlStats {
        let vegas_stats = self.vegas_controller.get_statistics();
        let state = self.get_flow_control_state();
        
        FlowControlStats {
            vegas_stats,
            peer_receive_window: self.peer_receive_window,
            in_flight_count: self.in_flight_count,
            send_permit: state.send_permit,
            max_send_rate: self.max_send_rate,
            has_rate_limit: self.max_send_rate.is_some(),
        }
    }
}

/// 流量控制统计信息
/// Flow control statistics
#[derive(Debug, Clone)]
pub struct FlowControlStats {
    pub vegas_stats: VegasStats,
    pub peer_receive_window: u32,
    pub in_flight_count: u32,
    pub send_permit: u32,
    pub max_send_rate: Option<u32>,
    pub has_rate_limit: bool,
}

impl std::fmt::Display for FlowControlStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "FlowControl[permit:{}, peer_window:{}, in_flight:{}, rate_limit:{}, {}]",
            self.send_permit,
            self.peer_receive_window,
            self.in_flight_count,
            if self.has_rate_limit { "enabled" } else { "disabled" },
            self.vegas_stats
        )
    }
}

impl Default for FlowControlCoordinator {
    fn default() -> Self {
        Self::new(Config::default())
    }
}