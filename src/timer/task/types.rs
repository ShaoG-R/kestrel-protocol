//! 定时器任务类型定义（纯泛型版本）
//! Timer task type definitions (Pure generic version)
//!
//! 本模块提供完全基于泛型的定时器回调系统，实现零成本抽象。
//!
//! This module provides a fully generic-based timer callback system 
//! that achieves zero-cost abstraction.

use crate::timer::{
    event::{TimerEventData, ConnectionId},
    wheel::TimerEntryId,
};
use crate::timer::event::traits::EventDataTrait;
use async_trait::async_trait;
use std::time::Duration;
use tokio::sync::mpsc;

use super::commands::{TimerTaskCommand, TimerError};

/// 定时器回调 trait，完全基于泛型
/// Timer callback trait, fully generic-based
#[async_trait]
pub trait TimerCallback<E: EventDataTrait>: Send + Sync + Clone + std::fmt::Debug + 'static {
    /// 处理定时器超时事件
    /// Handle timer timeout event
    async fn on_timeout(&self, event_data: TimerEventData<E>) -> Result<(), ()>;
}

/// 基于 mpsc::Sender 的回调实现
/// mpsc::Sender-based callback implementation
#[derive(Debug, Clone)]
pub struct SenderCallback<E: EventDataTrait> {
    sender: mpsc::Sender<TimerEventData<E>>,
}

impl<E: EventDataTrait> SenderCallback<E> {
    /// 创建新的发送者回调
    /// Create new sender callback
    pub fn new(sender: mpsc::Sender<TimerEventData<E>>) -> Self {
        Self { sender }
    }
}

#[async_trait]
impl<E: EventDataTrait> TimerCallback<E> for SenderCallback<E> {
    async fn on_timeout(&self, event_data: TimerEventData<E>) -> Result<(), ()> {
        // 使用 try_send 避免阻塞，如果发送失败则记录警告
        // Use try_send to avoid blocking, log warning if send fails
        if let Err(e) = self.sender.try_send(event_data) {
            tracing::warn!("Failed to send timer event: {:?}", e);
            return Err(());
        }
        Ok(())
    }
}

/// 基于闭包的回调实现
/// Closure-based callback implementation
#[derive(Clone)]
pub struct ClosureCallback<E: EventDataTrait, F> 
where
    F: Fn(TimerEventData<E>) -> Result<(), ()> + Send + Sync + Clone + 'static,
{
    callback: F,
    _phantom: std::marker::PhantomData<E>,
}

impl<E: EventDataTrait, F> ClosureCallback<E, F>
where
    F: Fn(TimerEventData<E>) -> Result<(), ()> + Send + Sync + Clone + 'static,
{
    /// 创建新的闭包回调
    /// Create new closure callback
    pub fn new(callback: F) -> Self {
        Self {
            callback,
            _phantom: std::marker::PhantomData,
        }
    }
}

#[async_trait]
impl<E: EventDataTrait, F> std::fmt::Debug for ClosureCallback<E, F>
where
    F: Fn(TimerEventData<E>) -> Result<(), ()> + Send + Sync + Clone + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClosureCallback")
            .field("callback", &"<closure>")
            .finish()
    }
}

#[async_trait]
impl<E: EventDataTrait, F> TimerCallback<E> for ClosureCallback<E, F>
where
    F: Fn(TimerEventData<E>) -> Result<(), ()> + Send + Sync + Clone + 'static,
{
    async fn on_timeout(&self, event_data: TimerEventData<E>) -> Result<(), ()> {
        (self.callback)(event_data)
    }
}

/// 空回调实现（用于测试或不需要处理回调的场景）
/// No-op callback implementation (for testing or scenarios where callback handling is not needed)
#[derive(Debug, Clone, Default)]
pub struct NoOpCallback<E: EventDataTrait> {
    _phantom: std::marker::PhantomData<E>,
}

impl<E: EventDataTrait> NoOpCallback<E> {
    /// 创建新的空回调
    /// Create new no-op callback
    pub fn new() -> Self {
        Self {
            _phantom: std::marker::PhantomData,
        }
    }
}

#[async_trait]
impl<E: EventDataTrait> TimerCallback<E> for NoOpCallback<E> {
    async fn on_timeout(&self, _event_data: TimerEventData<E>) -> Result<(), ()> {
        // 什么都不做
        // Do nothing
        Ok(())
    }
}

