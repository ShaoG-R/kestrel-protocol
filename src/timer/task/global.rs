//! 全局定时器任务核心实现
//! Global timer task core implementation
//!
//! 本模块包含全局定时器任务的核心逻辑，负责管理时间轮、处理命令、
//! 维护连接映射关系，以及高性能的批量操作处理。
//!
//! This module contains the core logic of the global timer task, responsible for
//! managing the timing wheel, processing commands, maintaining connection mappings,
//! and high-performance batch operation processing.

use crate::timer::{
    event::{TimerEvent, TimerEventId, ConnectionId},
    wheel::{TimingWheel, TimerEntryId},
};
use crate::timer::event::traits::EventDataTrait;
use crate::timer::event::traits::EventFactory;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use tokio::{
    sync::mpsc,
    time::{Instant, interval, sleep_until},
};
use tracing::{debug, info, trace, warn};

use super::types::{
    BatchProcessingBuffers, TimerRegistration, BatchTimerRegistration,
    BatchTimerCancellation, BatchTimerResult, TimerHandle,
};
use super::commands::{TimerTaskCommand, TimerError, TimerTaskStats};

/// 全局定时器任务
/// Global timer task
pub struct GlobalTimerTask<E: EventDataTrait> {
    /// 时间轮
    /// Timing wheel
    timing_wheel: TimingWheel<E>,
    /// 命令接收通道
    /// Command receiver channel
    command_rx: mpsc::Receiver<TimerTaskCommand<E>>,
    /// 命令发送通道（用于创建句柄）
    /// Command sender channel (for creating handles)
    command_tx: mpsc::Sender<TimerTaskCommand<E>>,
    /// 连接到定时器条目的映射（使用HashSet实现O(1)删除）
    /// Connection to timer entries mapping (using HashSet for O(1) deletion)
    connection_timers: HashMap<ConnectionId, HashSet<TimerEntryId>>,
    /// 定时器条目到连接的反向映射，用于O(1)查找
    /// Reverse mapping from timer entry to connection for O(1) lookup
    entry_to_connection: HashMap<TimerEntryId, ConnectionId>,
    /// 下一个分配的定时器事件ID
    /// Next timer event ID to allocate
    next_event_id: TimerEventId,
    /// 统计信息
    /// Statistics
    stats: TimerTaskStats,
    /// 预分配的容器，用于批量处理时减少内存分配
    /// Pre-allocated containers for reducing memory allocation during batch processing
    batch_processing_buffers: BatchProcessingBuffers,
    /// 定时器事件工厂 (智能策略选择)
    /// Timer event factory (smart strategy selection)
    event_factory: EventFactory<E>,
}

impl<E: EventDataTrait> GlobalTimerTask<E> {
    /// 创建新的全局定时器任务
    /// Create new global timer task
    pub fn new(command_buffer_size: usize) -> (Self, mpsc::Sender<TimerTaskCommand<E>>) {
        let (command_tx, command_rx) = mpsc::channel(command_buffer_size);
        // 使用更小的槽位持续时间以支持更精确的定时器
        let timing_wheel = TimingWheel::new(512, Duration::from_millis(10));
        
        let wheel_stats = timing_wheel.stats();
        let task = Self {
            timing_wheel,
            command_rx,
            command_tx: command_tx.clone(),
            connection_timers: HashMap::new(),
            entry_to_connection: HashMap::new(),
            next_event_id: 1,
            stats: TimerTaskStats {
                total_timers: 0,
                active_connections: 0,
                processed_timers: 0,
                cancelled_timers: 0,
                wheel_stats,
            },
            batch_processing_buffers: BatchProcessingBuffers::new(),
            event_factory: EventFactory::new(), // 智能策略选择，零配置
        };

        (task, command_tx)
    }

    /// 创建默认配置的全局定时器任务
    /// Create global timer task with default configuration
    pub fn new_default() -> (Self, mpsc::Sender<TimerTaskCommand<E>>) {
        Self::new(1024)
    }

