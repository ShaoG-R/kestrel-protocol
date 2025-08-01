//! 基于 crossbeam-queue 的安全高性能环形缓冲区
//! Safe high-performance ring buffer based on crossbeam-queue
//!
//! 使用成熟的 crossbeam-queue 库实现真正的无锁操作，无需任何 unsafe 代码。
//! Uses mature crossbeam-queue library for true lock-free operations without any unsafe code.

use std::sync::atomic::{AtomicUsize, Ordering};
use crossbeam_queue::ArrayQueue;
use crate::timer::event::traits::EventDataTrait;
use crate::timer::TimerEventData;

/// 安全的高性能环形缓冲区（基于 crossbeam）
/// Safe high-performance ring buffer (based on crossbeam)
///
/// 完全避免 unsafe 代码，使用 crossbeam-queue 的 SPSC 优化
/// Completely avoids unsafe code, uses crossbeam-queue's SPSC optimizations
pub struct RingBuffer<E: EventDataTrait> {
    /// 无锁数组队列
    /// Lock-free array queue
    queue: ArrayQueue<TimerEventData<E>>,
    /// 容量
    /// Capacity
    capacity: usize,
    /// 写入计数器（用于统计）
    /// Write counter (for statistics)
    write_count: AtomicUsize,
    /// 读取计数器（用于统计）
    /// Read counter (for statistics)
    read_count: AtomicUsize,
}

/// 兼容性别名，保持向后兼容
/// Compatibility aliases for backward compatibility
pub type TrueLockFreeRingBuffer<E> = RingBuffer<E>;
pub type SafeHighPerformanceRingBuffer<E> = RingBuffer<E>;
pub type HighPerformanceRingBuffer<E> = RingBuffer<E>;

