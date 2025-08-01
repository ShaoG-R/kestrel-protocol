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

/// 高性能批量处理缓冲区（本地定义）
/// High-performance batch processing buffers (local definition)
struct HybridBatchProcessingBuffers {
    /// 按连接分组的过期定时器ID（预分配容量）
    /// Expired timer IDs grouped by connection (pre-allocated capacity)
    expired_by_connection: HashMap<ConnectionId, Vec<TimerEntryId>>,
    /// 预分配的条目ID缓冲区用于批量移除
    /// Pre-allocated entry ID buffer for batch removal
    entry_ids_buffer: Vec<TimerEntryId>,
    /// 连接映射更新缓冲区
    /// Connection mapping update buffer
    connection_updates: HashMap<ConnectionId, Vec<TimerEntryId>>,
}

impl HybridBatchProcessingBuffers {
    fn new() -> Self {
        Self {
            expired_by_connection: HashMap::with_capacity(64),
            entry_ids_buffer: Vec::with_capacity(256),
            connection_updates: HashMap::with_capacity(64),
        }
    }
    
    fn clear(&mut self) {
        // 清空但保留容量以避免重新分配
        // Clear but retain capacity to avoid reallocation
        self.expired_by_connection.clear();
        self.entry_ids_buffer.clear(); 
        self.connection_updates.clear();
        
        // 如果缓冲区过大，适度收缩以控制内存使用
        // If buffers are too large, shrink moderately to control memory usage
        if self.expired_by_connection.capacity() > 512 {
            self.expired_by_connection.shrink_to(256);
        }
        if self.entry_ids_buffer.capacity() > 2048 {
            self.entry_ids_buffer.shrink_to(1024);
        }
        if self.connection_updates.capacity() > 512 {
            self.connection_updates.shrink_to(256);
        }
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
    /// 创建新的混合并行定时器任务（高性能版本）
    /// Create new hybrid parallel timer task (high-performance version)
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
            // 预分配更大的容量以减少重新分配
            // Pre-allocate larger capacity to reduce reallocations
            connection_timers: HashMap::with_capacity(128),
            entry_to_connection: HashMap::with_capacity(1024),
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

    /// 高性能混合并行推进时间轮（核心方法）
    /// High-performance hybrid parallel advance timing wheel (core method)
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

        // 单次遍历优化：同时收集连接映射信息和预提取通知数据，减少遍历次数
        // Single-pass optimization: collect connection mapping and pre-extract notification data to reduce iterations
        let mut notification_data = Vec::with_capacity(batch_size);
        self.batch_processing_buffers.entry_ids_buffer.reserve(batch_size);
        
        for entry in &expired_timers {
            let entry_id = entry.id;
            
            // 预提取通知数据（避免后续的 clone）
            // Pre-extract notification data (avoid subsequent clones)
            notification_data.push((
                entry_id,
                entry.event.data.connection_id,
                entry.event.data.timeout_event.clone(),
                entry.event.callback_tx.clone(),
                entry.event.data.clone(),
            ));
            
            // 添加到 ID 缓冲区用于批量移除
            // Add to ID buffer for batch removal
            self.batch_processing_buffers.entry_ids_buffer.push(entry_id);
            
            // 收集连接映射信息（优化的单次查找和移除）
            // Collect connection mapping info (optimized single lookup and removal)
            if let Some(conn_id) = self.entry_to_connection.remove(&entry_id) {
                self.batch_processing_buffers.connection_updates
                    .entry(conn_id)
                    .or_insert_with(|| Vec::with_capacity(4))
                    .push(entry_id);
            }
        }

        // 批量清理连接定时器映射（优化的批量更新）
        // Batch cleanup connection timer mappings (optimized batch update)
        for (conn_id, expired_ids) in self.batch_processing_buffers.connection_updates.drain() {
            if let Some(entry_ids) = self.connection_timers.get_mut(&conn_id) {
                // 使用向量化移除提升性能
                // Use vectorized removal for better performance
                for expired_id in &expired_ids {
                    entry_ids.remove(expired_id);
                }
                if entry_ids.is_empty() {
                    self.connection_timers.remove(&conn_id);
                }
            }
        }

        // 智能策略选择：根据批量大小和系统负载选择最优处理方式
        // Smart strategy selection: choose optimal processing based on batch size and system load
        let should_use_parallel = batch_size >= self.parallel_threshold && 
            self.stats.total_timers >= 100; // 额外的负载检查

        if should_use_parallel {
            // 使用混合并行系统处理大批量
            // Use hybrid parallel system for large batches
            match self.parallel_system.process_timer_batch(expired_timers).await {
                Ok(parallel_result) => {
                    trace!(
                        processed_count = parallel_result.processed_count,
                        strategy = ?parallel_result.strategy_used,
                        duration_us = parallel_result.processing_duration.as_micros(),
                        "Parallel batch processing completed"
                    );
                    
                    // 更新统计信息
                    // Update statistics
                    self.stats.processed_timers += parallel_result.processed_count as u64;
                    
                    // 高性能并发事件通知
                    // High-performance concurrent event notifications
                    self.send_timer_notifications_optimized(notification_data).await;
                }
                Err(e) => {
                    warn!(error = ?e, "Parallel processing failed, falling back to sequential");
                    // 失败时使用预提取的数据进行顺序处理
                    // Fallback to sequential processing using pre-extracted data
                    self.process_timers_from_notification_data_optimized(notification_data).await;
                }
            }
        } else {
            // 小批量直接使用优化的顺序处理
            // Small batches use optimized sequential processing directly
            self.process_timers_from_notification_data_optimized(notification_data).await;
        }
    }

