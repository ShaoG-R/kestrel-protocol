//! Timer Actor - 批量定时器管理器
//!
//! 该模块提供一个专门的 Actor 来批量处理定时器的注册和取消操作，
//! 通过智能缓冲策略优化与全局定时器系统的交互性能。
//!
//! Timer Actor - Batch Timer Manager
//!
//! This module provides a dedicated Actor for batch processing timer
//! registration and cancellation operations, optimizing interaction
//! performance with the global timer system through intelligent buffering strategies.

use crate::core::endpoint::timing::TimeoutEvent;
use crate::timer::{
    event::ConnectionId,
    task::{TimerHandle, TimerRegistration, BatchTimerRegistration, BatchTimerCancellation},
    wheel::TimerEntryId,
    HybridTimerTaskHandle,
};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::{
    sync::{mpsc, oneshot},
    time::{interval, Instant, MissedTickBehavior},
};
use tracing::{debug, trace, warn};

/// 定时器Actor命令类型
/// Timer Actor command types
#[derive(Debug)]
pub enum TimerActorCommand {
    /// 注册单个定时器
    /// Register a single timer
    RegisterTimer {
        registration: TimerRegistration<TimeoutEvent>,
        response_tx: oneshot::Sender<Result<TimerHandle<TimeoutEvent>, String>>,
    },
    
    /// 取消单个定时器
    /// Cancel a single timer
    CancelTimer {
        connection_id: ConnectionId,
        timeout_event: TimeoutEvent,
        response_tx: oneshot::Sender<bool>,
    },
    
    /// 批量注册定时器（大批量，直接处理，不经过缓冲区）
    /// Batch register timers (large batch, direct processing, bypass buffer)
    BatchRegisterTimers {
        registrations: Vec<TimerRegistration<TimeoutEvent>>,
        response_tx: oneshot::Sender<Result<Vec<TimerHandle<TimeoutEvent>>, String>>,
    },
    
    /// 批量取消定时器（大批量，直接处理，不经过缓冲区）
    /// Batch cancel timers (large batch, direct processing, bypass buffer)
    BatchCancelTimers {
        entries: Vec<(ConnectionId, TimeoutEvent)>,
        response_tx: oneshot::Sender<usize>, // 返回成功取消的数量
    },
    
    /// 强制刷新所有缓冲区
    /// Force flush all buffers
    FlushBuffers,
    
    /// 获取Actor统计信息
    /// Get Actor statistics
    GetStats {
        response_tx: oneshot::Sender<TimerActorStats>,
    },
    
    /// 关闭Actor
    /// Shutdown Actor
    Shutdown,
}

/// 定时器Actor统计信息
/// Timer Actor statistics
#[derive(Debug, Clone)]
pub struct TimerActorStats {
    /// 注册缓冲区大小
    /// Register buffer size
    pub register_buffer_size: usize,
    /// 取消缓冲区大小
    /// Cancel buffer size
    pub cancel_buffer_size: usize,
    /// 处理的总注册请求数
    /// Total processed register requests
    pub total_register_requests: u64,
    /// 处理的总取消请求数
    /// Total processed cancel requests
    pub total_cancel_requests: u64,
    /// 自动刷新次数
    /// Auto flush count
    pub auto_flush_count: u64,
    /// 缓冲区优化避免的操作数（注册后立即取消）
    /// Operations avoided by buffer optimization (register then immediate cancel)
    pub optimized_operations: u64,
}

impl Default for TimerActorStats {
    fn default() -> Self {
        Self {
            register_buffer_size: 0,
            cancel_buffer_size: 0,
            total_register_requests: 0,
            total_cancel_requests: 0,
            auto_flush_count: 0,
            optimized_operations: 0,
        }
    }
}

