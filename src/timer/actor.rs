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
    task::{TimerRegistration, BatchTimerRegistration, BatchTimerCancellation},
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

/// TimerActor内部定时器ID，用于唯一标识每个定时器
/// Internal timer ID for TimerActor, used to uniquely identify each timer
pub type ActorTimerId = u64;

/// 定时器Actor命令类型
/// Timer Actor command types
#[derive(Debug)]
pub enum TimerActorCommand {
    /// 注册单个定时器（返回内部ID）
    /// Register a single timer (returns internal ID)
    RegisterTimer {
        registration: TimerRegistration<TimeoutEvent>,
        response_tx: oneshot::Sender<Result<ActorTimerId, String>>,
    },
    
    /// 根据内部ID取消特定定时器
    /// Cancel specific timer by internal ID
    CancelTimerById {
        timer_id: ActorTimerId,
        response_tx: oneshot::Sender<bool>,
    },
    
    /// 取消连接的所有特定类型定时器（兼容旧接口）
    /// Cancel all timers of specific type for connection (backward compatibility)
    CancelTimer {
        connection_id: ConnectionId,
        timeout_event: TimeoutEvent,
        response_tx: oneshot::Sender<usize>, // 返回取消的数量
    },
    
    /// 取消连接的所有定时器
    /// Cancel all timers for connection
    CancelAllTimers {
        connection_id: ConnectionId,
        response_tx: oneshot::Sender<usize>,
    },
    
    /// 批量注册定时器（大批量，直接处理，不经过缓冲区）
    /// Batch register timers (large batch, direct processing, bypass buffer)
    BatchRegisterTimers {
        registrations: Vec<TimerRegistration<TimeoutEvent>>,
        response_tx: oneshot::Sender<Result<Vec<ActorTimerId>, String>>,
    },
    
    /// 批量取消定时器（通过内部ID）
    /// Batch cancel timers (by internal IDs)
    BatchCancelTimersByIds {
        timer_ids: Vec<ActorTimerId>,
        response_tx: oneshot::Sender<usize>,
    },
    
    /// 批量取消定时器（兼容旧接口）
    /// Batch cancel timers (backward compatibility)
    BatchCancelTimers {
        entries: Vec<(ConnectionId, TimeoutEvent)>,
        response_tx: oneshot::Sender<usize>,
    },
    
    /// 注册路径验证定时器
    /// Register path validation timer
    RegisterPathValidationTimer {
        connection_id: ConnectionId,
        path_id: u32, // 路径标识符
        delay: Duration,
        response_tx: oneshot::Sender<Result<ActorTimerId, String>>,
    },
    
    /// 注册重传定时器
    /// Register retransmission timer
    RegisterRetransmissionTimer {
        connection_id: ConnectionId,
        packet_id: u64, // 数据包标识符
        delay: Duration,
        response_tx: oneshot::Sender<Result<ActorTimerId, String>>,
    },
    
    /// 注册基于数据包的重传定时器（新版本）
    /// Register packet-based retransmission timer (new version)
    RegisterPacketRetransmissionTimer {
        connection_id: ConnectionId,
        sequence_number: u32, // 数据包序列号
        delay: Duration,
        response_tx: oneshot::Sender<Result<ActorTimerId, String>>,
    },
    
    /// 注册FIN处理定时器
    /// Register FIN processing timer
    RegisterFinProcessingTimer {
        connection_id: ConnectionId,
        delay: Duration,
        response_tx: oneshot::Sender<Result<ActorTimerId, String>>,
    },
    
    /// 强制刷新所有缓冲区
    /// Force flush all buffers
    FlushBuffers,
    
    /// 获取Actor统计信息
    /// Get Actor statistics
    GetStats {
        response_tx: oneshot::Sender<TimerActorStats>,
    },
    
    /// 查询定时器信息
    /// Query timer information
    QueryTimer {
        timer_id: ActorTimerId,
        response_tx: oneshot::Sender<Option<TimerInfo>>,
    },
    
    /// 关闭Actor
    /// Shutdown Actor
    Shutdown,
}

/// 定时器信息
/// Timer information
#[derive(Debug, Clone)]
pub struct TimerInfo {
    /// 内部定时器ID
    /// Internal timer ID
    pub timer_id: ActorTimerId,
    /// 连接ID
    /// Connection ID
    pub connection_id: ConnectionId,
    /// 超时事件类型
    /// Timeout event type
    pub timeout_event: TimeoutEvent,
    /// 延迟时间
    /// Delay duration
    pub delay: Duration,
    /// 创建时间
    /// Creation time
    pub created_at: Instant,
    /// 是否在缓冲区中（未提交）
    /// Whether in buffer (uncommitted)
    pub in_buffer: bool,
    /// 全局定时器条目ID（如果已提交）
    /// Global timer entry ID (if committed)
    pub entry_id: Option<TimerEntryId>,
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
    /// 活跃定时器总数
    /// Total active timers
    pub active_timer_count: usize,
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
    /// 下一个分配的定时器ID
    /// Next timer ID to allocate
    pub next_timer_id: ActorTimerId,
}

