//! 时间管理模块 - 管理端点的时间相关状态
//! Timing Management Module - Manages time-related state for endpoints
//!
//! 该模块封装了端点的所有时间相关字段和逻辑，包括连接开始时间、
//! 最后接收时间、超时检查等功能。
//!
//! This module encapsulates all time-related fields and logic for endpoints,
//! including connection start time, last receive time, timeout checks, etc.

use crate::config::Config;
use crate::timer::{
    event::{ConnectionId, TimerEventData},
    task::TimerRegistration,
    HybridTimerTaskHandle,
    TimerActorHandle,
};
use crate::core::endpoint::unified_scheduler::{TimeoutLayer, TimeoutCheckResult, UnifiedTimeoutScheduler};
use std::collections::HashMap;
use tokio::{
    sync::mpsc,
    time::{Duration, Instant},
};
use tracing::trace;

/// 超时事件类型枚举
/// Timeout event type enumeration
#[derive(Debug, Clone, PartialEq, Eq, Hash, Copy)]
pub enum TimeoutEvent {
    /// 空闲超时 - 连接长时间无活动
    /// Idle timeout - connection has been inactive for too long
    IdleTimeout,
    /// 路径验证超时 - 路径验证过程超时
    /// Path validation timeout - path validation process timed out
    PathValidationTimeout,
    /// 重传超时 - 数据包需要重传
    /// Retransmission timeout - packets need to be retransmitted
    RetransmissionTimeout,
    /// 连接超时 - 连接建立超时
    /// Connection timeout - connection establishment timed out
    ConnectionTimeout,
}

impl Default for TimeoutEvent {
    fn default() -> Self {
        Self::IdleTimeout
    }
}

/// 定时器管理器 - 封装全局定时器相关逻辑
/// Timer manager - encapsulates global timer related logic
pub struct TimerManager {
    /// 连接ID，用于全局定时器注册
    /// Connection ID for global timer registration
    connection_id: ConnectionId,

    /// 定时器Actor句柄，用于批量定时器操作
    /// Timer actor handle for batch timer operations
    timer_actor: TimerActorHandle,

    /// 接收超时事件的通道
    /// Channel for receiving timeout events
    timeout_rx: mpsc::Receiver<TimerEventData<TimeoutEvent>>,

    /// 发送超时事件的通道（用于注册定时器）
    /// Channel for sending timeout events (used for timer registration)
    timeout_tx: mpsc::Sender<TimerEventData<TimeoutEvent>>,

    /// 活跃定时器类型集合（简化跟踪）
    /// Active timer types set (simplified tracking)
    active_timer_types: HashMap<TimeoutEvent, bool>,
}

impl TimerManager {
    /// 创建新的定时器管理器
    /// Create new timer manager
    pub fn new(connection_id: ConnectionId, timer_handle: HybridTimerTaskHandle<TimeoutEvent>) -> Self {
        let (timeout_tx, timeout_rx) = mpsc::channel(32);
        
        // 创建定时器Actor用于批量处理
        // Create timer actor for batch processing
        let timer_actor = crate::timer::start_timer_actor(timer_handle, None);
        
        Self {
            connection_id,
            timer_actor,
            timeout_rx,
            timeout_tx,
            active_timer_types: HashMap::new(),
        }
    }

    /// 创建新的定时器管理器并返回接收通道
    /// Create new timer manager and return receiver channel
    /// 
    /// 用于事件驱动架构，将接收通道传递给ChannelManager
    /// Used for event-driven architecture, passing receiver channel to ChannelManager
    pub fn new_with_receiver(connection_id: ConnectionId, timer_handle: HybridTimerTaskHandle<TimeoutEvent>) -> (Self, mpsc::Receiver<TimerEventData<TimeoutEvent>>) {
        let (timeout_tx, timeout_rx) = mpsc::channel(32);
        
        // 创建定时器Actor用于批量处理
        // Create timer actor for batch processing
        let timer_actor = crate::timer::start_timer_actor(timer_handle, None);
        
        let manager = Self {
            connection_id,
            timer_actor,
            timeout_rx: mpsc::channel(1).1, // 创建一个空的接收端，实际接收由外部通道处理
            timeout_tx,
            active_timer_types: HashMap::new(),
        };

        (manager, timeout_rx)
    }

    /// 检查是否有到期的定时器事件（优化版本）
    /// Check for expired timer events (optimized version)
    pub fn check_timer_events(&mut self) -> Vec<TimeoutEvent> {
        // 预分配容量，避免动态增长
        // Pre-allocate capacity to avoid dynamic growth
        let mut events = Vec::with_capacity(8);

        while let Ok(event_data) = self.timeout_rx.try_recv() {
            let timeout_event = event_data.timeout_event;
            // 从活跃定时器中移除这个事件类型
            // Remove this event type from active timers
            self.active_timer_types.remove(&timeout_event);
            // 避免不必要的克隆，直接移动所有权
            // Avoid unnecessary cloning, move ownership directly
            events.push(timeout_event);
        }

        events
    }

    /// 注册定时器（使用TimerActor批量优化）
    /// Register timer (using TimerActor batch optimization)
    pub async fn register_timer(
        &mut self,
        timeout_event: TimeoutEvent,
        delay: Duration,
    ) -> Result<(), &'static str> {
        // 如果之前有同类型定时器，先标记为取消
        // If there was a previous timer of same type, mark for cancellation first
        if self.active_timer_types.contains_key(&timeout_event) {
            // 先取消旧的定时器
            // Cancel old timer first
            self.timer_actor.cancel_timer(self.connection_id, timeout_event).await;
        }
        
        // 注册新的定时器
        // Register new timer
        let registration = TimerRegistration::new(
            self.connection_id,
            delay,
            timeout_event,
            self.timeout_tx.clone(),
        );