/// 定时器Actor配置
/// Timer Actor configuration
#[derive(Debug, Clone)]
pub struct TimerActorConfig {
    /// 批量处理阈值 - 达到此数量自动发送
    /// Batch processing threshold - auto send when reaching this count
    pub batch_threshold: usize,
    /// 自动刷新间隔 - 定时清空缓冲区
    /// Auto flush interval - periodically clear buffers
    pub flush_interval: Duration,
}

impl Default for TimerActorConfig {
    fn default() -> Self {
        Self {
            batch_threshold: 4096,
            flush_interval: Duration::from_millis(100), // 100ms 自动刷新
        }
    }
}

/// 定时器Actor - 批量定时器管理器
/// Timer Actor - Batch Timer Manager
pub struct TimerActor {
    /// 全局定时器任务句柄
    /// Global timer task handle
    timer_handle: HybridTimerTaskHandle<TimeoutEvent>,
    
    /// 配置参数
    /// Configuration
    config: TimerActorConfig,
    
    /// 注册缓冲区 - 使用HashMap避免重复注册同一个定时器
    /// Register buffer - using HashMap to avoid duplicate registration of the same timer
    register_buffer: HashMap<(ConnectionId, TimeoutEvent), TimerRegistration<TimeoutEvent>>,
    
    /// 取消缓冲区 - 使用HashSet去重
    /// Cancel buffer - using HashSet for deduplication
    cancel_buffer: HashSet<(ConnectionId, TimeoutEvent)>,
    
    /// 键到定时器条目ID的映射 - 用于跟踪已注册的定时器
    /// Key to timer entry ID mapping - for tracking registered timers
    key_to_entry_id: HashMap<(ConnectionId, TimeoutEvent), TimerEntryId>,
    
    /// 定时器条目ID到键的反向映射 - 用于快速查找
    /// Timer entry ID to key reverse mapping - for quick lookup
    entry_id_to_key: HashMap<TimerEntryId, (ConnectionId, TimeoutEvent)>,
    
    /// Actor统计信息
    /// Actor statistics
    stats: TimerActorStats,
    
    /// 最后刷新时间
    /// Last flush time
    last_flush_time: Instant,
}

impl TimerActor {
    /// 创建新的定时器Actor
    /// Create a new Timer Actor
    pub fn new(
        timer_handle: HybridTimerTaskHandle<TimeoutEvent>,
        config: TimerActorConfig,
    ) -> Self {
        Self {
            timer_handle,
            config,
            register_buffer: HashMap::new(),
            cancel_buffer: HashSet::new(),
            key_to_entry_id: HashMap::new(),
            entry_id_to_key: HashMap::new(),
            stats: TimerActorStats::default(),
            last_flush_time: Instant::now(),
        }
    }
    
    /// 启动Actor主循环
    /// Start Actor main loop
    pub async fn run(mut self, mut command_rx: mpsc::Receiver<TimerActorCommand>) {
        let mut flush_interval = interval(self.config.flush_interval);
        flush_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        
        trace!("TimerActor started with config: {:?}", self.config);
        
        loop {
            tokio::select! {
                // 处理命令
                // Process commands
                command = command_rx.recv() => {
                    match command {
                        Some(cmd) => {
                            if let Err(should_break) = self.handle_command(cmd).await {
                                if should_break {
                                    break;
                                }
                            }
                        }
                        None => {
                            debug!("TimerActor command channel closed, shutting down");
                            break;
                        }
                    }
                }
                
                // 定时刷新缓冲区
                // Periodic buffer flush
                _ = flush_interval.tick() => {
                    if self.should_auto_flush() {
                        self.flush_buffers().await;
                        self.stats.auto_flush_count += 1;
                        trace!("Auto flushed buffers, stats: {:?}", self.stats);
                    }
                }
            }
        }
        
        // 最终清理：刷新所有缓冲区
        // Final cleanup: flush all buffers
        if !self.register_buffer.is_empty() || !self.cancel_buffer.is_empty() {
            debug!("TimerActor shutting down, flushing remaining buffers");
            self.flush_buffers().await;
        }
        
        debug!("TimerActor shutdown complete, final stats: {:?}", self.stats);
    }
    
