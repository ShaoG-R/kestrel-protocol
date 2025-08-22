use crate::timer::TimerEventData;
use crate::timer::event::lockfree_ring::RingBuffer;
use crate::timer::event::memory_pool::OptimizedBatchProcessor;
use crate::timer::event::traits::EventDataTrait;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicUsize, Ordering};

/// 零拷贝事件传递接口
/// Zero-copy event delivery interface
pub trait ZeroCopyEventDelivery<E: EventDataTrait> {
    /// 直接传递事件引用，避免克隆
    /// Directly deliver event reference, avoiding clones
    /// 注意：当前主要用于trait完整性，未来可能用于单事件优化
    /// Note: Currently used for trait completeness, may be used for single-event optimization in the future
    #[allow(dead_code)]
    fn deliver_event_ref(&self, event_ref: &TimerEventData<E>) -> bool;

    /// 批量传递事件引用
    /// Batch deliver event references
    fn batch_deliver_event_refs(&self, event_refs: &[&TimerEventData<E>]) -> usize;
}

/// 事件槽位（基于无锁队列）
/// Event slot (based on lock-free queue)
pub struct EventSlot<E: EventDataTrait> {
    /// 无锁环形缓冲区
    /// Lock-free ring buffer
    ring_buffer: RingBuffer<E>,
    /// 性能统计
    /// Performance statistics
    written_count: AtomicUsize,
    failed_writes: AtomicUsize,
}

impl<E: EventDataTrait> EventSlot<E> {
    /// 创建新的事件槽位
    /// Create new event slot
    pub fn new(capacity: usize) -> Self {
        Self {
            ring_buffer: RingBuffer::new(capacity),
            written_count: AtomicUsize::new(0),
            failed_writes: AtomicUsize::new(0),
        }
    }