    /// 高性能发送定时器通知（优化版本）
    /// High-performance send timer notifications (optimized version)
    async fn send_timer_notifications_optimized(
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
        
        if notification_count == 0 {
            return;
        }
        
        // 基于批量大小选择最优并发策略
        // Choose optimal concurrency strategy based on batch size
        if notification_count <= 16 {
            // 小批量使用顺序发送减少并发开销
            // Small batches use sequential sending to reduce concurrency overhead
            self.send_notifications_sequential(notification_data).await;
        } else {
            // 大批量使用分块并发处理
            // Large batches use chunked concurrent processing
            self.send_notifications_chunked_concurrent(notification_data).await;
        }
    }
    
    /// 顺序发送通知（小批量优化）
    /// Sequential notification sending (small batch optimization)
    async fn send_notifications_sequential(
        &mut self,
        notification_data: Vec<(
            crate::timer::wheel::TimerEntryId,
            crate::timer::event::ConnectionId,
            E,
            tokio::sync::mpsc::Sender<crate::timer::event::TimerEventData<E>>,
            crate::timer::event::TimerEventData<E>,
        )>
    ) {
        let mut successful_count = 0;
        
        for (entry_id, connection_id, event_type, callback_tx, event_data) in notification_data {
            if callback_tx.send(event_data).await.is_ok() {
                successful_count += 1;
                trace!(entry_id, connection_id, event_type = ?event_type, "Timer event sent");
            } else {
                warn!(entry_id, connection_id, "Failed to send timer event");
            }
        }
        
        self.stats.processed_timers += successful_count;
    }
    
    /// 分块并发发送通知（大批量优化）
    /// Chunked concurrent notification sending (large batch optimization)
    async fn send_notifications_chunked_concurrent(
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
        let chunk_size = std::cmp::max(16, notification_count / 8); // 动态块大小
        
        let mut successful_count = 0;
        
        // 分块处理以控制并发数量和内存使用
        // Process in chunks to control concurrency and memory usage
        for chunk in notification_data.chunks(chunk_size) {
            let chunk_futures: Vec<_> = chunk.iter()
                .map(|(entry_id, connection_id, event_type, callback_tx, event_data)| {
                    let entry_id = *entry_id;
                    let connection_id = *connection_id;
                    let event_type = event_type.clone();
                    let callback_tx = callback_tx.clone();
                    let event_data = event_data.clone();
                    
                    async move {
                        if callback_tx.send(event_data).await.is_ok() {
                            trace!(entry_id, connection_id, event_type = ?event_type, "Timer event sent");
                            1
                        } else {
                            warn!(entry_id, connection_id, "Failed to send timer event");
                            0
                        }
                    }
                })
                .collect();
            
            let chunk_results = futures::future::join_all(chunk_futures).await;
            successful_count += chunk_results.iter().sum::<usize>();
        }
        
        self.stats.processed_timers += successful_count as u64;
        
        if notification_count > 100 {
            debug!(
                total_notifications = notification_count,
                successful_notifications = successful_count,
                "Chunked concurrent timer notifications completed"
            );
        }
    }

