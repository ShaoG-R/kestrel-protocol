//! 全局定时器任务模块
//! Global timer task module

pub mod types;
pub mod commands;
pub mod global;
pub mod handle;

#[cfg(test)]
pub mod tests;

// 重新导出主要的公共类型
pub use types::{
    TimerRegistration, BatchTimerRegistration, BatchTimerCancellation,
    BatchTimerResult, TimerHandle,
};
pub use commands::{TimerTaskCommand, TimerError, TimerTaskStats};
pub use global::GlobalTimerTask;
pub use handle::{GlobalTimerTaskHandle, start_global_timer_task};