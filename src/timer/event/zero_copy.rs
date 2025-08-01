use std::marker::PhantomData;
use std::sync::atomic::{AtomicUsize, Ordering};
use arc_swap::ArcSwap;
use std::sync::Arc;
use crate::timer::event::traits::EventDataTrait;
use crate::timer::TimerEventData;

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

/// 高性能无锁事件槽位系统
/// High-performance lock-free event slot system
pub struct FastEventSlot<E: EventDataTrait> {
    /// 事件槽位，使用 ArcSwap 实现无锁读写
    /// Event slots using ArcSwap for lock-free read/write
    slots: Vec<ArcSwap<Option<TimerEventData<E>>>>,
    /// 写入索引
    /// Write index
    write_index: AtomicUsize,
    /// 槽位掩码（必须是2的幂-1）
    /// Slot mask (must be power of 2 - 1)
    slot_mask: usize,
}

impl<E: EventDataTrait> FastEventSlot<E> {
    /// 创建新的快速事件槽位
    /// Create new fast event slot
    pub fn new(slot_count: usize) -> Self {
        // 确保slot_count是2的幂
        // Ensure slot_count is a power of two
        let slot_count = slot_count.next_power_of_two();
        let slot_mask = slot_count - 1;

        let mut slots = Vec::with_capacity(slot_count);
        for _ in 0..slot_count {
            // 初始化每个槽位为 None
            // Initialize each slot with None
            slots.push(ArcSwap::new(Arc::new(None)));
        }

        Self {
            slots,
            write_index: AtomicUsize::new(0),
            slot_mask,
        }
    }

    /// 零拷贝写入事件（移动语义）
    /// Zero-copy write event (move semantics)
    pub fn write_event(&self, event: TimerEventData<E>) -> bool {
        let write_idx = self.write_index.fetch_add(1, Ordering::AcqRel);
        let slot_idx = write_idx & self.slot_mask;

        let slot = &self.slots[slot_idx];
        
        // 加载当前槽位的值
        // Load the current value of the slot
        let guard = slot.load();
        
        // 只有当槽位为空时才写入，这可以防止覆盖未读的事件
        // Only write if the slot is empty, which prevents overwriting unread events
        if guard.is_none() {
            let new_arc = Arc::new(Some(event));
            // 尝试原子地将 None 替换为 Some(event)
            // Atomically try to swap None with Some(event)
            if Arc::ptr_eq(&slot.compare_and_swap(&guard, new_arc), &guard) {
                return true; // 写入成功
            }
        }
        
        // 如果槽位不为空，或是在我们写入时被其它线程修改，则写入失败
        // The write fails if the slot was not empty or was modified by another thread
        false
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
        self.processed_count.fetch_add(delivered_count, Ordering::Relaxed);
        delivered_count
    }
}

/// 零拷贝事件批量分发器
/// Zero-copy batch event dispatcher  
pub struct ZeroCopyBatchDispatcher<E: EventDataTrait> {
    /// 快速事件槽位
    /// Fast event slots
    event_slots: Vec<FastEventSlot<E>>,
    /// 轮询索引
    /// Round-robin index
    round_robin_index: AtomicUsize,
    /// 批量处理阈值
    /// Batch processing threshold
    _batch_threshold: usize,
}

impl<E: EventDataTrait> ZeroCopyBatchDispatcher<E> {
    pub fn new(slot_count: usize, dispatcher_count: usize) -> Self {
        let mut event_slots = Vec::with_capacity(dispatcher_count);
        for _ in 0..dispatcher_count {
            event_slots.push(FastEventSlot::new(slot_count));
        }
        
        Self {
            event_slots,
            round_robin_index: AtomicUsize::new(0),
            _batch_threshold: 32, // 默认批量处理阈值
        }
    }
    
    /// 零拷贝批量事件分发
    /// Zero-copy batch event dispatch
    pub fn batch_dispatch_events(&self, events: Vec<TimerEventData<E>>) -> usize {
        let mut dispatched_count = 0;
        
        for (i, event) in events.into_iter().enumerate() {
            let dispatcher_idx = (self.round_robin_index.load(Ordering::Relaxed) + i) 
                % self.event_slots.len();
            if self.event_slots[dispatcher_idx].write_event(event) {
                dispatched_count += 1;
            }
        }
        
        self.round_robin_index.fetch_add(dispatched_count, Ordering::Relaxed);
        dispatched_count
    }
}