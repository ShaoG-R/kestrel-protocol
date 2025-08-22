//! 基于 pool.rs 设计的高性能内存池实现
//! High-performance memory pool implementation based on pool.rs design
//!
//! 直接使用 pool.rs 中的优秀设计，简化实现并提升性能。

// 重新导出 pool.rs 中的优秀设计
pub use super::pool::{PoolConfig, PoolStats, TimerEventPool};

use crate::timer::TimerEventData;
use crate::timer::event::traits::EventDataTrait;
use std::sync::atomic::{AtomicUsize, Ordering};

/// 优化的批量处理器（直接使用 pool.rs 的优秀设计）
/// Optimized batch processor (directly using excellent design from pool.rs)
pub struct OptimizedBatchProcessor<E: EventDataTrait> {
    /// 使用 pool.rs 中的事件池
    /// Using event pool from pool.rs
    event_pool: TimerEventPool<E>,
    /// 批量处理阈值
    /// Batch processing threshold
    batch_threshold: usize,
    /// 处理统计
    /// Processing statistics
    processed_count: AtomicUsize,
    small_batch_count: AtomicUsize,
    large_batch_count: AtomicUsize,
}

impl<E: EventDataTrait> OptimizedBatchProcessor<E>
where
    E: Default + Clone,
{
    /// 创建优化批量处理器
    /// Create optimized batch processor
    pub fn new(pool_size: usize, batch_threshold: usize) -> Self {
        let config = PoolConfig {
            max_size: pool_size,
            initial_size: pool_size / 4,
            enable_batch_optimization: true,
        };

        let event_pool = TimerEventPool::new(config);

        Self {
            event_pool,
            batch_threshold,
            processed_count: AtomicUsize::new(0),
            small_batch_count: AtomicUsize::new(0),
            large_batch_count: AtomicUsize::new(0),
        }
    }

    /// 高性能批量事件创建
    /// High-performance batch event creation
    pub fn create_events_optimized(&self, requests: &[(u32, E)]) -> Vec<TimerEventData<E>> {
        let batch_size = requests.len();
        self.processed_count
            .fetch_add(batch_size, Ordering::Relaxed);

        if batch_size >= self.batch_threshold {
            // 大批量：使用 SIMD 优化
            self.large_batch_count.fetch_add(1, Ordering::Relaxed);
            self.event_pool.batch_acquire(requests)
        } else {
            // 小批量：使用传统方式
            self.small_batch_count.fetch_add(1, Ordering::Relaxed);
            requests
                .iter()
                .map(|(conn_id, event)| self.event_pool.acquire(*conn_id, event.clone()))
                .collect()
        }
    }

    /// 批量处理完成，归还对象到池中
    /// Batch processing completed, return objects to pool
    pub fn process_completed(&self, events: Vec<TimerEventData<E>>) {
        self.event_pool.batch_release(events);
    }

    /// 获取性能统计
    /// Get performance statistics
    pub fn get_performance_stats(&self) -> (usize, usize, usize, f64, f64) {
        let processed = self.processed_count.load(Ordering::Relaxed);
        let small_batches = self.small_batch_count.load(Ordering::Relaxed);
        let large_batches = self.large_batch_count.load(Ordering::Relaxed);
        let pool_stats = self.event_pool.stats();

        let large_batch_ratio = if small_batches + large_batches > 0 {
            large_batches as f64 / (small_batches + large_batches) as f64
        } else {
            0.0
        };

        (
            processed,
            pool_stats.current_size,
            0,
            0.0,
            large_batch_ratio,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::endpoint::timing::TimeoutEvent;

    #[test]
    fn test_optimized_batch_processor() {
        let processor = OptimizedBatchProcessor::<TimeoutEvent>::new(100, 32);

        // 小批量处理
        let small_requests = vec![
            (1, TimeoutEvent::IdleTimeout),
            (2, TimeoutEvent::PathValidationTimeout),
        ];
        let small_events = processor.create_events_optimized(&small_requests);
        assert_eq!(small_events.len(), 2);

        // 大批量处理
        let large_requests: Vec<_> = (0..50).map(|i| (i, TimeoutEvent::IdleTimeout)).collect();
        let large_events = processor.create_events_optimized(&large_requests);
        assert_eq!(large_events.len(), 50);

        // 处理完成
        processor.process_completed(small_events);
        processor.process_completed(large_events);

        let (processed, _, _, _, large_batch_ratio) = processor.get_performance_stats();
        assert_eq!(processed, 52);
        assert!(large_batch_ratio > 0.0);
    }
}
