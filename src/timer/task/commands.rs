//! 定时器任务命令定义（完全泛型版本）
//! Timer task command definitions (Fully generic version)
//!
//! 本模块提供完全基于泛型的定时器任务命令系统，支持零成本抽象。
//!
//! This module provides a fully generic-based timer task command system
//! that supports zero-cost abstractions.

use crate::timer::{
    event::ConnectionId,
    wheel::TimerEntryId,
};
use crate::timer::event::traits::EventDataTrait;
use tokio::sync::oneshot;

use super::types::{
    TimerRegistration, BatchTimerRegistration, BatchTimerCancellation,
    BatchTimerResult, TimerHandle, TimerCallback,
};

/// 定时器任务命令（泛型版本）
/// Timer task commands (generic version)
#[derive(Debug)]
pub enum TimerTaskCommand<E, C> 
where
    E: EventDataTrait,
    C: TimerCallback<E>,
{
    /// 注册定时器
    /// Register timer
    RegisterTimer {
        registration: TimerRegistration<E, C>,
        response_tx: oneshot::Sender<Result<TimerHandle<E, C>, TimerError>>,
    },
    /// 批量注册定时器
    /// Batch register timers
    BatchRegisterTimers {
        batch_registration: BatchTimerRegistration<E, C>,
        response_tx: oneshot::Sender<BatchTimerResult<TimerHandle<E, C>>>,
    },
    /// 取消定时器
    /// Cancel timer
    CancelTimer {
        entry_id: TimerEntryId,
        response_tx: oneshot::Sender<bool>,
    },
    /// 批量取消定时器
    /// Batch cancel timers
    BatchCancelTimers {
        batch_cancellation: BatchTimerCancellation,
        response_tx: oneshot::Sender<BatchTimerResult<bool>>,
    },
    /// 清除连接的所有定时器
    /// Clear all timers for a connection
    ClearConnectionTimers {
        connection_id: ConnectionId,
        response_tx: oneshot::Sender<usize>,
    },
    /// 获取统计信息
    /// Get statistics
    GetStats {
        response_tx: oneshot::Sender<TimerTaskStats>,
    },
    /// 关闭定时器任务
    /// Shutdown timer task
    Shutdown,
}

/// 定时器错误类型
/// Timer error types
#[derive(Debug, Clone, thiserror::Error)]
pub enum TimerError {
    #[error("Timer task has been shutdown")]
    TaskShutdown,
    #[error("Channel error: {0}")]
    ChannelError(String),
    #[error("Timer not found")]
    TimerNotFound,
}

/// 定时器任务统计信息
/// Timer task statistics
#[derive(Debug, Clone)]
pub struct TimerTaskStats {
    /// 总定时器数
    /// Total number of timers
    pub total_timers: usize,
    /// 活跃连接数
    /// Number of active connections
    pub active_connections: usize,
    /// 已处理的定时器数
    /// Number of processed timers
    pub processed_timers: u64,
    /// 已取消的定时器数
    /// Number of cancelled timers
    pub cancelled_timers: u64,
    /// 时间轮统计信息
    /// Timing wheel statistics
    pub wheel_stats: crate::timer::wheel::TimingWheelStats,
}

impl std::fmt::Display for TimerTaskStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "TimerTaskStats {{ timers: {}, connections: {}, processed: {}, cancelled: {}, wheel: {} }}",
            self.total_timers,
            self.active_connections,
            self.processed_timers,
            self.cancelled_timers,
            self.wheel_stats
        )
    }
}

/// 常用的命令类型别名
/// Common command type aliases
pub type SenderTimerTaskCommand<E> = TimerTaskCommand<E, super::types::SenderCallback<E>>;
pub type NoOpTimerTaskCommand<E> = TimerTaskCommand<E, super::types::NoOpCallback<E>>;

/// 闭包回调的命令类型（需要在使用时指定具体的闭包类型）
/// Closure callback command type (specific closure type needs to be specified when used)
pub type ClosureTimerTaskCommand<E, F> = TimerTaskCommand<E, super::types::ClosureCallback<E, F>>;