        match self.timer_actor.register_timer(registration).await {
            Ok(_) => {
                // 标记这种类型的定时器为活跃
                // Mark this timer type as active
                self.active_timer_types.insert(timeout_event, true);
                trace!(timeout_event = ?timeout_event, delay = ?delay, "Registered timer via TimerActor");
                Ok(())
            }
            Err(_) => Err("Failed to register timer"),
        }
    }

    /// 取消指定类型的定时器
    /// Cancel timer of specified type
    pub async fn cancel_timer(&mut self, timeout_event: &TimeoutEvent) -> bool {
        if self.active_timer_types.remove(timeout_event).is_some() {
            let success = self.timer_actor.cancel_timer(self.connection_id, *timeout_event).await;
            trace!(timeout_event = ?timeout_event, success = success, "Cancelled timer via TimerActor");
            success
        } else {
            false
        }
    }

    /// 取消所有活跃的定时器
    /// Cancel all active timers
    pub async fn cancel_all_timers(&mut self) {
        if self.active_timer_types.is_empty() {
            return;
        }
        
        // 准备批量取消请求
        // Prepare batch cancellation requests
        let cancel_requests: Vec<_> = self.active_timer_types.keys()
            .map(|&timeout_event| (self.connection_id, timeout_event))
            .collect();
        
        // 清空活跃定时器跟踪
        // Clear active timer tracking
        self.active_timer_types.clear();
        
        if !cancel_requests.is_empty() {
            let cancelled_count = self.timer_actor.batch_cancel_timers(cancel_requests).await;
            trace!(cancelled = cancelled_count, "Batch cancelled all timers");
        }
    }

    /// 注册空闲超时定时器
    /// Register idle timeout timer
    pub async fn register_idle_timeout(&mut self, config: &Config) -> Result<(), &'static str> {
        self.register_timer(TimeoutEvent::IdleTimeout, config.connection.idle_timeout)
            .await
    }

    /// 注册路径验证超时定时器
    /// Register path validation timeout timer
    pub async fn register_path_validation_timeout(&mut self, delay: Duration) -> Result<(), &'static str> {
        self.register_timer(TimeoutEvent::PathValidationTimeout, delay)
            .await
    }

    /// 注册连接超时定时器
    /// Register connection timeout timer
    pub async fn register_connection_timeout(&mut self, delay: Duration) -> Result<(), &'static str> {
        self.register_timer(TimeoutEvent::ConnectionTimeout, delay)
            .await
    }

    /// 重置空闲超时定时器（在收到数据包时调用）- 优化版本
    /// Reset idle timeout timer (called when receiving packets) - optimized version
    pub async fn reset_idle_timeout(&mut self, config: &Config) -> Result<(), &'static str> {
        // 直接注册新定时器，自动替换旧定时器（更高效）
        // Directly register new timer, automatically replace old timer (more efficient)
        self.register_idle_timeout(config).await
    }
}

/// 时间管理器 - 管理连接的时间状态
/// Timing manager - manages connection timing state
pub struct TimingManager {
    /// 连接开始时间
    /// Connection start time
    start_time: Instant,

    /// 最后接收数据的时间
    /// Last time data was received
    last_recv_time: Instant,

    /// FIN挂起EOF标志，表示已收到FIN但还未向用户发送EOF
    /// FIN pending EOF flag, indicates FIN received but EOF not yet sent to user
    fin_pending_eof: bool,

    /// 定时器管理器
    /// Timer manager
    timer_manager: TimerManager,

    /// 统一超时事件调度器
    /// Unified timeout event scheduler
    unified_scheduler: UnifiedTimeoutScheduler,
}

impl TimingManager {
    /// 创建新的时间管理器
    /// Create new timing manager
    /// 
    /// 返回时间管理器和定时器事件接收通道，用于事件驱动架构
    /// Returns timing manager and timer event receiver channel for event-driven architecture
    pub fn new(connection_id: ConnectionId, timer_handle: HybridTimerTaskHandle<TimeoutEvent>) -> (Self, mpsc::Receiver<TimerEventData<TimeoutEvent>>) {
        let now = Instant::now();
        let (timer_manager, timer_rx) = TimerManager::new_with_receiver(connection_id, timer_handle);

        let timing_manager = Self {
            start_time: now,
            last_recv_time: now,
            fin_pending_eof: false,
            timer_manager,
            unified_scheduler: UnifiedTimeoutScheduler::new(),
        };

        (timing_manager, timer_rx)
    }

    /// 创建带指定开始时间的时间管理器
    /// Create timing manager with specified start time
    /// 
    /// 返回时间管理器和定时器事件接收通道，用于事件驱动架构
    /// Returns timing manager and timer event receiver channel for event-driven architecture
    pub fn with_start_time(
        start_time: Instant,
        connection_id: ConnectionId,
        timer_handle: HybridTimerTaskHandle<TimeoutEvent>,
    ) -> (Self, mpsc::Receiver<TimerEventData<TimeoutEvent>>) {
        let (timer_manager, timer_rx) = TimerManager::new_with_receiver(connection_id, timer_handle);

        let timing_manager = Self {
            start_time,
            last_recv_time: start_time,
            fin_pending_eof: false,
            timer_manager,
            unified_scheduler: UnifiedTimeoutScheduler::new(),
        };

        (timing_manager, timer_rx)
    }

    /// 检查是否有到期的定时器事件
    /// Check for expired timer events
    pub fn check_timer_events(&mut self) -> Vec<TimeoutEvent> {
        self.timer_manager.check_timer_events()
    }

