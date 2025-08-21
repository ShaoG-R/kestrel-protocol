//! 统一超时事件调度器
//!
//! 该模块提供了一个统一的超时事件调度器，用于协调所有层次的超时检查，
//! 减少重复计算和跨层通信开销，提升整体性能。
//!
//! Unified timeout event scheduler
//!
//! This module provides a unified timeout event scheduler that coordinates 
//! timeout checks across all layers, reducing duplicate computations and 
//! cross-layer communication overhead to improve overall performance.

use std::collections::BTreeMap;
use std::time::Duration;
use tokio::time::Instant;
use tracing::{debug, trace};

use crate::core::endpoint::timing::TimeoutEvent;

/// 超时源标识，用于区分不同层次的超时
/// Timeout source identifier to distinguish timeouts from different layers
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimeoutSource {
    /// 连接级超时（空闲、路径验证等）
    /// Connection-level timeouts (idle, path validation, etc.)
    Connection(TimeoutEvent),
    /// 可靠性层超时（重传、RTT等）
    /// Reliability layer timeouts (retransmission, RTT, etc.)
    Reliability,
    /// 全局定时器系统
    /// Global timer system
    GlobalTimer,
}



/// 统一超时层接口，各层需要实现此trait
/// Unified timeout layer interface, each layer should implement this trait
pub trait TimeoutLayer {
    /// 获取下一个超时的截止时间
    /// Get the deadline for the next timeout
    fn next_deadline(&self) -> Option<Instant>;
    
    /// 获取层名称（用于调试）
    /// Get layer name (for debugging)
    fn layer_name(&self) -> &'static str;
    
    /// 获取统计信息（可选）
    /// Get statistics (optional)
    fn stats(&self) -> Option<String> {
        None
    }
}

/// 超时模式，用于预测性优化
/// Timeout pattern for predictive optimization
#[derive(Debug, Clone)]
pub struct TimeoutPattern {
    /// 模式发生的时间
    /// Time when pattern occurred
    pub timestamp: Instant,
    /// 涉及的超时源
    /// Involved timeout sources
    pub sources: Vec<TimeoutSource>,
    /// 间隔时间
    /// Interval duration
    pub interval: Duration,
}

/// 统一超时事件调度器
/// Unified timeout event scheduler
pub struct UnifiedTimeoutScheduler {
    /// 预先计算的下次检查时间映射
    /// Pre-computed next check time mapping
    next_check_deadlines: BTreeMap<Instant, Vec<TimeoutSource>>,
    
    /// 缓存的统一截止时间计算结果
    /// Cached unified deadline calculation result
    cached_unified_deadline: Option<(Instant, Instant)>, // (computed_at, deadline)
    
    /// 历史超时模式，用于预测性优化
    /// Historical timeout patterns for predictive optimization
    timeout_patterns: Vec<TimeoutPattern>,
    
    /// 时间缓存，减少系统调用
    /// Time cache to reduce system calls
    time_cache: TimeCache,
    
    /// 性能统计
    /// Performance statistics
    stats: SchedulerStats,
}

/// 时间缓存，减少频繁的Instant::now()调用
/// Time cache to reduce frequent Instant::now() calls
#[derive(Debug)]
struct TimeCache {
    cached_now: Instant,
    cache_valid_until: Instant,
    cache_duration: Duration,
}

impl TimeCache {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            cached_now: now,
            cache_valid_until: now,
            cache_duration: Duration::from_millis(1), // 1ms缓存
        }
    }
    
    /// 获取当前时间，使用缓存优化
    /// Get current time with cache optimization
    fn now(&mut self) -> Instant {
        let real_now = Instant::now();
        
        // 如果缓存仍然有效，直接返回缓存值
        // If cache is still valid, return cached value directly
        if real_now < self.cache_valid_until {
            return self.cached_now;
        }
        
        // 更新缓存
        // Update cache
        self.cached_now = real_now;
        self.cache_valid_until = real_now + self.cache_duration;
        real_now
    }
}

/// 调度器性能统计
/// Scheduler performance statistics
#[derive(Debug, Default)]
pub struct SchedulerStats {
    /// 总检查次数
    /// Total check count
    pub total_checks: u64,
    /// 缓存命中次数
    /// Cache hit count
    pub cache_hits: u64,
    /// 批量检查次数
    /// Batch check count
    pub batch_checks: u64,
    /// 预测命中次数
    /// Prediction hit count
    pub prediction_hits: u64,
    /// 总检查耗时
    /// Total check duration
    pub total_check_duration: Duration,
    /// 最后检查时间
    /// Last check time
    pub last_check_time: Option<Instant>,
}