    /// 从预提取的数据发送定时器通知（备用方法）
    /// Send timer notifications from pre-extracted data (backup method)
    #[allow(dead_code)]
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

    /// 高性能优化的数据处理（新版本）
    /// High-performance optimized data processing (new version)
    async fn process_timers_from_notification_data_optimized(
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
        
        if processed_count == 0 {
            return;
        }
        
        // 直接调用优化的通知发送方法
        // Directly call optimized notification sending method
        self.send_timer_notifications_optimized(notification_data).await;
        
        if processed_count > 1 {
            trace!(processed_count, "Optimized batch processed expired timers");
        }
    }

    /// 从预提取的数据进行顺序处理（fallback，备用方法）
    /// Sequential processing from pre-extracted data (fallback, backup method)
    #[allow(dead_code)]
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

    /// 传统顺序处理定时器（作为fallback，备用方法）
    /// Traditional sequential timer processing (as fallback, backup method)
    #[allow(dead_code)]
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

    /// 高性能批量注册定时器（优化版本）
    /// High-performance batch register timers (optimized version)
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
        
        // 使用预分配的缓冲区避免重复分配
        // Use pre-allocated buffers to avoid repeated allocations
        let mut timers_for_wheel = Vec::with_capacity(total_count);
        let mut registration_data = Vec::with_capacity(total_count);
        
        // 预分配连接映射更新数据
        // Pre-allocate connection mapping update data
        self.batch_processing_buffers.connection_updates.clear();
        self.batch_processing_buffers.connection_updates.reserve(total_count / 4); // 估计连接数
        
        // 单次遍历构建所有需要的数据结构
        // Single-pass construction of all required data structures
        let mut pool_requests = Vec::with_capacity(total_count);
        let mut callback_txs = Vec::with_capacity(total_count);
        
        for registration in &registrations {
            pool_requests.push((registration.connection_id, registration.timeout_event.clone()));
            callback_txs.push(registration.callback_tx.clone());
        }
        
        // 批量创建定时器事件，智能策略选择，高性能优化
        // Batch create timer events, smart strategy selection, high performance optimization
        let timer_events = TimerEvent::batch_from_factory(
            &self.event_factory,
            start_event_id,
            &pool_requests,
            &callback_txs,
        );
        
        // 单次遍历构建时间轮数据和连接映射预处理
        // Single-pass construction of timing wheel data and connection mapping preprocessing
        for (timer_event, registration) in timer_events.into_iter().zip(registrations.iter()) {
            timers_for_wheel.push((registration.delay, timer_event));
            registration_data.push((registration.connection_id, registration.delay));
            
            // 预处理连接映射更新
            // Preprocess connection mapping updates
            self.batch_processing_buffers.connection_updates
                .entry(registration.connection_id)
                .or_insert_with(|| Vec::with_capacity(4));
        }
        
        // 使用时间轮的批量API一次性添加所有定时器
        // Use timing wheel's batch API to add all timers at once
        let entry_ids = self.timing_wheel.batch_add_timers(timers_for_wheel);
        
        // 高效批量更新映射关系
        // Efficiently batch update mapping relationships
        for (entry_id, (connection_id, _delay)) in entry_ids.iter().zip(registration_data.iter()) {
            // 使用预先分配的连接映射结构
            // Use pre-allocated connection mapping structure
            if let Some(connection_entries) = self.batch_processing_buffers.connection_updates.get_mut(connection_id) {
                connection_entries.push(*entry_id);
            }
            
            self.entry_to_connection.insert(*entry_id, *connection_id);
            
            // 创建定时器句柄
            // Create timer handle
            let handle = TimerHandle::new(*entry_id, self.command_tx.clone());
            result.successes.push(handle);
        }
        
        // 批量更新连接定时器映射
        // Batch update connection timer mappings
        for (connection_id, entry_ids) in self.batch_processing_buffers.connection_updates.drain() {
            let connection_timers = self.connection_timers.entry(connection_id).or_default();
            for entry_id in entry_ids {
                connection_timers.insert(entry_id);
            }
        }
        
        if total_count > 10 {
            trace!(
                batch_size = total_count,
                "High-performance batch registered timers successfully"
            );
        }
        
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timer::event::traits::EventDataTrait;
    use crate::timer::task::types::{TimerRegistration, BatchTimerRegistration, BatchTimerCancellation};
    use crate::timer::wheel::TimerEntry;
    use std::time::Duration;
    use tokio::time::Instant;

