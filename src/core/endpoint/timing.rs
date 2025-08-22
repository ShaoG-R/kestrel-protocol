//! 时间管理模块 - 管理端点的时间相关状态
//! Timing Management Module - Manages time-related state for endpoints
//!
//! 该模块封装了端点的所有时间相关字段和逻辑，包括连接开始时间、
//! 最后接收时间、超时检查等功能。
//!
//! This module encapsulates all time-related fields and logic for endpoints,
//! including connection start time, last receive time, timeout checks, etc.

use crate::config::Config;
// 统一调度器相关已移除，全面事件驱动
use crate::timer::{
    HybridTimerTaskHandle, TimerActorHandle,
    actor::ActorTimerId,
    event::{ConnectionId, TimerEventData},
    task::TimerRegistration,
    task::types::SenderCallback,
};
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
    timer_actor: TimerActorHandle<SenderCallback<TimeoutEvent>>,

    /// 发送超时事件的通道（用于注册定时器）
    /// Channel for sending timeout events (used for timer registration)
    timeout_tx: mpsc::Sender<TimerEventData<TimeoutEvent>>,

    /// 活跃定时器类型集合（简化跟踪）
    /// Active timer types set (simplified tracking)
    active_timer_types: HashMap<TimeoutEvent, bool>,
}

impl TimerManager {
    /// 创建新的定时器管理器并返回接收通道
    /// Create new timer manager and return receiver channel
    ///
    /// 用于事件驱动架构，将接收通道传递给ChannelManager
    /// Used for event-driven architecture, passing receiver channel to ChannelManager
    pub fn new_with_receiver(
        connection_id: ConnectionId,
        timer_handle: HybridTimerTaskHandle<TimeoutEvent, SenderCallback<TimeoutEvent>>,
    ) -> (Self, mpsc::Receiver<TimerEventData<TimeoutEvent>>) {
        // 单一通道由外部拥有接收端；内部不保留接收端，避免“双读”与语义歧义
        let (timeout_tx, external_rx) = mpsc::channel(32);

        // 创建定时器Actor用于批量处理
        // Create timer actor for batch processing
        let timer_actor = crate::timer::start_timer_actor(timer_handle, None);

        let manager = Self {
            connection_id,
            timer_actor,
            timeout_tx,
            active_timer_types: HashMap::new(),
        };

        (manager, external_rx)
    }

    // 轮询式事件检查已移除，事件通过外部通道驱动
    // Polling-based event checks have been removed; events are delivered via external channel

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
            self.timer_actor
                .cancel_timer(self.connection_id, timeout_event)
                .await;
        }

        // 注册新的定时器
        // Register new timer
        let registration = TimerRegistration::new(
            self.connection_id,
            delay,
            timeout_event,
            SenderCallback::new(self.timeout_tx.clone()),
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
            let cancelled_count = self
                .timer_actor
                .cancel_timer(self.connection_id, *timeout_event)
                .await;
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
        let cancel_requests: Vec<_> = self
            .active_timer_types
            .keys()
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
    pub async fn register_path_validation_timeout(
        &mut self,
        delay: Duration,
    ) -> Result<(), &'static str> {
        self.register_timer(TimeoutEvent::PathValidationTimeout, delay)
            .await
    }

    /// 注册连接超时定时器
    /// Register connection timeout timer
    pub async fn register_connection_timeout(
        &mut self,
        delay: Duration,
    ) -> Result<(), &'static str> {
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

    /// FIN是否已发送标志
    /// FIN sent flag
    fin_sent: bool,

    /// 定时器管理器
    /// Timer manager
    timer_manager: TimerManager,

    /// 路径验证统计信息
    /// Path validation statistics
    path_validation_stats: PathValidationStats,
    // 统一调度器已移除，全面事件驱动
}