/// 定时器注册请求（完全泛型版本）
/// Timer registration request (fully generic version)
#[derive(Debug, Clone)]
pub struct TimerRegistration<E, C> 
where 
    E: EventDataTrait,
    C: TimerCallback<E>,
{
    /// 连接ID
    /// Connection ID
    pub connection_id: ConnectionId,
    /// 延迟时间
    /// Delay duration
    pub delay: Duration,
    /// 超时事件类型
    /// Timeout event type
    pub timeout_event: E,
    /// 回调处理器，用于处理超时事件
    /// Callback handler for processing timeout events
    pub callback: C,
}

impl<E, C> TimerRegistration<E, C> 
where 
    E: EventDataTrait,
    C: TimerCallback<E>,
{
    /// 创建新的定时器注册请求
    /// Create new timer registration request
    pub fn new(
        connection_id: ConnectionId,
        delay: Duration,
        timeout_event: E,
        callback: C,
    ) -> Self {
        Self {
            connection_id,
            delay,
            timeout_event,
            callback,
        }
    }
}

/// 常用的定时器注册类型别名
/// Common timer registration type aliases
pub type SenderTimerRegistration<E> = TimerRegistration<E, SenderCallback<E>>;
pub type NoOpTimerRegistration<E> = TimerRegistration<E, NoOpCallback<E>>;

/// 具体类型的便捷构造方法
/// Convenience constructors for concrete types
impl<E: EventDataTrait> SenderTimerRegistration<E> {
    /// 创建发送者类型的定时器注册
    /// Create sender-type timer registration
    pub fn with_sender(
        connection_id: ConnectionId,
        delay: Duration,
        timeout_event: E,
        sender: mpsc::Sender<TimerEventData<E>>,
    ) -> Self {
        Self::new(
            connection_id,
            delay,
            timeout_event,
            SenderCallback::new(sender),
        )
    }
}

impl<E: EventDataTrait> NoOpTimerRegistration<E> {
    /// 创建空回调类型的定时器注册
    /// Create no-op callback type timer registration
    pub fn with_no_op(
        connection_id: ConnectionId,
        delay: Duration,
        timeout_event: E,
    ) -> Self {
        Self::new(
            connection_id,
            delay,
            timeout_event,
            NoOpCallback::new(),
        )
    }
}

/// 为闭包回调创建便捷方法
/// Convenience method for closure callback
impl<E: EventDataTrait, F> TimerRegistration<E, ClosureCallback<E, F>>
where
    F: Fn(TimerEventData<E>) -> Result<(), ()> + Send + Sync + Clone + 'static,
{
    /// 使用闭包创建定时器注册
    /// Create timer registration with closure
    pub fn with_closure(
        connection_id: ConnectionId,
        delay: Duration,
        timeout_event: E,
        callback: F,
    ) -> Self {
        Self::new(
            connection_id,
            delay,
            timeout_event,
            ClosureCallback::new(callback),
        )
    }
}

/// 批量定时器注册请求（泛型版本）
/// Batch timer registration request (generic version)
#[derive(Debug, Clone)]
pub struct BatchTimerRegistration<E, C>
where
    E: EventDataTrait,
    C: TimerCallback<E>,
{
    /// 批量注册列表
    /// Batch registration list
    pub registrations: Vec<TimerRegistration<E, C>>,
}

impl<E, C> BatchTimerRegistration<E, C>
where
    E: EventDataTrait,
    C: TimerCallback<E>,
{
    /// 创建新的批量注册请求
    /// Create new batch registration request
    pub fn new(registrations: Vec<TimerRegistration<E, C>>) -> Self {
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
    pub fn add(&mut self, registration: TimerRegistration<E, C>) {
        self.registrations.push(registration);
    }
}

/// 批量定时器取消请求
/// Batch timer cancellation request
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
pub struct BatchTimerResult<T> {
    /// 成功的结果
    /// Successful results
    pub successes: Vec<T>,
    /// 失败的结果及其错误
    /// Failed results with their errors
    pub failures: Vec<(usize, TimerError)>, // (index, error)
}

impl<T> Default for BatchTimerResult<T> {
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
pub struct TimerHandle<E: EventDataTrait, C: TimerCallback<E>> {
    /// 定时器条目ID
    /// Timer entry ID
    pub entry_id: TimerEntryId,
    /// 向定时器任务发送取消请求的通道
    /// Channel for sending cancel requests to timer task
    cancel_tx: mpsc::Sender<TimerTaskCommand<E, C>>,
}

impl<E: EventDataTrait, C: TimerCallback<E>> TimerHandle<E, C> {
    /// 创建新的定时器句柄
    /// Create new timer handle
    pub fn new(entry_id: TimerEntryId, cancel_tx: mpsc::Sender<TimerTaskCommand<E, C>>) -> Self {
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