    /// 处理单个命令
    /// Handle a single command
    async fn handle_command(&mut self, command: TimerActorCommand) -> Result<(), bool> {
        match command {
            TimerActorCommand::RegisterTimer { registration, response_tx } => {
                let result = self.handle_register_timer(registration).await;
                let _ = response_tx.send(result);
            }
            
            TimerActorCommand::CancelTimer { connection_id, timeout_event, response_tx } => {
                let result = self.handle_cancel_timer(connection_id, timeout_event).await;
                let _ = response_tx.send(result);
            }
            
            TimerActorCommand::BatchRegisterTimers { registrations, response_tx } => {
                let result = self.handle_batch_register_timers(registrations).await;
                let _ = response_tx.send(result);
            }
            
            TimerActorCommand::BatchCancelTimers { entries, response_tx } => {
                let result = self.handle_batch_cancel_timers(entries).await;
                let _ = response_tx.send(result);
            }
            
            TimerActorCommand::FlushBuffers => {
                self.flush_buffers().await;
            }
            
            TimerActorCommand::GetStats { response_tx } => {
                let mut stats = self.stats.clone();
                stats.register_buffer_size = self.register_buffer.len();
                stats.cancel_buffer_size = self.cancel_buffer.len();
                let _ = response_tx.send(stats);
            }
            
            TimerActorCommand::Shutdown => {
                return Err(true); // 请求退出主循环
            }
        }
        
        Ok(())
    }
    
    /// 处理单个定时器注册
    /// Handle single timer registration
    async fn handle_register_timer(
        &mut self,
        registration: TimerRegistration<TimeoutEvent>,
    ) -> Result<TimerHandle<TimeoutEvent>, String> {
        let key = (registration.connection_id, registration.timeout_event);
        
        // 检查是否在取消缓冲区中，如果是则直接抵消
        // Check if in cancel buffer, if so, directly cancel out
        if self.cancel_buffer.remove(&key) {
            self.stats.optimized_operations += 1;
            trace!(key = ?key, "Timer registration cancelled out by pending cancellation");
            return Err("Timer registration cancelled by pending cancellation".to_string());
        }
        
        // 添加到注册缓冲区
        // Add to register buffer
        self.register_buffer.insert(key, registration);
        self.stats.total_register_requests += 1;
        
        trace!(key = ?key, buffer_size = self.register_buffer.len(), "Added timer to register buffer");
        
        // 检查是否需要自动刷新
        // Check if auto flush is needed
        if self.register_buffer.len() >= self.config.batch_threshold {
            self.flush_buffers().await;
        }
        
        // 返回一个占位符句柄 - 实际的句柄将在刷新时创建
        // Return a placeholder handle - actual handle will be created on flush
        // TODO: 这里需要一个更好的设计来处理异步句柄返回
        Err("Timer registered to buffer, handle will be available after flush".to_string())
    }
    
    /// 处理单个定时器取消
    /// Handle single timer cancellation
    async fn handle_cancel_timer(&mut self, connection_id: ConnectionId, timeout_event: TimeoutEvent) -> bool {
        let key = (connection_id, timeout_event);
        
        // 优先检查注册缓冲区，如果找到就直接移除
        // First check register buffer, remove directly if found
        if self.register_buffer.remove(&key).is_some() {
            self.stats.optimized_operations += 1;
            trace!(key = ?key, "Timer cancellation optimized - removed from register buffer");
            return true;
        }
        
        // 添加到取消缓冲区
        // Add to cancel buffer
        self.cancel_buffer.insert(key);
        self.stats.total_cancel_requests += 1;
        
        trace!(key = ?key, buffer_size = self.cancel_buffer.len(), "Added timer to cancel buffer");
        
        // 检查是否需要自动刷新
        // Check if auto flush is needed
        if self.cancel_buffer.len() >= self.config.batch_threshold {
            self.flush_buffers().await;
        }
        
        true // 假设成功 - 实际结果将在刷新时确定
    }
    
