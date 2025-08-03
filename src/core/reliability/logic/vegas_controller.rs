//! Vegas拥塞控制器 - 专门处理拥塞控制逻辑
//! Vegas Controller - Specialized congestion control logic
//!
//! 职责：
//! - Vegas算法的拥塞控制决策
//! - 拥塞窗口管理
//! - 丢包响应处理
//! - RTT统计和分析

use crate::{
    config::Config,
};
use std::time::Duration;
use tokio::time::Instant;
use tracing::{debug, trace};

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
}

/// Vegas拥塞控制器
/// Vegas congestion controller
#[derive(Debug)]
pub struct VegasController {
    /// 当前拥塞窗口
    /// Current congestion window
    congestion_window: u32,
    
    /// 慢启动阈值
    /// Slow start threshold
    slow_start_threshold: u32,
    
    /// 当前状态
    /// Current state
    state: CongestionState,
    
    /// 最小RTT
    /// Minimum RTT
    min_rtt: Duration,
    
    /// 最后测量的RTT
    /// Last measured RTT
    last_rtt: Duration,
    
    /// 配置参数
    /// Configuration parameters
    config: Config,
}

impl VegasController {
    /// 创建新的Vegas控制器
    /// Create new Vegas controller
    pub fn new(config: Config) -> Self {
        Self {
            congestion_window: config.congestion_control.initial_cwnd_packets,
            slow_start_threshold: config.congestion_control.initial_ssthresh,
            state: CongestionState::SlowStart,
            // 使用合理的初始值而不是 u64::MAX 避免溢出
            min_rtt: Duration::from_millis(1000), // 1 second as safe initial value
            last_rtt: Duration::from_millis(1000),
            config,
        }
    }
    
    /// 处理ACK并做出拥塞控制决策
    /// Handle ACK and make congestion control decision
    pub fn handle_ack(&mut self, rtt: Duration) -> CongestionDecision {
        let old_window = self.congestion_window;
        let old_threshold = self.slow_start_threshold;
        let old_state = self.state;
        
        // 更新RTT统计
        self.update_rtt_stats(rtt);
        
        // 根据当前状态进行拥塞控制
        match self.state {
            CongestionState::SlowStart => {
                self.handle_slow_start();
            }
            CongestionState::CongestionAvoidance => {
                self.handle_congestion_avoidance(rtt);
            }
        }
        
        // 检查是否需要状态转换
        self.check_state_transition();
        
        let significant_change = old_window != self.congestion_window
            || old_threshold != self.slow_start_threshold
            || old_state != self.state;
        
        CongestionDecision {
            new_congestion_window: self.congestion_window,
            new_slow_start_threshold: self.slow_start_threshold,
            new_state: self.state,
            significant_change,
        }
    }
    
    /// 处理丢包事件
    /// Handle packet loss event
    pub fn handle_packet_loss(&mut self, _now: Instant) -> CongestionDecision {
        let old_window = self.congestion_window;
        let old_threshold = self.slow_start_threshold;
        
        // 判断是拥塞性丢包还是随机丢包
        let is_congestive_loss = self.is_congestive_loss();
        
        if is_congestive_loss {
            self.handle_congestive_loss();
        } else {
            self.handle_random_loss();
        }
        
        let significant_change = old_window != self.congestion_window
            || old_threshold != self.slow_start_threshold;
        
        CongestionDecision {
            new_congestion_window: self.congestion_window,
            new_slow_start_threshold: self.slow_start_threshold,
            new_state: self.state,
            significant_change,
        }
    }
    
    /// 获取当前拥塞窗口
    /// Get current congestion window
    pub fn congestion_window(&self) -> u32 {
        self.congestion_window
    }
    
    /// 获取当前状态
    /// Get current state
    pub fn current_state(&self) -> CongestionState {
        self.state
    }
    
    /// 获取慢启动阈值
    /// Get slow start threshold
    pub fn slow_start_threshold(&self) -> u32 {
        self.slow_start_threshold
    }
    
    /// 获取最小RTT
    /// Get minimum RTT
    pub fn min_rtt(&self) -> Duration {
        self.min_rtt
    }
    
    /// 获取最后RTT
    /// Get last RTT
    pub fn last_rtt(&self) -> Duration {
        self.last_rtt
    }
    
    /// 更新RTT统计
    /// Update RTT statistics
    fn update_rtt_stats(&mut self, rtt: Duration) {
        self.last_rtt = rtt;
        self.min_rtt = self.min_rtt.min(rtt);
        
        trace!(
            rtt_ms = rtt.as_millis(),
            min_rtt_ms = self.min_rtt.as_millis(),
            "RTT statistics updated"
        );
    }
    
    /// 处理慢启动阶段
    /// Handle slow start phase
    fn handle_slow_start(&mut self) {
        self.congestion_window += 1;
        
        trace!(
            cwnd = self.congestion_window,
            "Slow start: congestion window increased"
        );
    }
    
