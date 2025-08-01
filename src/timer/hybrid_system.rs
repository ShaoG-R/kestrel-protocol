//! 混合并行定时器系统集成
//! Hybrid Parallel Timer System Integration
//!
//! 该模块提供了使用HybridParallelTimerSystem的新定时器任务系统，
//! 作为GlobalTimerTask的高性能替代方案。
//!
//! This module provides a new timer task system using HybridParallelTimerSystem
//! as a high-performance replacement for GlobalTimerTask.

use crate::timer::{
    event::{TimerEvent, TimerEventId, ConnectionId},
    wheel::{TimingWheel, TimerEntry, TimerEntryId},
    parallel::core::HybridParallelTimerSystem,
    task::{
        commands::{TimerTaskCommand, TimerError},
        types::{
            TimerRegistration, BatchTimerRegistration, BatchTimerCancellation,
            BatchTimerResult, TimerHandle
        }
    },
    TimerTaskStats
};
use crate::timer::event::traits::{EventDataTrait, EventFactory};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

/// 批量处理缓冲区（本地定义）
/// Batch processing buffers (local definition)
struct HybridBatchProcessingBuffers {
    /// 按连接分组的过期定时器ID
    /// Expired timer IDs grouped by connection
    expired_by_connection: HashMap<ConnectionId, Vec<TimerEntryId>>,
}

impl HybridBatchProcessingBuffers {
    fn new() -> Self {
        Self {
            expired_by_connection: HashMap::new(),
        }
    }
    
    fn clear(&mut self) {
        self.expired_by_connection.clear();
    }
}
use tokio::{
    sync::mpsc,
    time::{Instant, interval, sleep_until},
};
use tracing::{debug, info, trace, warn};

/// 混合并行定时器任务
/// Hybrid parallel timer task
pub struct HybridTimerTask<E: EventDataTrait> {
    /// 时间轮（保持现有的定时器管理逻辑）
    /// Timing wheel (maintain existing timer management logic)
    timing_wheel: TimingWheel<E>,
    /// 混合并行处理系统
    /// Hybrid parallel processing system
    parallel_system: HybridParallelTimerSystem<E>,
    /// 命令接收通道
    /// Command receiver channel
    command_rx: mpsc::Receiver<TimerTaskCommand<E>>,
    /// 命令发送通道（用于创建句柄）
    /// Command sender channel (for creating handles)
    command_tx: mpsc::Sender<TimerTaskCommand<E>>,
    /// 连接到定时器条目的映射
    /// Connection to timer entries mapping
    connection_timers: HashMap<ConnectionId, HashSet<TimerEntryId>>,
    /// 定时器条目到连接的反向映射
    /// Reverse mapping from timer entry to connection
    entry_to_connection: HashMap<TimerEntryId, ConnectionId>,
    /// 下一个分配的定时器事件ID
    /// Next timer event ID to allocate
    next_event_id: TimerEventId,
    /// 统计信息
    /// Statistics
    stats: TimerTaskStats,
    /// 预分配的容器，用于批量处理
    /// Pre-allocated containers for batch processing
    batch_processing_buffers: HybridBatchProcessingBuffers,
    /// 并行处理的批量大小阈值
    /// Batch size threshold for parallel processing
    parallel_threshold: usize,
    /// 定时器事件工厂 (智能策略选择)
    /// Timer event factory (smart strategy selection)
    event_factory: EventFactory<E>,
}

impl<E: EventDataTrait> HybridTimerTask<E> {
    /// 创建新的混合并行定时器任务
    /// Create new hybrid parallel timer task
    pub fn new(command_buffer_size: usize, parallel_threshold: usize) -> (Self, mpsc::Sender<TimerTaskCommand<E>>) {
        let (command_tx, command_rx) = mpsc::channel(command_buffer_size);
        let timing_wheel = TimingWheel::new(512, Duration::from_millis(10));
        let parallel_system = HybridParallelTimerSystem::new();
        
        let wheel_stats = timing_wheel.stats();
        let task = Self {
            timing_wheel,
            parallel_system,
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
            batch_processing_buffers: HybridBatchProcessingBuffers::new(),
            parallel_threshold,
            event_factory: EventFactory::new(), // 智能策略选择，零配置
        };

        (task, command_tx)
    }