    /// 运行定时器任务主循环
    /// Run timer task main loop
    pub async fn run(mut self) {
        info!("Global timer task started");
        
        // 动态推进间隔：根据定时器数量调整，最小10ms，最大100ms
        // Dynamic advance interval: adjust based on timer count, min 10ms, max 100ms
        let mut advance_interval = self.calculate_advance_interval();
        let mut next_interval_update = Instant::now() + Duration::from_secs(1);
        
        loop {
            // 智能计算下一次唤醒时间
            // Intelligently calculate next wakeup time
            let next_wakeup = self.get_next_wakeup_time();
            let now = Instant::now();

            tokio::select! {
                // 处理命令
                // Process commands
                Some(command) = self.command_rx.recv() => {
                    if !self.handle_command(command).await {
                        break; // 收到关闭命令
                    }
                    // 命令处理后可能影响定时器，重新计算间隔
                    // After command processing, timers might change, recalculate interval
                    if now >= next_interval_update {
                        advance_interval = self.calculate_advance_interval();
                        next_interval_update = now + Duration::from_secs(1);
                    }
                }
                
                // 动态间隔推进时间轮
                // Dynamic interval advance timing wheel
                _ = advance_interval.tick() => {
                    self.advance_timing_wheel().await;
                }
                
                // 基于最早定时器的精确唤醒
                // Precise wakeup based on earliest timer
                _ = sleep_until(next_wakeup) => {
                    self.advance_timing_wheel().await;
                }
                
                // 定期更新推进间隔
                // Periodically update advance interval
                _ = sleep_until(next_interval_update) => {
                    advance_interval = self.calculate_advance_interval();
                    next_interval_update = now + Duration::from_secs(1);
                }
            }
        }

        info!("Global timer task shutdown completed");
    }

    /// 处理定时器任务命令
    /// Handle timer task command
    ///
    /// # Returns
    /// 返回false表示应该关闭任务
    /// Returns false if task should shutdown
    async fn handle_command(&mut self, command: TimerTaskCommand<E>) -> bool {
        match command {
            TimerTaskCommand::RegisterTimer { registration, response_tx } => {
                let result = self.register_timer(registration).await;
                if let Err(err) = response_tx.send(result) {
                    warn!(error = ?err, "Failed to send register timer response");
                }
            }
            
            TimerTaskCommand::BatchRegisterTimers { batch_registration, response_tx } => {
                let result = self.batch_register_timers(batch_registration).await;
                if let Err(err) = response_tx.send(result) {
                    warn!(error = ?err, "Failed to send batch register timers response");
                }
            }
            
            TimerTaskCommand::CancelTimer { entry_id, response_tx } => {
                let result = self.cancel_timer(entry_id);
                if let Err(err) = response_tx.send(result) {
                    warn!(error = ?err, "Failed to send cancel timer response");
                }
            }
            
            TimerTaskCommand::BatchCancelTimers { batch_cancellation, response_tx } => {
                let result = self.batch_cancel_timers(batch_cancellation);
                if let Err(err) = response_tx.send(result) {
                    warn!(error = ?err, "Failed to send batch cancel timers response");
                }
            }
            
            TimerTaskCommand::ClearConnectionTimers { connection_id, response_tx } => {
                let count = self.clear_connection_timers(connection_id);
                if let Err(err) = response_tx.send(count) {
                    warn!(error = ?err, "Failed to send clear timers response");
                }
            }
            
            TimerTaskCommand::GetStats { response_tx } => {
                self.update_stats();
                if let Err(err) = response_tx.send(self.stats.clone()) {
                    warn!(error = ?err, "Failed to send stats response");
                }
            }
            
            TimerTaskCommand::Shutdown => {
                info!("Received shutdown command");
                return false;
            }
        }
        
        true
    }

