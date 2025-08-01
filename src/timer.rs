//! 全局定时器任务模块
//! Global Timer Task Module
//!
//! 该模块实现了基于时间轮算法的全局定时器管理系统，用于高效管理
//! 所有连接的定时器需求，包括重传超时、空闲超时、路径验证超时等。
//!
//! This module implements a global timer management system based on the
//! timing wheel algorithm, efficiently managing all connection timer needs
//! including retransmission timeouts, idle timeouts, path validation timeouts, etc.

pub mod event;
pub mod parallel;
pub mod task;
pub mod wheel;

#[cfg(test)]
mod tests;

pub use event::{TimerEvent, TimerEventData};
pub use parallel::{
    HybridParallelTimerSystem, OptimalParallelStrategy, 
    ProcessedTimerData, ParallelProcessingResult, ParallelProcessingStats
};
pub use task::{
    GlobalTimerTask, GlobalTimerTaskHandle, start_global_timer_task,
    TimerRegistration, BatchTimerRegistration, BatchTimerCancellation,
    BatchTimerResult, TimerHandle, TimerTaskCommand, TimerError, TimerTaskStats,
};
pub use wheel::{TimingWheel, TimerEntry, TimerEntryId};