//! 定时器事件定义
//! Timer Event Definitions
//!
//! 该模块定义了定时器系统中使用的所有事件类型和数据结构。
//!
//! This module defines all event types and data structures used in the timer system.

use std::fmt;
use tokio::sync::mpsc;
use crate::core::endpoint::timing::TimeoutEvent;

/// 定时器事件ID，用于唯一标识一个定时器
/// Timer event ID, used to uniquely identify a timer
pub type TimerEventId = u64;

/// 连接ID，用于标识定时器属于哪个连接
/// Connection ID, used to identify which connection a timer belongs to
pub type ConnectionId = u32;

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
}

/// 定时器事件
/// Timer event
#[derive(Debug)]
pub struct TimerEvent {
    /// 事件ID
    /// Event ID
    pub id: TimerEventId,
    /// 事件数据
    /// Event data
    pub data: TimerEventData,
    /// 回调通道，用于向注册者发送超时通知
    /// Callback channel, used to send timeout notifications to registrant
    pub callback_tx: mpsc::Sender<TimerEventData>,
}

impl TimerEvent {
    /// 创建新的定时器事件
    /// Create new timer event
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

    /// 触发定时器事件，向注册者发送超时通知
    /// Trigger timer event, send timeout notification to registrant
    pub async fn trigger(self) {
        let timer_id = self.id;
        let connection_id = self.data.connection_id;
        let event_type = self.data.timeout_event.clone();
        
        if let Err(err) = self.callback_tx.send(self.data).await {
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