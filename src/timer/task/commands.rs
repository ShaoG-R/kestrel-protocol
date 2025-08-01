//! 定时器任务命令定义
//! Timer task command definitions
//!
//! 本模块包含定时器任务系统的命令枚举、错误类型和统计信息，
//! 定义了客户端与定时器任务之间的通信协议。
//!
//! This module contains command enums, error types, and statistics for the timer task system,
//! defining the communication protocol between clients and the timer task.

use crate::timer::{
    event::ConnectionId,
    wheel::TimerEntryId,
};
use crate::timer::event::traits::EventDataTrait;
use tokio::sync::oneshot;

use super::types::{
    TimerRegistration, BatchTimerRegistration, BatchTimerCancellation,
    BatchTimerResult, TimerHandle,
};

/// 定时器任务命令
/// Timer task commands
#[derive(Debug)]
pub enum TimerTaskCommand<E: EventDataTrait> {
    /// 注册定时器
    /// Register timer
    RegisterTimer {
        registration: TimerRegistration<E>,
        response_tx: oneshot::Sender<Result<TimerHandle<E>, TimerError>>,
    },
    /// 批量注册定时器
    /// Batch register timers
    BatchRegisterTimers {
        batch_registration: BatchTimerRegistration<E>,
        response_tx: oneshot::Sender<BatchTimerResult<TimerHandle<E>>>,
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