impl UnifiedTimeoutScheduler {
    /// 创建新的统一超时事件调度器
    /// Create a new unified timeout event scheduler
    pub fn new() -> Self {
        Self {
            next_check_deadlines: BTreeMap::new(),
            cached_unified_deadline: None,
            timeout_patterns: Vec::with_capacity(100), // 保留最近100个模式
            time_cache: TimeCache::new(),
            stats: SchedulerStats::default(),
        }
    }
    
    /// 计算统一的下次唤醒截止时间
    /// Calculate unified next wakeup deadline
    pub fn calculate_unified_deadline(&mut self, layers: &[&dyn TimeoutLayer]) -> Instant {
        let now = self.time_cache.now();
        self.stats.total_checks += 1;
        let check_start = Instant::now();
        
        // 检查缓存是否有效（1ms内的计算结果直接复用）
        // Check if cache is valid (reuse results computed within 1ms)
        if let Some((computed_at, deadline)) = self.cached_unified_deadline {
            if now.saturating_duration_since(computed_at) < Duration::from_millis(1) {
                self.stats.cache_hits += 1;
                trace!("使用缓存的统一截止时间: {:?}", deadline);
                return deadline;
            }
        }
        
        // 批量收集所有层的截止时间
        // Batch collect deadlines from all layers
        let mut all_deadlines = Vec::with_capacity(layers.len() * 2);
        for layer in layers {
            if let Some(deadline) = layer.next_deadline() {
                all_deadlines.push(deadline);
                trace!("收集到 {} 层的截止时间: {:?}", layer.layer_name(), deadline);
            }
        }
        
        // 基于历史模式进行预测性优化
        // Predictive optimization based on historical patterns
        if let Some(predicted) = self.predict_next_timeout(now) {
            all_deadlines.push(predicted);
            self.stats.prediction_hits += 1;
            trace!("预测的下次超时时间: {:?}", predicted);
        }
        
        // 计算统一截止时间
        // Calculate unified deadline
        let unified_deadline = all_deadlines.into_iter().min()
            .unwrap_or(now + Duration::from_millis(50)); // 默认50ms
            
        // 缓存结果
        // Cache result
        self.cached_unified_deadline = Some((now, unified_deadline));
        
        // 更新统计信息
        // Update statistics
        let check_duration = check_start.elapsed();
        self.stats.total_check_duration += check_duration;
        self.stats.last_check_time = Some(now);
        
        debug!(
            "计算统一截止时间: {:?}, 检查耗时: {:?}, 层数: {}",
            unified_deadline, check_duration, layers.len()
        );
        
        unified_deadline
    }
    
    // 轮询式统一超时检查已移除，改为事件驱动模型
    // The polling-based unified timeout check has been removed in favor of an event-driven model
    
    /// 基于历史模式预测下次超时时间
    /// Predict next timeout based on historical patterns
    fn predict_next_timeout(&self, now: Instant) -> Option<Instant> {
        // 简单的预测逻辑：查找最近的相似模式
        // Simple prediction logic: find recent similar patterns
        if self.timeout_patterns.len() < 3 {
            return None;
        }
        
        // 查找最近5分钟内的模式
        // Find patterns within the last 5 minutes
        let recent_cutoff = now - Duration::from_secs(300);
        let recent_patterns: Vec<_> = self.timeout_patterns
            .iter()
            .filter(|p| p.timestamp > recent_cutoff)
            .collect();
            
        if recent_patterns.len() < 2 {
            return None;
        }
        
        // 计算平均间隔
        // Calculate average interval
        let mut intervals = Vec::new();
        for window in recent_patterns.windows(2) {
            let interval = window[1].timestamp.saturating_duration_since(window[0].timestamp);
            intervals.push(interval);
        }
        
        if intervals.is_empty() {
            return None;
        }
        
        let avg_interval = intervals.iter().sum::<Duration>() / intervals.len() as u32;
        let last_pattern_time = recent_patterns.last()?.timestamp;
        
        // 预测下次超时时间
        // Predict next timeout time
        let predicted = last_pattern_time + avg_interval;
        
        // 只有当预测时间在合理范围内才返回
        // Only return if predicted time is within reasonable range
        if predicted > now && predicted < now + Duration::from_secs(60) {
            Some(predicted)
        } else {
            None
        }
    }
    
