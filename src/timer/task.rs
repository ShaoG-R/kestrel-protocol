//! 全局定时器任务模块
//! Global timer task module

pub mod commands;
pub mod types;

#[cfg(test)]
pub mod tests;

// 重新导出主要的公共类型
pub use commands::{TimerError, TimerTaskCommand, TimerTaskStats};
pub use types::{
    BatchTimerCancellation, BatchTimerRegistration, BatchTimerResult, TimerHandle,
    TimerRegistration,
};