    /// 注册定时器到全局定时器任务
    /// Register timer with global timer task
    pub async fn register_timer(
        &mut self,
        timeout_event: TimeoutEvent,
        delay: Duration,
    ) -> Result<(), &'static str> {
        self.timer_manager.register_timer(timeout_event, delay).await
    }

    /// 取消指定类型的定时器
    /// Cancel timer of specified type
    pub async fn cancel_timer(&mut self, timeout_event: &TimeoutEvent) -> bool {
        self.timer_manager.cancel_timer(timeout_event).await
    }

    /// 取消所有活跃的定时器
    /// Cancel all active timers
    pub async fn cancel_all_timers(&mut self) {
        self.timer_manager.cancel_all_timers().await
    }

    /// 注册空闲超时定时器
    /// Register idle timeout timer
    pub async fn register_idle_timeout(&mut self, config: &Config) -> Result<(), &'static str> {
        self.timer_manager.register_idle_timeout(config).await
    }

    /// 注册路径验证超时定时器
    /// Register path validation timeout timer
    pub async fn register_path_validation_timeout(&mut self, delay: Duration) -> Result<(), &'static str> {
        self.timer_manager.register_path_validation_timeout(delay).await
    }

    /// 注册连接超时定时器
    /// Register connection timeout timer
    pub async fn register_connection_timeout(&mut self, delay: Duration) -> Result<(), &'static str> {
        self.timer_manager.register_connection_timeout(delay).await
    }

    /// 重置空闲超时定时器（在收到数据包时调用）
    /// Reset idle timeout timer (called when receiving packets)
    pub async fn reset_idle_timeout(&mut self, config: &Config) -> Result<(), &'static str> {
        self.timer_manager.reset_idle_timeout(config).await
    }

    /// 获取连接开始时间
    /// Get connection start time
    pub fn start_time(&self) -> Instant {
        self.start_time
    }

    /// 获取最后接收时间
    /// Get last receive time
    pub fn last_recv_time(&self) -> Instant {
        self.last_recv_time
    }

    /// 更新最后接收时间
    /// Update last receive time
    pub fn update_last_recv_time(&mut self, time: Instant) {
        self.last_recv_time = time;
    }

    /// 更新最后接收时间为当前时间
    /// Update last receive time to now
    pub fn touch_last_recv_time(&mut self) {
        self.last_recv_time = Instant::now();
    }

    /// 获取连接持续时间
    /// Get connection duration
    pub fn connection_duration(&self) -> Duration {
        self.last_recv_time.duration_since(self.start_time)
    }

    /// 获取自上次接收数据以来的时间
    /// Get time since last receive
    pub fn time_since_last_recv(&self) -> Duration {
        Instant::now().duration_since(self.last_recv_time)
    }

    /// 检查是否超过了指定的空闲超时时间
    /// Check if exceeded specified idle timeout
    pub fn is_idle_timeout(&self, idle_timeout: Duration) -> bool {
        self.time_since_last_recv() > idle_timeout
    }

    /// 检查是否超过了指定的连接超时时间
    /// Check if exceeded specified connection timeout
    pub fn is_connection_timeout(&self, connection_timeout: Duration) -> bool {
        self.connection_duration() > connection_timeout
    }

    /// 获取FIN挂起EOF标志
    /// Get FIN pending EOF flag
    pub fn is_fin_pending_eof(&self) -> bool {
        self.fin_pending_eof
    }

    /// 设置FIN挂起EOF标志
    /// Set FIN pending EOF flag
    pub fn set_fin_pending_eof(&mut self, pending: bool) {
        self.fin_pending_eof = pending;
    }

    /// 标记FIN已处理，等待EOF
    /// Mark FIN processed, waiting for EOF
    pub fn mark_fin_processed(&mut self) {
        self.fin_pending_eof = true;
    }

    /// 清除FIN挂起EOF标志
    /// Clear FIN pending EOF flag
    pub fn clear_fin_pending_eof(&mut self) {
        self.fin_pending_eof = false;
    }

    /// 重置时间管理器（保持开始时间不变）
    /// Reset timing manager (keep start time unchanged)
    pub fn reset(&mut self) {
        let now = Instant::now();
        self.last_recv_time = now;
        self.fin_pending_eof = false;
    }

    /// 重置所有时间为当前时间
    /// Reset all times to current time
    pub fn reset_all(&mut self) {
        let now = Instant::now();
        self.start_time = now;
        self.last_recv_time = now;
        self.fin_pending_eof = false;
    }

    /// 获取统计信息的字符串表示
    /// Get string representation of statistics
    pub fn stats_string(&self) -> String {
        format!(
            "TimingStats {{ connection_duration: {:?}, time_since_last_recv: {:?}, fin_pending_eof: {} }}",
            self.connection_duration(),
            self.time_since_last_recv(),
            self.fin_pending_eof
        )
    }

    // === 超时控制逻辑封装 Timeout Control Logic Encapsulation ===

    /// 计算下一次唤醒时间，用于事件循环的超时控制
    /// Calculate next wakeup time for event loop timeout control
    ///
    /// 该方法封装了事件循环中的超时计算逻辑，根据连接状态和RTO截止时间
    /// 来确定下一次唤醒的时间点。
    ///
    /// This method encapsulates the timeout calculation logic in the event loop,
    /// determining the next wakeup time based on connection state and RTO deadline.
    pub fn calculate_next_wakeup(
        &self,
        config: &Config,
        is_syn_received: bool,
        rto_deadline: Option<Instant>,
    ) -> Instant {
        if is_syn_received {
            // 在SynReceived状态下，我们不设置超时，等待用户接受
            // In SynReceived state, we don't set a timeout, wait for user to accept
            Instant::now() + config.connection.idle_timeout
        } else {
            // 使用RTO截止时间，如果没有则使用空闲超时
            // Use RTO deadline, or idle timeout if none
            rto_deadline.unwrap_or_else(|| Instant::now() + config.connection.idle_timeout)
        }
    }

    /// 检查是否发生了空闲超时
    /// Check if idle timeout has occurred
    ///
    /// 该方法封装了空闲超时检查逻辑，用于判断连接是否因为长时间无活动而超时。
    ///
    /// This method encapsulates idle timeout checking logic to determine if
    /// the connection has timed out due to prolonged inactivity.
    pub fn check_idle_timeout(&self, config: &Config, now: Instant) -> bool {
        now.saturating_duration_since(self.last_recv_time) > config.connection.idle_timeout
    }

    /// 检查路径验证是否超时
    /// Check if path validation has timed out
    ///
    /// 该方法封装了路径验证超时检查逻辑，用于判断路径验证过程是否超时。
    ///
    /// This method encapsulates path validation timeout checking logic to determine
    /// if the path validation process has timed out.
    pub fn check_path_validation_timeout(&self, config: &Config, now: Instant) -> bool {
        now.saturating_duration_since(self.last_recv_time) > config.connection.idle_timeout
    }

    /// 获取超时检查结果
    /// Get timeout check results
    ///
    /// 该方法提供了一个统一的接口来检查各种超时情况，返回超时事件的可选值。
    ///
    /// This method provides a unified interface to check various timeout conditions,
    /// returning an optional timeout event.
    pub fn check_timeouts(&self, config: &Config, now: Instant) -> Option<TimeoutEvent> {
        // 检查空闲超时
        // Check idle timeout
        if self.check_idle_timeout(config, now) {
            return Some(TimeoutEvent::IdleTimeout);
        }

        // 如果没有超时，返回None
        // If no timeout, return None
        None
    }

    /// 更新最后接收时间并重置相关超时状态
    /// Update last receive time and reset related timeout states
    ///
    /// 该方法在接收到数据包时调用，用于更新时间戳并重置超时相关的状态。
    ///
    /// This method is called when receiving packets to update timestamps
    /// and reset timeout-related states.
    pub fn on_packet_received(&mut self, now: Instant) {
        self.last_recv_time = now;
        // 可以在这里添加其他接收数据包时需要重置的状态
        // Can add other states that need to be reset when receiving packets here
    }

    /// 获取距离下次超时检查的剩余时间
    /// Get remaining time until next timeout check
    ///
    /// 该方法计算距离下次可能发生超时的剩余时间，用于优化事件循环的等待时间。
    ///
    /// This method calculates the remaining time until the next possible timeout,
    /// used to optimize the event loop's wait time.
    pub fn time_until_next_timeout(&self, config: &Config) -> Duration {
        // 返回最小的剩余时间
        // Return the minimum remaining time
        config
            .connection
            .idle_timeout
            .saturating_sub(self.time_since_last_recv())
    }

    /// 检查是否应该触发超时处理
    /// Check if timeout handling should be triggered
    ///
    /// 该方法结合了超时检查和时间判断，用于确定是否需要在事件循环中触发超时处理。
    ///
    /// This method combines timeout checking and time judgment to determine
    /// if timeout handling should be triggered in the event loop.
    pub fn should_trigger_timeout_handling(&self, config: &Config, now: Instant) -> bool {
        self.check_timeouts(config, now).is_some()
    }

    /// 获取超时相关的调试信息
    /// Get timeout-related debug information
    ///
    /// 该方法返回包含超时状态的调试信息字符串，用于日志记录和调试。
    ///
    /// This method returns a debug information string containing timeout states
    /// for logging and debugging purposes.
    pub fn timeout_debug_info(&self, config: &Config) -> String {
        format!(
            "TimeoutDebug {{ time_since_last_recv: {:?}, idle_timeout: {:?}, remaining: {:?} }}",
            self.time_since_last_recv(),
            config.connection.idle_timeout,
            self.time_until_next_timeout(config)
        )
    }

    // === 分层超时管理接口 Layered Timeout Management Interface ===

    /// 检查连接级超时事件
    /// Check connection-level timeout events
    ///
    /// 该方法检查所有连接级的超时情况，返回发生的超时事件列表。
    /// 这是分层超时管理架构中连接层的统一入口。
    ///
    /// This method checks all connection-level timeout conditions and returns
    /// a list of timeout events that have occurred. This is the unified entry
    /// point for the connection layer in the layered timeout management architecture.
    pub fn check_connection_timeouts(&self, config: &Config, now: Instant) -> Vec<TimeoutEvent> {
        let mut events = Vec::new();

        // 检查空闲超时
        // Check idle timeout
        if self.check_idle_timeout(config, now) {
            events.push(TimeoutEvent::IdleTimeout);
        }

        // 检查路径验证超时
        // Check path validation timeout
        if self.check_path_validation_timeout(config, now) {
            events.push(TimeoutEvent::PathValidationTimeout);
        }

        events
    }

    /// 获取下一个连接级超时的截止时间
    /// Get the deadline for the next connection-level timeout
    ///
    /// 该方法计算所有连接级超时中最早的截止时间，用于事件循环的等待时间优化。
    ///
    /// This method calculates the earliest deadline among all connection-level
    /// timeouts, used for optimizing event loop wait times.
    pub fn next_connection_timeout_deadline(&self, config: &Config) -> Option<Instant> {
        let idle_deadline = self.last_recv_time + config.connection.idle_timeout;

        // 目前只有空闲超时，未来可以添加其他连接级超时
        // Currently only idle timeout, can add other connection-level timeouts in the future
        Some(idle_deadline)
    }

    // === 统一调度器方法 Unified Scheduler Methods ===

    /// 使用统一调度器计算下一次唤醒时间
    /// Calculate next wakeup time using unified scheduler
    pub fn calculate_unified_wakeup(&mut self, layers: &[&dyn TimeoutLayer]) -> Instant {
        self.unified_scheduler.calculate_unified_deadline(layers)
    }

    /// 使用统一调度器检查所有层的超时事件
    /// Check timeout events for all layers using unified scheduler
    pub fn check_unified_timeout_events(&mut self, layers: &mut [&mut dyn TimeoutLayer]) -> Vec<TimeoutCheckResult> {
        self.unified_scheduler.check_unified_timeout_events(layers)
    }

    /// 获取统一调度器的性能统计
    /// Get performance statistics from unified scheduler
    pub fn unified_scheduler_stats(&self) -> String {
        let stats = self.unified_scheduler.stats();
        format!(
            "UnifiedScheduler {{ total_checks: {}, cache_hits: {}, cache_hit_rate: {:.2}%, batch_checks: {}, prediction_hits: {}, avg_check_duration: {:?} }}",
            stats.total_checks,
            stats.cache_hits,
            self.unified_scheduler.cache_hit_rate() * 100.0,
            stats.batch_checks,
            stats.prediction_hits,
            self.unified_scheduler.avg_check_duration()
        )
    }

    /// 清理统一调度器的过期数据
    /// Cleanup expired data in unified scheduler
    pub fn cleanup_unified_scheduler(&mut self) {
        let now = Instant::now();
        self.unified_scheduler.cleanup_expired_data(now);
    }

    /// 重置统一调度器的统计信息
    /// Reset unified scheduler statistics
    pub fn reset_unified_scheduler_stats(&mut self) {
        self.unified_scheduler.reset_stats();
    }
}