    /// 测试用的超时事件类型
    /// Test timeout event type
    #[derive(Debug, Clone, Default, PartialEq)]
    struct TestTimeoutEvent {
        pub event_type: String,
        pub payload: Vec<u8>,
    }

    /// 创建测试用的定时器条目
    /// Create test timer entries
    #[allow(unused)]
    fn create_test_timer_entries<E: EventDataTrait>(count: usize) -> Vec<TimerEntry<E>> {
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let mut entries = Vec::with_capacity(count);
        let factory = crate::timer::event::traits::EventFactory::<E>::new();
        
        for i in 0..count {
            let timer_event = TimerEvent::from_factory(
                &factory,
                i as u64, // id
                (i % 10000) as u32, // connection_id
                E::default(),
                tx.clone(),
            );
            
            entries.push(TimerEntry {
                id: i as u64,
                expiry_time: tokio::time::Instant::now() + tokio::time::Duration::from_millis(1000),
                event: timer_event,
            });
        }
        
        entries
    }

    /// 创建测试用的定时器注册
    /// Create test timer registrations
    fn create_test_timer_registrations<E: EventDataTrait>(count: usize) -> Vec<TimerRegistration<E>> {
        let (tx, _rx) = tokio::sync::mpsc::channel(count);
        let mut registrations = Vec::with_capacity(count);
        
        for i in 0..count {
            registrations.push(TimerRegistration {
                connection_id: (i % 1000) as u32,
                delay: Duration::from_millis(100 + (i % 1000) as u64),
                timeout_event: E::default(),
                callback_tx: tx.clone(),
            });
        }
        
        registrations
    }

    #[tokio::test]
    async fn test_hybrid_timer_basic_functionality() {
        println!("\n🏷️  基本功能测试");
        println!("================");
        
        let (task, command_tx) = HybridTimerTask::<TestTimeoutEvent>::new_default();
        let handle = HybridTimerTaskHandle::new(command_tx);
        
        // 启动任务在后台
        let task_handle = tokio::spawn(async move {
            task.run().await;
        });
        
        // 测试单个定时器注册
        let (callback_tx, mut callback_rx) = tokio::sync::mpsc::channel(1);
        let registration = TimerRegistration {
            connection_id: 1,
            delay: Duration::from_millis(50),
            timeout_event: TestTimeoutEvent {
                event_type: "test".to_string(),
                payload: vec![1, 2, 3],
            },
            callback_tx,
        };
        
        let _timer_handle = handle.register_timer(registration).await.expect("Failed to register timer");
        
        // 等待定时器触发
        tokio::time::timeout(Duration::from_secs(1), callback_rx.recv())
            .await
            .expect("Timer should fire within timeout")
            .expect("Should receive timer event");
        
        // 关闭任务
        handle.shutdown().await.expect("Failed to shutdown");
        task_handle.await.expect("Task should complete");
        
        println!("✅ 基本功能测试通过");
    }

    #[tokio::test]
    async fn test_hybrid_timer_batch_operations() {
        println!("\n📦 批量操作测试");
        println!("================");
        
        let (task, command_tx) = HybridTimerTask::<TestTimeoutEvent>::new_default();
        let handle = HybridTimerTaskHandle::new(command_tx);
        
        // 启动任务在后台
        let task_handle = tokio::spawn(async move {
            task.run().await;
        });
        
        // 测试批量注册
        let registrations = create_test_timer_registrations(100);
        let batch_registration = BatchTimerRegistration { registrations };
        
        let start_time = Instant::now();
        let batch_result = handle.batch_register_timers(batch_registration).await.expect("Batch registration failed");
        let registration_time = start_time.elapsed();
        
        println!("批量注册100个定时器耗时: {:.3}ms", registration_time.as_secs_f64() * 1000.0);
        println!("成功注册: {}", batch_result.success_count());
        println!("失败注册: {}", batch_result.failure_count());
        
        assert_eq!(batch_result.success_count(), 100);
        
        // 测试批量取消
        let entry_ids: Vec<_> = batch_result.successes.iter().map(|h| h.entry_id).collect();
        let batch_cancellation = BatchTimerCancellation { entry_ids };
        
        let start_time = Instant::now();
        let cancel_result = handle.batch_cancel_timers(batch_cancellation).await.expect("Batch cancellation failed");
        let cancellation_time = start_time.elapsed();
        
        println!("批量取消100个定时器耗时: {:.3}ms", cancellation_time.as_secs_f64() * 1000.0);
        println!("成功取消: {}", cancel_result.success_count());
        
        // 关闭任务
        handle.shutdown().await.expect("Failed to shutdown");
        task_handle.await.expect("Task should complete");
        
        println!("✅ 批量操作测试通过");
    }

