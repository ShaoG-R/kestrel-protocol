//! 时间管理模块 - 管理端点的时间相关状态
//! Timing Management Module - Manages time-related state for endpoints
//!
//! 该模块封装了端点的所有时间相关字段和逻辑，包括连接开始时间、
//! 最后接收时间、超时检查等功能。
//!
//! This module encapsulates all time-related fields and logic for endpoints,
//! including connection start time, last receive time, timeout checks, etc.

use crate::config::Config;
use tokio::time::{Duration, Instant};

/// 超时事件类型枚举
/// Timeout event type enumeration
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// 时间管理器
/// Timing manager
#[derive(Debug, Clone)]
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
}

impl TimingManager {
    /// 创建新的时间管理器
    /// Create new timing manager
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            start_time: now,
            last_recv_time: now,
            fin_pending_eof: false,
        }
    }

    /// 创建带指定开始时间的时间管理器
    /// Create timing manager with specified start time
    pub fn with_start_time(start_time: Instant) -> Self {
        Self {
            start_time,
            last_recv_time: start_time,
            fin_pending_eof: false,
        }
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
        let idle_remaining = config
            .connection
            .idle_timeout
            .saturating_sub(self.time_since_last_recv());

        // 返回最小的剩余时间
        // Return the minimum remaining time
        idle_remaining
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
}



