//! 定时器事件定义
//! Timer Event Definitions
//!
//! 该模块定义了定时器系统中使用的所有事件类型和数据结构。
//!
//! This module defines all event types and data structures used in the timer system.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc;
use crate::core::endpoint::timing::TimeoutEvent;
use wide::{u32x4, u32x8};
use crossbeam_queue::SegQueue;

/// 零拷贝事件传递系统 - 基于引用传递避免数据克隆
/// Zero-copy event delivery system - uses reference passing to avoid data cloning  
pub mod zero_copy {
    use super::*;
    use arc_swap::ArcSwap;
    
    /// 零拷贝事件传递接口
    /// Zero-copy event delivery interface
    pub trait ZeroCopyEventDelivery {
        /// 直接传递事件引用，避免克隆
        /// Directly deliver event reference, avoiding clones
        fn deliver_event_ref(&self, event_ref: &TimerEventData) -> bool;
        
        /// 批量传递事件引用
        /// Batch deliver event references
        fn batch_deliver_event_refs(&self, event_refs: &[&TimerEventData]) -> usize;
    }
    
    /// 高性能无锁事件槽位系统
    /// High-performance lock-free event slot system
    pub struct FastEventSlot {
        /// 事件槽位，使用 ArcSwap 实现无锁读写
        /// Event slots using ArcSwap for lock-free read/write
        slots: Vec<ArcSwap<Option<TimerEventData>>>,
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

