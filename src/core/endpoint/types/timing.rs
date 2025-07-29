//! 时间管理模块 - 管理端点的时间相关状态
//! Timing Management Module - Manages time-related state for endpoints
//!
//! 该模块封装了端点的所有时间相关字段和逻辑，包括连接开始时间、
//! 最后接收时间、超时检查等功能。
//!
//! This module encapsulates all time-related fields and logic for endpoints,
//! including connection start time, last receive time, timeout checks, etc.

use tokio::time::{Duration, Instant};

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
}

impl Default for TimingManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{sleep, Duration};

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
}