    /// 处理拥塞避免阶段
    /// Handle congestion avoidance phase
    fn handle_congestion_avoidance(&mut self, rtt: Duration) {
        // 避免除零错误和无效计算
        if rtt.is_zero() || self.min_rtt.is_zero() {
            trace!("Skipping congestion avoidance: invalid RTT values");
            return;
        }
        
        // 优化版Vegas算法：使用整数运算避免浮点精度问题
        // diff = cwnd * (rtt - min_rtt) / min_rtt
        // 通过重新排列避免浮点除法：diff = cwnd * (rtt - min_rtt) / min_rtt
        let rtt_micros = rtt.as_micros();
        let min_rtt_micros = self.min_rtt.as_micros();
        
        if rtt_micros <= min_rtt_micros {
            // RTT没有增加，增加窗口
            self.congestion_window += 1;
            trace!(
                cwnd = self.congestion_window,
                rtt_us = rtt_micros,
                min_rtt_us = min_rtt_micros,
                "Congestion avoidance: RTT not increased, growing window"
            );
            return;
        }
        
        // 计算差值，使用微秒精度避免浮点运算
        let rtt_diff_micros = rtt_micros - min_rtt_micros;
        let diff_packets_scaled = (self.congestion_window as u128 * rtt_diff_micros) / min_rtt_micros;
        let diff_packets = diff_packets_scaled as u32;
        
        let alpha = self.config.congestion_control.vegas_alpha_packets;
        let beta = self.config.congestion_control.vegas_beta_packets;
        
        if diff_packets < alpha {
            self.congestion_window += 1;
            trace!(
                cwnd = self.congestion_window,
                diff = diff_packets,
                alpha = alpha,
                "Congestion avoidance: increasing window"
            );
        } else if diff_packets > beta {
            self.congestion_window = (self.congestion_window - 1)
                .max(self.config.congestion_control.min_cwnd_packets);
            trace!(
                cwnd = self.congestion_window,
                diff = diff_packets,
                beta = beta,
                "Congestion avoidance: decreasing window"
            );
        } else {
            trace!(
                cwnd = self.congestion_window,
                diff = diff_packets,
                "Congestion avoidance: window stable"
            );
        }
    }
    
    /// 检查状态转换
    /// Check state transition
    fn check_state_transition(&mut self) {
        if self.state == CongestionState::SlowStart 
            && self.congestion_window >= self.slow_start_threshold {
            self.state = CongestionState::CongestionAvoidance;
            trace!("State transitioned to congestion avoidance");
        }
    }
    
    /// 判断是否为拥塞性丢包
    /// Determine if loss is congestive
    fn is_congestive_loss(&self) -> bool {
        // 如果RTT显著增加（超过min_rtt的20%），认为是拥塞性丢包
        // 避免除零错误：当min_rtt为零时默认为非拥塞性丢包
        if self.min_rtt.is_zero() {
            return false;
        }
        
        self.last_rtt > self.min_rtt + (self.min_rtt / 5)
    }
    
    /// 处理拥塞性丢包
    /// Handle congestive loss
    fn handle_congestive_loss(&mut self) {
        self.slow_start_threshold = (self.congestion_window / 2)
            .max(self.config.congestion_control.min_cwnd_packets);
        self.congestion_window = self.slow_start_threshold;
        self.state = CongestionState::SlowStart;
        
        debug!(
            new_ssthresh = self.slow_start_threshold,
            new_cwnd = self.congestion_window,
            "Congestive loss: severe reduction"
        );
    }
    
    /// 处理随机丢包
    /// Handle random loss
    fn handle_random_loss(&mut self) {
        self.congestion_window = ((self.congestion_window as f32) 
            * self.config.congestion_control.vegas_gentle_decrease_factor) as u32;
        self.congestion_window = self.congestion_window
            .max(self.config.congestion_control.min_cwnd_packets);
        
        debug!(
            new_cwnd = self.congestion_window,
            "Random loss: gentle reduction"
        );
    }
    
    /// 重置控制器状态
    /// Reset controller state
    pub fn reset(&mut self) {
        self.congestion_window = self.config.congestion_control.initial_cwnd_packets;
        self.slow_start_threshold = self.config.congestion_control.initial_ssthresh;
        self.state = CongestionState::SlowStart;
        // 使用与构造函数相同的初始值
        self.min_rtt = Duration::from_millis(1000);
        self.last_rtt = Duration::from_millis(1000);
        
        debug!("Vegas controller reset to initial state");
    }
    
    /// 获取拥塞控制统计信息
    /// Get congestion control statistics
    pub fn get_statistics(&self) -> VegasStats {
        VegasStats {
            congestion_window: self.congestion_window,
            slow_start_threshold: self.slow_start_threshold,
            state: self.state,
            min_rtt: self.min_rtt,
            last_rtt: self.last_rtt,
        }
    }
}

/// Vegas拥塞控制统计信息
/// Vegas congestion control statistics
#[derive(Debug, Clone)]
pub struct VegasStats {
    pub congestion_window: u32,
    pub slow_start_threshold: u32,
    pub state: CongestionState,
    pub min_rtt: Duration,
    pub last_rtt: Duration,
}

impl std::fmt::Display for VegasStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Vegas[cwnd:{}, ssthresh:{}, state:{:?}, min_rtt:{:.1}ms, last_rtt:{:.1}ms]",
            self.congestion_window,
            self.slow_start_threshold,
            self.state,
            self.min_rtt.as_secs_f64() * 1000.0,
            self.last_rtt.as_secs_f64() * 1000.0
        )
    }
}

impl Default for VegasController {
    fn default() -> Self {
        Self::new(Config::default())
    }
}