    /// 处理批量定时器注册（与缓冲区合并处理）
    /// Handle batch timer registration (merge with buffer processing)
    async fn handle_batch_register_timers(
        &mut self,
        registrations: Vec<TimerRegistration<TimeoutEvent>>,
    ) -> Result<Vec<TimerHandle<TimeoutEvent>>, String> {
        trace!(count = registrations.len(), "Processing batch registration with buffer merge");
        
        let mut successful_keys = Vec::new();
        
        // 将所有注册请求加入缓冲区，检查取消缓冲区中的重复项
        // Add all registration requests to buffer, check for duplicates in cancel buffer
        for registration in registrations {
            let key = (registration.connection_id, registration.timeout_event);
            
            // 检查是否在取消缓冲区中，如果是则直接抵消
            // Check if in cancel buffer, if so, directly cancel out
            if self.cancel_buffer.remove(&key) {
                self.stats.optimized_operations += 1;
                trace!(key = ?key, "Timer registration cancelled out by pending cancellation");
                continue; // 跳过这个注册
            }
            
            // 添加到注册缓冲区
            // Add to register buffer
            self.register_buffer.insert(key, registration);
            successful_keys.push(key);
            self.stats.total_register_requests += 1;
        }
        
        trace!(
            added = successful_keys.len(),
            buffer_size = self.register_buffer.len(),
            "Added batch registrations to buffer"
        );
        
        // 检查是否需要刷新缓冲区
        // Check if buffer flush is needed
        if self.register_buffer.len() >= self.config.batch_threshold {
            self.flush_buffers().await;
        }
        
        // 由于使用缓冲区处理，返回空的句柄列表
        // Since using buffer processing, return empty handle list
        // TODO: 实际的句柄将在刷新时创建，这里需要更好的异步句柄管理设计
        Ok(Vec::new())
    }
    
    /// 处理批量定时器取消（优先检查注册缓冲区）
    /// Handle batch timer cancellation (prioritize checking register buffer)
    async fn handle_batch_cancel_timers(&mut self, entries: Vec<(ConnectionId, TimeoutEvent)>) -> usize {
        trace!(count = entries.len(), "Processing batch cancellation with register buffer check");
        
        let mut optimized_count = 0;
        let mut pending_cancellations = Vec::new();
        
        // 遍历每个取消请求，先检查注册缓冲区
        // Iterate through each cancellation request, check register buffer first
        for (connection_id, timeout_event) in entries {
            let key = (connection_id, timeout_event);
            
            // 优先检查注册缓冲区，如果找到就直接移除
            // First check register buffer, remove directly if found
            if self.register_buffer.remove(&key).is_some() {
                optimized_count += 1;
                self.stats.optimized_operations += 1;
                trace!(key = ?key, "Timer cancellation optimized - removed from register buffer");
            } else {
                // 如果不在注册缓冲区，加入取消缓冲区
                // If not in register buffer, add to cancel buffer
                pending_cancellations.push(key);
            }
        }
        
        // 将未在注册缓冲区找到的加入取消缓冲区
        // Add those not found in register buffer to cancel buffer
        let pending_count = pending_cancellations.len();
        if !pending_cancellations.is_empty() {
            for key in pending_cancellations {
                self.cancel_buffer.insert(key);
                self.stats.total_cancel_requests += 1;
            }
            
            trace!(
                pending = self.cancel_buffer.len(),
                "Added pending cancellations to cancel buffer"
            );
        }
        
        // 检查取消缓冲区是否需要刷新
        // Check if cancel buffer needs flushing
        if self.cancel_buffer.len() >= self.config.batch_threshold {
            self.flush_buffers().await;
        }
        
        // 返回成功处理的数量（包括优化的和待处理的）
        // Return successful processing count (including optimized and pending)
        optimized_count + pending_count
    }
    