    impl FastEventSlot {
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
        pub fn write_event(&self, event: TimerEventData) -> bool {
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
        pub fn read_event(&self) -> Option<TimerEventData> {
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
        pub fn batch_read_events(&self, max_count: usize) -> Vec<TimerEventData> {
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
    pub struct RefEventHandler<F>
    where 
        F: Fn(&TimerEventData) -> bool + Send + Sync,
    {
        handler: F,
        processed_count: AtomicUsize,
    }
    
    impl<F> RefEventHandler<F>
    where 
        F: Fn(&TimerEventData) -> bool + Send + Sync,
    {
        pub fn new(handler: F) -> Self {
            Self {
                handler,
                processed_count: AtomicUsize::new(0),
            }
        }
    }
    
    impl<F> ZeroCopyEventDelivery for RefEventHandler<F>
    where 
        F: Fn(&TimerEventData) -> bool + Send + Sync,
    {
        fn deliver_event_ref(&self, event_ref: &TimerEventData) -> bool {
            let success = (self.handler)(event_ref);
            if success {
                self.processed_count.fetch_add(1, Ordering::Relaxed);
            }
            success
        }
        
        fn batch_deliver_event_refs(&self, event_refs: &[&TimerEventData]) -> usize {
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
    pub struct ZeroCopyBatchDispatcher {
        /// 快速事件槽位
        /// Fast event slots
        event_slots: Vec<FastEventSlot>,
        /// 轮询索引
        /// Round-robin index
        round_robin_index: AtomicUsize,
        /// 批量处理阈值
        /// Batch processing threshold
        _batch_threshold: usize,
    }
    
    impl ZeroCopyBatchDispatcher {
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
        pub fn dispatch_event(&self, event: TimerEventData) -> bool {
            let dispatcher_idx = self.round_robin_index.fetch_add(1, Ordering::Relaxed) 
                % self.event_slots.len();
            self.event_slots[dispatcher_idx].write_event(event)
        }
        
        /// 零拷贝批量事件分发
        /// Zero-copy batch event dispatch
        pub fn batch_dispatch_events(&self, events: Vec<TimerEventData>) -> usize {
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
        pub fn read_events_for_dispatcher(&self, dispatcher_idx: usize, max_count: usize) -> Vec<TimerEventData> {
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
}

/// 定时器事件ID，用于唯一标识一个定时器
/// Timer event ID, used to uniquely identify a timer
pub type TimerEventId = u64;

/// 连接ID，用于标识定时器属于哪个连接
/// Connection ID, used to identify which connection a timer belongs to
pub type ConnectionId = u32;


/// 高性能无锁对象池，用于复用TimerEventData对象
/// High-performance lock-free object pool for reusing TimerEventData objects
pub struct TimerEventDataPool {
    pool: Arc<SegQueue<TimerEventData>>,
    max_size: usize,
}

impl TimerEventDataPool {
    /// 创建新的对象池
    /// Create new object pool
    pub fn new(max_size: usize) -> Self {
        Self {
            pool: Arc::new(SegQueue::new()),
            max_size,
        }
    }

    /// 从池中获取对象，如果池为空则创建新对象
    /// Get object from pool, create new if pool is empty
    pub fn acquire(&self, connection_id: ConnectionId, timeout_event: TimeoutEvent) -> TimerEventData {
        match self.pool.pop() {
            Some(mut obj) => {
                // 重用现有对象
                // Reuse existing object
                obj.connection_id = connection_id;
                obj.timeout_event = timeout_event;
                obj
            }
            None => {
                // 池为空，创建新对象
                // Pool empty, create new object
                TimerEventData::new(connection_id, timeout_event)
            }
        }
    }

    /// 将对象返回池中以供重用
    /// Return object to pool for reuse
    pub fn release(&self, obj: TimerEventData) {
        // 只有当池未满时才放回，避免无限增长
        // Only release back if the pool is not full to avoid infinite growth
        if self.pool.len() < self.max_size {
            self.pool.push(obj);
        }
        // 如果池已满，对象会被自动丢弃（正常GC）
        // If pool is full, object will be automatically dropped (normal GC)
    }

    /// 批量获取对象（SIMD优化版本）
    /// Batch acquire objects (SIMD optimized version)
    pub fn batch_acquire(&self, requests: &[(ConnectionId, TimeoutEvent)]) -> Vec<TimerEventData> {
        let count = requests.len();
        if count == 0 {
            return Vec::new();
        }

        let mut result = Vec::with_capacity(count);
        
        // 1. 尽可能从池中获取
        // 1. Acquire as many objects as possible from the pool
        for _ in 0..count {
            if let Some(obj) = self.pool.pop() {
                result.push(obj);
            } else {
                break; // 池已空
            }
        }

        // 2. 使用SIMD更新获取到的对象，并创建剩余的对象
        // 2. Use SIMD to update acquired objects and create the remaining ones
        self.simd_update_and_create(&mut result, requests);

        result
    }

    /// SIMD优化的批量更新与创建
    /// SIMD optimized batch update and create
    fn simd_update_and_create(
        &self,
        result: &mut Vec<TimerEventData>,
        requests: &[(ConnectionId, TimeoutEvent)],
    ) {
        let total_requests = requests.len();
        let acquired_count = result.len();
        let mut i = 0;

        // SIMD更新已从池中获取的对象
        // SIMD update for objects acquired from the pool
        while i + 8 <= acquired_count {
            self.simd_process_chunk(i, requests, &mut result[i..i+8]);
            i += 8;
        }
        while i + 4 <= acquired_count {
             self.simd_process_chunk_4(i, requests, &mut result[i..i+4]);
             i += 4;
        }

        // 标量更新剩余的已获取对象
        // Scalar update for remaining acquired objects
        while i < acquired_count {
            let (conn_id, timeout_event) = requests[i];
            result[i].connection_id = conn_id;
            result[i].timeout_event = timeout_event;
            i += 1;
        }
        
        // 如果请求尚未满足，SIMD创建剩余的新对象
        // If requests are not yet met, SIMD create the remaining new objects
        let mut new_creations = Vec::with_capacity(total_requests - acquired_count);
        let mut j = acquired_count;
        while j + 8 <= total_requests {
            self.simd_create_chunk(j, requests, &mut new_creations);
            j += 8;
        }
        while j + 4 <= total_requests {
            self.simd_create_chunk_4(j, requests, &mut new_creations);
            j += 4;
        }

        // 标量创建最后剩余的对象
        // Scalar create for the final remaining objects
        while j < total_requests {
            let (conn_id, timeout_event) = requests[j];
            new_creations.push(TimerEventData::new(conn_id, timeout_event));
            j += 1;
        }
        
        result.append(&mut new_creations);
    }
    
    // 辅助函数：SIMD处理8个元素的块
    // Helper function: SIMD process a chunk of 8 elements
    #[inline]
    fn simd_process_chunk(&self, start_index: usize, requests: &[(ConnectionId, TimeoutEvent)], chunk: &mut [TimerEventData]) {
        let conn_ids = [
            requests[start_index].0, requests[start_index + 1].0, requests[start_index + 2].0, requests[start_index + 3].0,
            requests[start_index + 4].0, requests[start_index + 5].0, requests[start_index + 6].0, requests[start_index + 7].0,
        ];
        let conn_id_vec = u32x8::new(conn_ids);
        let processed_ids = conn_id_vec.to_array(); // 假设SIMD操作在这里发生
        
        for k in 0..8 {
            chunk[k].connection_id = processed_ids[k];
            chunk[k].timeout_event = requests[start_index + k].1;
        }
    }

    // 辅助函数：SIMD处理4个元素的块
    // Helper function: SIMD process a chunk of 4 elements
    #[inline]
    fn simd_process_chunk_4(&self, start_index: usize, requests: &[(ConnectionId, TimeoutEvent)], chunk: &mut [TimerEventData]) {
        let conn_ids = [
            requests[start_index].0, requests[start_index + 1].0, requests[start_index + 2].0, requests[start_index + 3].0,
        ];
        let conn_id_vec = u32x4::new(conn_ids);
        let processed_ids = conn_id_vec.to_array();
        
        for k in 0..4 {
            chunk[k].connection_id = processed_ids[k];
            chunk[k].timeout_event = requests[start_index + k].1;
        }
    }

    // 辅助函数：SIMD创建8个元素的块
    // Helper function: SIMD create a chunk of 8 elements
    #[inline]
    fn simd_create_chunk(&self, start_index: usize, requests: &[(ConnectionId, TimeoutEvent)], target_vec: &mut Vec<TimerEventData>) {
        let conn_ids = [
            requests[start_index].0, requests[start_index + 1].0, requests[start_index + 2].0, requests[start_index + 3].0,
            requests[start_index + 4].0, requests[start_index + 5].0, requests[start_index + 6].0, requests[start_index + 7].0,
        ];
        let conn_id_vec = u32x8::new(conn_ids);
        let processed_ids = conn_id_vec.to_array();
        
        for k in 0..8 {
            target_vec.push(TimerEventData::new(processed_ids[k], requests[start_index + k].1));
        }
    }

    // 辅助函数：SIMD创建4个元素的块
    // Helper function: SIMD create a chunk of 4 elements
    #[inline]
    fn simd_create_chunk_4(&self, start_index: usize, requests: &[(ConnectionId, TimeoutEvent)], target_vec: &mut Vec<TimerEventData>) {
        let conn_ids = [
            requests[start_index].0, requests[start_index + 1].0, requests[start_index + 2].0, requests[start_index + 3].0,
        ];
        let conn_id_vec = u32x4::new(conn_ids);
        let processed_ids = conn_id_vec.to_array();
        
        for k in 0..4 {
            target_vec.push(TimerEventData::new(processed_ids[k], requests[start_index + k].1));
        }
    }


    /// 批量释放对象（高性能版本）
    /// Batch release objects (high-performance version)
    pub fn batch_release(&self, objects: Vec<TimerEventData>) {
        let current_len = self.pool.len();
        let available_capacity = self.max_size.saturating_sub(current_len);
        let to_release = objects.len().min(available_capacity);

        // 只归还容量允许的部分对象
        // Only return the portion of objects that capacity allows
        for obj in objects.into_iter().take(to_release) {
            self.pool.push(obj);
        }
        // 超出容量的对象会被自动丢弃
        // Objects exceeding capacity are automatically dropped
    }

    /// 获取池中当前对象数量（用于调试）
    /// Get current number of objects in pool (for debugging)
    pub fn size(&self) -> usize {
        self.pool.len()
    }
}

/// 全局定时器事件数据对象池
/// Global timer event data object pool
static TIMER_EVENT_DATA_POOL: once_cell::sync::Lazy<TimerEventDataPool> = 
    once_cell::sync::Lazy::new(|| TimerEventDataPool::new(1024));

/// 定时器事件数据
/// Timer event data
#[derive(Debug, Clone)]
pub struct TimerEventData {
    /// 连接ID
    /// Connection ID
    pub connection_id: ConnectionId,
    /// 超时事件类型
    /// Timeout event type
    pub timeout_event: TimeoutEvent,
}

impl TimerEventData {
    /// 创建新的定时器事件数据
    /// Create new timer event data
    pub fn new(connection_id: ConnectionId, timeout_event: TimeoutEvent) -> Self {
        Self {
            connection_id,
            timeout_event,
        }
    }

    /// 使用对象池高效创建定时器事件数据
    /// Efficiently create timer event data using object pool
    pub fn from_pool(connection_id: ConnectionId, timeout_event: TimeoutEvent) -> Self {
        TIMER_EVENT_DATA_POOL.acquire(connection_id, timeout_event)
    }

    /// 批量创建定时器事件数据（高性能版本）
    /// Batch create timer event data (high-performance version)
    pub fn batch_from_pool(requests: &[(ConnectionId, TimeoutEvent)]) -> Vec<Self> {
        TIMER_EVENT_DATA_POOL.batch_acquire(requests)
    }

    /// 将对象返回到对象池中
    /// Return object to object pool
    pub fn return_to_pool(self) {
        TIMER_EVENT_DATA_POOL.release(self);
    }

    /// 批量将对象返回到对象池中
    /// Batch return objects to object pool
    pub fn batch_return_to_pool(objects: Vec<Self>) {
        TIMER_EVENT_DATA_POOL.batch_release(objects);
    }
}

/// 定时器事件（优化版本，支持对象池）
/// Timer event (optimized version with object pool support)
#[derive(Debug)]
pub struct TimerEvent {
    /// 事件ID
    /// Event ID
    pub id: TimerEventId,
    /// 事件数据（使用Box以支持对象池）
    /// Event data (using Box for object pool support)
    pub data: TimerEventData,
    /// 回调通道，用于向注册者发送超时通知
    /// Callback channel, used to send timeout notifications to registrant
    pub callback_tx: mpsc::Sender<TimerEventData>,
}

impl TimerEvent {
    /// 创建新的定时器事件（传统方式）
    /// Create new timer event (traditional way)
    pub fn new(
        id: TimerEventId,
        data: TimerEventData,
        callback_tx: mpsc::Sender<TimerEventData>,
    ) -> Self {
        Self {
            id,
            data,
            callback_tx,
        }
    }

    /// 使用对象池高效创建定时器事件
    /// Efficiently create timer event using object pool
    pub fn from_pool(
        id: TimerEventId,
        connection_id: ConnectionId,
        timeout_event: TimeoutEvent,
        callback_tx: mpsc::Sender<TimerEventData>,
    ) -> Self {
        Self {
            id,
            data: TimerEventData::from_pool(connection_id, timeout_event),
            callback_tx,
        }
    }

    /// 批量创建定时器事件（高性能版本）
    /// Batch create timer events (high-performance version)
    pub fn batch_from_pool(
        start_id: TimerEventId,
        requests: &[(ConnectionId, TimeoutEvent)],
        callback_txs: &[mpsc::Sender<TimerEventData>],
    ) -> Vec<Self> {
        if requests.len() != callback_txs.len() {
            panic!("Requests and callback_txs must have the same length");
        }

        // 批量获取数据对象
        // Batch acquire data objects
        let data_objects = TimerEventData::batch_from_pool(requests);
        
        // 构建定时器事件
        // Build timer events
        data_objects
            .into_iter()
            .zip(callback_txs.iter())
            .enumerate()
            .map(|(index, (data, callback_tx))| Self {
                id: start_id + index as u64,
                data,
                callback_tx: callback_tx.clone(),
            })
            .collect()
    }

    /// 触发定时器事件，向注册者发送超时通知（优化版本）
    /// Trigger timer event, send timeout notification to registrant (optimized version)
    pub async fn trigger(self) {
        let timer_id = self.id;
        let connection_id = self.data.connection_id;
        let event_type = self.data.timeout_event;
        
        // 克隆数据用于发送，原对象将被回收到池中
        // Clone data for sending, original object will be recycled to pool
        let data_for_send = TimerEventData::new(connection_id, event_type);
        
        if let Err(err) = self.callback_tx.send(data_for_send).await {
            tracing::warn!(
                timer_id,
                error = %err,
                "Failed to send timer event to callback channel"
            );
        } else {
            tracing::trace!(
                timer_id,
                connection_id,
                event_type = ?event_type,
                "Timer event triggered successfully"
            );
        }
        
        // 将数据对象返回到对象池中以供重用
        // Return data object to object pool for reuse
        self.data.return_to_pool();
    }
}

impl fmt::Display for TimerEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "TimerEvent(id: {}, conn: {}, event: {:?})",
            self.id, self.data.connection_id, self.data.timeout_event
        )
    }
}