    /// 更新超时模式历史
    /// Update timeout pattern history
    #[allow(dead_code)]
    fn update_timeout_patterns(&mut self, now: Instant, sources: Vec<TimeoutSource>) {
        // 计算与上次模式的间隔
        // Calculate interval from last pattern
        let interval = self.timeout_patterns
            .last()
            .map(|last| now.duration_since(last.timestamp))
            .unwrap_or(Duration::ZERO);
            
        let pattern = TimeoutPattern {
            timestamp: now,
            sources,
            interval,
        };
        
        self.timeout_patterns.push(pattern);
        
        // 保持历史记录在合理大小
        // Keep history at reasonable size
        if self.timeout_patterns.len() > 100 {
            self.timeout_patterns.remove(0);
        }
    }
    
    /// 清除过期的缓存和模式
    /// Clear expired cache and patterns
    pub fn cleanup_expired_data(&mut self, now: Instant) {
        // 清除过期的截止时间缓存
        // Clear expired deadline cache
        if let Some((computed_at, _)) = self.cached_unified_deadline {
            if now.saturating_duration_since(computed_at) > Duration::from_millis(5) {
                self.cached_unified_deadline = None;
            }
        }
        
        // 清除过期的超时模式（超过1小时的）
        // Clear expired timeout patterns (older than 1 hour)
        let cutoff = now - Duration::from_secs(3600);
        self.timeout_patterns.retain(|pattern| pattern.timestamp > cutoff);
        
        // 清理截止时间映射中的过期条目
        // Clean up expired entries in deadline mapping
        let expired_keys: Vec<_> = self.next_check_deadlines
            .range(..now)
            .map(|(k, _)| *k)
            .collect();
            
        for key in expired_keys {
            self.next_check_deadlines.remove(&key);
        }
    }
    
    /// 获取性能统计信息
    /// Get performance statistics
    pub fn stats(&self) -> &SchedulerStats {
        &self.stats
    }
    
    /// 重置统计信息
    /// Reset statistics
    pub fn reset_stats(&mut self) {
        self.stats = SchedulerStats::default();
    }
    
    /// 获取缓存命中率
    /// Get cache hit rate
    pub fn cache_hit_rate(&self) -> f64 {
        if self.stats.total_checks == 0 {
            0.0
        } else {
            self.stats.cache_hits as f64 / self.stats.total_checks as f64
        }
    }
    
    /// 获取平均检查耗时
    /// Get average check duration
    pub fn avg_check_duration(&self) -> Duration {
        if self.stats.total_checks == 0 {
            Duration::ZERO
        } else {
            self.stats.total_check_duration / self.stats.total_checks as u32
        }
    }
}