// === TimeoutLayer trait 实现 TimeoutLayer trait implementation ===

impl TimeoutLayer for TimingManager {
    fn next_deadline(&self) -> Option<Instant> {
        // 使用默认的空闲超时配置计算下一个截止时间
        // Calculate next deadline using default idle timeout config
        let default_idle_timeout = Duration::from_secs(30);
        Some(self.last_recv_time + default_idle_timeout)
    }
    
    fn check_timeout_events(&mut self, _now: Instant) -> TimeoutCheckResult {
        // 检查全局定时器事件（职责分离：仅返回事件，不包含重传帧）
        // Check global timer events (separated responsibility: only events, no retransmission frames)
        let events = self.check_timer_events();
        
        TimeoutCheckResult {
            events,
        }
    }
    
    fn layer_name(&self) -> &'static str {
        "TimingManager"
    }
    
    fn stats(&self) -> Option<String> {
        Some(format!(
            "connection_duration: {:?}, time_since_last_recv: {:?}, fin_pending_eof: {}",
            self.connection_duration(),
            self.time_since_last_recv(),
            self.fin_pending_eof
        ))
    }
}

// 注意：TimingManager 不再实现 Default 和 Clone，因为它需要明确的参数
// Note: TimingManager no longer implements Default and Clone as it requires explicit parameters