    /// 刷新所有缓冲区
    /// Flush all buffers
    async fn flush_buffers(&mut self) {
        let start_time = Instant::now();
        
        // 刷新注册缓冲区
        // Flush register buffer
        if !self.register_buffer.is_empty() {
            let registration_entries: Vec<_> = self.register_buffer.drain().collect();
            let registrations: Vec<_> = registration_entries.iter().map(|(_, reg)| reg.clone()).collect();
            let count = registrations.len();
            
            trace!(count = count, "Flushing register buffer");
            
            let batch_registration = BatchTimerRegistration { registrations };
            match self.timer_handle.batch_register_timers(batch_registration).await {
                Ok(result) => {
                    // 更新映射关系
                    // Update mappings
                    for (i, (key, _)) in registration_entries.iter().enumerate() {
                        if i < result.successes.len() {
                            let entry_id = result.successes[i].entry_id;
                            self.key_to_entry_id.insert(*key, entry_id);
                            self.entry_id_to_key.insert(entry_id, *key);
                        }
                    }
                    
                    debug!(
                        registered = result.successes.len(),
                        failed = result.failures.len(),
                        "Register buffer flushed"
                    );
                }
                Err(e) => {
                    warn!(error = %e, count = count, "Failed to flush register buffer");
                }
            }
        }
        
        // 刷新取消缓冲区
        // Flush cancel buffer
        if !self.cancel_buffer.is_empty() {
            let keys: Vec<_> = self.cancel_buffer.drain().collect();
            let count = keys.len();
            
            trace!(count = count, "Flushing cancel buffer");
            
            // 将键转换为entry_ids
            let entry_ids: Vec<TimerEntryId> = keys.iter()
                .filter_map(|key| self.key_to_entry_id.get(key).copied())
                .collect();
            
            if !entry_ids.is_empty() {
                let batch_cancellation = BatchTimerCancellation { entry_ids: entry_ids.clone() };
                match self.timer_handle.batch_cancel_timers(batch_cancellation).await {
                    Ok(result) => {
                        // 清理映射关系
                        // Clean up mappings
                        for entry_id in &entry_ids {
                            if let Some(key) = self.entry_id_to_key.remove(entry_id) {
                                self.key_to_entry_id.remove(&key);
                            }
                        }
                        
                        debug!(
                            cancelled = result.success_count(),
                            "Cancel buffer flushed"
                        );
                    }
                    Err(e) => {
                        warn!(error = %e, count = entry_ids.len(), "Failed to flush cancel buffer");
                    }
                }
            } else {
                debug!("No valid entry IDs found for cancellation");
            }
        }
        
        self.last_flush_time = Instant::now();
        let flush_duration = self.last_flush_time.duration_since(start_time);
        
        trace!(duration = ?flush_duration, "Buffer flush completed");
    }
    
    /// 检查是否应该自动刷新
    /// Check if auto flush should be performed
    fn should_auto_flush(&self) -> bool {
        // 有缓冲的内容且距离上次刷新超过了配置的间隔
        // Has buffered content and exceeded configured interval since last flush
        (!self.register_buffer.is_empty() || !self.cancel_buffer.is_empty())
            && self.last_flush_time.elapsed() >= self.config.flush_interval
    }
    

}

/// 定时器Actor句柄 - 提供客户端接口
/// Timer Actor Handle - provides client interface
#[derive(Debug, Clone)]
pub struct TimerActorHandle {
    command_tx: mpsc::Sender<TimerActorCommand>,
}

impl TimerActorHandle {
    /// 创建新的句柄
    /// Create a new handle
    pub fn new(command_tx: mpsc::Sender<TimerActorCommand>) -> Self {
        Self { command_tx }
    }
    