    #[tokio::test]
    async fn test_hybrid_timer_connection_management() {
        println!("\n🔗 连接管理测试");
        println!("================");
        
        let (task, command_tx) = HybridTimerTask::<TestTimeoutEvent>::new_default();
        let handle = HybridTimerTaskHandle::new(command_tx);
        
        // 启动任务在后台
        let task_handle = tokio::spawn(async move {
            task.run().await;
        });
        
        // 为多个连接注册定时器
        let mut handles_by_connection = std::collections::HashMap::new();
        
        for conn_id in 1..=10 {
            let mut connection_handles = Vec::new();
            
            for i in 0..10 {
                let (callback_tx, _callback_rx) = tokio::sync::mpsc::channel(1);
                let registration = TimerRegistration {
                    connection_id: conn_id,
                    delay: Duration::from_secs(10), // 长时间，避免自动过期
                    timeout_event: TestTimeoutEvent {
                        event_type: format!("conn_{}_timer_{}", conn_id, i),
                        payload: vec![conn_id as u8, i as u8],
                    },
                    callback_tx,
                };
                
                let timer_handle = handle.register_timer(registration).await.expect("Failed to register timer");
                connection_handles.push(timer_handle);
            }
            
            handles_by_connection.insert(conn_id, connection_handles);
        }
        
        // 测试获取统计信息
        let stats = handle.get_stats().await.expect("Failed to get stats");
        println!("总定时器数: {}", stats.total_timers);
        println!("活跃连接数: {}", stats.active_connections);
        assert_eq!(stats.active_connections, 10);
        
        // 测试清理特定连接的定时器
        let cleared_count = handle.clear_connection_timers(5).await.expect("Failed to clear connection timers");
        println!("连接5清理的定时器数: {}", cleared_count);
        assert_eq!(cleared_count, 10);
        
        // 验证统计信息更新
        let stats_after_clear = handle.get_stats().await.expect("Failed to get updated stats");
        println!("清理后活跃连接数: {}", stats_after_clear.active_connections);
        assert_eq!(stats_after_clear.active_connections, 9);
        
        // 关闭任务
        handle.shutdown().await.expect("Failed to shutdown");
        task_handle.await.expect("Task should complete");
        
        println!("✅ 连接管理测试通过");
    }

