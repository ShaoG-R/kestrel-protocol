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
    /// 读取索引
    /// Read index
    read_index: AtomicUsize,
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
            read_index: AtomicUsize::new(0),
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

    /// 零拷贝读取事件（移动语义）
    /// Zero-copy read event (move semantics)
    pub fn read_event(&self) -> Option<TimerEventData<E>> {
        let read_idx = self.read_index.load(Ordering::Acquire);
        let write_idx = self.write_index.load(Ordering::Acquire);

        if read_idx == write_idx {
            return None; // 队列为空
        }

        let slot_idx = read_idx & self.slot_mask;
        let slot = &self.slots[slot_idx];
        
        // 原子地取出槽位中的值，并用 None 替换它
        // Atomically take the value from the slot, replacing it with None
        let maybe_event_arc = slot.swap(Arc::new(None));

        // 因为我们已经“消耗”了这个槽位，所以推进读取索引
        // Advance the read index as we have "consumed" this slot
        self.read_index.store(read_idx.wrapping_add(1), Ordering::Release);

        // 尝试从 Arc 中解包事件数据
        // Try to unwrap the event data from the Arc
        // 在SPSC模型中，try_unwrap 应该总是成功的
        // In an SPSC model, try_unwrap should always succeed
        Arc::try_unwrap(maybe_event_arc).ok().flatten()
    }

    /// 批量读取事件（零拷贝）
    /// Batch read events (zero-copy)
    pub fn batch_read_events(&self, max_count: usize) -> Vec<TimerEventData<E>> {
        let mut events = Vec::with_capacity(max_count);

        for _ in 0..max_count {
            if let Some(event) = self.read_event() {
                events.push(event);
            } else {
                break; // 队列已空
            }
        }

        events
    }

    /// 获取未读事件数量
    /// Get count of unread events
    pub fn pending_count(&self) -> usize {
        let write_idx = self.write_index.load(Ordering::Acquire);
        let read_idx = self.read_index.load(Ordering::Acquire);
        // 返回写入索引和读取索引之间的差值
        // Return the difference between the write and read indices
        write_idx.wrapping_sub(read_idx)
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
    
    /// 零拷贝单事件分发
    /// Zero-copy single event dispatch
    pub fn dispatch_event(&self, event: TimerEventData<E>) -> bool {
        let dispatcher_idx = self.round_robin_index.fetch_add(1, Ordering::Relaxed) 
            % self.event_slots.len();
        self.event_slots[dispatcher_idx].write_event(event)
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
    
    /// 为特定分发器读取事件
    /// Read events for specific dispatcher
    pub fn read_events_for_dispatcher(&self, dispatcher_idx: usize, max_count: usize) -> Vec<TimerEventData<E>> {
        if dispatcher_idx < self.event_slots.len() {
            self.event_slots[dispatcher_idx].batch_read_events(max_count)
        } else {
            Vec::new()
        }
    }
    
    /// 获取总待处理事件数
    /// Get total pending event count
    pub fn total_pending_count(&self) -> usize {
        self.event_slots.iter()
            .map(|slot| slot.pending_count())
            .sum()
    }
}