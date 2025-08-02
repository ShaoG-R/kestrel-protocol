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
    actor::ActorTimerId,
};
use crate::core::endpoint::unified_scheduler::{TimeoutLayer, TimeoutCheckResult, UnifiedTimeoutScheduler};
use std::collections::HashMap;
use tokio::{
    sync::mpsc,
    time::{Duration, Instant},
};
use tracing::{trace, warn};

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
    /// 连接超时 - 连接建立超时
    /// Connection timeout - connection establishment timed out
    ConnectionTimeout,
    /// FIN处理超时 - 检查是否可以发送EOF
    /// FIN processing timeout - check if EOF can be sent
    FinProcessingTimeout,
    /// 基于数据包的重传超时 - 包含具体的数据包信息（新版本）
    /// Packet-based retransmission timeout - contains specific packet info (new version)
    PacketRetransmissionTimeout {
        /// 数据包序列号
        /// Packet sequence number
        sequence_number: u32,
        /// 定时器ID，用于精确匹配
        /// Timer ID for precise matching
        timer_id: crate::timer::actor::ActorTimerId,
    },
}

// Copy trait 现在可以自动派生，因为所有字段都支持 Copy
// Copy trait can now be automatically derived since all fields support Copy

impl Default for TimeoutEvent {
    fn default() -> Self {
        Self::IdleTimeout
    }
}

/// 路径验证统计信息
/// Path validation statistics
#[derive(Debug, Clone, Default)]
pub struct PathValidationStats {
    /// 总验证次数
    /// Total validations
    pub total_validations: u64,
    /// 成功次数
    /// Successful validations
    pub successful_validations: u64,
    /// 超时次数
    /// Timeout count
    pub timeout_count: u64,
    /// 挑战失败次数
    /// Challenge failure count
    pub challenge_failure_count: u64,
    /// 平均验证时间
    /// Average validation time
    pub average_validation_time: Duration,
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
            let cancelled_count = self.timer_actor.cancel_timer(self.connection_id, *timeout_event).await;
            let success = cancelled_count > 0;
            trace!(timeout_event = ?timeout_event, cancelled_count = cancelled_count, success = success, "Cancelled timer via TimerActor");
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

    /// FIN处理定时器ID（如果有活跃的FIN处理定时器）
    /// FIN processing timer ID (if there's an active FIN processing timer)
    fin_processing_timer_id: Option<ActorTimerId>,

    /// 定时器管理器
    /// Timer manager
    timer_manager: TimerManager,

    /// 路径验证统计信息
    /// Path validation statistics
    path_validation_stats: PathValidationStats,

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
        let (timer_manager, timer_rx) = TimerManager::new_with_receiver(connection_id, timer_handle.clone());

        let timing_manager = Self {
            start_time: now,
            last_recv_time: now,
            fin_pending_eof: false,
            fin_processing_timer_id: None,
            timer_manager,
            path_validation_stats: PathValidationStats::default(),
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
        let (timer_manager, timer_rx) = TimerManager::new_with_receiver(connection_id, timer_handle.clone());

        let timing_manager = Self {
            start_time,
            last_recv_time: start_time,
            fin_pending_eof: false,
            fin_processing_timer_id: None,
            timer_manager,
            path_validation_stats: PathValidationStats::default(),
            unified_scheduler: UnifiedTimeoutScheduler::new(),
        };

        (timing_manager, timer_rx)
    }

    /// 检查是否有到期的定时器事件
    /// Check for expired timer events
    pub fn check_timer_events(&mut self) -> Vec<TimeoutEvent> {
        self.timer_manager.check_timer_events()
    }

    /// 处理超时事件（包含路径验证和FIN处理超时）
    /// Handle timeout event (including path validation and FIN processing timeout handling)
    pub async fn handle_timeout_event(&mut self, timeout_event: TimeoutEvent) -> Result<(), &'static str> {
        match timeout_event {
            TimeoutEvent::PathValidationTimeout => {
                // 路径验证超时已由lifecycle manager处理，这里只记录统计信息
                // Path validation timeout is handled by lifecycle manager, only record statistics here
                self.path_validation_stats.timeout_count += 1;
                warn!("Path validation timeout occurred, statistics updated");
                Ok(())
            }
            TimeoutEvent::FinProcessingTimeout => {
                // 清除定时器ID，因为定时器已触发
                // Clear timer ID since timer has fired
                self.fin_processing_timer_id = None;
                
                trace!("FIN processing timeout triggered, checking if EOF can be sent");
                
                // 这里我们只是设置一个标志，实际的EOF处理会在上层代码中完成
                // Here we just set a flag, actual EOF handling will be done in upper layer code
                // 因为EOF处理需要访问channels等Endpoint的其他字段
                // Because EOF handling requires access to channels and other Endpoint fields
                Ok(())
            }
            _ => {
                // 其他超时事件的处理保持不变
                // Other timeout event handling remains unchanged
                Ok(())
            }
        }
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

