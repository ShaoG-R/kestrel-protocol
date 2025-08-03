//! 定时器事件处理器 - 专门处理定时器事件
//! Timer Event Handler - Specialized timer event processing
//!
//! 职责：
//! - 处理定时器超时事件
//! - 管理定时器生命周期
//! - 协调定时器与数据包状态

use super::super::data::in_flight_store::InFlightPacketStore;
use crate::{
    core::endpoint::timing::TimeoutEvent,
    timer::{
        actor::{ActorTimerId, SenderTimerActorHandle},
        event::{ConnectionId, TimerEventData},
        task::types::SenderCallback,
    },
};
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, trace, warn};

/// 定时器处理结果
/// Timer handling result
#[derive(Debug)]
pub struct TimerHandlingResult {
    /// 找到的数据包序列号
    /// Found packet sequence number
    pub sequence_number: Option<u32>,
    
    /// 是否需要处理重传
    /// Whether retransmission handling is needed
    pub needs_retransmission_handling: bool,
    
    /// 是否需要重新调度定时器
    /// Whether timer rescheduling is needed
    pub needs_timer_reschedule: bool,
}

/// 定时器事件处理器
/// Timer event handler
#[derive(Debug)]
pub struct TimerEventHandler {
    /// 连接ID
    /// Connection ID
    connection_id: ConnectionId,
    
    /// 定时器Actor句柄
    /// Timer actor handle
    timer_actor: SenderTimerActorHandle,
    
    /// 定时器事件发送通道
    /// Timer event sender channel
    timeout_tx: mpsc::Sender<TimerEventData<TimeoutEvent>>,
}

impl TimerEventHandler {
    /// 创建新的定时器事件处理器
    /// Create new timer event handler
    pub fn new(
        connection_id: ConnectionId,
        timer_actor: SenderTimerActorHandle,
        timeout_tx: mpsc::Sender<TimerEventData<TimeoutEvent>>,
    ) -> Self {
        Self {
            connection_id,
            timer_actor,
            timeout_tx,
        }
    }
    
    /// 处理定时器超时事件
    /// Handle timer timeout event
    pub fn handle_timer_timeout(
        &self,
        store: &mut InFlightPacketStore,
        timer_id: ActorTimerId,
    ) -> TimerHandlingResult {
        // 通过定时器ID查找对应的序列号
        if let Some(seq) = store.find_sequence_by_timer(timer_id) {
            // 清理定时器映射
            store.remove_timer_mapping(timer_id);
            
            // 检查数据包是否仍然存在
            if let Some(_packet) = store.get_packet(seq) {
                trace!(
                    timer_id = timer_id,
                    seq = seq,
                    "Timer timeout event processed, packet found"
                );
                
                TimerHandlingResult {
                    sequence_number: Some(seq),
                    needs_retransmission_handling: true,
                    needs_timer_reschedule: false, // 重传决策器会决定是否需要重新调度
                }
            } else {
                debug!(
                    timer_id = timer_id,
                    seq = seq,
                    "Timer timeout event processed, but packet no longer exists"
                );
                
                TimerHandlingResult {
                    sequence_number: Some(seq),
                    needs_retransmission_handling: false,
                    needs_timer_reschedule: false,
                }
            }
        } else {
            warn!(
                timer_id = timer_id,
                "Timer timeout for unknown timer ID"
            );
            
            TimerHandlingResult {
                sequence_number: None,
                needs_retransmission_handling: false,
                needs_timer_reschedule: false,
            }
        }
    }
    
    /// 为数据包调度重传定时器（增强错误处理）
    /// Schedule retransmission timer for packet (enhanced error handling)
    pub async fn schedule_retransmission_timer(
        &self,
        store: &mut InFlightPacketStore,
        seq: u32,
        rto: Duration,
    ) -> Result<ActorTimerId, String> {
        // 验证输入参数
        if rto.is_zero() {
            let error = "Invalid RTO: cannot be zero".to_string();
            warn!(seq = seq, error = error);
            return Err(error);
        }
        
        if rto > Duration::from_secs(300) { // 5分钟上限
            let error = format!("RTO too large: {} seconds", rto.as_secs());
            warn!(seq = seq, error = error);
            return Err(error);
        }
        
        // 检查序列号是否已有定时器
        if let Some(packet) = store.get_packet(seq) {
            if packet.timer_id.is_some() {
                debug!(seq = seq, "Packet already has timer, cancelling first");
                self.cancel_retransmission_timer(store, seq).await;
            }
        }
        
        // 使用回调式ID注入，确保事件中的timer_id与实际分配的一致
        // Use callback-based ID injection to ensure timer_id in event matches the allocated one
        let timer_id = self.timer_actor.register_timer_with_callback(
            self.connection_id,
            rto,
            SenderCallback::new(self.timeout_tx.clone()),
            move |timer_id| TimeoutEvent::PacketRetransmissionTimeout {
                sequence_number: seq,
                timer_id, // 使用实际分配的timer_id
            }
        ).await;
        
        match timer_id {
            Ok(timer_id) => {
                // 设置定时器映射
                store.set_timer_mapping(timer_id, seq);
                
                trace!(
                    seq = seq,
                    timer_id = timer_id,
                    rto_ms = rto.as_millis(),
                    "Scheduled retransmission timer with callback-based ID injection"
                );
                
                Ok(timer_id)
            }
            Err(e) => {
                warn!(seq = seq, error = e, "Failed to schedule retransmission timer");
                Err(e)
            }
        }
    }
    