    /// 创建默认配置的混合并行定时器任务
    /// Create hybrid parallel timer task with default configuration
    pub fn new_default() -> (Self, mpsc::Sender<TimerTaskCommand<E>>) {
        // 默认并行阈值为32，即超过32个定时器时使用并行处理
        // Default parallel threshold is 32, use parallel processing when more than 32 timers
        Self::new(1024, 32)
    }

    /// 运行定时器任务主循环
    /// Run timer task main loop
    pub async fn run(mut self) {
        info!("Hybrid parallel timer task started");
        
        let mut advance_interval = self.calculate_advance_interval();
        let mut next_interval_update = Instant::now() + Duration::from_secs(1);
        
        loop {
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
                    self.advance_timing_wheel_hybrid().await;
                }
                
                // 基于最早定时器的精确唤醒
                // Precise wakeup based on earliest timer
                _ = sleep_until(next_wakeup) => {
                    self.advance_timing_wheel_hybrid().await;
                }
                
                // 定期更新推进间隔
                // Periodically update advance interval
                _ = sleep_until(next_interval_update) => {
                    advance_interval = self.calculate_advance_interval();
                    next_interval_update = now + Duration::from_secs(1);
                }
            }
        }

        info!("Hybrid parallel timer task shutdown completed");
    }

    /// 混合并行推进时间轮（核心方法）
    /// Hybrid parallel advance timing wheel (core method)
    async fn advance_timing_wheel_hybrid(&mut self) {
        let now = Instant::now();
        let expired_timers = self.timing_wheel.advance(now);

        if expired_timers.is_empty() {
            return;
        }

        let batch_size = expired_timers.len();
        
        // 清空并重用缓冲区
        // Clear and reuse buffers
        self.batch_processing_buffers.clear();

        // 收集连接映射信息
        // Collect connection mapping info
        for entry in &expired_timers {
            if let Some(&conn_id) = self.entry_to_connection.get(&entry.id) {
                self.batch_processing_buffers.expired_by_connection
                    .entry(conn_id)
                    .or_insert_with(|| Vec::with_capacity(4))
                    .push(entry.id);
            }
        }

        // 批量清理映射关系
        // Batch cleanup mapping relationships
        for entry in &expired_timers {
            self.entry_to_connection.remove(&entry.id);
        }

        // 清理连接定时器映射
        // Cleanup connection timer mappings
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

        // 关键：根据批量大小选择处理策略
        // Key: Choose processing strategy based on batch size
        if batch_size >= self.parallel_threshold {
            // 先提取事件通知所需的信息，避免后续访问移动后的数据
            // Extract event notification info first to avoid accessing moved data later
            let notification_data: Vec<_> = expired_timers.iter()
                .map(|entry| (
                    entry.id,
                    entry.event.data.connection_id,
                    entry.event.data.timeout_event.clone(),
                    entry.event.callback_tx.clone(),
                    entry.event.data.clone(),
                ))
                .collect();

            // 使用混合并行系统处理大批量
            // Use hybrid parallel system for large batches
            match self.parallel_system.process_timer_batch(expired_timers).await {
                Ok(parallel_result) => {
                    info!(
                        processed_count = parallel_result.processed_count,
                        strategy = ?parallel_result.strategy_used,
                        duration_ms = parallel_result.processing_duration.as_millis(),
                        "Parallel batch processing completed"
                    );
                    
                    // 更新统计信息
                    // Update statistics
                    self.stats.processed_timers += parallel_result.processed_count as u64;
                    
                    // 记录并行处理的详细统计
                    // Log detailed parallel processing stats
                    let parallel_stats = self.parallel_system.get_stats();
                    debug!(
                        parallel_stats = ?parallel_stats,
                        "Parallel processing statistics"
                    );

                    // 并行处理完成后，发送事件通知
                    // After parallel processing, send event notifications
                    self.send_timer_notifications_from_data(notification_data).await;
                }
                Err(e) => {
                    warn!(error = ?e, "Parallel processing failed, falling back to sequential");
                    // 失败时使用预提取的数据进行顺序处理
                    // Fallback to sequential processing using pre-extracted data
                    self.process_timers_from_notification_data(notification_data).await;
                }
            }
        } else {
            // 小批量使用传统顺序处理
            // Use traditional sequential processing for small batches
            self.process_timers_sequential(expired_timers).await;
        }
    }

    /// 从预提取的数据发送定时器通知
    /// Send timer notifications from pre-extracted data
    async fn send_timer_notifications_from_data(
        &mut self, 
        notification_data: Vec<(
            crate::timer::wheel::TimerEntryId,
            crate::timer::event::ConnectionId,
            E,
            tokio::sync::mpsc::Sender<crate::timer::event::TimerEventData<E>>,
            crate::timer::event::TimerEventData<E>,
        )>
    ) {
        let notification_count = notification_data.len();
        
        // 并发发送所有定时器事件通知
        // Concurrently send all timer event notifications
        let notification_futures: Vec<_> = notification_data
            .into_iter()
            .map(|(entry_id, connection_id, event_type, callback_tx, event_data)| {
                async move {
                    if let Err(e) = callback_tx.send(event_data).await {
                        warn!(
                            entry_id,
                            connection_id,
                            error = ?e,
                            "Failed to send timer event notification"
                        );
                        false
                    } else {
                        trace!(
                            entry_id,
                            connection_id,
                            event_type = ?event_type,
                            "Timer event notification sent successfully"
                        );
                        true
                    }
                }
            })
            .collect();

        // 并发执行所有通知发送
        // Execute all notification sends concurrently
        let results = futures::future::join_all(notification_futures).await;
        
        let successful_notifications = results.iter().filter(|&&success| success).count();
        
        debug!(
            total_notifications = notification_count,
            successful_notifications,
            "Timer notifications sent after parallel processing"
        );
    }

    /// 从预提取的数据进行顺序处理（fallback）
    /// Sequential processing from pre-extracted data (fallback)
    async fn process_timers_from_notification_data(
        &mut self, 
        notification_data: Vec<(
            crate::timer::wheel::TimerEntryId,
            crate::timer::event::ConnectionId,
            E,
            tokio::sync::mpsc::Sender<crate::timer::event::TimerEventData<E>>,
            crate::timer::event::TimerEventData<E>,
        )>
    ) {
        let processed_count = notification_data.len();
        
        // 顺序发送所有定时器事件通知
        // Sequentially send all timer event notifications
        for (entry_id, connection_id, event_type, callback_tx, event_data) in notification_data {
            if let Err(e) = callback_tx.send(event_data).await {
                warn!(
                    entry_id,
                    connection_id,
                    error = ?e,
                    "Failed to send timer event"
                );
            } else {
                trace!(
                    entry_id,
                    connection_id,
                    event_type = ?event_type,
                    "Timer event processed sequentially"
                );
            }
        }
        
        // 更新统计信息
        // Update statistics
        self.stats.processed_timers += processed_count as u64;
        
        if processed_count > 1 {
            debug!(processed_count, "Sequential batch processed expired timers from fallback");
        }
    }

    /// 传统顺序处理定时器（作为fallback）
    /// Traditional sequential timer processing (as fallback)
    async fn process_timers_sequential(&mut self, expired_timers: Vec<TimerEntry<E>>) {
        let processed_count = expired_timers.len();
        
        // 顺序触发所有定时器
        // Sequentially trigger all timers
        for entry in expired_timers {
            let entry_id = entry.id;
            let connection_id = entry.event.data.connection_id;
            let event_type = &entry.event.data.timeout_event;
            
            // 使用现有的事件触发机制
            // Use existing event trigger mechanism
            if let Err(e) = entry.event.callback_tx.send(entry.event.data.clone()).await {
                warn!(
                    entry_id,
                    connection_id,
                    error = ?e,
                    "Failed to send timer event"
                );
            } else {
                trace!(
                    entry_id,
                    connection_id,
                    event_type = ?event_type,
                    "Timer event processed sequentially"
                );
            }
        }
        
        // 更新统计信息
        // Update statistics
        self.stats.processed_timers += processed_count as u64;
        
        if processed_count > 1 {
            debug!(processed_count, "Sequential batch processed expired timers");
        }
    }

    /// 处理定时器任务命令（复用现有逻辑）
    /// Handle timer task command (reuse existing logic)
    async fn handle_command(&mut self, command: TimerTaskCommand<E>) -> bool {
        // 这里可以复用现有的GlobalTimerTask的handle_command逻辑
        // We can reuse the existing GlobalTimerTask's handle_command logic here
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
                match result {
                    Ok(cancelled) => {
                        if let Err(err) = response_tx.send(cancelled) {
                            warn!(error = ?err, "Failed to send cancel timer response");
                        }
                    }
                    Err(timer_error) => {
                        warn!(error = ?timer_error, "Failed to cancel timer");
                        let _ = response_tx.send(false); // 发送false表示取消失败
                    }
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
                return false; // 返回false表示应该关闭
            }
        }
        true
    }

    // 以下方法可以从现有的GlobalTimerTask中复制或适配：
    // The following methods can be copied or adapted from existing GlobalTimerTask:
    
    /// 注册单个定时器
    /// Register single timer
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

    /// 批量注册定时器
    /// Batch register timers  
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

    /// 取消定时器
    /// Cancel timer
    fn cancel_timer(&mut self, entry_id: TimerEntryId) -> Result<bool, TimerError> {
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
        
        Ok(cancelled)
    }

    /// 批量取消定时器
    /// Batch cancel timers
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

    /// 清理连接的所有定时器
    /// Clear all timers for connection
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

    /// 更新统计信息
    /// Update statistics
    fn update_stats(&mut self) {
        self.stats.total_timers = self.timing_wheel.timer_count();
        self.stats.active_connections = self.connection_timers.len();
        self.stats.wheel_stats = self.timing_wheel.stats();
    }

    /// 计算动态推进间隔
    /// Calculate dynamic advance interval
    fn calculate_advance_interval(&self) -> tokio::time::Interval {
        let timer_count = self.timing_wheel.timer_count();
        
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

    /// 获取下次唤醒时间
    /// Get next wakeup time
    fn get_next_wakeup_time(&mut self) -> Instant {
        match self.timing_wheel.next_expiry_time() {
            Some(expiry) => {
                let now = Instant::now();
                if expiry <= now + Duration::from_millis(5) {
                    now + Duration::from_millis(5)
                } else {
                    expiry
                }
            }
            None => Instant::now() + Duration::from_secs(1),
        }
    }

    /// 获取并行处理系统的性能统计
    /// Get parallel processing system performance statistics
    pub fn get_parallel_stats(&self) -> &crate::timer::parallel::types::ParallelProcessingStats {
        self.parallel_system.get_stats()
    }
}