    /// 设置FIN挂起EOF标志（异步版本，用于注册定时器）
    /// Set FIN pending EOF flag (async version for timer registration)
    pub async fn set_fin_pending_eof(&mut self, pending: bool) {
        if pending && !self.fin_pending_eof {
            // 设置FIN pending EOF时，注册一个短期定时器来检查EOF条件
            // When setting FIN pending EOF, register a short-term timer to check EOF conditions
            self.fin_pending_eof = true;
            self.schedule_fin_processing_check().await;
        } else if !pending {
            // 清除FIN pending EOF时，取消定时器
            // When clearing FIN pending EOF, cancel the timer
            self.fin_pending_eof = false;
            if let Some(timer_id) = self.fin_processing_timer_id.take() {
                self.timer_manager.timer_actor.cancel_timer_by_id(timer_id).await;
            }
        }
    }

    /// 标记FIN已处理，等待EOF（异步版本）
    /// Mark FIN processed, waiting for EOF (async version)
    pub async fn mark_fin_processed(&mut self) {
        self.set_fin_pending_eof(true).await;
    }

    /// 清除FIN挂起EOF标志（异步版本）
    /// Clear FIN pending EOF flag (async version)
    pub async fn clear_fin_pending_eof(&mut self) {
        self.set_fin_pending_eof(false).await;
    }

    /// 调度FIN处理检查
    /// Schedule FIN processing check
    pub async fn schedule_fin_processing_check(&mut self) {
        // 如果已有活跃的定时器，先取消
        // If there's already an active timer, cancel it first
        if let Some(timer_id) = self.fin_processing_timer_id.take() {
            self.timer_manager.timer_actor.cancel_timer_by_id(timer_id).await;
        }
        
        // 注册新的FIN处理定时器（10ms后检查）
        // Register new FIN processing timer (check after 10ms)
        let delay = Duration::from_millis(10);
        match self.timer_manager.timer_actor.register_fin_processing_timer(
            self.timer_manager.connection_id,
            delay
        ).await {
            Ok(timer_id) => {
                self.fin_processing_timer_id = Some(timer_id);
                trace!("Scheduled FIN processing check in {:?}", delay);
            }
            Err(e) => {
                warn!("Failed to schedule FIN processing check: {}", e);
            }
        }
    }

    /// 检查是否应该发送EOF（供上层调用）
    /// Check if EOF should be sent (for upper layer to call)
    pub fn should_send_eof(&self, is_recv_buffer_empty: bool) -> bool {
        self.fin_pending_eof && is_recv_buffer_empty
    }

    // === 路径验证方法 Path Validation Methods ===
    // 注意：路径验证状态现在由 lifecycle manager 管理
    // Note: Path validation state is now managed by lifecycle manager

    /// 获取路径验证统计信息
    /// Get path validation statistics
    pub fn path_validation_stats(&self) -> &PathValidationStats {
        &self.path_validation_stats
    }

    /// 更新路径验证统计信息 - 验证开始
    /// Update path validation statistics - validation started
    pub fn on_path_validation_started(&mut self) {
        self.path_validation_stats.total_validations += 1;
    }

    /// 更新路径验证统计信息 - 验证成功
    /// Update path validation statistics - validation succeeded  
    pub fn on_path_validation_succeeded(&mut self, validation_time: Duration) {
        self.update_average_validation_time(validation_time);
        self.path_validation_stats.successful_validations += 1;
    }

    /// 更新路径验证统计信息 - 验证失败
    /// Update path validation statistics - validation failed
    pub fn on_path_validation_failed(&mut self, is_challenge_failed: bool) {
        if is_challenge_failed {
            self.path_validation_stats.challenge_failure_count += 1;
        }
    }