    /// 注册定时器
    /// Register timer
    async fn register_timer(&mut self, registration: TimerRegistration<E>) -> Result<TimerHandle<E>, TimerError> {
        let event_id = self.next_event_id;
        self.next_event_id += 1;

        // 使用智能工厂创建定时器事件 - 自动策略优化
        // Create timer event using smart factory - automatic strategy optimization
        let timeout_event_for_log = registration.timeout_event.clone();
        let timer_event = TimerEvent::from_factory(
            &self.event_factory,
            event_id,
            registration.connection_id,
            registration.timeout_event,
            registration.callback_tx,
        );

        // 添加到时间轮
        // Add to timing wheel
        let entry_id = self.timing_wheel.add_timer(registration.delay, timer_event);

        // 记录连接关联
        // Record connection association
        self.connection_timers
            .entry(registration.connection_id)
            .or_default()
            .insert(entry_id);
        
        // 记录反向映射
        // Record reverse mapping
        self.entry_to_connection.insert(entry_id, registration.connection_id);

        // 创建定时器句柄
        // Create timer handle
        let handle = TimerHandle::new(entry_id, self.command_tx.clone());

        trace!(
            event_id,
            entry_id,
            connection_id = registration.connection_id,
            delay_ms = registration.delay.as_millis(),
            event_type = ?timeout_event_for_log,
            "Timer registered successfully"
        );

        Ok(handle)
    }

    /// 批量注册定时器（高性能版本）
    /// Batch register timers (high-performance version)
    async fn batch_register_timers(&mut self, batch_registration: BatchTimerRegistration<E>) -> BatchTimerResult<TimerHandle<E>> {
        let registrations = batch_registration.registrations;
        let total_count = registrations.len();
        
        if total_count == 0 {
            return BatchTimerResult::new();
        }

        let mut result = BatchTimerResult::with_capacity(total_count);
        
        // 预分配event IDs，减少分配开销
        // Pre-allocate event IDs to reduce allocation overhead
        let start_event_id = self.next_event_id;
        self.next_event_id += total_count as u64;
        
        // 批量创建定时器事件，为时间轮批量操作准备数据
        // Batch create timer events, prepare data for timing wheel batch operation
        let mut timers_for_wheel = Vec::with_capacity(total_count);
        let mut registration_data = Vec::with_capacity(total_count);
        
        // 为批量对象池API准备数据
        // Prepare data for batch object pool API
        let pool_requests: Vec<_> = registrations.iter()
            .map(|reg| (reg.connection_id, reg.timeout_event.clone()))
            .collect();
        let callback_txs: Vec<_> = registrations.iter()
            .map(|reg| reg.callback_tx.clone())
            .collect();
        
        // 批量创建定时器事件，智能策略选择，高性能优化
        // Batch create timer events, smart strategy selection, high performance optimization
        let timer_events = TimerEvent::batch_from_factory(
            &self.event_factory,
            start_event_id,
            &pool_requests,
            &callback_txs,
        );
        
        // 构建时间轮数据
        // Build timing wheel data
        for (timer_event, registration) in timer_events.into_iter().zip(registrations.iter()) {
            timers_for_wheel.push((registration.delay, timer_event));
            registration_data.push((registration.connection_id, registration.delay));
        }
        
        // 使用时间轮的批量API一次性添加所有定时器
        // Use timing wheel's batch API to add all timers at once
        let entry_ids = self.timing_wheel.batch_add_timers(timers_for_wheel);
        
        // 批量更新映射关系
        // Batch update mapping relationships
        for (entry_id, (connection_id, _delay)) in entry_ids.iter().zip(registration_data.iter()) {
            self.connection_timers
                .entry(*connection_id)
                .or_default()
                .insert(*entry_id);
            
            self.entry_to_connection.insert(*entry_id, *connection_id);
            
            // 创建定时器句柄
            // Create timer handle
            let handle = TimerHandle::new(*entry_id, self.command_tx.clone());
            result.successes.push(handle);
        }
        
        trace!(
            batch_size = total_count,
            "Batch registered timers successfully using wheel batch API"
        );
        
        result
    }