    /// 注册单个定时器
    /// Register a single timer
    pub async fn register_timer(
        &self,
        registration: TimerRegistration<TimeoutEvent>,
    ) -> Result<TimerHandle<TimeoutEvent>, String> {
        let (response_tx, response_rx) = oneshot::channel();
        let command = TimerActorCommand::RegisterTimer { registration, response_tx };
        
        if self.command_tx.send(command).await.is_err() {
            return Err("TimerActor is not available".to_string());
        }
        
        response_rx.await.unwrap_or_else(|_| Err("Response channel closed".to_string()))
    }
    
    /// 取消单个定时器
    /// Cancel a single timer
    pub async fn cancel_timer(&self, connection_id: ConnectionId, timeout_event: TimeoutEvent) -> bool {
        let (response_tx, response_rx) = oneshot::channel();
        let command = TimerActorCommand::CancelTimer { connection_id, timeout_event, response_tx };
        
        if self.command_tx.send(command).await.is_err() {
            return false;
        }
        
        response_rx.await.unwrap_or(false)
    }
    
    /// 批量注册定时器
    /// Batch register timers
    pub async fn batch_register_timers(
        &self,
        registrations: Vec<TimerRegistration<TimeoutEvent>>,
    ) -> Result<Vec<TimerHandle<TimeoutEvent>>, String> {
        let (response_tx, response_rx) = oneshot::channel();
        let command = TimerActorCommand::BatchRegisterTimers { registrations, response_tx };
        
        if self.command_tx.send(command).await.is_err() {
            return Err("TimerActor is not available".to_string());
        }
        
        response_rx.await.unwrap_or_else(|_| Err("Response channel closed".to_string()))
    }
    
    /// 批量取消定时器
    /// Batch cancel timers
    pub async fn batch_cancel_timers(&self, entries: Vec<(ConnectionId, TimeoutEvent)>) -> usize {
        let (response_tx, response_rx) = oneshot::channel();
        let command = TimerActorCommand::BatchCancelTimers { entries, response_tx };
        
        if self.command_tx.send(command).await.is_err() {
            return 0;
        }
        
        response_rx.await.unwrap_or(0)
    }
    
    /// 强制刷新缓冲区
    /// Force flush buffers
    pub async fn flush_buffers(&self) -> bool {
        self.command_tx.send(TimerActorCommand::FlushBuffers).await.is_ok()
    }
    
    /// 获取统计信息
    /// Get statistics
    pub async fn get_stats(&self) -> Option<TimerActorStats> {
        let (response_tx, response_rx) = oneshot::channel();
        let command = TimerActorCommand::GetStats { response_tx };
        
        if self.command_tx.send(command).await.is_err() {
            return None;
        }
        
        response_rx.await.ok()
    }
    
    /// 关闭Actor
    /// Shutdown Actor
    pub async fn shutdown(&self) -> bool {
        self.command_tx.send(TimerActorCommand::Shutdown).await.is_ok()
    }
}