    /// 更新平均验证时间
    /// Update average validation time
    fn update_average_validation_time(&mut self, new_time: Duration) {
        if self.path_validation_stats.successful_validations == 0 {
            self.path_validation_stats.average_validation_time = new_time;
        } else {
            let total_time = self.path_validation_stats.average_validation_time * (self.path_validation_stats.successful_validations as u32)
                + new_time;
            self.path_validation_stats.average_validation_time = total_time / ((self.path_validation_stats.successful_validations + 1) as u32);
        }
    }

    /// 重置时间管理器（保持开始时间不变）
    /// Reset timing manager (keep start time unchanged)
    pub async fn reset(&mut self) {
        let now = Instant::now();
        self.last_recv_time = now;
        self.fin_pending_eof = false;
        
        // 取消FIN处理定时器
        // Cancel FIN processing timer
        if let Some(timer_id) = self.fin_processing_timer_id.take() {
            self.timer_manager.timer_actor.cancel_timer_by_id(timer_id).await;
        }
        
        // 重置路径验证统计信息（可选）
        // Reset path validation statistics (optional)
        // self.path_validation_stats = PathValidationStats::default();
    }

    /// 重置所有时间为当前时间（异步版本）
    /// Reset all times to current time (async version)
    pub async fn reset_all(&mut self) {
        let now = Instant::now();
        self.start_time = now;
        self.last_recv_time = now;
        self.fin_pending_eof = false;
        
        // 取消FIN处理定时器
        // Cancel FIN processing timer
        if let Some(timer_id) = self.fin_processing_timer_id.take() {
            self.timer_manager.timer_actor.cancel_timer_by_id(timer_id).await;
        }
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
    /// 注意：实际的路径验证状态现在由lifecycle manager管理
    ///
    /// This method encapsulates path validation timeout checking logic to determine
    /// if the path validation process has timed out.
    /// Note: Actual path validation state is now managed by lifecycle manager
    pub fn check_path_validation_timeout(&self, config: &Config, now: Instant) -> bool {
        // 这里使用通用的空闲超时检查，实际的路径验证超时由lifecycle manager和定时器系统处理
        // Use generic idle timeout check here, actual path validation timeout is handled by lifecycle manager and timer system
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

#[cfg(test)]
mod tests {
    //! Tests for timing module after refactoring to use lifecycle manager for path validation
    //! 
    //! 重构后使用lifecycle manager管理路径验证的timing模块测试

    use super::*;
    use crate::config::Config;
    use tokio::time::{sleep, Duration};

    // 创建测试用的定时器句柄
    fn create_test_timer_handle() -> HybridTimerTaskHandle<TimeoutEvent> {
        crate::timer::start_hybrid_timer_task::<TimeoutEvent>()
    }

    #[tokio::test]
    async fn test_timing_manager_creation() {
        let connection_id = 1;
        let timer_handle = create_test_timer_handle();
        let (manager, _timer_rx) = TimingManager::new(connection_id, timer_handle.clone());

        assert!(manager.connection_duration() <= Duration::from_millis(1));
        assert!(manager.time_since_last_recv() <= Duration::from_millis(1));
        assert!(!manager.is_fin_pending_eof());
        
        // 清理定时器任务
        let _ = timer_handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_timing_manager_with_start_time() {
        let start = Instant::now();
        let connection_id = 1;
        let timer_handle = create_test_timer_handle();
        let (manager, _timer_rx) = TimingManager::with_start_time(start, connection_id, timer_handle.clone());

        assert_eq!(manager.start_time(), start);
        assert_eq!(manager.last_recv_time(), start);
        
        // 清理定时器任务
        let _ = timer_handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_timing_manager_updates() {
        let connection_id = 1;
        let timer_handle = create_test_timer_handle();
        let (mut manager, _timer_rx) = TimingManager::new(connection_id, timer_handle.clone());
        let initial_start = manager.start_time();

        // 等待一小段时间
        sleep(Duration::from_millis(10)).await;

        // 更新最后接收时间
        manager.touch_last_recv_time();

        // 检查连接持续时间是否增加
        assert!(manager.connection_duration() >= Duration::from_millis(10));
        assert!(manager.time_since_last_recv() <= Duration::from_millis(1));

        // 检查开始时间没有改变
        assert_eq!(manager.start_time(), initial_start);
        
        // 清理定时器任务
        let _ = timer_handle.shutdown().await;
    }

    #[test]
    fn test_timeout_event_enum() {
        // 测试超时事件枚举的基本功能
        assert_eq!(TimeoutEvent::IdleTimeout, TimeoutEvent::IdleTimeout);
        assert_ne!(TimeoutEvent::IdleTimeout, TimeoutEvent::PathValidationTimeout);
        
        // 测试新的基于数据包的重传超时事件
        let packet_timeout1 = TimeoutEvent::PacketRetransmissionTimeout { sequence_number: 1, timer_id: 123 };
        let packet_timeout2 = TimeoutEvent::PacketRetransmissionTimeout { sequence_number: 1, timer_id: 123 };
        let packet_timeout3 = TimeoutEvent::PacketRetransmissionTimeout { sequence_number: 2, timer_id: 124 };
        
        assert_eq!(packet_timeout1, packet_timeout2);
        assert_ne!(packet_timeout1, packet_timeout3);
        assert_ne!(packet_timeout1, TimeoutEvent::ConnectionTimeout);

        // 测试Debug trait
        let event = TimeoutEvent::IdleTimeout;
        let debug_str = format!("{:?}", event);
        assert!(debug_str.contains("IdleTimeout"));

        // 测试Clone trait
        let event1 = TimeoutEvent::PathValidationTimeout;
        let event2 = event1.clone();
        assert_eq!(event1, event2);
    }

    #[tokio::test]
    async fn test_fin_pending_eof_operations() {
        let connection_id = 1;
        let timer_handle = create_test_timer_handle();
        let (mut manager, _timer_rx) = TimingManager::new(connection_id, timer_handle.clone());

        // 初始状态
        assert!(!manager.is_fin_pending_eof());

        // 设置FIN挂起
        manager.set_fin_pending_eof(true).await;
        assert!(manager.is_fin_pending_eof());

        // 清除FIN挂起
        manager.clear_fin_pending_eof().await;
        assert!(!manager.is_fin_pending_eof());
        
        // 清理定时器任务
        let _ = timer_handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_timer_operations() {
        let connection_id = 1;
        let timer_handle = create_test_timer_handle();
        let (mut manager, _timer_rx) = TimingManager::new(connection_id, timer_handle.clone());
        let config = Config::default();

        // 注册空闲超时定时器
        let result = manager.register_idle_timeout(&config).await;
        assert!(result.is_ok());

        // 注册路径验证超时定时器
        let result = manager.register_path_validation_timeout(Duration::from_millis(100)).await;
        assert!(result.is_ok());

        // 取消定时器
        let cancelled = manager.cancel_timer(&TimeoutEvent::IdleTimeout).await;
        assert!(cancelled);
        
        // 清理定时器任务
        let _ = timer_handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_timeout_event_handling() {
        let connection_id = 1;
        let timer_handle = create_test_timer_handle();
        let (mut manager, _timer_rx) = TimingManager::new(connection_id, timer_handle.clone());

        // 测试FIN处理超时
        let result = manager.handle_timeout_event(TimeoutEvent::FinProcessingTimeout).await;
        assert!(result.is_ok());

        // 测试路径验证超时
        let result = manager.handle_timeout_event(TimeoutEvent::PathValidationTimeout).await;
        assert!(result.is_ok());
        
        // 验证统计信息更新
        assert_eq!(manager.path_validation_stats().timeout_count, 1);
        
        // 清理定时器任务
        let _ = timer_handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_path_validation_statistics() {
        let connection_id = 1;
        let timer_handle = create_test_timer_handle();
        let (mut manager, _timer_rx) = TimingManager::new(connection_id, timer_handle.clone());

        // 初始统计信息
        let stats = manager.path_validation_stats();
        assert_eq!(stats.total_validations, 0);
        assert_eq!(stats.successful_validations, 0);
        assert_eq!(stats.timeout_count, 0);
        assert_eq!(stats.challenge_failure_count, 0);

        // 模拟路径验证开始
        manager.on_path_validation_started();
        assert_eq!(manager.path_validation_stats().total_validations, 1);

        // 模拟路径验证成功
        let validation_time = Duration::from_millis(100);
        manager.on_path_validation_succeeded(validation_time);
        assert_eq!(manager.path_validation_stats().successful_validations, 1);
        assert_eq!(manager.path_validation_stats().average_validation_time, validation_time);

        // 模拟路径验证失败
        manager.on_path_validation_failed(true);
        assert_eq!(manager.path_validation_stats().challenge_failure_count, 1);
        
        // 清理定时器任务
        let _ = timer_handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_timeout_checking() {
        let connection_id = 1;
        let timer_handle = create_test_timer_handle();
        let (manager, _timer_rx) = TimingManager::new(connection_id, timer_handle.clone());
        let config = Config::default();
        let now = Instant::now();

        // 刚创建时不应该超时
        assert!(!manager.check_idle_timeout(&config, now));
        assert!(!manager.should_trigger_timeout_handling(&config, now));

        // 模拟超时情况
        let timeout_time = now + config.connection.idle_timeout + Duration::from_millis(100);
        assert!(manager.check_idle_timeout(&config, timeout_time));
        assert!(manager.should_trigger_timeout_handling(&config, timeout_time));
        
        // 清理定时器任务
        let _ = timer_handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_packet_received_handling() {
        let connection_id = 1;
        let timer_handle = create_test_timer_handle();
        let (mut manager, _timer_rx) = TimingManager::new(connection_id, timer_handle.clone());
        let initial_time = manager.last_recv_time();

        // 等待一小段时间
        sleep(Duration::from_millis(10)).await;

        // 模拟收到数据包
        let now = Instant::now();
        manager.on_packet_received(now);

        // 检查最后接收时间是否更新
        assert!(manager.last_recv_time() > initial_time);
        assert!((manager.last_recv_time() - now).as_millis() < 5); // 允许小误差
        
        // 清理定时器任务
        let _ = timer_handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_connection_timeout_deadline() {
        let connection_id = 1;
        let timer_handle = create_test_timer_handle();
        let (mut manager, _timer_rx) = TimingManager::new(connection_id, timer_handle.clone());
        let config = Config::default();

        let initial_deadline = manager.next_connection_timeout_deadline(&config);
        assert!(initial_deadline.is_some());

        // 等待一段时间后更新活动时间
        sleep(Duration::from_millis(50)).await;
        manager.touch_last_recv_time();

        let updated_deadline = manager.next_connection_timeout_deadline(&config);
        assert!(updated_deadline.is_some());
        
        // 新的截止时间应该比初始截止时间晚
        assert!(updated_deadline.unwrap() > initial_deadline.unwrap());
        
        // 清理定时器任务
        let _ = timer_handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_reset_operations() {
        let connection_id = 1;
        let timer_handle = create_test_timer_handle();
        let (mut manager, _timer_rx) = TimingManager::new(connection_id, timer_handle.clone());
        let original_start = manager.start_time();

        // 设置一些状态
        manager.set_fin_pending_eof(true).await;
        sleep(Duration::from_millis(10)).await;

        // 重置管理器
        manager.reset().await;
        
        // 检查重置后的状态
        assert!(!manager.is_fin_pending_eof());
        assert_eq!(manager.start_time(), original_start); // 开始时间应该保持不变

        // 测试完全重置
        manager.reset_all().await;
        assert!(manager.start_time() > original_start); // 开始时间应该更新
        
        // 清理定时器任务
        let _ = timer_handle.shutdown().await;
    }

    #[test]
    fn test_path_validation_stats() {
        let stats = PathValidationStats::default();
        assert_eq!(stats.total_validations, 0);
        assert_eq!(stats.successful_validations, 0);
        assert_eq!(stats.timeout_count, 0);
        assert_eq!(stats.challenge_failure_count, 0);
        assert_eq!(stats.average_validation_time, Duration::ZERO);
    }

    #[tokio::test]
    async fn test_unified_scheduler_integration() {
        let connection_id = 1;
        let timer_handle = create_test_timer_handle();
        let (mut manager, _timer_rx) = TimingManager::new(connection_id, timer_handle.clone());

        // 测试统一调度器的基本功能
        let stats = manager.unified_scheduler_stats();
        assert!(stats.contains("UnifiedScheduler"));

        // 清理过期数据
        manager.cleanup_unified_scheduler();

        // 重置统计信息
        manager.reset_unified_scheduler_stats();
        
        // 清理定时器任务
        let _ = timer_handle.shutdown().await;
    }
}