// #[cfg(test)]
// mod tests {
//     use super::*;
//     use tokio::time::{Duration, sleep};
//     use crate::timer::task::{GlobalTimerTaskHandle, start_global_timer_task};

//     // 创建测试用的定时器句柄
//     fn create_test_timer_handle() -> HybridTimerTaskHandle<TimeoutEvent> {
//         // 为测试创建真实的全局定时器任务
//         crate::timer::task::start_global_timer_task()
//     }

//     #[tokio::test]
//     async fn test_timing_manager_creation() {
//         let connection_id = 1;
//         let timer_handle = create_test_timer_handle();
//         let manager = TimingManager::new(connection_id, timer_handle.clone());

//         assert!(manager.connection_duration() <= Duration::from_millis(1));
//         assert!(manager.time_since_last_recv() <= Duration::from_millis(1));
//         assert!(!manager.is_fin_pending_eof());
        
//         // 清理定时器任务
//         let _ = timer_handle.shutdown().await;
//     }

//     #[tokio::test]
//     async fn test_timing_manager_with_start_time() {
//         let start = Instant::now();
//         let connection_id = 1;
//         let timer_handle = create_test_timer_handle();
//         let manager = TimingManager::with_start_time(start, connection_id, timer_handle.clone());

//         assert_eq!(manager.start_time(), start);
//         assert_eq!(manager.last_recv_time(), start);
        
//         // 清理定时器任务
//         let _ = timer_handle.shutdown().await;
//     }

//     #[tokio::test]
//     async fn test_timing_manager_updates() {
//         let connection_id = 1;
//         let timer_handle = create_test_timer_handle();
//         let mut manager = TimingManager::new(connection_id, timer_handle.clone());
//         let initial_start = manager.start_time();

//         // 等待一小段时间
//         sleep(Duration::from_millis(10)).await;

//         // 更新最后接收时间
//         manager.touch_last_recv_time();

//         // 检查连接持续时间是否增加
//         assert!(manager.connection_duration() >= Duration::from_millis(10));
//         assert!(manager.time_since_last_recv() <= Duration::from_millis(1));

//         // 检查开始时间没有改变
//         assert_eq!(manager.start_time(), initial_start);
        
//         // 清理定时器任务
//         let _ = timer_handle.shutdown().await;
//     }

//     // 为了测试基本的时间功能，我们创建一个不需要定时器的简化测试结构
//     struct SimpleTimingState {
//         start_time: Instant,
//         last_recv_time: Instant,
//         fin_pending_eof: bool,
//     }

//     impl SimpleTimingState {
//         fn new() -> Self {
//             let now = Instant::now();
//             Self {
//                 start_time: now,
//                 last_recv_time: now,
//                 fin_pending_eof: false,
//             }
//         }

//         fn connection_duration(&self) -> Duration {
//             self.last_recv_time.duration_since(self.start_time)
//         }

//         fn time_since_last_recv(&self) -> Duration {
//             Instant::now().duration_since(self.last_recv_time)
//         }

//         fn is_idle_timeout(&self, idle_timeout: Duration) -> bool {
//             self.time_since_last_recv() > idle_timeout
//         }

//         fn touch_last_recv_time(&mut self) {
//             self.last_recv_time = Instant::now();
//         }
//     }

//     #[tokio::test]
//     async fn test_timeout_checks() {
//         let mut state = SimpleTimingState::new();

//         // 测试空闲超时
//         assert!(!state.is_idle_timeout(Duration::from_secs(1)));

//         // 等待一段时间后检查超时
//         sleep(Duration::from_millis(50)).await;
//         assert!(state.is_idle_timeout(Duration::from_millis(10)));

//         // 更新接收时间后应该不再超时
//         state.touch_last_recv_time();
//         assert!(!state.is_idle_timeout(Duration::from_millis(10)));
//     }

//     #[test]
//     fn test_fin_pending_eof_operations() {
//         let mut state = SimpleTimingState::new();

//         // 初始状态
//         assert!(!state.fin_pending_eof);

//         // 设置FIN挂起
//         state.fin_pending_eof = true;
//         assert!(state.fin_pending_eof);

//         // 清除FIN挂起
//         state.fin_pending_eof = false;
//         assert!(!state.fin_pending_eof);
//     }

//     #[tokio::test]
//     async fn test_timing_state_updates() {
//         let mut state = SimpleTimingState::new();
//         let initial_start = state.start_time;

//         // 等待一小段时间
//         sleep(Duration::from_millis(10)).await;

//         // 更新最后接收时间
//         state.touch_last_recv_time();

//         // 检查连接持续时间是否增加
//         assert!(state.connection_duration() >= Duration::from_millis(10));
//         assert!(state.time_since_last_recv() <= Duration::from_millis(1));

//         // 检查开始时间没有改变
//         assert_eq!(state.start_time, initial_start);
//     }