/// 混合并行定时器任务句柄
/// Hybrid parallel timer task handle  
#[derive(Clone)]
pub struct HybridTimerTaskHandle<E: EventDataTrait> {
    command_tx: mpsc::Sender<TimerTaskCommand<E>>,
}

impl<E: EventDataTrait> HybridTimerTaskHandle<E> {
    /// 创建新的句柄
    /// Create new handle
    pub fn new(command_tx: mpsc::Sender<TimerTaskCommand<E>>) -> Self {
        Self { command_tx }
    }

    /// 获取命令发送通道的克隆
    /// Get clone of command sender channel
    pub fn command_sender(&self) -> mpsc::Sender<TimerTaskCommand<E>> {
        self.command_tx.clone()
    }
    
    /// 注册定时器
    /// Register timer
    pub async fn register_timer(
        &self,
        registration: TimerRegistration<E>,
    ) -> Result<TimerHandle<E>, TimerError> {
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        
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
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        
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
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        
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
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        
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
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        
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

/// 启动混合并行定时器任务
/// Start hybrid parallel timer task
pub fn start_hybrid_timer_task<E: EventDataTrait>() -> HybridTimerTaskHandle<E> {
    let (task, command_tx) = HybridTimerTask::new_default();
    let handle = HybridTimerTaskHandle::new(command_tx.clone());
    
    tokio::spawn(async move {
        task.run().await;
    });
    
    info!("Hybrid parallel timer task started");
    handle
}