    #[tokio::test]
    #[ignore] // 性能测试，需要单独运行: cargo test test_comprehensive_hybrid_benchmark -- --ignored
    async fn test_comprehensive_hybrid_benchmark() {
        // 等待系统稳定
        tokio::time::sleep(Duration::from_millis(100)).await;
        
        println!("\n🏆 混合并行定时器系统综合性能基准测试");
        println!("=========================================");
        println!("对比不同批量大小下的混合处理系统性能");
        println!();

        let (task, command_tx) = HybridTimerTask::<TestTimeoutEvent>::new(2048, 32);
        let handle = HybridTimerTaskHandle::new(command_tx.clone());
        
        // 启动任务在后台
        let task_handle = tokio::spawn(async move {
            task.run().await;
        });
        
        // 测试场景：不同批量大小下的性能对比
        let benchmark_cases = vec![
            (1, "单个定时器"),
            (8, "超小批量"),
            (16, "小批量-1"),
            (32, "小批量-2 (阈值)"),
            (64, "中批量-1"),
            (128, "中批量-2"),
            (256, "大批量-1"),
            (512, "大批量-2"),
            (1024, "超大批量-1"),
            (2048, "超大批量-2"),
            (4096, "巨大批量"),
        ];

        for (batch_size, scenario) in benchmark_cases {
            println!("🏁 {} ({} 个定时器):", scenario, batch_size);
            
            // 预热阶段
            for _ in 0..5 {
                let warmup_registrations = create_test_timer_registrations(batch_size);
                let warmup_batch = BatchTimerRegistration { registrations: warmup_registrations };
                let _ = handle.batch_register_timers(warmup_batch).await;
            }
            
            // 基准测试
            let benchmark_iterations = if batch_size <= 32 { 1000 } else if batch_size <= 512 { 500 } else { 100 };
            let mut registration_durations = Vec::with_capacity(benchmark_iterations);
            let mut cancellation_durations = Vec::with_capacity(benchmark_iterations);
            
            let benchmark_start = Instant::now();
            
            for _ in 0..benchmark_iterations {
                // 测试批量注册性能
                let registrations = create_test_timer_registrations(batch_size);
                let batch_registration = BatchTimerRegistration { registrations };
                
                let reg_start = Instant::now();
                let batch_result = handle.batch_register_timers(batch_registration).await
                    .expect("Batch registration failed");
                registration_durations.push(reg_start.elapsed());
                
                // 测试批量取消性能
                let entry_ids: Vec<_> = batch_result.successes.iter().map(|h| h.entry_id).collect();
                let batch_cancellation = BatchTimerCancellation { entry_ids };
                
                let cancel_start = Instant::now();
                let _ = handle.batch_cancel_timers(batch_cancellation).await
                    .expect("Batch cancellation failed");
                cancellation_durations.push(cancel_start.elapsed());
            }
            
            let total_benchmark_time = benchmark_start.elapsed();
            
            // 计算统计数据
            let avg_reg_duration = registration_durations.iter().sum::<Duration>() / registration_durations.len() as u32;
            let min_reg_duration = *registration_durations.iter().min().unwrap();
            let max_reg_duration = *registration_durations.iter().max().unwrap();
            
            let avg_cancel_duration = cancellation_durations.iter().sum::<Duration>() / cancellation_durations.len() as u32;
            let min_cancel_duration = *cancellation_durations.iter().min().unwrap();
            let max_cancel_duration = *cancellation_durations.iter().max().unwrap();
            
            let reg_nanos_per_op = if batch_size > 0 {
                avg_reg_duration.as_nanos() / batch_size as u128
            } else {
                0
            };
            
            let cancel_nanos_per_op = if batch_size > 0 {
                avg_cancel_duration.as_nanos() / batch_size as u128
            } else {
                0
            };
            
            let total_ops = batch_size as f64 * benchmark_iterations as f64 * 2.0; // 注册 + 取消
            let throughput = if total_benchmark_time.as_secs_f64() > 0.0 {
                total_ops / total_benchmark_time.as_secs_f64()
            } else {
                0.0
            };
            
            println!("  📈 注册性能指标:");
            println!("    平均延迟: {:.3}µs", avg_reg_duration.as_secs_f64() * 1_000_000.0);
            println!("    最小延迟: {:.3}µs", min_reg_duration.as_secs_f64() * 1_000_000.0);
            println!("    最大延迟: {:.3}µs", max_reg_duration.as_secs_f64() * 1_000_000.0);
            println!("    每操作: {} 纳秒", reg_nanos_per_op);
            println!();
            
            println!("  📉 取消性能指标:");
            println!("    平均延迟: {:.3}µs", avg_cancel_duration.as_secs_f64() * 1_000_000.0);
            println!("    最小延迟: {:.3}µs", min_cancel_duration.as_secs_f64() * 1_000_000.0);
            println!("    最大延迟: {:.3}µs", max_cancel_duration.as_secs_f64() * 1_000_000.0);
            println!("    每操作: {} 纳秒", cancel_nanos_per_op);
            println!();
            
            println!("  🎯 整体性能:");
            println!("    总吞吐量: {:.0} ops/sec", throughput);
            println!("    测试迭代: {} 次", benchmark_iterations);
            println!();
            
            // 性能等级评估
            let overall_nanos_per_op = (reg_nanos_per_op + cancel_nanos_per_op) / 2;
            let performance_grade = match overall_nanos_per_op {
                0..=100 => "S级 (卓越)",
                101..=200 => "A级 (优秀)",
                201..=400 => "B级 (良好)",
                401..=800 => "C级 (一般)",
                _ => "D级 (需改进)",
            };
            
            println!("  🏆 性能等级: {}", performance_grade);
            
            // 预期模式提示
            if batch_size >= 32 {
                println!("  🔄 预期使用: 混合并行处理");
            } else {
                println!("  ⚡ 预期使用: 顺序处理");
            }
            
            println!();
        }

        // 获取最终统计信息
        let final_stats = handle.get_stats().await.expect("Failed to get final stats");
        println!("🎯 最终系统统计:");
        println!("  总处理定时器: {}", final_stats.processed_timers);
        println!("  取消定时器: {}", final_stats.cancelled_timers);
        println!("  活跃连接: {}", final_stats.active_connections);
        println!("  总定时器: {}", final_stats.total_timers);
        
        // 关闭任务
        handle.shutdown().await.expect("Failed to shutdown");
        task_handle.await.expect("Task should complete");
        
        println!("\n✅ 综合性能基准测试完成");
    }