//     // === 超时控制逻辑测试 Timeout Control Logic Tests ===

//     #[tokio::test]
//     async fn test_calculate_next_wakeup_syn_received() {
//         use crate::config::Config;

//         let connection_id = 1;
//         let timer_handle = create_test_timer_handle();
//         let manager = TimingManager::new(connection_id, timer_handle.clone());
//         let config = Config::default();
//         let now = Instant::now();

//         // 在SynReceived状态下，应该使用idle_timeout
//         // In SynReceived state, should use idle_timeout
//         let wakeup = manager.calculate_next_wakeup(&config, true, None);
//         let expected = now + config.connection.idle_timeout;

//         // 允许一些时间误差
//         // Allow some time tolerance
//         assert!(wakeup >= expected - Duration::from_millis(100));
//         assert!(wakeup <= expected + Duration::from_millis(100));
        
//         // 清理定时器任务
//         let _ = timer_handle.shutdown().await;
//     }

//     #[tokio::test]
//     async fn test_calculate_next_wakeup_with_rto() {
//         use crate::config::Config;

//         let connection_id = 1;
//         let timer_handle = create_test_timer_handle();
//         let manager = TimingManager::new(connection_id, timer_handle.clone());
//         let config = Config::default();
//         let now = Instant::now();
//         let rto_deadline = now + Duration::from_millis(500);

//         // 有RTO截止时间时，应该使用RTO截止时间
//         // When RTO deadline exists, should use RTO deadline
//         let wakeup = manager.calculate_next_wakeup(&config, false, Some(rto_deadline));
//         assert_eq!(wakeup, rto_deadline);
        
//         // 清理定时器任务
//         let _ = timer_handle.shutdown().await;
//     }

//     #[tokio::test]
//     async fn test_calculate_next_wakeup_without_rto() {
//         use crate::config::Config;

//         let connection_id = 1;
//         let timer_handle = create_test_timer_handle();
//         let manager = TimingManager::new(connection_id, timer_handle.clone());
//         let config = Config::default();
//         let now = Instant::now();

//         // 没有RTO截止时间时，应该使用idle_timeout
//         // When no RTO deadline, should use idle_timeout
//         let wakeup = manager.calculate_next_wakeup(&config, false, None);
//         let expected = now + config.connection.idle_timeout;

//         // 允许一些时间误差
//         // Allow some time tolerance
//         assert!(wakeup >= expected - Duration::from_millis(100));
//         assert!(wakeup <= expected + Duration::from_millis(100));
        
//         // 清理定时器任务
//         let _ = timer_handle.shutdown().await;
//     }

//     #[tokio::test]
//     async fn test_check_idle_timeout() {
//         use crate::config::Config;

//         // 使用简化的状态测试超时逻辑
//         let mut state = SimpleTimingState::new();
//         let config = Config::default();
//         let now = Instant::now();

//         // 刚创建时不应该超时
//         // Should not timeout when just created
//         let time_since_recv = now.saturating_duration_since(state.last_recv_time);
//         assert!(time_since_recv <= config.connection.idle_timeout);

//         // 等待超过idle_timeout时间
//         // Wait longer than idle_timeout
//         sleep(Duration::from_millis(50)).await;
//         assert!(state.is_idle_timeout(Duration::from_millis(10)));

//         // 更新接收时间后不应该超时
//         // Should not timeout after updating receive time
//         state.touch_last_recv_time();
//         assert!(!state.is_idle_timeout(Duration::from_millis(10)));
//     }

//     #[tokio::test]
//     async fn test_global_timer_integration() {
//         let connection_id = 1;
//         let timer_handle = create_test_timer_handle();
//         let mut manager = TimingManager::new(connection_id, timer_handle.clone());
//         let config = Config::default();

//         // 注册空闲超时定时器
//         let result = manager.register_idle_timeout(&config).await;
//         assert!(result.is_ok());

//         // 注册路径验证超时定时器
//         let result = manager.register_path_validation_timeout(Duration::from_millis(100)).await;
//         assert!(result.is_ok());

//         // 等待定时器到期
//         sleep(Duration::from_millis(150)).await;

//         // 检查定时器事件
//         let events = manager.check_timer_events();
//         assert!(!events.is_empty());
//         assert!(events.contains(&TimeoutEvent::PathValidationTimeout));
        
//         // 清理定时器任务
//         let _ = timer_handle.shutdown().await;
//     }

//     #[tokio::test]
//     async fn test_timer_cancellation() {
//         let connection_id = 2;
//         let timer_handle = create_test_timer_handle();
//         let mut manager = TimingManager::new(connection_id, timer_handle.clone());

//         // 注册定时器
//         let result = manager.register_timer(TimeoutEvent::IdleTimeout, Duration::from_millis(200)).await;
//         assert!(result.is_ok());

//         // 立即取消定时器
//         let cancelled = manager.cancel_timer(&TimeoutEvent::IdleTimeout).await;
//         assert!(cancelled);

//         // 等待一段时间，确保定时器不会触发
//         sleep(Duration::from_millis(250)).await;

//         // 检查没有事件
//         let events = manager.check_timer_events();
//         assert!(events.is_empty());
        
//         // 清理定时器任务
//         let _ = timer_handle.shutdown().await;
//     }

//     #[tokio::test]
//     async fn test_timer_reset_functionality() {
//         let connection_id = 3;
//         let timer_handle = create_test_timer_handle();
//         let mut manager = TimingManager::new(connection_id, timer_handle.clone());
//         let config = Config::default();

//         // 注册空闲超时定时器
//         manager.register_idle_timeout(&config).await.unwrap();

//         // 等待一半时间
//         sleep(Duration::from_millis(config.connection.idle_timeout.as_millis() as u64 / 4)).await;

//         // 重置空闲超时定时器（模拟收到数据包）
//         let result = manager.reset_idle_timeout(&config).await;
//         assert!(result.is_ok());

//         // 再等待一半时间（总共等待了原始超时时间的3/4，但重置后应该不会超时）
//         sleep(Duration::from_millis(config.connection.idle_timeout.as_millis() as u64 / 2)).await;