impl Default for TimingManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{Duration, sleep};

    #[test]
    fn test_timing_manager_creation() {
        let manager = TimingManager::new();

        assert!(manager.connection_duration() <= Duration::from_millis(1));
        assert!(manager.time_since_last_recv() <= Duration::from_millis(1));
        assert!(!manager.is_fin_pending_eof());
    }

    #[test]
    fn test_timing_manager_with_start_time() {
        let start = Instant::now();
        let manager = TimingManager::with_start_time(start);

        assert_eq!(manager.start_time(), start);
        assert_eq!(manager.last_recv_time(), start);
    }

    #[tokio::test]
    async fn test_timing_manager_updates() {
        let mut manager = TimingManager::new();
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
    }

    #[tokio::test]
    async fn test_timeout_checks() {
        let mut manager = TimingManager::new();

        // 测试空闲超时
        assert!(!manager.is_idle_timeout(Duration::from_secs(1)));

        // 等待一段时间后检查超时
        sleep(Duration::from_millis(50)).await;
        assert!(manager.is_idle_timeout(Duration::from_millis(10)));

        // 更新接收时间后应该不再超时
        manager.touch_last_recv_time();
        assert!(!manager.is_idle_timeout(Duration::from_millis(10)));
    }

    #[test]
    fn test_fin_pending_eof_operations() {
        let mut manager = TimingManager::new();

        // 初始状态
        assert!(!manager.is_fin_pending_eof());

        // 设置FIN挂起
        manager.mark_fin_processed();
        assert!(manager.is_fin_pending_eof());

        // 清除FIN挂起
        manager.clear_fin_pending_eof();
        assert!(!manager.is_fin_pending_eof());

        // 使用直接设置方法
        manager.set_fin_pending_eof(true);
        assert!(manager.is_fin_pending_eof());
    }

    #[test]
    fn test_timing_manager_reset() {
        let mut manager = TimingManager::new();
        let original_start = manager.start_time();

        // 修改状态
        manager.mark_fin_processed();

        // 重置（保持开始时间）
        manager.reset();
        assert_eq!(manager.start_time(), original_start);
        assert!(!manager.is_fin_pending_eof());

        // 完全重置
        manager.mark_fin_processed();
        manager.reset_all();
        assert!(manager.start_time() > original_start);
        assert!(!manager.is_fin_pending_eof());
    }

    #[test]
    fn test_stats_string() {
        let manager = TimingManager::new();
        let stats = manager.stats_string();

        assert!(stats.contains("TimingStats"));
        assert!(stats.contains("connection_duration"));
        assert!(stats.contains("time_since_last_recv"));
        assert!(stats.contains("fin_pending_eof"));
    }

    // === 超时控制逻辑测试 Timeout Control Logic Tests ===

    #[test]
    fn test_calculate_next_wakeup_syn_received() {
        use crate::config::Config;

        let manager = TimingManager::new();
        let config = Config::default();
        let now = Instant::now();

        // 在SynReceived状态下，应该使用idle_timeout
        // In SynReceived state, should use idle_timeout
        let wakeup = manager.calculate_next_wakeup(&config, true, None);
        let expected = now + config.connection.idle_timeout;

        // 允许一些时间误差
        // Allow some time tolerance
        assert!(wakeup >= expected - Duration::from_millis(10));
        assert!(wakeup <= expected + Duration::from_millis(10));
    }

    #[test]
    fn test_calculate_next_wakeup_with_rto() {
        use crate::config::Config;

        let manager = TimingManager::new();
        let config = Config::default();
        let rto_deadline = Instant::now() + Duration::from_millis(500);

        // 有RTO截止时间时，应该使用RTO截止时间
        // When RTO deadline exists, should use RTO deadline
        let wakeup = manager.calculate_next_wakeup(&config, false, Some(rto_deadline));
        assert_eq!(wakeup, rto_deadline);
    }

    #[test]
    fn test_calculate_next_wakeup_without_rto() {
        use crate::config::Config;

        let manager = TimingManager::new();
        let config = Config::default();
        let now = Instant::now();

        // 没有RTO截止时间时，应该使用idle_timeout
        // When no RTO deadline, should use idle_timeout
        let wakeup = manager.calculate_next_wakeup(&config, false, None);
        let expected = now + config.connection.idle_timeout;

        // 允许一些时间误差
        // Allow some time tolerance
        assert!(wakeup >= expected - Duration::from_millis(10));
        assert!(wakeup <= expected + Duration::from_millis(10));
    }

    #[tokio::test]
    async fn test_check_idle_timeout() {
        use crate::config::Config;

        let mut manager = TimingManager::new();
        let config = Config::default();
        let now = Instant::now();

        // 刚创建时不应该超时
        // Should not timeout when just created
        assert!(!manager.check_idle_timeout(&config, now));

        // 等待超过idle_timeout时间
        // Wait longer than idle_timeout
        sleep(Duration::from_millis(50)).await;
        let later = now + config.connection.idle_timeout + Duration::from_millis(100);
        assert!(manager.check_idle_timeout(&config, later));

        // 更新接收时间后不应该超时
        // Should not timeout after updating receive time
        manager.on_packet_received(later);
        assert!(!manager.check_idle_timeout(&config, later));
    }

    #[tokio::test]
    async fn test_check_path_validation_timeout() {
        use crate::config::Config;

        let manager = TimingManager::new();
        let config = Config::default();
        let now = Instant::now();

        // 刚创建时不应该超时
        // Should not timeout when just created
        assert!(!manager.check_path_validation_timeout(&config, now));

        // 等待超过idle_timeout时间（路径验证使用相同的超时时间）
        // Wait longer than idle_timeout (path validation uses same timeout)
        let later = now + config.connection.idle_timeout + Duration::from_millis(100);
        assert!(manager.check_path_validation_timeout(&config, later));
    }

    #[test]
    fn test_check_timeouts() {
        use crate::config::Config;

        let manager = TimingManager::new();
        let config = Config::default();
        let now = Instant::now();

        // 刚创建时不应该有任何超时
        // Should not have any timeout when just created
        assert_eq!(manager.check_timeouts(&config, now), None);

        // 超过idle_timeout时间后应该有空闲超时
        // Should have idle timeout after exceeding idle_timeout
        let later = now + config.connection.idle_timeout + Duration::from_millis(100);
        assert_eq!(
            manager.check_timeouts(&config, later),
            Some(TimeoutEvent::IdleTimeout)
        );
    }

    #[test]
    fn test_on_packet_received() {
        let mut manager = TimingManager::new();
        let initial_time = manager.last_recv_time();

        // 等待一段时间
        // Wait for some time
        let new_time = initial_time + Duration::from_millis(100);
        manager.on_packet_received(new_time);

        // 最后接收时间应该被更新
        // Last receive time should be updated
        assert_eq!(manager.last_recv_time(), new_time);
        assert!(manager.last_recv_time() > initial_time);
    }

    #[test]
    fn test_time_until_next_timeout() {
        use crate::config::Config;

        let manager = TimingManager::new();
        let config = Config::default();

        // 刚创建时，剩余时间应该接近idle_timeout
        // When just created, remaining time should be close to idle_timeout
        let remaining = manager.time_until_next_timeout(&config);
        assert!(remaining <= config.connection.idle_timeout);
        assert!(remaining >= config.connection.idle_timeout - Duration::from_millis(10));
    }

    #[tokio::test]
    async fn test_time_until_next_timeout_after_delay() {
        use crate::config::Config;

        let manager = TimingManager::new();
        let config = Config::default();

        // 等待一段时间
        // Wait for some time
        sleep(Duration::from_millis(50)).await;

        let remaining = manager.time_until_next_timeout(&config);
        // 剩余时间应该减少
        // Remaining time should be reduced
        assert!(remaining < config.connection.idle_timeout);
        assert!(remaining >= Duration::ZERO);
    }



    #[test]
    fn test_should_trigger_timeout_handling() {
        use crate::config::Config;

        let manager = TimingManager::new();
        let config = Config::default();
        let now = Instant::now();

        // 刚创建时不应该触发超时处理
        // Should not trigger timeout handling when just created
        assert!(!manager.should_trigger_timeout_handling(&config, now));

        // 超时后应该触发超时处理
        // Should trigger timeout handling after timeout
        let later = now + config.connection.idle_timeout + Duration::from_millis(100);
        assert!(manager.should_trigger_timeout_handling(&config, later));
    }

    #[test]
    fn test_timeout_debug_info() {
        use crate::config::Config;

        let manager = TimingManager::new();
        let config = Config::default();

        let debug_info = manager.timeout_debug_info(&config);

        // 检查调试信息包含预期的字段
        // Check that debug info contains expected fields
        assert!(debug_info.contains("TimeoutDebug"));
        assert!(debug_info.contains("time_since_last_recv"));
        assert!(debug_info.contains("idle_timeout"));
        assert!(debug_info.contains("remaining"));
    }

    // === 分层超时管理接口测试 Layered Timeout Management Interface Tests ===

    #[test]
    fn test_check_connection_timeouts_no_timeout() {
        use crate::config::Config;

        let manager = TimingManager::new();
        let config = Config::default();
        let now = Instant::now();

        // 刚创建时不应该有任何超时
        // Should not have any timeout when just created
        let events = manager.check_connection_timeouts(&config, now);
        assert!(events.is_empty());
    }

    #[test]
    fn test_check_connection_timeouts_idle_timeout() {
        use crate::config::Config;

        let manager = TimingManager::new();
        let config = Config::default();
        let now = Instant::now();

        // 超过空闲超时时间后应该有空闲超时事件
        // Should have idle timeout event after exceeding idle timeout
        let later = now + config.connection.idle_timeout + Duration::from_millis(100);
        let events = manager.check_connection_timeouts(&config, later);
        
        // 由于路径验证超时和空闲超时使用相同的配置，会同时触发两个事件
        // Since path validation timeout and idle timeout use the same config, both events will be triggered
        assert_eq!(events.len(), 2);
        assert!(events.contains(&TimeoutEvent::IdleTimeout));
        assert!(events.contains(&TimeoutEvent::PathValidationTimeout));
    }

    #[test]
    fn test_check_connection_timeouts_path_validation_timeout() {
        use crate::config::Config;

        let manager = TimingManager::new();
        let config = Config::default();
        let now = Instant::now();

        // 超过路径验证超时时间后应该有路径验证超时事件
        // Should have path validation timeout event after exceeding path validation timeout
        let later = now + config.connection.idle_timeout + Duration::from_millis(100);
        let events = manager.check_connection_timeouts(&config, later);
        
        // 由于路径验证超时使用相同的idle_timeout配置，所以会同时触发两个事件
        // Since path validation timeout uses the same idle_timeout config, both events will be triggered
        assert_eq!(events.len(), 2);
        assert!(events.contains(&TimeoutEvent::IdleTimeout));
        assert!(events.contains(&TimeoutEvent::PathValidationTimeout));
    }

    #[test]
    fn test_next_connection_timeout_deadline() {
        use crate::config::Config;

        let manager = TimingManager::new();
        let config = Config::default();

        // 应该返回空闲超时的截止时间
        // Should return idle timeout deadline
        let deadline = manager.next_connection_timeout_deadline(&config);
        assert!(deadline.is_some());

        let expected_deadline = manager.last_recv_time() + config.connection.idle_timeout;
        assert_eq!(deadline.unwrap(), expected_deadline);
    }

    #[tokio::test]
    async fn test_connection_timeout_deadline_after_activity() {
        use crate::config::Config;

        let mut manager = TimingManager::new();
        let config = Config::default();
        let initial_deadline = manager.next_connection_timeout_deadline(&config).unwrap();

        // 等待一段时间后更新活动时间
        // Wait some time then update activity time
        sleep(Duration::from_millis(50)).await;
        manager.touch_last_recv_time();

        // 截止时间应该被推迟
        // Deadline should be postponed
        let new_deadline = manager.next_connection_timeout_deadline(&config).unwrap();
        assert!(new_deadline > initial_deadline);
    }

    #[test]
    fn test_timeout_event_enum() {
        // 测试超时事件枚举的基本功能
        // Test basic functionality of timeout event enum
        assert_eq!(TimeoutEvent::IdleTimeout, TimeoutEvent::IdleTimeout);
        assert_ne!(TimeoutEvent::IdleTimeout, TimeoutEvent::PathValidationTimeout);
        assert_ne!(TimeoutEvent::RetransmissionTimeout, TimeoutEvent::ConnectionTimeout);

        // 测试Debug trait
        // Test Debug trait
        let event = TimeoutEvent::IdleTimeout;
        let debug_str = format!("{:?}", event);
        assert!(debug_str.contains("IdleTimeout"));

        // 测试Clone trait
        // Test Clone trait
        let event1 = TimeoutEvent::PathValidationTimeout;
        let event2 = event1.clone();
        assert_eq!(event1, event2);
    }
}