impl TimingManager {
    /// 创建新的时间管理器
    /// Create new timing manager
    ///
    /// 返回时间管理器和定时器事件接收通道，用于事件驱动架构
    /// Returns timing manager and timer event receiver channel for event-driven architecture
    pub fn new(
        connection_id: ConnectionId,
        timer_handle: HybridTimerTaskHandle<TimeoutEvent, SenderCallback<TimeoutEvent>>,
    ) -> (Self, mpsc::Receiver<TimerEventData<TimeoutEvent>>) {
        let now = Instant::now();
        let (timer_manager, timer_rx) =
            TimerManager::new_with_receiver(connection_id, timer_handle.clone());

        let timing_manager = Self {
            start_time: now,
            last_recv_time: now,
            fin_pending_eof: false,
            fin_processing_timer_id: None,
            fin_sent: false,
            timer_manager,
            path_validation_stats: PathValidationStats::default(),
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
        timer_handle: HybridTimerTaskHandle<TimeoutEvent, SenderCallback<TimeoutEvent>>,
    ) -> (Self, mpsc::Receiver<TimerEventData<TimeoutEvent>>) {
        let (timer_manager, timer_rx) =
            TimerManager::new_with_receiver(connection_id, timer_handle.clone());

        let timing_manager = Self {
            start_time,
            last_recv_time: start_time,
            fin_pending_eof: false,
            fin_processing_timer_id: None,
            fin_sent: false,
            timer_manager,
            path_validation_stats: PathValidationStats::default(),
        };

        (timing_manager, timer_rx)
    }

    // 轮询式定时器事件检查已移除，事件通过外部通道传递
    // Polling-based timer event check has been removed; events are delivered via external channel

    /// 处理超时事件（包含路径验证和FIN处理超时）
    /// Handle timeout event (including path validation and FIN processing timeout handling)
    pub async fn handle_timeout_event(
        &mut self,
        timeout_event: TimeoutEvent,
    ) -> Result<(), &'static str> {
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
        self.timer_manager
            .register_timer(timeout_event, delay)
            .await
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
    pub async fn register_path_validation_timeout(
        &mut self,
        delay: Duration,
    ) -> Result<(), &'static str> {
        self.timer_manager
            .register_path_validation_timeout(delay)
            .await
    }

    /// 注册连接超时定时器
    /// Register connection timeout timer
    pub async fn register_connection_timeout(
        &mut self,
        delay: Duration,
    ) -> Result<(), &'static str> {
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