//         // 检查事件（应该没有超时事件）
//         let events = manager.check_timer_events();
//         assert!(events.is_empty());
        
//         // 清理定时器任务
//         let _ = timer_handle.shutdown().await;
//     }

//     #[tokio::test]
//     async fn test_on_packet_received() {
//         let connection_id = 4;
//         let timer_handle = create_test_timer_handle();
//         let mut manager = TimingManager::new(connection_id, timer_handle.clone());
//         let initial_time = manager.last_recv_time();

//         // 等待一小段时间
//         sleep(Duration::from_millis(10)).await;

//         // 模拟收到数据包
//         let now = Instant::now();
//         manager.on_packet_received(now);

//         // 检查最后接收时间是否更新
//         assert!(manager.last_recv_time() > initial_time);
//         assert!((manager.last_recv_time() - now).as_millis() < 5); // 允许小误差
        
//         // 清理定时器任务
//         let _ = timer_handle.shutdown().await;
//     }

//     #[tokio::test]
//     async fn test_time_until_next_timeout() {
//         use crate::config::Config;

//         let connection_id = 5;
//         let timer_handle = create_test_timer_handle();
//         let manager = TimingManager::new(connection_id, timer_handle.clone());
//         let config = Config::default();

//         // 刚创建时，剩余时间应该接近完整的idle_timeout
//         let remaining = manager.time_until_next_timeout(&config);
//         assert!(remaining <= config.connection.idle_timeout);
//         assert!(remaining >= config.connection.idle_timeout - Duration::from_millis(10));
        
//         // 清理定时器任务
//         let _ = timer_handle.shutdown().await;
//     }

//     #[tokio::test]
//     async fn test_time_until_next_timeout_after_delay() {
//         use crate::config::Config;

//         let connection_id = 6;
//         let timer_handle = create_test_timer_handle();
//         let mut manager = TimingManager::new(connection_id, timer_handle.clone());
//         let config = Config::default();

//         // 等待一段时间
//         sleep(Duration::from_millis(100)).await;

//         // 剩余时间应该减少
//         let remaining = manager.time_until_next_timeout(&config);
//         assert!(remaining < config.connection.idle_timeout);

//         // 更新接收时间后，剩余时间应该重置
//         manager.touch_last_recv_time();
//         let remaining_after_update = manager.time_until_next_timeout(&config);
//         assert!(remaining_after_update > remaining);
        
//         // 清理定时器任务
//         let _ = timer_handle.shutdown().await;
//     }

//     #[tokio::test]
//     async fn test_should_trigger_timeout_handling() {
//         use crate::config::Config;

//         let connection_id = 7;
//         let timer_handle = create_test_timer_handle();
//         let manager = TimingManager::new(connection_id, timer_handle.clone());
//         let config = Config::default();
//         let now = Instant::now();

//         // 刚创建时不应该触发超时处理
//         assert!(!manager.should_trigger_timeout_handling(&config, now));

//         // 模拟超时情况
//         let future_time = now + config.connection.idle_timeout + Duration::from_millis(100);
//         assert!(manager.should_trigger_timeout_handling(&config, future_time));
        
//         // 清理定时器任务
//         let _ = timer_handle.shutdown().await;
//     }

//     #[tokio::test]
//     async fn test_timeout_debug_info() {
//         use crate::config::Config;

//         let connection_id = 8;
//         let timer_handle = create_test_timer_handle();
//         let manager = TimingManager::new(connection_id, timer_handle.clone());
//         let config = Config::default();

//         let debug_info = manager.timeout_debug_info(&config);
//         assert!(debug_info.contains("TimeoutDebug"));
//         assert!(debug_info.contains("time_since_last_recv"));
//         assert!(debug_info.contains("idle_timeout"));
//         assert!(debug_info.contains("remaining"));
        
//         // 清理定时器任务
//         let _ = timer_handle.shutdown().await;
//     }

//     // === 分层超时管理接口测试 Layered Timeout Management Interface Tests ===

//     #[tokio::test]
//     async fn test_check_connection_timeouts_no_timeout() {
//         use crate::config::Config;

//         let connection_id = 9;
//         let timer_handle = create_test_timer_handle();
//         let manager = TimingManager::new(connection_id, timer_handle.clone());
//         let config = Config::default();
//         let now = Instant::now();

//         // 刚创建时不应该有超时事件
//         let events = manager.check_connection_timeouts(&config, now);
//         assert!(events.is_empty());
        
//         // 清理定时器任务
//         let _ = timer_handle.shutdown().await;
//     }

//     #[tokio::test]
//     async fn test_check_connection_timeouts_idle_timeout() {
//         use crate::config::Config;

//         let connection_id = 10;
//         let timer_handle = create_test_timer_handle();
//         let manager = TimingManager::new(connection_id, timer_handle.clone());
//         let config = Config::default();

//         // 模拟空闲超时
//         let timeout_time = manager.last_recv_time() + config.connection.idle_timeout + Duration::from_millis(100);
//         let events = manager.check_connection_timeouts(&config, timeout_time);
        
//         assert_eq!(events.len(), 2); // IdleTimeout 和 PathValidationTimeout
//         assert!(events.contains(&TimeoutEvent::IdleTimeout));
//         assert!(events.contains(&TimeoutEvent::PathValidationTimeout));
        
//         // 清理定时器任务
//         let _ = timer_handle.shutdown().await;
//     }

//     #[tokio::test]
//     async fn test_next_connection_timeout_deadline() {
//         use crate::config::Config;

//         let connection_id = 11;
//         let timer_handle = create_test_timer_handle();
//         let manager = TimingManager::new(connection_id, timer_handle.clone());
//         let config = Config::default();

//         let deadline = manager.next_connection_timeout_deadline(&config);
//         assert!(deadline.is_some());
        
//         let expected_deadline = manager.last_recv_time() + config.connection.idle_timeout;
//         assert_eq!(deadline.unwrap(), expected_deadline);
        
//         // 清理定时器任务
//         let _ = timer_handle.shutdown().await;
//     }

//     #[tokio::test]
//     async fn test_connection_timeout_deadline_after_activity() {
//         use crate::config::Config;