impl<E: EventDataTrait> RingBuffer<E> {
    /// 创建新的安全高性能环形缓冲区
    /// Create new safe high-performance ring buffer
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1); // 确保容量至少为1
        
        Self {
            queue: ArrayQueue::new(capacity),
            capacity,
            write_count: AtomicUsize::new(0),
            read_count: AtomicUsize::new(0),
        }
    }
    
    /// 尝试写入事件（零拷贝移动语义）- 完全线程安全
    /// Try to write event (zero-copy move semantics) - fully thread-safe
    #[inline]
    pub fn try_write(&self, event: TimerEventData<E>) -> Result<(), TimerEventData<E>> {
        match self.queue.push(event) {
            Ok(()) => {
                self.write_count.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(event) => Err(event), // 缓冲区满，返回事件
        }
    }
    
    /// 尝试读取事件 - 完全线程安全
    /// Try to read event - fully thread-safe
    #[inline]
    pub fn try_read(&self) -> Option<TimerEventData<E>> {
        match self.queue.pop() {
            Some(event) => {
                self.read_count.fetch_add(1, Ordering::Relaxed);
                Some(event)
            }
            None => None,
        }
    }
    
    /// 批量写入事件（高性能版本）
    /// Batch write events (high-performance version)
    pub fn batch_write(&self, events: Vec<TimerEventData<E>>) -> usize {
        if events.is_empty() {
            return 0;
        }
        
        let mut written = 0;
        for event in events {
            match self.queue.push(event) {
                Ok(()) => written += 1,
                Err(_) => break, // 缓冲区满，停止写入
            }
        }
        
        self.write_count.fetch_add(written, Ordering::Relaxed);
        written
    }
    
    /// 批量读取事件
    /// Batch read events
    pub fn batch_read(&self, max_events: usize) -> Vec<TimerEventData<E>> {
        let mut events = Vec::with_capacity(max_events);
        
        for _ in 0..max_events {
            match self.queue.pop() {
                Some(event) => events.push(event),
                None => break,
            }
        }
        
        self.read_count.fetch_add(events.len(), Ordering::Relaxed);
        events
    }
    
    /// 获取缓冲区使用率 - 线程安全的查询操作
    /// Get buffer utilization - thread-safe query operation
    pub fn utilization(&self) -> f64 {
        let queue_len = self.queue.len();
        queue_len as f64 / self.capacity as f64
    }
    
    /// 是否为空
    /// Is empty
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
    
    /// 是否已满
    /// Is full
    #[inline]
    pub fn is_full(&self) -> bool {
        self.queue.is_full()
    }
    
    /// 获取当前队列长度
    /// Get current queue length
    #[inline]
    pub fn len(&self) -> usize {
        self.queue.len()
    }
    
    /// 获取容量
    /// Get capacity
    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

/// 优化的零拷贝批量事件分发器（使用高性能环形缓冲区）
/// Optimized zero-copy batch event dispatcher (using high-performance ring buffer)
pub struct OptimizedZeroCopyDispatcher<E: EventDataTrait> {
    /// 环形缓冲区数组
    /// Ring buffer array
    ring_buffers: Vec<RingBuffer<E>>,
    /// 分发索引（轮询）
    /// Dispatch index (round-robin)
    dispatch_index: AtomicUsize,
    /// 性能统计
    /// Performance statistics
    total_dispatched: AtomicUsize,
    total_rejected: AtomicUsize,
}

impl<E: EventDataTrait> OptimizedZeroCopyDispatcher<E> {
    /// 创建优化的分发器
    /// Create optimized dispatcher
    pub fn new(buffer_capacity: usize, dispatcher_count: usize) -> Self {
        let mut ring_buffers = Vec::with_capacity(dispatcher_count);
        for _ in 0..dispatcher_count {
            ring_buffers.push(RingBuffer::<E>::new(buffer_capacity));
        }
        
        Self {
            ring_buffers,
            dispatch_index: AtomicUsize::new(0),
            total_dispatched: AtomicUsize::new(0),
            total_rejected: AtomicUsize::new(0),
        }
    }
    
    /// 高性能批量事件分发
    /// High-performance batch event dispatch
    pub fn batch_dispatch_events(&self, events: Vec<TimerEventData<E>>) -> usize {
        if events.is_empty() {
            return 0;
        }
        
        let events_len = events.len();
        let dispatcher_count = self.ring_buffers.len();
        
        // 智能负载均衡：优先使用利用率低的缓冲区
        // Smart load balancing: prioritize buffers with low utilization
        let start_index = self.dispatch_index.load(Ordering::Relaxed);
        let mut best_dispatcher = start_index % dispatcher_count;
        let mut min_utilization = self.ring_buffers[best_dispatcher].utilization();
        
        // 快速选择最佳分发器（最多检查3个）
        // Quick selection of best dispatcher (check at most 3)
        for i in 1..=std::cmp::min(3, dispatcher_count) {
            let idx = (start_index + i) % dispatcher_count;
            let utilization = self.ring_buffers[idx].utilization();
            if utilization < min_utilization {
                min_utilization = utilization;
                best_dispatcher = idx;
            }
        }
        
        // 批量写入到选定的缓冲区
        // Batch write to selected buffer
        let dispatched = self.ring_buffers[best_dispatcher].batch_write(events);
        
        // 更新统计信息
        // Update statistics
        self.total_dispatched.fetch_add(dispatched, Ordering::Relaxed);
        self.total_rejected.fetch_add(events_len - dispatched, Ordering::Relaxed);
        
        // 更新分发索引
        // Update dispatch index
        self.dispatch_index.store(
            (best_dispatcher + 1) % dispatcher_count, 
            Ordering::Relaxed
        );
        
        dispatched
    }
    
    /// 批量消费事件（用于测试和监控）
    /// Batch consume events (for testing and monitoring)
    pub fn batch_consume_events(&self, max_events_per_buffer: usize) -> Vec<TimerEventData<E>> {
        let mut all_events = Vec::new();
        
        for ring_buffer in &self.ring_buffers {
            let events = ring_buffer.batch_read(max_events_per_buffer);
            all_events.extend(events);
        }
        
        all_events
    }
    
    /// 获取性能统计
    /// Get performance statistics
    pub fn get_stats(&self) -> (usize, usize, f64) {
        let dispatched = self.total_dispatched.load(Ordering::Relaxed);
        let rejected = self.total_rejected.load(Ordering::Relaxed);
        let avg_utilization = self.ring_buffers.iter()
            .map(|buf| buf.utilization())
            .sum::<f64>() / self.ring_buffers.len() as f64;
        
        (dispatched, rejected, avg_utilization)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::endpoint::timing::TimeoutEvent;
    
    fn create_test_event(id: u32) -> TimerEventData<TimeoutEvent> {
        TimerEventData::new(id, TimeoutEvent::IdleTimeout)
    }
    
    #[test]
    fn test_ring_buffer_basic_operations() {
        let buffer = RingBuffer::<TimeoutEvent>::new(16);
        
        // 测试写入
        let event = create_test_event(1);
        assert!(buffer.try_write(event).is_ok());
        
        // 测试读取
        let read_event = buffer.try_read().unwrap();
        assert_eq!(read_event.connection_id, 1);
        
        // 测试空缓冲区
        assert!(buffer.try_read().is_none());
    }
    
    #[test]
    fn test_ring_buffer_batch_operations() {
        let buffer = RingBuffer::<TimeoutEvent>::new(64);
        
        // 批量写入
        let events: Vec<_> = (0..32).map(create_test_event).collect();
        let written = buffer.batch_write(events);
        assert_eq!(written, 32);
        
        // 批量读取
        let read_events = buffer.batch_read(20);
        assert_eq!(read_events.len(), 20);
        
        let remaining_events = buffer.batch_read(20);
        assert_eq!(remaining_events.len(), 12);
    }
    
    #[test]
    fn test_optimized_dispatcher() {
        let dispatcher = OptimizedZeroCopyDispatcher::<TimeoutEvent>::new(128, 4);
        
        // 批量分发
        let events: Vec<_> = (0..100).map(create_test_event).collect();
        let dispatched = dispatcher.batch_dispatch_events(events);
        assert_eq!(dispatched, 100);
        
        // 检查统计
        let (total_dispatched, total_rejected, avg_util) = dispatcher.get_stats();
        assert_eq!(total_dispatched, 100);
        assert_eq!(total_rejected, 0);
        assert!(avg_util > 0.0);
    }
    
    #[test]
    fn test_ring_buffer_capacity_behavior() {
        let buffer = RingBuffer::<TimeoutEvent>::new(8);
        
        // 先测试ringbuf的实际容量行为
        let mut written_count = 0;
        for i in 0..20 {
            match buffer.try_write(create_test_event(i)) {
                Ok(()) => {
                    written_count += 1;
                    println!("成功写入事件 {}, 总计: {}", i, written_count);
                }
                Err(_) => {
                    println!("写入事件 {} 失败，缓冲区已满，已写入 {} 个事件", i, written_count);
                    break;
                }
            }
        }
        
        // ringbuf的SPSC缓冲区通常容量-1是可用空间
        // 对于容量8，应该能写入7个元素
        assert!(written_count >= 7, "至少应该能写入7个元素，实际写入: {}", written_count);
        assert!(written_count <= 8, "最多应该能写入8个元素，实际写入: {}", written_count);
        
        // 读取一个事件后应该能再写入一个
        let read_event = buffer.try_read();
        assert!(read_event.is_some(), "应该能读取到事件");
        
        // 现在应该能再写入一个
        let write_result = buffer.try_write(create_test_event(100));
        assert!(write_result.is_ok(), "读取后应该能写入新事件");
        
        println!("缓冲区容量测试通过：实际可写入 {} 个事件", written_count);
    }
}