    /// 批量取消定时器（高性能版本）
    /// Batch cancel timers (high-performance version)
    fn batch_cancel_timers(&mut self, batch_cancellation: BatchTimerCancellation) -> BatchTimerResult<bool> {
        let entry_ids = batch_cancellation.entry_ids;
        let total_count = entry_ids.len();
        
        if total_count == 0 {
            return BatchTimerResult::new();
        }

        let mut result = BatchTimerResult::with_capacity(total_count);
        
        // 使用时间轮的批量取消API
        // Use timing wheel's batch cancel API
        let cancelled_count = self.timing_wheel.batch_cancel_timers(&entry_ids);
        
        // 批量收集要移除的映射信息
        // Batch collect mapping info to remove
        let mut connections_to_update: HashMap<ConnectionId, Vec<TimerEntryId>> = HashMap::new();
        
        for (index, entry_id) in entry_ids.iter().enumerate() {
            // 检查是否在entry_to_connection映射中（表示成功取消）
            // Check if exists in entry_to_connection mapping (indicates successful cancellation)
            if let Some(connection_id) = self.entry_to_connection.remove(entry_id) {
                // 收集连接映射信息，批量处理
                // Collect connection mapping info for batch processing
                connections_to_update
                    .entry(connection_id)
                    .or_default()
                    .push(*entry_id);
                
                self.stats.cancelled_timers += 1;
                result.successes.push(true);
            } else {
                result.failures.push((index, TimerError::TimerNotFound));
            }
        }
        
        // 批量更新连接定时器映射
        // Batch update connection timer mappings  
        for (connection_id, cancelled_entry_ids) in connections_to_update {
            if let Some(entry_ids_set) = self.connection_timers.get_mut(&connection_id) {
                for entry_id in cancelled_entry_ids {
                    entry_ids_set.remove(&entry_id);
                }
                
                // 如果该连接没有更多定时器，移除连接条目
                // If connection has no more timers, remove connection entry
                if entry_ids_set.is_empty() {
                    self.connection_timers.remove(&connection_id);
                }
            }
        }
        
        trace!(
            batch_size = total_count,
            cancelled_count = result.success_count(),
            wheel_cancelled_count = cancelled_count,
            "Batch cancelled timers using wheel batch API"
        );
        
        result
    }

    /// 取消定时器
    /// Cancel timer
    fn cancel_timer(&mut self, entry_id: TimerEntryId) -> bool {
        let cancelled = self.timing_wheel.cancel_timer(entry_id);
        
        if cancelled {
            // 使用反向映射快速定位连接并移除定时器条目
            // Use reverse mapping to quickly locate connection and remove timer entry
            if let Some(connection_id) = self.entry_to_connection.remove(&entry_id) {
                if let Some(entry_ids) = self.connection_timers.get_mut(&connection_id) {
                    entry_ids.remove(&entry_id);
                    // 如果该连接没有更多定时器，移除连接条目
                    // If connection has no more timers, remove connection entry
                    if entry_ids.is_empty() {
                        self.connection_timers.remove(&connection_id);
                    }
                }
            }
            
            self.stats.cancelled_timers += 1;
            trace!(entry_id, "Timer cancelled successfully");
        }
        
        cancelled
    }

    /// 清除连接的所有定时器
    /// Clear all timers for a connection
    fn clear_connection_timers(&mut self, connection_id: ConnectionId) -> usize {
        let mut count = 0;
        
        if let Some(entry_ids) = self.connection_timers.remove(&connection_id) {
            for entry_id in &entry_ids {
                if self.timing_wheel.cancel_timer(*entry_id) {
                    // 从反向映射中移除
                    // Remove from reverse mapping
                    self.entry_to_connection.remove(entry_id);
                    count += 1;
                    self.stats.cancelled_timers += 1;
                }
            }
            
            debug!(
                connection_id,
                count,
                "Cleared all timers for connection"
            );
        }
        
        count
    }