impl Default for UnifiedTimeoutScheduler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    // 测试用的模拟超时层
    struct MockTimeoutLayer {
        name: &'static str,
        next_deadline: Option<Instant>,
    }
    
    impl MockTimeoutLayer {
        fn new(name: &'static str, deadline_offset: Option<Duration>) -> Self {
            Self {
                name,
                next_deadline: deadline_offset.map(|offset| Instant::now() + offset),
            }
        }
    }
    
    impl TimeoutLayer for MockTimeoutLayer {
        fn next_deadline(&self) -> Option<Instant> {
            self.next_deadline
        }
        
        fn layer_name(&self) -> &'static str {
            self.name
        }
    }
    
    #[test]
    fn test_unified_scheduler_creation() {
        let scheduler = UnifiedTimeoutScheduler::new();
        assert_eq!(scheduler.stats().total_checks, 0);
        assert_eq!(scheduler.cache_hit_rate(), 0.0);
    }
    
    #[test]
    fn test_calculate_unified_deadline() {
        let mut scheduler = UnifiedTimeoutScheduler::new();
        
        let layer1 = MockTimeoutLayer::new("test1", Some(Duration::from_millis(100)));
        let layer2 = MockTimeoutLayer::new("test2", Some(Duration::from_millis(200)));
        let layer3 = MockTimeoutLayer::new("test3", None);
        
        let layers: Vec<&dyn TimeoutLayer> = vec![&layer1, &layer2, &layer3];
        
        let deadline = scheduler.calculate_unified_deadline(&layers);
        let now = Instant::now();
        
        // 应该选择最早的截止时间
        assert!(deadline >= now + Duration::from_millis(90));
        assert!(deadline <= now + Duration::from_millis(110));
        
        assert_eq!(scheduler.stats().total_checks, 1);
    }
    
    #[test]
    fn test_cache_functionality() {
        let mut scheduler = UnifiedTimeoutScheduler::new();
        
        let layer = MockTimeoutLayer::new("test", Some(Duration::from_millis(100)));
        let layers: Vec<&dyn TimeoutLayer> = vec![&layer];
        
        // 第一次调用
        let deadline1 = scheduler.calculate_unified_deadline(&layers);
        assert_eq!(scheduler.stats().total_checks, 1);
        assert_eq!(scheduler.stats().cache_hits, 0);
        
        // 立即第二次调用，应该命中缓存
        let deadline2 = scheduler.calculate_unified_deadline(&layers);
        assert_eq!(scheduler.stats().total_checks, 2);
        assert_eq!(scheduler.stats().cache_hits, 1);
        assert_eq!(deadline1, deadline2);
        
        assert_eq!(scheduler.cache_hit_rate(), 0.5);
    }
    
    #[tokio::test]
    async fn test_unified_timeout_check() {
        // 轮询式统一超时检查已移除，此测试不再适用
        // The polling-based unified timeout check has been removed; this test is no longer applicable
        let _scheduler = UnifiedTimeoutScheduler::new();
    }
    
    #[test]
    fn test_time_cache() {
        let mut cache = TimeCache::new();
        
        let time1 = cache.now();
        let time2 = cache.now();
        
        // 在缓存有效期内，应该返回相同的时间
        assert_eq!(time1, time2);
    }
    
    #[tokio::test]
    async fn test_cleanup_expired_data() {
        let mut scheduler = UnifiedTimeoutScheduler::new();
        let now = Instant::now();
        
        // 添加一些过期的模式
        scheduler.timeout_patterns.push(TimeoutPattern {
            timestamp: now - Duration::from_secs(7200), // 2小时前
            sources: vec![TimeoutSource::Connection(TimeoutEvent::IdleTimeout)],
            interval: Duration::from_secs(1),
        });
        
        scheduler.timeout_patterns.push(TimeoutPattern {
            timestamp: now - Duration::from_secs(1800), // 30分钟前
            sources: vec![TimeoutSource::Reliability],
            interval: Duration::from_secs(2),
        });
        
        assert_eq!(scheduler.timeout_patterns.len(), 2);
        
        scheduler.cleanup_expired_data(now);
        
        // 应该只保留30分钟前的模式
        assert_eq!(scheduler.timeout_patterns.len(), 1);
    }
    
    #[test]
    fn test_prediction_with_insufficient_data() {
        let scheduler = UnifiedTimeoutScheduler::new();
        let now = Instant::now();
        
        // 没有足够的历史数据，不应该有预测
        assert!(scheduler.predict_next_timeout(now).is_none());
    }
    
    #[test]
    fn test_statistics() {
        let mut scheduler = UnifiedTimeoutScheduler::new();
        
        // 初始统计
        assert_eq!(scheduler.stats().total_checks, 0);
        assert_eq!(scheduler.cache_hit_rate(), 0.0);
        assert_eq!(scheduler.avg_check_duration(), Duration::ZERO);
        
        // 执行一些操作
        let layer = MockTimeoutLayer::new("test", Some(Duration::from_millis(100)));
        let layers: Vec<&dyn TimeoutLayer> = vec![&layer];
        
        scheduler.calculate_unified_deadline(&layers);
        scheduler.calculate_unified_deadline(&layers); // 缓存命中
        
        assert_eq!(scheduler.stats().total_checks, 2);
        assert_eq!(scheduler.stats().cache_hits, 1);
        assert_eq!(scheduler.cache_hit_rate(), 0.5);
        assert!(scheduler.avg_check_duration() > Duration::ZERO);
        
        // 重置统计
        scheduler.reset_stats();
        assert_eq!(scheduler.stats().total_checks, 0);
        assert_eq!(scheduler.cache_hit_rate(), 0.0);
    }
}