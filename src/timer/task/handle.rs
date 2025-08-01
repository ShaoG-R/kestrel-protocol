//! 全局定时器任务句柄
//! Global timer task handle
//!
//! 本模块包含全局定时器任务的客户端句柄，提供了与定时器任务通信的
//! 高级接口，以及启动定时器任务的便捷函数。
//!
//! This module contains the client handle for the global timer task, providing
//! high-level interfaces for communicating with the timer task, and convenient
//! functions for starting the timer task.

use crate::timer::event::{ConnectionId};
use crate::timer::event::traits::EventDataTrait;
use tokio::sync::{mpsc, oneshot};
use tracing::info;

use super::types::{
    TimerRegistration, BatchTimerRegistration, BatchTimerCancellation,
    BatchTimerResult, TimerHandle,
};
use super::commands::{TimerTaskCommand, TimerError, TimerTaskStats};
use super::global::GlobalTimerTask;

/// 全局定时器任务的句柄，用于启动和管理定时器任务
/// Handle for global timer task, used to start and manage timer task
#[derive(Clone)]
pub struct GlobalTimerTaskHandle<E: EventDataTrait> {
    /// 命令发送通道
    /// Command sender channel
    command_tx: mpsc::Sender<TimerTaskCommand<E>>,
}

impl<E: EventDataTrait> GlobalTimerTaskHandle<E> {
    /// 创建新的任务句柄
    /// Create new task handle
    pub fn new(command_tx: mpsc::Sender<TimerTaskCommand<E>>) -> Self {
        Self { command_tx }
    }

    /// 注册定时器
    /// Register timer
    pub async fn register_timer(
        &self,
        registration: TimerRegistration<E>,
    ) -> Result<TimerHandle<E>, TimerError> {
        let (response_tx, response_rx) = oneshot::channel();
        
        let command = TimerTaskCommand::RegisterTimer {
            registration,
            response_tx,
        };

        self.command_tx
            .send(command)
            .await
            .map_err(|_| TimerError::TaskShutdown)?;

        response_rx
            .await
            .map_err(|_| TimerError::TaskShutdown)?
    }

    /// 批量注册定时器（高性能版本）
    /// Batch register timers (high-performance version)
    pub async fn batch_register_timers(
        &self,
        batch_registration: BatchTimerRegistration<E>,
    ) -> Result<BatchTimerResult<TimerHandle<E>>, TimerError> {
        let (response_tx, response_rx) = oneshot::channel();
        
        let command = TimerTaskCommand::BatchRegisterTimers {
            batch_registration,
            response_tx,
        };

        self.command_tx
            .send(command)
            .await
            .map_err(|_| TimerError::TaskShutdown)?;

        response_rx
            .await
            .map_err(|_| TimerError::TaskShutdown)
    }

    /// 批量取消定时器（高性能版本）
    /// Batch cancel timers (high-performance version)
    pub async fn batch_cancel_timers(
        &self,
        batch_cancellation: BatchTimerCancellation,
    ) -> Result<BatchTimerResult<bool>, TimerError> {
        let (response_tx, response_rx) = oneshot::channel();
        
        let command = TimerTaskCommand::BatchCancelTimers {
            batch_cancellation,
            response_tx,
        };

        self.command_tx
            .send(command)
            .await
            .map_err(|_| TimerError::TaskShutdown)?;

        response_rx
            .await
            .map_err(|_| TimerError::TaskShutdown)
    }

    /// 清除连接的所有定时器
    /// Clear all timers for a connection
    pub async fn clear_connection_timers(&self, connection_id: ConnectionId) -> Result<usize, TimerError> {
        let (response_tx, response_rx) = oneshot::channel();
        
        let command = TimerTaskCommand::ClearConnectionTimers {
            connection_id,
            response_tx,
        };

        self.command_tx
            .send(command)
            .await
            .map_err(|_| TimerError::TaskShutdown)?;

        response_rx
            .await
            .map_err(|_| TimerError::TaskShutdown)
    }

    /// 获取统计信息
    /// Get statistics
    pub async fn get_stats(&self) -> Result<TimerTaskStats, TimerError> {
        let (response_tx, response_rx) = oneshot::channel();
        
        let command = TimerTaskCommand::GetStats { response_tx };

        self.command_tx
            .send(command)
            .await
            .map_err(|_| TimerError::TaskShutdown)?;

        response_rx
            .await
            .map_err(|_| TimerError::TaskShutdown)
    }

    /// 关闭定时器任务
    /// Shutdown timer task
    pub async fn shutdown(&self) -> Result<(), TimerError> {
        self.command_tx
            .send(TimerTaskCommand::Shutdown)
            .await
            .map_err(|_| TimerError::TaskShutdown)
    }
}

/// 启动全局定时器任务
/// Start global timer task
pub fn start_global_timer_task<E: EventDataTrait>() -> GlobalTimerTaskHandle<E> {
    let (task, command_tx) = GlobalTimerTask::new_default();
    let handle = GlobalTimerTaskHandle::new(command_tx.clone());
    
    tokio::spawn(async move {
        task.run().await;
    });
    
    info!("Global timer task started");
    handle
}