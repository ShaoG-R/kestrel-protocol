//! 全局定时器任务模块
//! Global timer task module

pub mod types;
pub mod commands;

#[cfg(test)]
pub mod tests;

// 重新导出主要的公共类型
pub use types::{
    TimerRegistration, BatchTimerRegistration, BatchTimerCancellation,
    BatchTimerResult, TimerHandle,
};
pub use commands::{TimerTaskCommand, TimerError, TimerTaskStats};