    /// 零拷贝写入事件（移动语义）
    /// Zero-copy write event (move semantics)
    #[inline(always)]
    #[allow(dead_code)]
    pub fn write_event(&self, event: TimerEventData<E>) -> bool {
        match self.ring_buffer.try_write(event) {
            Ok(()) => {
                self.written_count.fetch_add(1, Ordering::Relaxed);
                true
            }
            Err(_) => {
                self.failed_writes.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    /// 尝试写入事件，失败时返回原始事件（用于重试）
    /// Try to write event, return original event on failure (for retry)
    #[inline(always)]
    pub fn try_write_event(&self, event: TimerEventData<E>) -> Result<(), TimerEventData<E>> {
        match self.ring_buffer.try_write(event) {
            Ok(()) => {
                self.written_count.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(event) => {
                self.failed_writes.fetch_add(1, Ordering::Relaxed);
                Err(event)
            }
        }
    }

    /// 批量写入事件（高性能版本）
    /// Batch write events (high-performance version)
    #[inline(always)]
    #[allow(dead_code)]
    pub fn batch_write_events(&self, events: Vec<TimerEventData<E>>) -> usize {
        let written = self.ring_buffer.batch_write(events);
        self.written_count.fetch_add(written, Ordering::Relaxed);
        written
    }

    /// 尝试读取事件
    /// Try to read event
    #[inline(always)]
    #[allow(dead_code)]
    pub fn try_read(&self) -> Option<TimerEventData<E>> {
        self.ring_buffer.try_read()
    }

    /// 批量读取事件
    /// Batch read events
    #[inline(always)]
    #[allow(dead_code)]
    pub fn batch_read(&self, max_events: usize) -> Vec<TimerEventData<E>> {
        self.ring_buffer.batch_read(max_events)
    }

    /// 获取性能统计
    /// Get performance statistics
    pub fn get_stats(&self) -> (usize, usize, f64) {
        let written = self.written_count.load(Ordering::Relaxed);
        let failed = self.failed_writes.load(Ordering::Relaxed);
        let utilization = self.ring_buffer.utilization();
        (written, failed, utilization)
    }
}
/// 引用传递事件处理器
/// Reference-passing event handler
pub struct RefEventHandler<E: EventDataTrait, F>
where
    F: Fn(&TimerEventData<E>) -> bool + Send + Sync,
{
    handler: F,
    processed_count: AtomicUsize,
    marker: PhantomData<E>,
}

impl<E: EventDataTrait, F> RefEventHandler<E, F>
where
    F: Fn(&TimerEventData<E>) -> bool + Send + Sync,
{
    pub fn new(handler: F) -> Self {
        Self {
            handler,
            processed_count: AtomicUsize::new(0),
            marker: PhantomData,
        }
    }
}

impl<E: EventDataTrait, F> ZeroCopyEventDelivery<E> for RefEventHandler<E, F>
where
    F: Fn(&TimerEventData<E>) -> bool + Send + Sync,
{
    fn deliver_event_ref(&self, event_ref: &TimerEventData<E>) -> bool {
        let success = (self.handler)(event_ref);
        if success {
            self.processed_count.fetch_add(1, Ordering::Relaxed);
        }
        success
    }

    fn batch_deliver_event_refs(&self, event_refs: &[&TimerEventData<E>]) -> usize {
        let mut delivered_count = 0;
        for event_ref in event_refs {
            if (self.handler)(event_ref) {
                delivered_count += 1;
            }
        }
        self.processed_count
            .fetch_add(delivered_count, Ordering::Relaxed);
        delivered_count
    }
}

/// 智能零拷贝批量分发器（集成优化批量处理器和无锁队列）
/// Smart zero-copy batch dispatcher (integrated with optimized batch processor and lock-free queues)
pub struct SmartZeroCopyDispatcher<E: EventDataTrait>
where
    E: Default + Clone,
{
    /// 事件槽位
    /// Event slots
    event_slots: Vec<EventSlot<E>>,
    /// 优化批量处理器（集成了事件工厂和内存池）
    /// Optimized batch processor (integrated with event factory and memory pool)
    batch_processor: OptimizedBatchProcessor<E>,
    /// 分发索引（智能负载均衡）
    /// Dispatch index (smart load balancing)
    dispatch_index: AtomicUsize,
    /// 批量处理阈值
    /// Batch processing threshold
    batch_threshold: usize,
    /// 性能统计
    /// Performance statistics
    total_dispatched: AtomicUsize,
    total_rejected: AtomicUsize,
}

impl<E: EventDataTrait> SmartZeroCopyDispatcher<E>
where
    E: Default + Clone,
{
    /// 创建智能零拷贝分发器
    /// Create smart zero-copy dispatcher
    pub fn new(slot_capacity: usize, dispatcher_count: usize, pool_size: usize) -> Self {
        let mut event_slots = Vec::with_capacity(dispatcher_count);
        for _ in 0..dispatcher_count {
            event_slots.push(EventSlot::new(slot_capacity));
        }

        let batch_threshold = 32; // 智能批量处理阈值
        let batch_processor = OptimizedBatchProcessor::new(pool_size, batch_threshold);

        Self {
            event_slots,
            batch_processor,
            dispatch_index: AtomicUsize::new(0),
            batch_threshold,
            total_dispatched: AtomicUsize::new(0),
            total_rejected: AtomicUsize::new(0),
        }
    }

    /// 高性能批量事件分发（智能负载均衡）
    /// High-performance batch event dispatch (smart load balancing)
    pub fn batch_dispatch_events(&self, events: Vec<TimerEventData<E>>) -> usize {
        if events.is_empty() {
            return 0;
        }

        let events_len = events.len();
        let dispatcher_count = self.event_slots.len();

        // 智能选择最佳分发器（基于利用率）
        // Smart selection of best dispatcher (based on utilization)
        let start_index = self.dispatch_index.load(Ordering::Relaxed);
        let mut best_dispatcher = start_index % dispatcher_count;
        let mut min_utilization = f64::MAX;

        // 快速检查前3个分发器，选择利用率最低的
        // Quick check of first 3 dispatchers, select the one with lowest utilization
        for i in 0..std::cmp::min(3, dispatcher_count) {
            let idx = (start_index + i) % dispatcher_count;
            let (_, _, utilization) = self.event_slots[idx].get_stats();
            if utilization < min_utilization {
                min_utilization = utilization;
                best_dispatcher = idx;
            }
        }

        // 智能分发：根据批量大小选择不同策略
        // Smart dispatch: choose different strategy based on batch size
        let dispatched = if events_len >= self.batch_threshold {
            // 大批量：优先填满最佳分发器，然后分散到其他分发器
            // Large batch: fill best dispatcher first, then distribute to others
            self.dispatch_large_batch(events, best_dispatcher, dispatcher_count)
        } else {
            // 小批量：轮询分发以确保延迟最低
            // Small batch: round-robin dispatch for minimum latency
            self.dispatch_small_batch(events, best_dispatcher, dispatcher_count)
        };

        // 更新统计信息
        // Update statistics
        self.total_dispatched
            .fetch_add(dispatched, Ordering::Relaxed);
        self.total_rejected
            .fetch_add(events_len - dispatched, Ordering::Relaxed);

        // 更新分发索引
        // Update dispatch index
        self.dispatch_index
            .store((best_dispatcher + 1) % dispatcher_count, Ordering::Relaxed);

        dispatched
    }

    /// 大批量分发策略：智能分散到多个分发器
    /// Large batch dispatch strategy: intelligently distribute to multiple dispatchers
    fn dispatch_large_batch(
        &self,
        events: Vec<TimerEventData<E>>,
        best_dispatcher: usize,
        dispatcher_count: usize,
    ) -> usize {
        let events_per_dispatcher = events.len() / dispatcher_count;
        let mut total_dispatched = 0;
        let mut event_iter = events.into_iter();

        // 首先尝试在最佳分发器上处理更多事件
        // First try to handle more events on the best dispatcher
        let best_batch_size = std::cmp::min(events_per_dispatcher * 2, event_iter.len());
        let mut best_batch = Vec::with_capacity(best_batch_size);

        for _ in 0..best_batch_size {
            if let Some(event) = event_iter.next() {
                best_batch.push(event);
            } else {
                break;
            }
        }

        if !best_batch.is_empty() {
            total_dispatched += self.event_slots[best_dispatcher].batch_write_events(best_batch);
        }

        // 将剩余事件分散到其他分发器
        // Distribute remaining events to other dispatchers
        let mut current_dispatcher = (best_dispatcher + 1) % dispatcher_count;
        let mut current_batch = Vec::with_capacity(events_per_dispatcher);

        for event in event_iter {
            current_batch.push(event);

            // 当批量达到合适大小或没有更多事件时，写入当前分发器
            // Write to current dispatcher when batch reaches appropriate size or no more events
            if current_batch.len() >= events_per_dispatcher {
                total_dispatched +=
                    self.event_slots[current_dispatcher].batch_write_events(current_batch);
                current_batch = Vec::with_capacity(events_per_dispatcher);
                current_dispatcher = (current_dispatcher + 1) % dispatcher_count;
            }
        }

        // 处理剩余的事件
        // Handle remaining events
        if !current_batch.is_empty() {
            total_dispatched +=
                self.event_slots[current_dispatcher].batch_write_events(current_batch);
        }

        total_dispatched
    }

    /// 小批量分发策略：轮询分发确保低延迟
    /// Small batch dispatch strategy: round-robin for low latency
    fn dispatch_small_batch(
        &self,
        events: Vec<TimerEventData<E>>,
        best_dispatcher: usize,
        dispatcher_count: usize,
    ) -> usize {
        let mut dispatched_count = 0;
        let mut current_dispatcher = best_dispatcher;

        for mut event in events {
            let mut placed = false;
            let original_dispatcher = current_dispatcher;

            // 尝试所有分发器（每个事件只试一轮）
            // Try all dispatchers (only one round per event)
            loop {
                match self.event_slots[current_dispatcher].try_write_event(event) {
                    Ok(()) => {
                        dispatched_count += 1;
                        placed = true;
                        current_dispatcher = (current_dispatcher + 1) % dispatcher_count;
                        break;
                    }
                    Err(returned_event) => {
                        event = returned_event;
                        current_dispatcher = (current_dispatcher + 1) % dispatcher_count;

                        // 如果回到起始分发器，说明所有分发器都满了
                        // If we're back to the starting dispatcher, all are full
                        if current_dispatcher == original_dispatcher {
                            break;
                        }
                    }
                }
            }

            // 如果无法放置事件，停止尝试后续事件
            // If unable to place event, stop trying subsequent events
            if !placed {
                break;
            }
        }

        dispatched_count
    }

    /// 智能批量创建并分发事件（使用优化批量处理器）
    /// Smart batch create and dispatch events (using optimized batch processor)
    pub fn create_and_dispatch_events(&self, requests: &[(u32, E)]) -> usize {
        if requests.is_empty() {
            return 0;
        }

        // 对于小批量，直接创建事件避免批量处理器的开销
        // For small batches, create events directly to avoid batch processor overhead
        if requests.len() <= 64 {
            let events: Vec<_> = requests
                .iter()
                .map(|(connection_id, event_data)| {
                    TimerEventData::new(*connection_id, event_data.clone())
                })
                .collect();
            return self.batch_dispatch_events(events);
        }

        // 大批量使用优化批量处理器
        // Use optimized batch processor for large batches
        let events = self.batch_processor.create_events_optimized(requests);
        self.batch_dispatch_events(events)
    }

    /// 批量消费事件（用于测试和监控）
    /// Batch consume events (for testing and monitoring)
    pub fn batch_consume_events(&self, max_events_per_slot: usize) -> Vec<TimerEventData<E>> {
        let mut all_events = Vec::new();

        for slot in &self.event_slots {
            let events = slot.batch_read(max_events_per_slot);
            all_events.extend(events);
        }

        all_events
    }

    /// 批量归还事件到内存池
    /// Batch return events to memory pool
    pub fn batch_return_to_pool(&self, events: Vec<TimerEventData<E>>) {
        self.batch_processor.process_completed(events);
    }

    /// 获取详细性能统计
    /// Get detailed performance statistics
    #[cfg(test)]
    pub fn get_detailed_stats(&self) -> DetailedPerformanceStats {
        let dispatched = self.total_dispatched.load(Ordering::Relaxed);
        let rejected = self.total_rejected.load(Ordering::Relaxed);

        // 获取批量处理器的性能统计
        // Get batch processor performance statistics
        let (_processed_events, pool_size, _, _, large_batch_ratio) =
            self.batch_processor.get_performance_stats();

        let avg_utilization = self
            .event_slots
            .iter()
            .map(|slot| {
                let (_, _, util) = slot.get_stats();
                util
            })
            .sum::<f64>()
            / self.event_slots.len() as f64;

        DetailedPerformanceStats {
            total_dispatched: dispatched,
            total_rejected: rejected,
            success_rate: if dispatched + rejected > 0 {
                dispatched as f64 / (dispatched + rejected) as f64
            } else {
                1.0
            },
            memory_pool_hit_rate: large_batch_ratio, // 大批量处理率作为命中率指标
            memory_pool_size: pool_size,
            memory_pool_reuse_rate: large_batch_ratio, // 大批量处理率作为复用率指标
            average_slot_utilization: avg_utilization,
            dispatcher_count: self.event_slots.len(),
        }
    }
}

/// 详细性能统计结构
/// Detailed performance statistics structure
#[derive(Debug, Clone)]
#[cfg(test)]
pub struct DetailedPerformanceStats {
    pub total_dispatched: usize,
    pub total_rejected: usize,
    pub success_rate: f64,
    pub memory_pool_hit_rate: f64,
    pub memory_pool_size: usize,
    pub memory_pool_reuse_rate: f64,
    pub average_slot_utilization: f64,
    pub dispatcher_count: usize,
}
#[cfg(test)]
impl DetailedPerformanceStats {
    /// 打印详细统计信息
    /// Print detailed statistics
    pub fn print_summary(&self) {
        println!("=== 智能零拷贝分发器性能统计 ===");
        println!("📊 事件分发统计:");
        println!("  - 成功分发: {}", self.total_dispatched);
        println!("  - 分发失败: {}", self.total_rejected);
        println!("  - 成功率: {:.1}%", self.success_rate * 100.0);
        println!("🔋 内存池统计:");
        println!("  - 池命中率: {:.1}%", self.memory_pool_hit_rate * 100.0);
        println!("  - 当前池大小: {}", self.memory_pool_size);
        println!("  - 复用率: {:.1}%", self.memory_pool_reuse_rate * 100.0);
        println!("⚡ 分发器统计:");
        println!(
            "  - 平均利用率: {:.1}%",
            self.average_slot_utilization * 100.0
        );
        println!("  - 分发器数量: {}", self.dispatcher_count);
        println!();
    }
}

/// 兼容性别名，保持向后兼容
/// Compatibility alias for backward compatibility
pub type ZeroCopyBatchDispatcher<E> = SmartZeroCopyDispatcher<E>;
