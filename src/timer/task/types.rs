//! 定时器任务类型定义
//! Timer task type definitions
//!
//! 本模块包含定时器任务系统中使用的各种数据结构和类型定义，
//! 包括注册请求、批量操作结果、定时器句柄等。
//!
//! This module contains various data structures and type definitions used in
//! the timer task system, including registration requests, batch operation results,
//! timer handles, etc.

use crate::timer::{
    event::{TimerEventData, ConnectionId},
    wheel::TimerEntryId,
};
use crate::timer::event::traits::EventDataTrait;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;

use super::commands::{TimerTaskCommand, TimerError};

/// 批量处理缓冲区，用于减少内存分配
/// Batch processing buffers for reducing memory allocation
#[derive(Debug)]
pub(super) struct BatchProcessingBuffers {
    /// 连接到过期定时器ID的映射缓冲区
    /// Buffer for connection to expired timer IDs mapping
    pub(super) expired_by_connection: HashMap<u32, Vec<u64>>,
}

impl BatchProcessingBuffers {
    /// 创建新的批量处理缓冲区
    /// Create new batch processing buffers
    pub(super) fn new() -> Self {
        Self {
            expired_by_connection: HashMap::with_capacity(64),
        }
    }

    /// 清空所有缓冲区以供重用
    /// Clear all buffers for reuse
    pub(super) fn clear(&mut self) {
        self.expired_by_connection.clear();
    }
}

/// 定时器注册请求
/// Timer registration request
#[derive(Debug, Clone)]
pub struct TimerRegistration<E: EventDataTrait> {
    /// 连接ID
    /// Connection ID
    pub connection_id: ConnectionId,
    /// 延迟时间
    /// Delay duration
    pub delay: Duration,
    /// 超时事件类型
    /// Timeout event type
    pub timeout_event: E,
    /// 回调通道，用于接收超时通知
    /// Callback channel for receiving timeout notifications
    pub callback_tx: mpsc::Sender<TimerEventData<E>>,
}

impl<E: EventDataTrait> TimerRegistration<E> {
    /// 创建新的定时器注册请求
    /// Create new timer registration request
    pub fn new(
        connection_id: ConnectionId,
        delay: Duration,
        timeout_event: E,
        callback_tx: mpsc::Sender<TimerEventData<E>>,
    ) -> Self {
        Self {
            connection_id,
            delay,
            timeout_event,
            callback_tx,
        }
    }
}

/// 批量定时器注册请求
/// Batch timer registration request
#[derive(Debug, Clone)]
pub struct BatchTimerRegistration<E: EventDataTrait> {
    /// 批量注册列表
    /// Batch registration list
    pub registrations: Vec<TimerRegistration<E>>,
}

impl<E: EventDataTrait> BatchTimerRegistration<E> {
    /// 创建新的批量注册请求
    /// Create new batch registration request
    pub fn new(registrations: Vec<TimerRegistration<E>>) -> Self {
        Self { registrations }
    }

    /// 创建预分配容量的批量注册请求
    /// Create batch registration request with pre-allocated capacity
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            registrations: Vec::with_capacity(capacity),
        }
    }

    /// 添加注册请求
    /// Add registration request
    pub fn add(&mut self, registration: TimerRegistration<E>) {
        self.registrations.push(registration);
    }
}

/// 批量定时器取消请求
/// Batch timer cancellation request
#[derive(Debug)]
pub struct BatchTimerCancellation {
    /// 要取消的定时器条目ID列表
    /// List of timer entry IDs to cancel
    pub entry_ids: Vec<TimerEntryId>,
}

impl BatchTimerCancellation {
    /// 创建新的批量取消请求
    /// Create new batch cancellation request
    pub fn new(entry_ids: Vec<TimerEntryId>) -> Self {
        Self { entry_ids }
    }

    /// 创建预分配容量的批量取消请求
    /// Create batch cancellation request with pre-allocated capacity
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entry_ids: Vec::with_capacity(capacity),
        }
    }

    /// 添加要取消的定时器ID
    /// Add timer ID to cancel
    pub fn add(&mut self, entry_id: TimerEntryId) {
        self.entry_ids.push(entry_id);
    }
}

/// 批量操作结果
/// Batch operation result
#[derive(Debug)]
pub struct BatchTimerResult<T> {
    /// 成功的结果
    /// Successful results
    pub successes: Vec<T>,
    /// 失败的结果及其错误
    /// Failed results with their errors
    pub failures: Vec<(usize, TimerError)>, // (index, error)
}

impl<E: EventDataTrait> Default for BatchTimerResult<TimerHandle<E>> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> BatchTimerResult<T> {
    /// 创建新的批量结果
    /// Create new batch result
    pub fn new() -> Self {
        Self {
            successes: Vec::new(),
            failures: Vec::new(),
        }
    }

    /// 创建预分配容量的批量结果
    /// Create batch result with pre-allocated capacity
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            successes: Vec::with_capacity(capacity),
            failures: Vec::new(),
        }
    }

    /// 获取成功数量
    /// Get success count
    pub fn success_count(&self) -> usize {
        self.successes.len()
    }

    /// 获取失败数量
    /// Get failure count
    pub fn failure_count(&self) -> usize {
        self.failures.len()
    }

    /// 是否全部成功
    /// Check if all operations succeeded
    pub fn all_succeeded(&self) -> bool {
        self.failures.is_empty()
    }
}

/// 定时器句柄，用于取消定时器
/// Timer handle for canceling timers
#[derive(Debug, Clone)]
pub struct TimerHandle<E: EventDataTrait> {
    /// 定时器条目ID
    /// Timer entry ID
    pub entry_id: TimerEntryId,
    /// 向定时器任务发送取消请求的通道
    /// Channel for sending cancel requests to timer task
    cancel_tx: mpsc::Sender<TimerTaskCommand<E>>,
}

impl<E: EventDataTrait> TimerHandle<E> {
    /// 创建新的定时器句柄
    /// Create new timer handle
    pub fn new(entry_id: TimerEntryId, cancel_tx: mpsc::Sender<TimerTaskCommand<E>>) -> Self {
        Self {
            entry_id,
            cancel_tx,
        }
    }

    /// 取消定时器
    /// Cancel timer
    pub async fn cancel(&self) -> Result<bool, TimerError> {
        use tokio::sync::oneshot;
        
        let (response_tx, response_rx) = oneshot::channel();
        
        let command = TimerTaskCommand::CancelTimer {
            entry_id: self.entry_id,
            response_tx,
        };

        self.cancel_tx
            .send(command)
            .await
            .map_err(|_| TimerError::TaskShutdown)?;

        response_rx
            .await
            .map_err(|_| TimerError::TaskShutdown)
    }
}