//         let connection_id = 12;
//         let timer_handle = create_test_timer_handle();
//         let mut manager = TimingManager::new(connection_id, timer_handle.clone());
//         let config = Config::default();

//         let initial_deadline = manager.next_connection_timeout_deadline(&config).unwrap();

//         // 等待一段时间后更新活动时间
//         sleep(Duration::from_millis(50)).await;
//         manager.touch_last_recv_time();

//         let updated_deadline = manager.next_connection_timeout_deadline(&config).unwrap();
        
//         // 新的截止时间应该比初始截止时间晚
//         assert!(updated_deadline > initial_deadline);
        
//         // 清理定时器任务
//         let _ = timer_handle.shutdown().await;
//     }

//     #[test]
//     fn test_timeout_event_enum() {
//         // 测试超时事件枚举的基本功能
//         // Test basic functionality of timeout event enum
//         assert_eq!(TimeoutEvent::IdleTimeout, TimeoutEvent::IdleTimeout);
//         assert_ne!(
//             TimeoutEvent::IdleTimeout,
//             TimeoutEvent::PathValidationTimeout
//         );
//         assert_ne!(
//             TimeoutEvent::RetransmissionTimeout,
//             TimeoutEvent::ConnectionTimeout
//         );

//         // 测试Debug trait
//         // Test Debug trait
//         let event = TimeoutEvent::IdleTimeout;
//         let debug_str = format!("{:?}", event);
//         assert!(debug_str.contains("IdleTimeout"));

//         // 测试Clone trait
//         // Test Clone trait
//         let event1 = TimeoutEvent::PathValidationTimeout;
//         let event2 = event1.clone();
//         assert_eq!(event1, event2);
//     }

//     #[tokio::test]
//     async fn test_caller_side_performance() {
//         let handle = start_global_timer_task();
//         let mut manager = TimerManager::new(1, handle.clone());
//         let config = Config::default();

//         // 性能测试：频繁的定时器重置操作
//         // Performance test: frequent timer reset operations
//         let start_time = tokio::time::Instant::now();
        
//         for _ in 0..100 {
//             // 这个操作现在应该更高效，因为：
//             // 1. 减少了不必要的克隆
//             // 2. 异步取消不阻塞注册
//             // 3. 使用了对象池
//             // This operation should now be more efficient because:
//             // 1. Reduced unnecessary cloning
//             // 2. Async cancellation doesn't block registration  
//             // 3. Uses object pool
//             manager.reset_idle_timeout(&config).await.unwrap();
//         }
        
//         let duration = start_time.elapsed();
//         println!("100次定时器重置耗时: {:?}", duration);
//         println!("平均每次重置: {:?}", duration / 100);

//         // 性能断言：100次重置应该在100ms内完成（debug模式下的宽松要求）
//         // Performance assertion: 100 resets should complete within 100ms (lenient for debug mode)
//         assert!(duration < Duration::from_millis(100), 
//             "调用端性能不达标: {:?}", duration);

//         // 测试事件检查的性能
//         // Test event checking performance  
//         let start_time = tokio::time::Instant::now();
//         for _ in 0..1000 {
//             manager.check_timer_events();
//         }
//         let check_duration = start_time.elapsed();
//         println!("1000次事件检查耗时: {:?}", check_duration);
//         println!("平均每次检查: {:?}", check_duration / 1000);

//         handle.shutdown().await.unwrap();
//     }

//     #[tokio::test]
//     async fn test_unified_scheduler_performance() {
//         let handle = start_global_timer_task();
//         let mut timing_manager = TimingManager::new(1, handle.clone());

//         // 性能测试：统一调度器的性能
//         // Performance test: unified scheduler performance
//         let start_time = tokio::time::Instant::now();
        
//         // 模拟多个层的超时检查
//         // Simulate timeout checking for multiple layers
//         for _ in 0..100 {
//             // 创建一个临时的 TimingManager 作为层
//             // Create a temporary TimingManager as a layer
//                     // 使用统一调度器计算唤醒时间（直接测试）
//         // Use unified scheduler to calculate wakeup time (direct test)
//         let temp_manager = TimingManager::new(2, handle.clone());
//         let layers: Vec<&dyn TimeoutLayer> = vec![&temp_manager];
//         let _wakeup_time = timing_manager.calculate_unified_wakeup(&layers);
//         }
        
//         let duration = start_time.elapsed();
//         println!("100次统一调度器唤醒计算耗时: {:?}", duration);
//         println!("平均每次计算: {:?}", duration / 100);

//         // 检查缓存命中率
//         // Check cache hit rate
//         let stats = timing_manager.unified_scheduler_stats();
//         println!("统一调度器统计: {}", stats);

//         // 性能断言：100次计算应该在50ms内完成
//         // Performance assertion: 100 calculations should complete within 50ms
//         assert!(duration < Duration::from_millis(50), 
//             "统一调度器性能不达标: {:?}", duration);

//         handle.shutdown().await.unwrap();
//     }

//     #[tokio::test]
//     async fn test_unified_scheduler_cache_efficiency() {
//         let handle = start_global_timer_task();
//         let mut timing_manager = TimingManager::new(1, handle.clone());

//         // 测试缓存效率（使用新的分离接口）
//         // Test cache efficiency (using new separated interfaces)
//         let temp_manager = TimingManager::new(2, handle.clone());
//         let layers: Vec<&dyn TimeoutLayer> = vec![&temp_manager];
        
//         // 连续多次调用应该命中缓存
//         // Multiple consecutive calls should hit cache
//         for _ in 0..10 {
//             let _wakeup_time = timing_manager.calculate_unified_wakeup(&layers);
//         }
        
//         // 检查缓存命中率应该很高
//         // Cache hit rate should be high
//         let cache_hit_rate = timing_manager.unified_scheduler.cache_hit_rate();
//         println!("缓存命中率: {:.2}%", cache_hit_rate * 100.0);
        
//         // 除了第一次，其他都应该命中缓存
//         // All calls except the first should hit cache
//         assert!(cache_hit_rate >= 0.8, "缓存命中率过低: {:.2}%", cache_hit_rate * 100.0);

//         handle.shutdown().await.unwrap();
//     }
// }
