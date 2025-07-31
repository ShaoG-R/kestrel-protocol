//! 定时器事件定义
//! Timer Event Definitions
//!
//! 该模块定义了定时器系统中使用的所有事件类型和数据结构。
//!
//! This module defines all event types and data structures used in the timer system.

use std::fmt;
use std::sync::Mutex;
use tokio::sync::mpsc;
use crate::core::endpoint::timing::TimeoutEvent;

/// 定时器事件ID，用于唯一标识一个定时器
/// Timer event ID, used to uniquely identify a timer
pub type TimerEventId = u64;

/// 连接ID，用于标识定时器属于哪个连接
/// Connection ID, used to identify which connection a timer belongs to
pub type ConnectionId = u32;

/// 高性能对象池，用于复用TimerEventData对象
/// High-performance object pool for reusing TimerEventData objects
pub struct TimerEventDataPool {
    pool: Mutex<Vec<Box<TimerEventData>>>,
    max_size: usize,
    /// 批量操作缓存，避免频繁锁定
    /// Batch operation cache to avoid frequent locking
    batch_cache: Mutex<Vec<Box<TimerEventData>>>,
    batch_cache_size: usize,
}

impl TimerEventDataPool {
    /// 创建新的对象池
    /// Create new object pool
    pub fn new(max_size: usize) -> Self {
        let batch_cache_size = (max_size / 4).max(64); // 批量缓存大小为池大小的1/4，最少64个
        Self {
            pool: Mutex::new(Vec::with_capacity(max_size)),
            max_size,
            batch_cache: Mutex::new(Vec::with_capacity(batch_cache_size)),
            batch_cache_size,
        }
    }

    /// 从池中获取对象，如果池为空则创建新对象
    /// Get object from pool, create new if pool is empty
    pub fn acquire(&self, connection_id: ConnectionId, timeout_event: TimeoutEvent) -> Box<TimerEventData> {
        if let Ok(mut pool) = self.pool.lock() {
            if let Some(mut obj) = pool.pop() {
                // 重用现有对象
                // Reuse existing object
                obj.connection_id = connection_id;
                obj.timeout_event = timeout_event;
                return obj;
            }
        }
        // 池为空或锁定失败，创建新对象
        // Pool empty or lock failed, create new object
        Box::new(TimerEventData::new(connection_id, timeout_event))
    }

    /// 将对象返回池中以供重用
    /// Return object to pool for reuse
    pub fn release(&self, obj: Box<TimerEventData>) {
        if let Ok(mut pool) = self.pool.lock() {
            if pool.len() < self.max_size {
                pool.push(obj);
            }
            // 如果池已满，对象会被自动丢弃（正常GC）
            // If pool is full, object will be automatically dropped (normal GC)
        }
    }

    /// 批量获取对象（高性能版本）
    /// Batch acquire objects (high-performance version)
    pub fn batch_acquire(&self, requests: &[(ConnectionId, TimeoutEvent)]) -> Vec<Box<TimerEventData>> {
        let count = requests.len();
        if count == 0 {
            return Vec::new();
        }

        let mut result = Vec::with_capacity(count);
        
        // 尝试从批量缓存获取
        // Try to get from batch cache first
        if let Ok(mut batch_cache) = self.batch_cache.try_lock() {
            while result.len() < count && !batch_cache.is_empty() {
                if let Some(mut obj) = batch_cache.pop() {
                    let (connection_id, timeout_event) = &requests[result.len()];
                    obj.connection_id = *connection_id;
                    obj.timeout_event = timeout_event.clone();
                    result.push(obj);
                }
            }
        }
        
        // 如果批量缓存不够，从主池获取
        // If batch cache is not enough, get from main pool
        if result.len() < count {
            if let Ok(mut pool) = self.pool.lock() {
                while result.len() < count && !pool.is_empty() {
                    if let Some(mut obj) = pool.pop() {
                        let (connection_id, timeout_event) = &requests[result.len()];
                        obj.connection_id = *connection_id;
                        obj.timeout_event = timeout_event.clone();
                        result.push(obj);
                    }
                }
            }
        }
        
        // 对于剩余的请求，创建新对象
        // Create new objects for remaining requests
        while result.len() < count {
            let (connection_id, timeout_event) = &requests[result.len()];
            result.push(Box::new(TimerEventData::new(*connection_id, timeout_event.clone())));
        }
        
        result
    }

    /// 批量释放对象（高性能版本）
    /// Batch release objects (high-performance version)
    pub fn batch_release(&self, objects: Vec<Box<TimerEventData>>) {
        if objects.is_empty() {
            return;
        }

        let mut remaining = objects;
        
        // 优先放入批量缓存
        // Prioritize batch cache
        if let Ok(mut batch_cache) = self.batch_cache.try_lock() {
            while batch_cache.len() < self.batch_cache_size && !remaining.is_empty() {
                batch_cache.push(remaining.pop().unwrap());
            }
        }
        
        // 剩余的放入主池
        // Put remaining into main pool
        if !remaining.is_empty() {
            if let Ok(mut pool) = self.pool.lock() {
                for obj in remaining {
                    if pool.len() < self.max_size {
                        pool.push(obj);
                    }
                    // 如果池已满，对象会被自动丢弃（正常GC）
                    // If pool is full, objects will be automatically dropped (normal GC)
                }
            }
        }
    }

    /// 获取池中当前对象数量（用于调试）
    /// Get current number of objects in pool (for debugging)
    pub fn size(&self) -> usize {
        let pool_size = self.pool.lock().map(|pool| pool.len()).unwrap_or(0);
        let cache_size = self.batch_cache.lock().map(|cache| cache.len()).unwrap_or(0);
        pool_size + cache_size
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
    pub fn from_pool(connection_id: ConnectionId, timeout_event: TimeoutEvent) -> Box<Self> {
        TIMER_EVENT_DATA_POOL.acquire(connection_id, timeout_event)
    }

    /// 批量创建定时器事件数据（高性能版本）
    /// Batch create timer event data (high-performance version)
    pub fn batch_from_pool(requests: &[(ConnectionId, TimeoutEvent)]) -> Vec<Box<Self>> {
        TIMER_EVENT_DATA_POOL.batch_acquire(requests)
    }

    /// 将对象返回到对象池中
    /// Return object to object pool
    pub fn return_to_pool(self: Box<Self>) {
        TIMER_EVENT_DATA_POOL.release(self);
    }

    /// 批量将对象返回到对象池中
    /// Batch return objects to object pool
    pub fn batch_return_to_pool(objects: Vec<Box<Self>>) {
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
    pub data: Box<TimerEventData>,
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
            data: Box::new(data),
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
        let event_type = self.data.timeout_event.clone();
        
        // 克隆数据用于发送，原对象将被回收到池中
        // Clone data for sending, original object will be recycled to pool
        let data_for_send = TimerEventData::new(connection_id, event_type.clone());
        
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