    /// 推进时间轮并处理到期的定时器（高性能版本）
    /// Advance timing wheel and process expired timers (high-performance version)
    async fn advance_timing_wheel(&mut self) {
        let now = Instant::now();
        let expired_timers = self.timing_wheel.advance(now);

        if expired_timers.is_empty() {
            return;
        }

        // 清空并重用缓冲区，减少内存分配
        // Clear and reuse buffers to reduce memory allocation
        self.batch_processing_buffers.clear();

        // 第一步：使用预分配缓冲区收集连接映射信息
        // Step 1: Use pre-allocated buffers to collect connection mapping info
        for entry in &expired_timers {
            if let Some(&conn_id) = self.entry_to_connection.get(&entry.id) {
                self.batch_processing_buffers.expired_by_connection
                    .entry(conn_id)
                    .or_insert_with(|| Vec::with_capacity(4))
                    .push(entry.id);
            }
        }

        // 第二步：批量清理映射关系
        // Step 2: Batch cleanup mapping relationships
        for entry in &expired_timers {
            self.entry_to_connection.remove(&entry.id);
        }

        // 第三步：高效清理连接定时器映射
        // Step 3: Efficiently cleanup connection timer mappings
        for (conn_id, expired_ids) in self.batch_processing_buffers.expired_by_connection.drain() {
            if let Some(entry_ids) = self.connection_timers.get_mut(&conn_id) {
                for expired_id in expired_ids {
                    entry_ids.remove(&expired_id);
                }
                if entry_ids.is_empty() {
                    self.connection_timers.remove(&conn_id);
                }
            }
        }

        // 第四步：批量并发触发所有定时器 (使用智能工厂)
        // Step 4: Batch concurrent trigger all timers (using smart factory)
        let factory = self.event_factory.clone(); // 工厂可以安全克隆，轻量级
        let trigger_futures: Vec<_> = expired_timers
            .into_iter()
            .map(|entry| {
                let entry_id = entry.id;
                let connection_id = entry.event.data.connection_id;
                let event_type = entry.event.data.timeout_event.clone();
                let factory_ref = factory.clone(); // 每个任务都可以有自己的工厂引用（零成本）
                
                async move {
                    entry.event.trigger_with_factory(&factory_ref).await;
                    (entry_id, connection_id, event_type)
                }
            })
            .collect();

        // 并发执行所有定时器触发
        // Execute all timer triggers concurrently
        let results = futures::future::join_all(trigger_futures).await;
        
        // 更新统计信息
        // Update statistics
        let processed_count = results.len();
        self.stats.processed_timers += processed_count as u64;
        
        // 高效日志记录
        // Efficient logging
        if processed_count > 1 {
            debug!(processed_count, "Batch processed expired timers");
        }
        
        for (entry_id, connection_id, event_type) in results {
            trace!(entry_id, connection_id, event_type = ?event_type, "Timer event processed");
        }
    }

    /// 更新统计信息
    /// Update statistics
    fn update_stats(&mut self) {
        self.stats.total_timers = self.timing_wheel.timer_count();
        self.stats.active_connections = self.connection_timers.len();
        self.stats.wheel_stats = self.timing_wheel.stats();
    }

    /// 根据当前定时器数量计算动态推进间隔
    /// Calculate dynamic advance interval based on current timer count
    fn calculate_advance_interval(&self) -> tokio::time::Interval {
        let timer_count = self.timing_wheel.timer_count();
        
        // 根据定时器数量动态调整间隔：
        // - 少于100个定时器：100ms间隔
        // - 100-1000个定时器：50ms间隔  
        // - 1000-5000个定时器：20ms间隔
        // - 超过5000个定时器：10ms间隔
        // Dynamically adjust interval based on timer count:
        // - Less than 100 timers: 100ms interval
        // - 100-1000 timers: 50ms interval
        // - 1000-5000 timers: 20ms interval
        // - More than 5000 timers: 10ms interval
        let interval_ms = if timer_count < 100 {
            100
        } else if timer_count < 1000 {
            50
        } else if timer_count < 5000 {
            20
        } else {
            10
        };
        
        interval(Duration::from_millis(interval_ms))
    }

    /// 智能获取下次唤醒时间
    /// Intelligently get next wakeup time
    fn get_next_wakeup_time(&mut self) -> Instant {
        match self.timing_wheel.next_expiry_time() {
            Some(expiry) => {
                let now = Instant::now();
                // 如果下次到期时间太近（小于5ms），使用当前时间+5ms避免忙等待
                // If next expiry is too soon (less than 5ms), use current time + 5ms to avoid busy waiting
                if expiry <= now + Duration::from_millis(5) {
                    now + Duration::from_millis(5)
                } else {
                    expiry
                }
            }
            // 如果没有定时器，使用较长的默认间隔
            // If no timers, use longer default interval
            None => Instant::now() + Duration::from_secs(1),
        }
    }
}