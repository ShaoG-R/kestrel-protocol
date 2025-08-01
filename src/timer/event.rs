//! 定时器事件定义
//! Timer Event Definitions
//!
//! 该模块定义了定时器系统中使用的所有事件类型和数据结构。
//!
//! This module defines all event types and data structures used in the timer system.

pub(super) mod zero_copy;
pub mod traits;
pub mod pool_manager;

use std::fmt;
use std::fmt::Debug;
use tokio::sync::mpsc;
use crate::timer::event::traits::EventDataTrait;
use crate::timer::event::pool_manager::TimerEventPool;

/// 定时器事件ID，用于唯一标识一个定时器
/// Timer event ID, used to uniquely identify a timer
pub type TimerEventId = u64;

/// 连接ID，用于标识定时器属于哪个连接
/// Connection ID, used to identify which connection a timer belongs to
pub type ConnectionId = u32;




/// 定时器事件数据
/// Timer event data
pub struct TimerEventData<E: EventDataTrait> {
    /// 连接ID
    /// Connection ID
    pub connection_id: ConnectionId,
    /// 超时事件类型
    /// Timeout event type
    pub timeout_event: E,
}

impl<E: EventDataTrait> Debug for TimerEventData<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TimerEventData")
            .field("connection_id", &self.connection_id)
            .field("timeout_event", &self.timeout_event)
            .finish()
    }
}

impl<E: EventDataTrait> Clone for TimerEventData<E> {
    fn clone(&self) -> Self {
        Self {
            connection_id: self.connection_id,
            timeout_event: self.timeout_event.clone(),
        }
    }
}

impl<E: EventDataTrait> Copy for TimerEventData<E> {}

impl<E: EventDataTrait> TimerEventData<E> {
    /// 创建新的定时器事件数据
    /// Create new timer event data
    pub fn new(connection_id: ConnectionId, timeout_event: E) -> Self {
        Self {
            connection_id,
            timeout_event,
        }
    }

    /// 使用对象池高效创建定时器事件数据
    /// Efficiently create timer event data using object pool
    pub fn from_pool(pool: &TimerEventPool<E>, connection_id: ConnectionId, timeout_event: E) -> Self {
        pool.acquire(connection_id, timeout_event)
    }

    /// 批量创建定时器事件数据（高性能版本）
    /// Batch create timer event data (high-performance version)
    pub fn batch_from_pool(pool: &TimerEventPool<E>, requests: &[(ConnectionId, E)]) -> Vec<Self> {
        pool.batch_acquire(requests)
    }

    /// 将对象返回到对象池中
    /// Return object to object pool
    pub fn return_to_pool(self, pool: &TimerEventPool<E>) {
        pool.release(self);
    }

    /// 批量将对象返回到对象池中
    /// Batch return objects to object pool
    pub fn batch_return_to_pool(objects: Vec<Self>, pool: &TimerEventPool<E>) {
        pool.batch_release(objects);
    }
}

/// 定时器事件（优化版本，支持对象池）
/// Timer event (optimized version with object pool support)
#[derive(Debug)]
pub struct TimerEvent<E: EventDataTrait> {
    /// 事件ID
    /// Event ID
    pub id: TimerEventId,
    /// 事件数据（使用Box以支持对象池）
    /// Event data (using Box for object pool support)
    pub data: TimerEventData<E>,
    /// 回调通道，用于向注册者发送超时通知
    /// Callback channel, used to send timeout notifications to registrant
    pub callback_tx: mpsc::Sender<TimerEventData<E>>,
}

impl<E: EventDataTrait> TimerEvent<E> {
    /// 创建新的定时器事件（传统方式）
    /// Create new timer event (traditional way)
    pub fn new(
        id: TimerEventId,
        data: TimerEventData<E>,
        callback_tx: mpsc::Sender<TimerEventData<E>>,
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
        pool: &TimerEventPool<E>,
        id: TimerEventId,
        connection_id: ConnectionId,
        timeout_event: E,
        callback_tx: mpsc::Sender<TimerEventData<E>>,
    ) -> Self {
        Self {
            id,
            data: TimerEventData::from_pool(pool, connection_id, timeout_event),
            callback_tx,
        }
    }

    /// 批量创建定时器事件（高性能版本）
    /// Batch create timer events (high-performance version)
    pub fn batch_from_pool(
        pool: &TimerEventPool<E>,
        start_id: TimerEventId,
        requests: &[(ConnectionId, E)],
        callback_txs: &[mpsc::Sender<TimerEventData<E>>],
    ) -> Vec<Self> {
        if requests.len() != callback_txs.len() {
            panic!("Requests and callback_txs must have the same length");
        }

        // 批量获取数据对象
        // Batch acquire data objects
        let data_objects = TimerEventData::batch_from_pool(pool, requests);
        
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
    pub async fn trigger(self, pool: &TimerEventPool<E>) {
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
        self.data.return_to_pool(pool);
    }
}

impl<E: EventDataTrait> fmt::Display for TimerEvent<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "TimerEvent(id: {}, conn: {}, event: {:?})",
            self.id, self.data.connection_id, self.data.timeout_event
        )
    }
}