impl Default for TimerActorStats {
    fn default() -> Self {
        Self {
            register_buffer_size: 0,
            cancel_buffer_size: 0,
            active_timer_count: 0,
            total_register_requests: 0,
            total_cancel_requests: 0,
            auto_flush_count: 0,
            optimized_operations: 0,
            next_timer_id: 1,
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

/// 内部定时器信息（缓冲区中的）
/// Internal timer information (in buffer)
#[derive(Debug, Clone)]
struct BufferedTimer {
    /// 内部定时器ID
    /// Internal timer ID
    #[allow(dead_code)] // 保留以备将来使用和调试 // Kept for future use and debugging
    timer_id: ActorTimerId,
    /// 定时器注册信息
    /// Timer registration
    registration: TimerRegistration<TimeoutEvent>,
    /// 创建时间
    /// Creation time
    created_at: Instant,
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
    
    /// 下一个分配的内部定时器ID
    /// Next internal timer ID to allocate
    next_timer_id: ActorTimerId,
    
    /// 注册缓冲区 - 使用内部ID索引
    /// Register buffer - indexed by internal ID
    register_buffer: HashMap<ActorTimerId, BufferedTimer>,
    
    /// 取消缓冲区 - 等待取消的内部ID
    /// Cancel buffer - internal IDs waiting to be cancelled
    cancel_buffer: HashSet<ActorTimerId>,
    
    /// 活跃定时器映射 - 内部ID到全局条目ID的映射
    /// Active timer mapping - internal ID to global entry ID
    active_timers: HashMap<ActorTimerId, TimerEntryId>,
    
    /// 反向映射 - 全局条目ID到内部ID的映射
    /// Reverse mapping - global entry ID to internal ID
    entry_to_timer_id: HashMap<TimerEntryId, ActorTimerId>,
    
    /// 连接定时器索引 - 按连接ID索引的定时器列表
    /// Connection timer index - timer lists indexed by connection ID
    connection_timers: HashMap<ConnectionId, HashSet<ActorTimerId>>,
    
    /// 类型定时器索引 - 按类型索引的定时器列表（用于兼容性API）
    /// Type timer index - timer lists indexed by type (for compatibility API)
    type_timers: HashMap<(ConnectionId, TimeoutEvent), HashSet<ActorTimerId>>,
    
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
            next_timer_id: 1,
            register_buffer: HashMap::new(),
            cancel_buffer: HashSet::new(),
            active_timers: HashMap::new(),
            entry_to_timer_id: HashMap::new(),
            connection_timers: HashMap::new(),
            type_timers: HashMap::new(),
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
            
            TimerActorCommand::CancelTimerById { timer_id, response_tx } => {
                let result = self.handle_cancel_timer_by_id(timer_id).await;
                let _ = response_tx.send(result);
            }
            
            TimerActorCommand::CancelTimer { connection_id, timeout_event, response_tx } => {
                let result = self.handle_cancel_timer_by_type(connection_id, timeout_event).await;
                let _ = response_tx.send(result);
            }
            
            TimerActorCommand::CancelAllTimers { connection_id, response_tx } => {
                let result = self.handle_cancel_all_timers(connection_id).await;
                let _ = response_tx.send(result);
            }
            
            TimerActorCommand::BatchRegisterTimers { registrations, response_tx } => {
                let result = self.handle_batch_register_timers(registrations).await;
                let _ = response_tx.send(result);
            }
            
            TimerActorCommand::BatchCancelTimersByIds { timer_ids, response_tx } => {
                let result = self.handle_batch_cancel_timers_by_ids(timer_ids).await;
                let _ = response_tx.send(result);
            }
            
            TimerActorCommand::BatchCancelTimers { entries, response_tx } => {
                let result = self.handle_batch_cancel_timers_by_type(entries).await;
                let _ = response_tx.send(result);
            }
            
            TimerActorCommand::RegisterPathValidationTimer { connection_id, path_id, delay, response_tx } => {
                let result = self.handle_register_path_validation_timer(connection_id, path_id, delay).await;
                let _ = response_tx.send(result);
            }
            
            TimerActorCommand::RegisterRetransmissionTimer { connection_id, packet_id, delay, response_tx } => {
                let result = self.handle_register_retransmission_timer(connection_id, packet_id, delay).await;
                let _ = response_tx.send(result);
            }
            
            TimerActorCommand::RegisterPacketRetransmissionTimer { connection_id, sequence_number, delay, response_tx } => {
                let result = self.handle_register_packet_retransmission_timer(connection_id, sequence_number, delay).await;
                let _ = response_tx.send(result);
            }
            
            TimerActorCommand::RegisterFinProcessingTimer { connection_id, delay, response_tx } => {
                let result = self.handle_register_fin_processing_timer(connection_id, delay).await;
                let _ = response_tx.send(result);
            }
            
            TimerActorCommand::FlushBuffers => {
                self.flush_buffers().await;
            }
            
            TimerActorCommand::GetStats { response_tx } => {
                let stats = self.get_current_stats();
                let _ = response_tx.send(stats);
            }
            
            TimerActorCommand::QueryTimer { timer_id, response_tx } => {
                let info = self.query_timer_info(timer_id);
                let _ = response_tx.send(info);
            }
            
            TimerActorCommand::Shutdown => {
                return Err(true); // 请求退出主循环
            }
        }
        
        Ok(())
    }
    
    /// 分配新的定时器ID
    /// Allocate new timer ID
    fn allocate_timer_id(&mut self) -> ActorTimerId {
        let id = self.next_timer_id;
        self.next_timer_id += 1;
        self.stats.next_timer_id = self.next_timer_id;
        id
    }
    
    /// 处理单个定时器注册
    /// Handle single timer registration
    async fn handle_register_timer(
        &mut self,
        registration: TimerRegistration<TimeoutEvent>,
    ) -> Result<ActorTimerId, String> {
        let timer_id = self.allocate_timer_id();
        
        // 检查是否在取消缓冲区中，如果是则直接抵消
        // Check if in cancel buffer, if so, directly cancel out
        if self.cancel_buffer.remove(&timer_id) {
            self.stats.optimized_operations += 1;
            trace!(timer_id = timer_id, "Timer registration cancelled out by pending cancellation");
            return Err("Timer registration cancelled by pending cancellation".to_string());
        }
        
        // 创建缓冲定时器
        // Create buffered timer
        let buffered_timer = BufferedTimer {
            timer_id,
            registration: registration.clone(),
            created_at: Instant::now(),
        };
        
        // 添加到注册缓冲区
        // Add to register buffer
        self.register_buffer.insert(timer_id, buffered_timer);
        
        // 更新索引
        // Update indices
        self.connection_timers
            .entry(registration.connection_id)
            .or_default()
            .insert(timer_id);
            
        let type_key = (registration.connection_id, registration.timeout_event);
        self.type_timers
            .entry(type_key)
            .or_default()
            .insert(timer_id);
        
        self.stats.total_register_requests += 1;
        
        trace!(
            timer_id = timer_id,
            connection_id = registration.connection_id,
            timeout_event = ?registration.timeout_event,
            buffer_size = self.register_buffer.len(),
            "Added timer to register buffer"
        );
        
        // 检查是否需要自动刷新
        // Check if auto flush is needed
        if self.register_buffer.len() >= self.config.batch_threshold {
            self.flush_buffers().await;
        }
        
        Ok(timer_id)
    }
    
    /// 根据内部ID取消特定定时器
    /// Cancel specific timer by internal ID
    async fn handle_cancel_timer_by_id(&mut self, timer_id: ActorTimerId) -> bool {
        // 优先检查注册缓冲区，如果找到就直接移除
        // First check register buffer, remove directly if found
        if let Some(buffered_timer) = self.register_buffer.remove(&timer_id) {
            // 从索引中移除
            // Remove from indices
            self.remove_timer_from_indices(timer_id, &buffered_timer);
            
            self.stats.optimized_operations += 1;
            self.stats.total_cancel_requests += 1; // 所有成功的取消操作都应该计数
            trace!(timer_id = timer_id, "Timer cancellation optimized - removed from register buffer");
            return true;
        }
        
        // 检查是否是活跃定时器
        // Check if it's an active timer
        if self.active_timers.contains_key(&timer_id) {
            // 添加到取消缓冲区
            // Add to cancel buffer
            self.cancel_buffer.insert(timer_id);
            self.stats.total_cancel_requests += 1;
            
            trace!(timer_id = timer_id, buffer_size = self.cancel_buffer.len(), "Added timer to cancel buffer");
            
            // 检查是否需要自动刷新
            // Check if auto flush is needed
            if self.cancel_buffer.len() >= self.config.batch_threshold {
                self.flush_buffers().await;
            }
            
            return true;
        }
        
        false // 定时器不存在
    }
    
    /// 取消连接的所有特定类型定时器（兼容性方法）
    /// Cancel all timers of specific type for connection (compatibility method)
    async fn handle_cancel_timer_by_type(&mut self, connection_id: ConnectionId, timeout_event: TimeoutEvent) -> usize {
        let type_key = (connection_id, timeout_event);
        
        // 获取该类型的所有定时器ID
        // Get all timer IDs of this type
        let timer_ids: Vec<ActorTimerId> = self.type_timers
            .get(&type_key)
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default();
        
        let mut cancelled_count = 0;
        
        for timer_id in timer_ids {
            if self.handle_cancel_timer_by_id(timer_id).await {
                cancelled_count += 1;
            }
        }
        
        cancelled_count
    }
    
    /// 取消连接的所有定时器
    /// Cancel all timers for connection
    async fn handle_cancel_all_timers(&mut self, connection_id: ConnectionId) -> usize {
        // 获取该连接的所有定时器ID
        // Get all timer IDs for this connection
        let timer_ids: Vec<ActorTimerId> = self.connection_timers
            .get(&connection_id)
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default();
        
        let mut cancelled_count = 0;
        
        for timer_id in timer_ids {
            if self.handle_cancel_timer_by_id(timer_id).await {
                cancelled_count += 1;
            }
        }
        
        cancelled_count
    }
    
    /// 从索引中移除定时器
    /// Remove timer from indices
    fn remove_timer_from_indices(&mut self, timer_id: ActorTimerId, buffered_timer: &BufferedTimer) {
        // 从连接索引中移除
        // Remove from connection index
        if let Some(connection_set) = self.connection_timers.get_mut(&buffered_timer.registration.connection_id) {
            connection_set.remove(&timer_id);
            if connection_set.is_empty() {
                self.connection_timers.remove(&buffered_timer.registration.connection_id);
            }
        }
        
        // 从类型索引中移除
        // Remove from type index
        let type_key = (buffered_timer.registration.connection_id, buffered_timer.registration.timeout_event);
        if let Some(type_set) = self.type_timers.get_mut(&type_key) {
            type_set.remove(&timer_id);
            if type_set.is_empty() {
                self.type_timers.remove(&type_key);
            }
        }
    }
    
    /// 注册路径验证定时器
    /// Register path validation timer
    async fn handle_register_path_validation_timer(
        &mut self,
        connection_id: ConnectionId,
        path_id: u32,
        delay: Duration,
    ) -> Result<ActorTimerId, String> {
        // 创建带路径ID信息的PathValidationTimeout事件
        // Create PathValidationTimeout event with path ID information
        let timeout_event = TimeoutEvent::PathValidationTimeout;
        
        let (callback_tx, _callback_rx) = mpsc::channel(1); // 临时通道，将被TimerActor处理
        let registration = TimerRegistration::new(
            connection_id,
            delay,
            timeout_event,
            callback_tx,
        );
        
        let timer_id = self.handle_register_timer(registration).await?;
        
        trace!(
            timer_id = timer_id,
            connection_id = connection_id,
            path_id = path_id,
            delay = ?delay,
            "Registered path validation timer"
        );
        
        Ok(timer_id)
    }
    
    /// 注册重传定时器
    /// Register retransmission timer
    async fn handle_register_retransmission_timer(
        &mut self,
        connection_id: ConnectionId,
        packet_id: u64,
        delay: Duration,
    ) -> Result<ActorTimerId, String> {
        let timeout_event = TimeoutEvent::RetransmissionTimeout;
        
        let (callback_tx, _callback_rx) = mpsc::channel(1);
        let registration = TimerRegistration::new(
            connection_id,
            delay,
            timeout_event,
            callback_tx,
        );
        
        let timer_id = self.handle_register_timer(registration).await?;
        
        trace!(
            timer_id = timer_id,
            connection_id = connection_id,
            packet_id = packet_id,
            delay = ?delay,
            "Registered retransmission timer"
        );
        
        Ok(timer_id)
    }
    
    /// 注册基于数据包的重传定时器（新版本）
    /// Register packet-based retransmission timer (new version)
    async fn handle_register_packet_retransmission_timer(
        &mut self,
        connection_id: ConnectionId,
        sequence_number: u32,
        delay: Duration,
    ) -> Result<ActorTimerId, String> {
        // 使用新的PacketRetransmissionTimeout事件，包含具体的数据包信息
        // Use new PacketRetransmissionTimeout event with specific packet information
        let timer_id = self.allocate_timer_id();
        let timeout_event = TimeoutEvent::PacketRetransmissionTimeout {
            sequence_number,
            timer_id,
        };
        
        let (callback_tx, _callback_rx) = mpsc::channel(1);
        let registration = TimerRegistration::new(
            connection_id,
            delay,
            timeout_event,
            callback_tx,
        );
        
        let timer_id = self.handle_register_timer(registration).await?;
        
        trace!(
            timer_id = timer_id,
            connection_id = connection_id,
            sequence_number = sequence_number,
            delay = ?delay,
            "Registered packet-based retransmission timer with packet information"
        );
        
        Ok(timer_id)
    }
    
    /// 注册FIN处理定时器
    /// Register FIN processing timer
    async fn handle_register_fin_processing_timer(
        &mut self,
        connection_id: ConnectionId,
        delay: Duration,
    ) -> Result<ActorTimerId, String> {
        // 使用ConnectionTimeout来处理FIN处理超时
        // Use ConnectionTimeout to handle FIN processing timeout
        let timeout_event = TimeoutEvent::ConnectionTimeout;
        
        let (callback_tx, _callback_rx) = mpsc::channel(1);
        let registration = TimerRegistration::new(
            connection_id,
            delay,
            timeout_event,
            callback_tx,
        );
        
        let timer_id = self.handle_register_timer(registration).await?;
        
        trace!(
            timer_id = timer_id,
            connection_id = connection_id,
            delay = ?delay,
            "Registered FIN processing timer"
        );
        
        Ok(timer_id)
    }
    
    /// 获取当前统计信息
    /// Get current statistics
    fn get_current_stats(&self) -> TimerActorStats {
        let mut stats = self.stats.clone();
        stats.register_buffer_size = self.register_buffer.len();
        stats.cancel_buffer_size = self.cancel_buffer.len();
        stats.active_timer_count = self.active_timers.len();
        stats.next_timer_id = self.next_timer_id;
        stats
    }
    
    /// 查询定时器信息
    /// Query timer information
    fn query_timer_info(&self, timer_id: ActorTimerId) -> Option<TimerInfo> {
        // 首先检查缓冲区
        // First check buffer
        if let Some(buffered_timer) = self.register_buffer.get(&timer_id) {
            return Some(TimerInfo {
                timer_id,
                connection_id: buffered_timer.registration.connection_id,
                timeout_event: buffered_timer.registration.timeout_event,
                delay: buffered_timer.registration.delay,
                created_at: buffered_timer.created_at,
                in_buffer: true,
                entry_id: None,
            });
        }
        
        // 然后检查活跃定时器
        // Then check active timers
        if let Some(&entry_id) = self.active_timers.get(&timer_id) {
            // 从反向映射中查找连接信息（这需要遍历，不太高效，但用于调试）
            // Find connection info from reverse mapping (requires iteration, not efficient, but for debugging)
            for ((connection_id, timeout_event), timer_set) in &self.type_timers {
                if timer_set.contains(&timer_id) {
                    return Some(TimerInfo {
                        timer_id,
                        connection_id: *connection_id,
                        timeout_event: *timeout_event,
                        delay: Duration::from_secs(0), // 无法从活跃定时器中获取原始延迟
                        created_at: Instant::now(), // 无法获取确切创建时间
                        in_buffer: false,
                        entry_id: Some(entry_id),
                    });
                }
            }
        }
        
        None
    }
    
    /// 处理批量定时器注册
    /// Handle batch timer registration
    async fn handle_batch_register_timers(
        &mut self,
        registrations: Vec<TimerRegistration<TimeoutEvent>>,
    ) -> Result<Vec<ActorTimerId>, String> {
        trace!(count = registrations.len(), "Processing batch registration");
        
        let mut timer_ids = Vec::with_capacity(registrations.len());
        
        // 逐个注册定时器
        // Register timers one by one
        for registration in registrations {
            match self.handle_register_timer(registration).await {
                Ok(timer_id) => {
                    timer_ids.push(timer_id);
                }
                Err(_) => {
                    // 继续处理其他注册，不因单个失败而停止
                    // Continue processing others, don't stop for single failure
                    continue;
                }
            }
        }
        
        trace!(
            registered = timer_ids.len(),
            "Batch registration completed"
        );
        
        Ok(timer_ids)
    }
    
    /// 处理批量定时器取消（通过内部ID）
    /// Handle batch timer cancellation (by internal IDs)
    async fn handle_batch_cancel_timers_by_ids(&mut self, timer_ids: Vec<ActorTimerId>) -> usize {
        trace!(count = timer_ids.len(), "Processing batch cancellation by IDs");
        
        let mut cancelled_count = 0;
        
        for timer_id in timer_ids {
            if self.handle_cancel_timer_by_id(timer_id).await {
                cancelled_count += 1;
            }
        }
        
        trace!(cancelled = cancelled_count, "Batch cancellation by IDs completed");
        cancelled_count
    }
    
    /// 处理批量定时器取消（兼容性方法）
    /// Handle batch timer cancellation (compatibility method)
    async fn handle_batch_cancel_timers_by_type(&mut self, entries: Vec<(ConnectionId, TimeoutEvent)>) -> usize {
        trace!(count = entries.len(), "Processing batch cancellation by type");
        
        let mut cancelled_count = 0;
        
        for (connection_id, timeout_event) in entries {
            cancelled_count += self.handle_cancel_timer_by_type(connection_id, timeout_event).await;
        }
        
        trace!(cancelled = cancelled_count, "Batch cancellation by type completed");
        cancelled_count
    }

    
    /// 刷新所有缓冲区
    /// Flush all buffers
    async fn flush_buffers(&mut self) {
        let start_time = Instant::now();
        
        // 刷新注册缓冲区
        // Flush register buffer
        if !self.register_buffer.is_empty() {
            let buffered_timers: Vec<_> = self.register_buffer.drain().collect();
            let registrations: Vec<_> = buffered_timers.iter().map(|(_, bt)| bt.registration.clone()).collect();
            let count = registrations.len();
            
            trace!(count = count, "Flushing register buffer");
            
            let batch_registration = BatchTimerRegistration { registrations };
            match self.timer_handle.batch_register_timers(batch_registration).await {
                Ok(result) => {
                    // 更新映射关系
                    // Update mappings
                    for (i, (timer_id, _)) in buffered_timers.iter().enumerate() {
                        if i < result.successes.len() {
                            let entry_id = result.successes[i].entry_id;
                            self.active_timers.insert(*timer_id, entry_id);
                            self.entry_to_timer_id.insert(entry_id, *timer_id);
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
                    
                    // 失败时需要清理索引
                    // Clean up indices on failure
                    for (timer_id, buffered_timer) in buffered_timers {
                        self.remove_timer_from_indices(timer_id, &buffered_timer);
                    }
                }
            }
        }
        
        // 刷新取消缓冲区
        // Flush cancel buffer
        if !self.cancel_buffer.is_empty() {
            let timer_ids: Vec<_> = self.cancel_buffer.drain().collect();
            let count = timer_ids.len();
            
            trace!(count = count, "Flushing cancel buffer");
            
            // 将内部ID转换为entry_ids
            // Convert internal IDs to entry_ids
            let entry_ids: Vec<TimerEntryId> = timer_ids.iter()
                .filter_map(|timer_id| self.active_timers.get(timer_id).copied())
                .collect();
            
            if !entry_ids.is_empty() {
                let batch_cancellation = BatchTimerCancellation { entry_ids: entry_ids.clone() };
                match self.timer_handle.batch_cancel_timers(batch_cancellation).await {
                    Ok(result) => {
                        // 清理映射关系和索引
                        // Clean up mappings and indices
                        for entry_id in &entry_ids {
                            if let Some(timer_id) = self.entry_to_timer_id.remove(entry_id) {
                                self.active_timers.remove(&timer_id);
                                
                                // 从索引中移除（需要重构这部分以避免需要buffered_timer参数）
                                // Remove from indices (need to refactor this part to avoid needing buffered_timer parameter)
                                self.remove_timer_from_all_indices(timer_id);
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
    
    /// 从所有索引中移除定时器（通过遍历）
    /// Remove timer from all indices (by iteration)
    fn remove_timer_from_all_indices(&mut self, timer_id: ActorTimerId) {
        // 从连接索引中移除
        // Remove from connection index
        let mut connections_to_remove = Vec::new();
        for (connection_id, timer_set) in &mut self.connection_timers {
            timer_set.remove(&timer_id);
            if timer_set.is_empty() {
                connections_to_remove.push(*connection_id);
            }
        }
        for connection_id in connections_to_remove {
            self.connection_timers.remove(&connection_id);
        }
        
        // 从类型索引中移除
        // Remove from type index
        let mut types_to_remove = Vec::new();
        for (type_key, timer_set) in &mut self.type_timers {
            timer_set.remove(&timer_id);
            if timer_set.is_empty() {
                types_to_remove.push(*type_key);
            }
        }
        for type_key in types_to_remove {
            self.type_timers.remove(&type_key);
        }
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
    ) -> Result<ActorTimerId, String> {
        let (response_tx, response_rx) = oneshot::channel();
        let command = TimerActorCommand::RegisterTimer { registration, response_tx };
        
        if self.command_tx.send(command).await.is_err() {
            return Err("TimerActor is not available".to_string());
        }
        
        response_rx.await.unwrap_or_else(|_| Err("Response channel closed".to_string()))
    }
    
    /// 根据内部ID取消特定定时器
    /// Cancel specific timer by internal ID
    pub async fn cancel_timer_by_id(&self, timer_id: ActorTimerId) -> bool {
        let (response_tx, response_rx) = oneshot::channel();
        let command = TimerActorCommand::CancelTimerById { timer_id, response_tx };
        
        if self.command_tx.send(command).await.is_err() {
            return false;
        }
        
        response_rx.await.unwrap_or(false)
    }
    
    /// 取消连接的所有特定类型定时器（兼容性方法）
    /// Cancel all timers of specific type for connection (compatibility method)
    pub async fn cancel_timer(&self, connection_id: ConnectionId, timeout_event: TimeoutEvent) -> usize {
        let (response_tx, response_rx) = oneshot::channel();
        let command = TimerActorCommand::CancelTimer { connection_id, timeout_event, response_tx };
        
        if self.command_tx.send(command).await.is_err() {
            return 0;
        }
        
        response_rx.await.unwrap_or(0)
    }
    
    /// 取消连接的所有定时器
    /// Cancel all timers for connection
    pub async fn cancel_all_timers(&self, connection_id: ConnectionId) -> usize {
        let (response_tx, response_rx) = oneshot::channel();
        let command = TimerActorCommand::CancelAllTimers { connection_id, response_tx };
        
        if self.command_tx.send(command).await.is_err() {
            return 0;
        }
        
        response_rx.await.unwrap_or(0)
    }
    
    /// 批量注册定时器
    /// Batch register timers
    pub async fn batch_register_timers(
        &self,
        registrations: Vec<TimerRegistration<TimeoutEvent>>,
    ) -> Result<Vec<ActorTimerId>, String> {
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
    
    /// 批量取消定时器（通过内部ID）
    /// Batch cancel timers (by internal IDs)
    pub async fn batch_cancel_timers_by_ids(&self, timer_ids: Vec<ActorTimerId>) -> usize {
        let (response_tx, response_rx) = oneshot::channel();
        let command = TimerActorCommand::BatchCancelTimersByIds { timer_ids, response_tx };
        
        if self.command_tx.send(command).await.is_err() {
            return 0;
        }
        
        response_rx.await.unwrap_or(0)
    }
    
    /// 注册路径验证定时器
    /// Register path validation timer
    pub async fn register_path_validation_timer(
        &self,
        connection_id: ConnectionId,
        path_id: u32,
        delay: Duration,
    ) -> Result<ActorTimerId, String> {
        let (response_tx, response_rx) = oneshot::channel();
        let command = TimerActorCommand::RegisterPathValidationTimer { 
            connection_id, 
            path_id, 
            delay, 
            response_tx 
        };
        
        if self.command_tx.send(command).await.is_err() {
            return Err("TimerActor is not available".to_string());
        }
        
        response_rx.await.unwrap_or_else(|_| Err("Response channel closed".to_string()))
    }
    
    /// 注册重传定时器
    /// Register retransmission timer
    pub async fn register_retransmission_timer(
        &self,
        connection_id: ConnectionId,
        packet_id: u64,
        delay: Duration,
    ) -> Result<ActorTimerId, String> {
        let (response_tx, response_rx) = oneshot::channel();
        let command = TimerActorCommand::RegisterRetransmissionTimer { 
            connection_id, 
            packet_id, 
            delay, 
            response_tx 
        };
        
        if self.command_tx.send(command).await.is_err() {
            return Err("TimerActor is not available".to_string());
        }
        
        response_rx.await.unwrap_or_else(|_| Err("Response channel closed".to_string()))
    }
    
    /// 注册基于数据包的重传定时器（新版本）
    /// Register packet-based retransmission timer (new version)
    pub async fn register_packet_retransmission_timer(
        &self,
        connection_id: ConnectionId,
        sequence_number: u32,
        delay: Duration,
    ) -> Result<ActorTimerId, String> {
        let (response_tx, response_rx) = oneshot::channel();
        let command = TimerActorCommand::RegisterPacketRetransmissionTimer { 
            connection_id, 
            sequence_number, 
            delay, 
            response_tx 
        };
        
        if self.command_tx.send(command).await.is_err() {
            return Err("TimerActor is not available".to_string());
        }
        
        response_rx.await.unwrap_or_else(|_| Err("Response channel closed".to_string()))
    }
    
    /// 注册FIN处理定时器
    /// Register FIN processing timer
    pub async fn register_fin_processing_timer(
        &self,
        connection_id: ConnectionId,
        delay: Duration,
    ) -> Result<ActorTimerId, String> {
        let (response_tx, response_rx) = oneshot::channel();
        let command = TimerActorCommand::RegisterFinProcessingTimer { 
            connection_id, 
            delay, 
            response_tx 
        };
        
        if self.command_tx.send(command).await.is_err() {
            return Err("TimerActor is not available".to_string());
        }
        
        response_rx.await.unwrap_or_else(|_| Err("Response channel closed".to_string()))
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
    
    /// 查询定时器信息
    /// Query timer information
    pub async fn query_timer(&self, timer_id: ActorTimerId) -> Option<TimerInfo> {
        let (response_tx, response_rx) = oneshot::channel();
        let command = TimerActorCommand::QueryTimer { timer_id, response_tx };
        
        if self.command_tx.send(command).await.is_err() {
            return None;
        }
        
        response_rx.await.ok().flatten()
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
        let cancelled_count = actor_handle.cancel_timer(123, TimeoutEvent::IdleTimeout).await;
        assert!(cancelled_count > 0);
        
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
        assert_eq!(cancelled_count, 3); // 只有3个实际存在的定时器被取消
        
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

    #[tokio::test]
    async fn test_timer_actor_query_timer_info() {
        let config = TimerActorConfig::default();
        let timer_handle = start_hybrid_timer_task::<TimeoutEvent>();
        let actor_handle = start_timer_actor(timer_handle, Some(config));
        
        let (callback_tx, _callback_rx) = mpsc::channel(1);
        
        // 注册一个定时器
        let registration = TimerRegistration::new(
            100,
            Duration::from_secs(30),
            TimeoutEvent::IdleTimeout,
            callback_tx.clone(),
        );
        
        let timer_id = actor_handle.register_timer(registration).await.unwrap();
        
        // 查询定时器信息
        let timer_info = actor_handle.query_timer(timer_id).await;
        assert!(timer_info.is_some());
        
        let info = timer_info.unwrap();
        assert_eq!(info.timer_id, timer_id);
        assert_eq!(info.connection_id, 100);
        assert_eq!(info.timeout_event, TimeoutEvent::IdleTimeout);
        assert_eq!(info.delay, Duration::from_secs(30));
        assert!(info.in_buffer); // 应该还在缓冲区中
        assert!(info.entry_id.is_none()); // 还没有全局条目ID
        
        // 查询不存在的定时器
        let nonexistent_info = actor_handle.query_timer(999999).await;
        assert!(nonexistent_info.is_none());
        
        actor_handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_timer_actor_specialized_timers() {
        let config = TimerActorConfig::default();
        let timer_handle = start_hybrid_timer_task::<TimeoutEvent>();
        let actor_handle = start_timer_actor(timer_handle, Some(config));
        
        // 测试路径验证定时器
        let path_timer_id = actor_handle
            .register_path_validation_timer(100, 1, Duration::from_millis(500))
            .await.unwrap();
        assert!(path_timer_id > 0);
        
        // 测试重传定时器
        let retx_timer_id = actor_handle
            .register_retransmission_timer(100, 12345, Duration::from_millis(100))
            .await.unwrap();
        assert!(retx_timer_id > 0);
        
        // 测试FIN处理定时器
        let fin_timer_id = actor_handle
            .register_fin_processing_timer(100, Duration::from_millis(50))
            .await.unwrap();
        assert!(fin_timer_id > 0);
        
        // 验证所有定时器都有不同的ID
        assert_ne!(path_timer_id, retx_timer_id);
        assert_ne!(retx_timer_id, fin_timer_id);
        assert_ne!(path_timer_id, fin_timer_id);
        
        // 测试统计信息
        let stats = actor_handle.get_stats().await.unwrap();
        assert!(stats.total_register_requests > 0);
        
        actor_handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_timer_actor_cancel_by_id() {
        let config = TimerActorConfig::default();
        let timer_handle = start_hybrid_timer_task::<TimeoutEvent>();
        let actor_handle = start_timer_actor(timer_handle, Some(config));
        
        let (callback_tx, _callback_rx) = mpsc::channel(1);
        
        // 注册多个定时器
        let mut timer_ids = Vec::new();
        for i in 0..3 {
            let registration = TimerRegistration::new(
                100 + i,
                Duration::from_secs(30),
                TimeoutEvent::IdleTimeout,
                callback_tx.clone(),
            );
            let timer_id = actor_handle.register_timer(registration).await.unwrap();
            timer_ids.push(timer_id);
        }
        
        // 通过ID取消定时器
        let cancelled1 = actor_handle.cancel_timer_by_id(timer_ids[0]).await;
        assert!(cancelled1);
        
        let cancelled2 = actor_handle.cancel_timer_by_id(timer_ids[1]).await;
        assert!(cancelled2);
        
        // 再次取消同一个定时器应该返回false
        let cancelled_again = actor_handle.cancel_timer_by_id(timer_ids[0]).await;
        assert!(!cancelled_again);
        
        // 取消不存在的定时器应该返回false
        let cancelled_nonexistent = actor_handle.cancel_timer_by_id(999999).await;
        assert!(!cancelled_nonexistent);
        
        actor_handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_timer_actor_batch_cancel_by_ids() {
        let config = TimerActorConfig::default();
        let timer_handle = start_hybrid_timer_task::<TimeoutEvent>();
        let actor_handle = start_timer_actor(timer_handle, Some(config));
        
        let (callback_tx, _callback_rx) = mpsc::channel(1);
        
        // 注册多个定时器
        let mut timer_ids = Vec::new();
        for i in 0..5 {
            let registration = TimerRegistration::new(
                100 + i,
                Duration::from_secs(30),
                TimeoutEvent::IdleTimeout,
                callback_tx.clone(),
            );
            let timer_id = actor_handle.register_timer(registration).await.unwrap();
            timer_ids.push(timer_id);
        }
        
        // 批量取消定时器（包含一些存在的和一些不存在的ID）
        let mut cancel_ids = timer_ids[0..3].to_vec(); // 前3个存在的
        cancel_ids.push(999999); // 不存在的
        cancel_ids.push(999998); // 不存在的
        
        let cancelled_count = actor_handle.batch_cancel_timers_by_ids(cancel_ids).await;
        assert_eq!(cancelled_count, 3); // 只有3个存在的被取消
        
        // 验证剩余的定时器仍然存在
        let remaining_info = actor_handle.query_timer(timer_ids[3]).await;
        assert!(remaining_info.is_some());
        
        actor_handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_timer_actor_connection_cleanup() {
        let config = TimerActorConfig::default();
        let timer_handle = start_hybrid_timer_task::<TimeoutEvent>();
        let actor_handle = start_timer_actor(timer_handle, Some(config));
        
        let (callback_tx, _callback_rx) = mpsc::channel(1);
        
        // 为同一个连接注册多种类型的定时器
        let connection_id = 100;
        let mut timer_ids = Vec::new();
        
        // 空闲超时定时器
        timer_ids.push(actor_handle.register_timer(TimerRegistration::new(
            connection_id,
            Duration::from_secs(30),
            TimeoutEvent::IdleTimeout,
            callback_tx.clone(),
        )).await.unwrap());
        
        // 路径验证定时器
        timer_ids.push(actor_handle.register_path_validation_timer(
            connection_id, 1, Duration::from_millis(500)
        ).await.unwrap());
        
        // 重传定时器
        timer_ids.push(actor_handle.register_retransmission_timer(
            connection_id, 12345, Duration::from_millis(100)
        ).await.unwrap());
        
        // 验证所有定时器都存在
        for &timer_id in &timer_ids {
            let info = actor_handle.query_timer(timer_id).await;
            assert!(info.is_some());
        }
        
        // 取消该连接的所有定时器
        let cancelled_count = actor_handle.cancel_all_timers(connection_id).await;
        assert_eq!(cancelled_count, 3);
        
        // 验证所有定时器都被取消
        for &timer_id in &timer_ids {
            let info = actor_handle.query_timer(timer_id).await;
            assert!(info.is_none());
        }
        
        actor_handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_timer_actor_auto_flush() {
        let config = TimerActorConfig {
            batch_threshold: 100, // 设置很高的阈值，不会触发自动批量
            flush_interval: Duration::from_millis(50), // 短的刷新间隔
        };
        
        let timer_handle = start_hybrid_timer_task::<TimeoutEvent>();
        let actor_handle = start_timer_actor(timer_handle, Some(config));
        
        let (callback_tx, _callback_rx) = mpsc::channel(1);
        
        // 注册几个定时器
        for i in 0..3 {
            let registration = TimerRegistration::new(
                100 + i,
                Duration::from_secs(30),
                TimeoutEvent::IdleTimeout,
                callback_tx.clone(),
            );
            let _ = actor_handle.register_timer(registration).await.unwrap();
        }
        
        // 等待自动刷新
        tokio::time::sleep(Duration::from_millis(100)).await;
        
        // 检查统计信息
        let stats = actor_handle.get_stats().await.unwrap();
        println!("Auto flush stats:");
        println!("  Auto flush count: {}", stats.auto_flush_count);
        println!("  Register buffer size: {}", stats.register_buffer_size);
        
        // 应该发生了自动刷新
        assert!(stats.auto_flush_count > 0 || stats.register_buffer_size == 0);
        
        actor_handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_timer_actor_manual_flush() {
        let config = TimerActorConfig {
            batch_threshold: 100, // 设置很高的阈值，不会触发自动批量
            flush_interval: Duration::from_secs(60), // 长的刷新间隔，不会自动刷新
        };
        
        let timer_handle = start_hybrid_timer_task::<TimeoutEvent>();
        let actor_handle = start_timer_actor(timer_handle, Some(config));
        
        let (callback_tx, _callback_rx) = mpsc::channel(1);
        
        // 注册几个定时器
        for i in 0..3 {
            let registration = TimerRegistration::new(
                100 + i,
                Duration::from_secs(30),
                TimeoutEvent::IdleTimeout,
                callback_tx.clone(),
            );
            let _ = actor_handle.register_timer(registration).await.unwrap();
        }
        
        // 检查缓冲区有内容
        let stats_before = actor_handle.get_stats().await.unwrap();
        assert!(stats_before.register_buffer_size > 0);
        
        // 手动刷新
        let flush_result = actor_handle.flush_buffers().await;
        assert!(flush_result);
        
        // 检查缓冲区被清空
        let stats_after = actor_handle.get_stats().await.unwrap();
        assert_eq!(stats_after.register_buffer_size, 0);
        
        actor_handle.shutdown().await;
    }

    #[tokio::test]
    async fn test_timer_actor_error_handling() {
        let config = TimerActorConfig::default();
        let timer_handle = start_hybrid_timer_task::<TimeoutEvent>();
        let actor_handle = start_timer_actor(timer_handle.clone(), Some(config));
        
        // 先关闭actor
        actor_handle.shutdown().await;
        
        // 关闭底层定时器任务
        timer_handle.shutdown().await.unwrap();
        
        let (callback_tx, _callback_rx) = mpsc::channel(1);
        
        // 尝试在关闭后注册定时器应该失败
        let registration = TimerRegistration::new(
            100,
            Duration::from_secs(30),
            TimeoutEvent::IdleTimeout,
            callback_tx.clone(),
        );
        
        let result = actor_handle.register_timer(registration).await;
        assert!(result.is_err());
        
        // 其他操作也应该失败或返回默认值
        let stats = actor_handle.get_stats().await;
        assert!(stats.is_none());
        
        let query_result = actor_handle.query_timer(1).await;
        assert!(query_result.is_none());
    }

    #[tokio::test]
    async fn test_timer_actor_statistics_tracking() {
        let config = TimerActorConfig::default();
        let timer_handle = start_hybrid_timer_task::<TimeoutEvent>();
        let actor_handle = start_timer_actor(timer_handle, Some(config));
        
        let (callback_tx, _callback_rx) = mpsc::channel(1);
        
        // 初始统计信息
        let initial_stats = actor_handle.get_stats().await.unwrap();
        assert_eq!(initial_stats.total_register_requests, 0);
        assert_eq!(initial_stats.total_cancel_requests, 0);
        assert_eq!(initial_stats.optimized_operations, 0);
        
        // 注册一些定时器
        let mut timer_ids = Vec::new();
        for i in 0..5 {
            let registration = TimerRegistration::new(
                100 + i,
                Duration::from_secs(30),
                TimeoutEvent::IdleTimeout,
                callback_tx.clone(),
            );
            let timer_id = actor_handle.register_timer(registration).await.unwrap();
            timer_ids.push(timer_id);
        }
        
        // 批量注册一些定时器
        let mut batch_registrations = Vec::new();
        for i in 0..3 {
            batch_registrations.push(TimerRegistration::new(
                200 + i,
                Duration::from_secs(20),
                TimeoutEvent::PathValidationTimeout,
                callback_tx.clone(),
            ));
        }
        let _batch_ids = actor_handle.batch_register_timers(batch_registrations).await.unwrap();
        
        // 取消一些定时器
        let cancelled1 = actor_handle.cancel_timer_by_id(timer_ids[0]).await;
        assert!(cancelled1);
        
        let cancelled2 = actor_handle.cancel_all_timers(101).await;
        assert!(cancelled2 > 0);
        
        // 检查最终统计信息
        let final_stats = actor_handle.get_stats().await.unwrap();
        println!("Final statistics:");
        println!("  Total register requests: {}", final_stats.total_register_requests);
        println!("  Total cancel requests: {}", final_stats.total_cancel_requests);
        println!("  Active timer count: {}", final_stats.active_timer_count);
        println!("  Optimized operations: {}", final_stats.optimized_operations);
        
        // 验证统计信息的合理性
        assert!(final_stats.total_register_requests >= 8); // 至少注册了8个定时器
        assert!(final_stats.total_cancel_requests > 0); // 至少有一些取消操作
        
        actor_handle.shutdown().await;
    }
}