    #[tokio::test]
    #[ignore] // 压力测试，需要单独运行
    async fn test_hybrid_timer_stress_test() {
        println!("\n💪 混合定时器系统压力测试");
        println!("=========================");
        
        let (task, command_tx) = HybridTimerTask::<TestTimeoutEvent>::new(4096, 64);
        let handle = HybridTimerTaskHandle::new(command_tx);
        
        // 启动任务在后台
        let task_handle = tokio::spawn(async move {
            task.run().await;
        });
        
        // 并发注册大量定时器
        let concurrent_tasks = 10;
        let timers_per_task = 1000;
        let total_timers = concurrent_tasks * timers_per_task;
        
        println!("启动 {} 个并发任务，每个任务处理 {} 个定时器", concurrent_tasks, timers_per_task);
        println!("总定时器数: {}", total_timers);
        
        let stress_start = Instant::now();
        let mut handles = Vec::new();
        
        for task_id in 0..concurrent_tasks {
            let handle_clone = handle.clone();
            let task_handle = tokio::spawn(async move {
                let mut task_timers = Vec::new();
                
                // 批量注册
                for batch_start in (0..timers_per_task).step_by(100) {
                    let batch_end = std::cmp::min(batch_start + 100, timers_per_task);
                    let batch_size = batch_end - batch_start;
                    
                    let registrations = create_test_timer_registrations(batch_size);
                    let batch_registration = BatchTimerRegistration { registrations };
                    
                    match handle_clone.batch_register_timers(batch_registration).await {
                        Ok(batch_result) => {
                            task_timers.extend(batch_result.successes);
                        }
                        Err(e) => {
                            eprintln!("Task {} batch registration failed: {:?}", task_id, e);
                            return 0;
                        }
                    }
                }
                
                // 批量取消一半定时器
                let half_count = task_timers.len() / 2;
                if half_count > 0 {
                    let entry_ids: Vec<_> = task_timers[..half_count].iter().map(|h| h.entry_id).collect();
                    let batch_cancellation = BatchTimerCancellation { entry_ids };
                    
                    if let Err(e) = handle_clone.batch_cancel_timers(batch_cancellation).await {
                        eprintln!("Task {} batch cancellation failed: {:?}", task_id, e);
                    }
                }
                
                task_timers.len()
            });
            
            handles.push(task_handle);
        }
        
        // 等待所有任务完成
        let mut total_processed = 0;
        for handle in handles {
            match handle.await {
                Ok(processed) => total_processed += processed,
                Err(e) => eprintln!("Task failed: {:?}", e),
            }
        }
        
        let stress_duration = stress_start.elapsed();
        
        println!("压力测试完成！");
        println!("总处理时间: {:.3}s", stress_duration.as_secs_f64());
        println!("总处理定时器: {}", total_processed);
        println!("平均吞吐量: {:.0} timers/sec", total_processed as f64 / stress_duration.as_secs_f64());
        
        // 获取最终统计
        let final_stats = handle.get_stats().await.expect("Failed to get final stats");
        println!("系统最终状态:");
        println!("  活跃定时器: {}", final_stats.total_timers);
        println!("  活跃连接: {}", final_stats.active_connections);
        println!("  已处理定时器: {}", final_stats.processed_timers);
        println!("  已取消定时器: {}", final_stats.cancelled_timers);
        
        // 关闭任务
        handle.shutdown().await.expect("Failed to shutdown");
        task_handle.await.expect("Task should complete");
        
        println!("✅ 压力测试通过");
    }