    /// 取消数据包的重传定时器
    /// Cancel retransmission timer for packet
    pub async fn cancel_retransmission_timer(
        &self,
        store: &mut InFlightPacketStore,
        seq: u32,
    ) -> bool {
        if let Some(packet) = store.get_packet(seq) {
            if let Some(timer_id) = packet.timer_id {
                // 移除定时器映射
                store.remove_timer_mapping(timer_id);
                
                // 取消定时器
                self.timer_actor.cancel_timer_by_id(timer_id).await;
                
                trace!(
                    seq = seq,
                    timer_id = timer_id,
                    "Cancelled retransmission timer"
                );
                
                return true;
            }
        }
        
        false
    }
    
    /// 批量取消重传定时器
    /// Batch cancel retransmission timers
    pub async fn batch_cancel_retransmission_timers(
        &self,
        store: &mut InFlightPacketStore,
        sequences: &[u32],
    ) -> usize {
        let mut timer_ids = Vec::new();
        
        for &seq in sequences {
            if let Some(packet) = store.get_packet(seq) {
                if let Some(timer_id) = packet.timer_id {
                    timer_ids.push(timer_id);
                    store.remove_timer_mapping(timer_id);
                }
            }
        }
        
        if !timer_ids.is_empty() {
            let cancelled_count = self.timer_actor.batch_cancel_timers_by_ids(timer_ids).await;
            
            debug!(
                requested_count = sequences.len(),
                cancelled_count = cancelled_count,
                "Batch cancelled retransmission timers"
            );
            
            cancelled_count
        } else {
            0
        }
    }
    
    /// 重新调度数据包的重传定时器
    /// Reschedule retransmission timer for packet
    pub async fn reschedule_retransmission_timer(
        &self,
        store: &mut InFlightPacketStore,
        seq: u32,
        rto: Duration,
    ) -> Result<ActorTimerId, String> {
        // 先取消现有定时器（如果有）
        self.cancel_retransmission_timer(store, seq).await;
        
        // 调度新定时器
        self.schedule_retransmission_timer(store, seq, rto).await
    }
    
    /// 批量重新调度定时器（优化版）
    /// Batch reschedule timers (optimized version)
    pub async fn batch_reschedule_timers(
        &self,
        store: &mut InFlightPacketStore,
        sequences: &[u32],
        rto: Duration,
    ) -> Vec<(u32, Result<ActorTimerId, String>)> {
        let mut results = Vec::with_capacity(sequences.len());
        
        // 先批量取消现有定时器
        let cancelled_count = self.batch_cancel_retransmission_timers(store, sequences).await;
        
        debug!(
            requested = sequences.len(),
            cancelled = cancelled_count,
            "Batch cancelled timers before rescheduling"
        );
        
        // 并发调度新定时器（但要控制并发度避免过载timer actor）
        const MAX_CONCURRENT: usize = 10;
        
        for chunk in sequences.chunks(MAX_CONCURRENT) {
            let mut tasks = Vec::new();
            
            for &seq in chunk {
                let connection_id = self.connection_id;
                let timer_actor = self.timer_actor.clone();
                let timeout_tx = self.timeout_tx.clone();
                
                let task = tokio::spawn(async move {
                    let timer_id = timer_actor.register_timer_with_callback(
                        connection_id,
                        rto,
                        SenderCallback::new(timeout_tx),
                        move |timer_id| TimeoutEvent::PacketRetransmissionTimeout {
                            sequence_number: seq,
                            timer_id,
                        }
                    ).await;
                    (seq, timer_id)
                });
                
                tasks.push(task);
            }
            
            // 等待这批任务完成
            for task in tasks {
                match task.await {
                    Ok((seq, timer_result)) => {
                        match timer_result {
                            Ok(timer_id) => {
                                store.set_timer_mapping(timer_id, seq);
                                results.push((seq, Ok(timer_id)));
                            }
                            Err(e) => {
                                results.push((seq, Err(e)));
                            }
                        }
                    }
                    Err(join_error) => {
                        // 处理task join错误
                        let error_msg = format!("Task join error: {}", join_error);
                        debug!(seq = "unknown", error = error_msg, "Failed to join timer scheduling task");
                    }
                }
            }
        }
        
        results
    }
    
    /// 清理所有定时器
    /// Clean up all timers
    pub async fn cleanup_all_timers(&self, store: &mut InFlightPacketStore) {
        let all_sequences = store.get_all_sequences();
        
        if !all_sequences.is_empty() {
            let cancelled_count = self.batch_cancel_retransmission_timers(store, &all_sequences).await;
            debug!(cancelled_count = cancelled_count, "Cleaned up all retransmission timers");
        }
    }
    
    /// 获取活跃定时器数量
    /// Get active timer count
    pub fn get_active_timer_count(&self, store: &InFlightPacketStore) -> usize {
        store.get_all_sequences()
            .iter()
            .filter_map(|&seq| store.get_packet(seq))
            .filter(|packet| packet.timer_id.is_some())
            .count()
    }
}