    // 手动超时检查接口已移除，改为事件驱动

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
                self.timer_manager
                    .timer_actor
                    .cancel_timer_by_id(timer_id)
                    .await;
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
            self.timer_manager
                .timer_actor
                .cancel_timer_by_id(timer_id)
                .await;
        }

        // 注册新的FIN处理定时器（10ms后检查）
        // Register new FIN processing timer (check after 10ms)
        let delay = Duration::from_millis(10);
        let registration = TimerRegistration::new(
            self.timer_manager.connection_id,
            delay,
            TimeoutEvent::FinProcessingTimeout,
            SenderCallback::new(self.timer_manager.timeout_tx.clone()),
        );
        match self
            .timer_manager
            .timer_actor
            .register_timer(registration)
            .await
        {
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

    /// 检查是否已发送FIN帧
    /// Check if FIN frame has been sent
    pub fn is_fin_sent(&self) -> bool {
        self.fin_sent
    }

    /// 标记FIN帧已发送
    /// Mark FIN frame as sent
    pub fn mark_fin_sent(&mut self) {
        self.fin_sent = true;
    }

    /// 更新平均验证时间
    /// Update average validation time
    fn update_average_validation_time(&mut self, new_time: Duration) {
        if self.path_validation_stats.successful_validations == 0 {
            self.path_validation_stats.average_validation_time = new_time;
        } else {
            let total_time = self.path_validation_stats.average_validation_time
                * (self.path_validation_stats.successful_validations as u32)
                + new_time;
            self.path_validation_stats.average_validation_time =
                total_time / ((self.path_validation_stats.successful_validations + 1) as u32);
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
            self.timer_manager
                .timer_actor
                .cancel_timer_by_id(timer_id)
                .await;
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
            self.timer_manager
                .timer_actor
                .cancel_timer_by_id(timer_id)
                .await;
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

    /// 获取定时器事件发送通道的克隆
    /// Get a clone of the timer event sender channel
    pub fn get_timeout_tx(&self) -> mpsc::Sender<TimerEventData<TimeoutEvent>> {
        self.timer_manager.timeout_tx.clone()
    }

    /// 更新最后接收时间并重置相关超时状态
    /// Update last receive time and reset related timeout states
    ///
    /// 该方法在接收到数据包时调用，用于更新时间戳
    ///
    /// This method is called when receiving packets to update timestamps
    pub fn on_packet_received(&mut self, now: Instant) {
        self.last_recv_time = now;
        // 可以在这里添加其他接收数据包时需要重置的状态
        // Can add other states that need to be reset when receiving packets here
    }

    // 统一调度器方法已移除
}

// 统一调度器的 TimeoutLayer 实现已移除

// 注意：TimingManager 不再实现 Default 和 Clone，因为它需要明确的参数
// Note: TimingManager no longer implements Default and Clone as it requires explicit parameters

#[cfg(test)]
mod tests {
    //! Tests for timing module after refactoring to use lifecycle manager for path validation
    //!
    //! 重构后使用lifecycle manager管理路径验证的timing模块测试

    use super::*;
    use crate::config::Config;
    use tokio::time::{Duration, sleep, timeout};

    // 创建测试用的定时器句柄
    fn create_test_timer_handle()
    -> HybridTimerTaskHandle<TimeoutEvent, SenderCallback<TimeoutEvent>> {
        crate::timer::start_hybrid_timer_task::<TimeoutEvent, SenderCallback<TimeoutEvent>>()
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
        let (manager, _timer_rx) =
            TimingManager::with_start_time(start, connection_id, timer_handle.clone());

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
        assert_ne!(
            TimeoutEvent::IdleTimeout,
            TimeoutEvent::PathValidationTimeout
        );

        // 测试新的基于数据包的重传超时事件
        let packet_timeout1 = TimeoutEvent::PacketRetransmissionTimeout {
            sequence_number: 1,
            timer_id: 123,
        };
        let packet_timeout2 = TimeoutEvent::PacketRetransmissionTimeout {
            sequence_number: 1,
            timer_id: 123,
        };
        let packet_timeout3 = TimeoutEvent::PacketRetransmissionTimeout {
            sequence_number: 2,
            timer_id: 124,
        };

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
        let result = manager
            .register_path_validation_timeout(Duration::from_millis(100))
            .await;
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
        let result = manager
            .handle_timeout_event(TimeoutEvent::FinProcessingTimeout)
            .await;
        assert!(result.is_ok());

        // 测试路径验证超时
        let result = manager
            .handle_timeout_event(TimeoutEvent::PathValidationTimeout)
            .await;
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
        assert_eq!(
            manager.path_validation_stats().average_validation_time,
            validation_time
        );

        // 模拟路径验证失败
        manager.on_path_validation_failed(true);
        assert_eq!(manager.path_validation_stats().challenge_failure_count, 1);

        // 清理定时器任务
        let _ = timer_handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_event_driven_idle_timeout() {
        let connection_id = 42;
        let timer_handle = create_test_timer_handle();
        let (mut manager, mut timer_rx) = TimingManager::new(connection_id, timer_handle.clone());

        // 使用较小的空闲超时，验证事件驱动模型
        let mut config = Config::default();
        config.connection.idle_timeout = Duration::from_millis(50);

        // 注册空闲超时定时器
        let res = manager.register_idle_timeout(&config).await;
        assert!(res.is_ok());

        // 在合理时间窗口内应收到 IdleTimeout 事件
        let recv_result = timeout(Duration::from_millis(500), timer_rx.recv()).await;
        assert!(recv_result.is_ok());
        let evt = recv_result.unwrap();
        assert!(evt.is_some());
        let evt = evt.unwrap();
        assert_eq!(evt.connection_id, connection_id);
        assert_eq!(evt.timeout_event, TimeoutEvent::IdleTimeout);

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

    // 手动计算连接级截止时间相关方法已移除，改为事件驱动，不再测试

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

    // 统一调度器相关接口已移除
}