    #[tokio::test]
    async fn test_hybrid_timer_memory_efficiency() {
        println!("\n🧠 内存效率测试");
        println!("================");
        
        let (task, command_tx) = HybridTimerTask::<TestTimeoutEvent>::new_default();
        let handle = HybridTimerTaskHandle::new(command_tx);
        
        // 启动任务在后台
        let task_handle = tokio::spawn(async move {
            task.run().await;
        });
        
        // 测试大批量注册时的内存使用
        let large_batch_size = 10000;
        println!("测试大批量 ({}) 定时器注册的内存效率", large_batch_size);
        
        let registrations = create_test_timer_registrations(large_batch_size);
        let batch_registration = BatchTimerRegistration { registrations };
        
        let memory_start = std::alloc::System.used_memory();
        let reg_start = Instant::now();
        
        let batch_result = handle.batch_register_timers(batch_registration).await
            .expect("Large batch registration failed");
        
        let reg_duration = reg_start.elapsed();
        let memory_after_reg = std::alloc::System.used_memory();
        
        println!("注册完成耗时: {:.3}ms", reg_duration.as_secs_f64() * 1000.0);
        println!("内存增长: {} bytes", memory_after_reg.saturating_sub(memory_start));
        println!("成功注册: {}", batch_result.success_count());
        
        // 测试批量取消
        let entry_ids: Vec<_> = batch_result.successes.iter().map(|h| h.entry_id).collect();
        let batch_cancellation = BatchTimerCancellation { entry_ids };
        
        let cancel_start = Instant::now();
        let cancel_result = handle.batch_cancel_timers(batch_cancellation).await
            .expect("Large batch cancellation failed");
        let cancel_duration = cancel_start.elapsed();
        let memory_after_cancel = std::alloc::System.used_memory();
        
        println!("取消完成耗时: {:.3}ms", cancel_duration.as_secs_f64() * 1000.0);
        println!("取消后内存: {} bytes (相对于注册后: {})", 
                 memory_after_cancel, 
                 memory_after_cancel as i64 - memory_after_reg as i64);
        println!("成功取消: {}", cancel_result.success_count());
        
        // 关闭任务
        handle.shutdown().await.expect("Failed to shutdown");
        task_handle.await.expect("Task should complete");
        
        println!("✅ 内存效率测试通过");
    }

    // TODO: 添加一个自定义内存分配器追踪器来更准确地测量内存使用
    trait MemoryUsage {
        fn used_memory(&self) -> usize;
    }
    
    impl MemoryUsage for std::alloc::System {
        fn used_memory(&self) -> usize {
            // 这是一个占位符实现，在实际环境中应该使用真实的内存追踪
            // This is a placeholder implementation, should use real memory tracking in production
            0
        }
    }

    #[tokio::test]
    async fn test_hybrid_timer_threshold_behavior() {
        println!("\n🎯 并行阈值行为测试");
        println!("===================");
        
        // 测试不同并行阈值的行为
        let threshold_tests = vec![
            (16, "低阈值"),
            (32, "标准阈值"),
            (64, "高阈值"),
            (128, "超高阈值"),
        ];
        
        for (threshold, name) in threshold_tests {
            println!("测试 {} (阈值: {})", name, threshold);
            
            let (task, command_tx) = HybridTimerTask::<TestTimeoutEvent>::new(1024, threshold);
            let handle = HybridTimerTaskHandle::new(command_tx);
            
            // 启动任务
            let task_handle = tokio::spawn(async move {
                task.run().await;
            });
            
            // 测试阈值前后的性能差异
            let test_sizes = vec![threshold / 2, threshold - 1, threshold, threshold + 1, threshold * 2];
            
            for batch_size in test_sizes {
                let registrations = create_test_timer_registrations(batch_size);
                let batch_registration = BatchTimerRegistration { registrations };
                
                let start = Instant::now();
                let result = handle.batch_register_timers(batch_registration).await
                    .expect("Registration failed");
                let duration = start.elapsed();
                
                let mode = if batch_size >= threshold { "并行" } else { "顺序" };
                println!("  批量大小 {}: {:.3}µs ({}模式)", 
                         batch_size, 
                         duration.as_secs_f64() * 1_000_000.0,
                         mode);
                
                // 清理
                let entry_ids: Vec<_> = result.successes.iter().map(|h| h.entry_id).collect();
                let batch_cancellation = BatchTimerCancellation { entry_ids };
                let _ = handle.batch_cancel_timers(batch_cancellation).await;
            }
            
            // 关闭任务
            handle.shutdown().await.expect("Failed to shutdown");
            task_handle.await.expect("Task should complete");
            
            println!();
        }
        
        println!("✅ 并行阈值行为测试通过");
    }
}