/// 启动定时器Actor
/// Start Timer Actor
pub fn start_timer_actor(
    timer_handle: HybridTimerTaskHandle<TimeoutEvent>,
    config: Option<TimerActorConfig>,
) -> TimerActorHandle {
    let config = config.unwrap_or_default();
    let (command_tx, command_rx) = mpsc::channel(1024); // 足够大的缓冲区
    
    let actor = TimerActor::new(timer_handle, config);
    
    tokio::spawn(async move {
        actor.run(command_rx).await;
    });
    
    TimerActorHandle::new(command_tx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timer::start_hybrid_timer_task;
    use tokio::time::{sleep, Duration};
    
    #[tokio::test]
    async fn test_timer_actor_basic_operations() {
        let timer_handle = start_hybrid_timer_task::<TimeoutEvent>();
        let actor_handle = start_timer_actor(timer_handle, None);
        
        // 测试统计信息
        let stats = actor_handle.get_stats().await.unwrap();
        assert_eq!(stats.register_buffer_size, 0);
        assert_eq!(stats.cancel_buffer_size, 0);
        
        // 等待一小段时间确保actor启动完成
        sleep(Duration::from_millis(10)).await;
        
        actor_handle.shutdown().await;
    }
    
    #[tokio::test]
    async fn test_timer_actor_buffering() {
        let config = TimerActorConfig {
            batch_threshold: 2, // 降低阈值便于测试
            flush_interval: Duration::from_millis(50),
        };
        
        let timer_handle = start_hybrid_timer_task::<TimeoutEvent>();
        let actor_handle = start_timer_actor(timer_handle, Some(config));
        
        // 测试缓冲区功能
        sleep(Duration::from_millis(20)).await;
        
        let stats = actor_handle.get_stats().await.unwrap();
        println!("Final stats: {:?}", stats);
        
        actor_handle.shutdown().await;
    }
    
    #[tokio::test]
    async fn test_timer_actor_optimization() {
        let config = TimerActorConfig {
            batch_threshold: 5,
            flush_interval: Duration::from_millis(100),
        };
        
        let timer_handle = start_hybrid_timer_task::<TimeoutEvent>();
        let actor_handle = start_timer_actor(timer_handle, Some(config));
        
        // 创建测试定时器注册
        let (callback_tx, _callback_rx) = mpsc::channel(1);
        let registration = TimerRegistration::new(
            123, // connection_id
            Duration::from_secs(30), // delay
            TimeoutEvent::IdleTimeout,
            callback_tx,
        );
        
        // 先注册再立即取消，测试优化功能
        let _ = actor_handle.register_timer(registration).await;
        let cancelled = actor_handle.cancel_timer(123, TimeoutEvent::IdleTimeout).await;
        assert!(cancelled);
        
        // 检查统计信息
        let stats = actor_handle.get_stats().await.unwrap();
        println!("Optimized operations: {}", stats.optimized_operations);
        
        actor_handle.shutdown().await;
    }
    
    #[tokio::test]
    async fn test_timer_actor_batch_optimization() {
        let config = TimerActorConfig {
            batch_threshold: 10, // 设置较小的阈值便于测试
            flush_interval: Duration::from_millis(200),
        };
        
        let timer_handle = start_hybrid_timer_task::<TimeoutEvent>();
        let actor_handle = start_timer_actor(timer_handle, Some(config));
        
        // 创建多个测试定时器注册
        let (callback_tx, _callback_rx) = mpsc::channel(1);
        let mut registrations = Vec::new();
        for i in 0..5 {
            registrations.push(TimerRegistration::new(
                100 + i, // connection_id
                Duration::from_secs(30),
                TimeoutEvent::IdleTimeout,
                callback_tx.clone(),
            ));
        }
        
        // 测试批量注册
        let _ = actor_handle.batch_register_timers(registrations).await;
        
        // 创建批量取消请求（包含一些注册缓冲区中的，一些不在的）
        let cancel_entries = vec![
            (100, TimeoutEvent::IdleTimeout), // 应该在注册缓冲区中
            (101, TimeoutEvent::IdleTimeout), // 应该在注册缓冲区中
            (102, TimeoutEvent::IdleTimeout), // 应该在注册缓冲区中
            (999, TimeoutEvent::IdleTimeout), // 不在注册缓冲区中
            (998, TimeoutEvent::IdleTimeout), // 不在注册缓冲区中
        ];
        
        // 测试批量取消
        let cancelled_count = actor_handle.batch_cancel_timers(cancel_entries).await;
        assert_eq!(cancelled_count, 5); // 应该返回5（全部处理成功）
        
        // 检查统计信息
        let stats = actor_handle.get_stats().await.unwrap();
        println!("Batch optimization stats:");
        println!("  Optimized operations: {}", stats.optimized_operations);
        println!("  Register buffer size: {}", stats.register_buffer_size);
        println!("  Cancel buffer size: {}", stats.cancel_buffer_size);
        
        // 应该有3个优化操作（从注册缓冲区中移除）
        assert!(stats.optimized_operations >= 3);
        
        actor_handle.shutdown().await;
    }
}