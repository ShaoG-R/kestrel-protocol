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
}

impl TimerEventDataPool {
    /// 创建新的对象池
    /// Create new object pool
    pub fn new(max_size: usize) -> Self {
        Self {
            pool: Mutex::new(Vec::with_capacity(max_size)),
            max_size,
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

    /// 获取池中当前对象数量（用于调试）
    /// Get current number of objects in pool (for debugging)
    pub fn size(&self) -> usize {
        self.pool.lock().map(|pool| pool.len()).unwrap_or(0)
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

    /// 将对象返回到对象池中
    /// Return object to object pool
    pub fn return_to_pool(self: Box<Self>) {
        TIMER_EVENT_DATA_POOL.release(self);
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