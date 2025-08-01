//! 定时器事件定义
//! Timer Event Definitions
//!
//! 该模块定义了定时器系统中使用的所有事件类型和数据结构。
//!
//! This module defines all event types and data structures used in the timer system.

pub(super) mod zero_copy;
pub mod traits;
pub mod pool;

#[cfg(test)]
mod zero_copy_tests;

use std::fmt;
use std::fmt::Debug;
use tokio::sync::mpsc;
use crate::timer::event::traits::EventDataTrait;
use crate::timer::event::pool::TimerEventPool;

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

    /// 使用智能工厂创建定时器事件 (推荐)
    /// Create timer event using smart factory (recommended)
    pub fn from_factory(
        factory: &traits::EventFactory<E>,
        id: TimerEventId,
        connection_id: ConnectionId,
        timeout_event: E,
        callback_tx: mpsc::Sender<TimerEventData<E>>,
    ) -> Self {
        Self {
            id,
            data: factory.create_event(connection_id, timeout_event),
            callback_tx,
        }
    }


    /// 批量创建定时器事件（智能工厂版本，推荐）
    /// Batch create timer events (smart factory version, recommended)
    pub fn batch_from_factory(
        factory: &traits::EventFactory<E>,
        start_id: TimerEventId,
        requests: &[(ConnectionId, E)],
        callback_txs: &[mpsc::Sender<TimerEventData<E>>],
    ) -> Vec<Self> {
        if requests.len() != callback_txs.len() {
            panic!("Requests and callback_txs must have the same length");
        }

        // 批量获取数据对象 (智能策略选择)
        // Batch acquire data objects (smart strategy selection)
        let data_objects = factory.batch_create_events(requests);
        
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

    /// 触发定时器事件，向注册者发送超时通知（智能工厂版本，推荐）
    /// Trigger timer event, send timeout notification to registrant (smart factory version, recommended)
    pub async fn trigger_with_factory(self, factory: &traits::EventFactory<E>) {
        let timer_id = self.id;
        let connection_id = self.data.connection_id;
        let event_type = self.data.timeout_event.clone();
        
        // 使用智能工厂创建数据用于发送（Copy类型零开销，非Copy类型智能管理）
        // Use smart factory to create data for sending (zero-cost for Copy types, smart management for non-Copy types)
        let data_for_send = factory.create_event(connection_id, event_type);
        
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
                event_type = ?self.data.timeout_event,
                "Timer event triggered successfully"
            );
        }
        
        // 智能工厂会自动处理资源回收，无需手动管理
        // Smart factory automatically handles resource recycling, no manual management needed
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