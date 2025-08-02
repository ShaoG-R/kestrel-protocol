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
        task::types::SenderTimerRegistration,
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
    
    /// 为数据包调度重传定时器
    /// Schedule retransmission timer for packet
    pub async fn schedule_retransmission_timer(
        &self,
        store: &mut InFlightPacketStore,
        seq: u32,
        rto: Duration,
    ) -> Result<ActorTimerId, String> {
        // 创建PacketRetransmissionTimeout事件
        let timeout_event = TimeoutEvent::PacketRetransmissionTimeout {
            sequence_number: seq,
            timer_id: 0, // 实际的timer_id由Actor分配
        };
        
        // 创建定时器注册
        let registration = SenderTimerRegistration::with_sender(
            self.connection_id,
            rto,
            timeout_event,
            self.timeout_tx.clone(),
        );
        
        match self.timer_actor.register_timer(registration).await {
            Ok(timer_id) => {
                // 设置定时器映射
                store.set_timer_mapping(timer_id, seq);
                
                trace!(
                    seq = seq,
                    timer_id = timer_id,
                    rto_ms = rto.as_millis(),
                    "Scheduled retransmission timer"
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
    
    /// 批量重新调度定时器
    /// Batch reschedule timers
    pub async fn batch_reschedule_timers(
        &self,
        store: &mut InFlightPacketStore,
        sequences: &[u32],
        rto: Duration,
    ) -> Vec<(u32, Result<ActorTimerId, String>)> {
        let mut results = Vec::new();
        
        // 先批量取消现有定时器
        self.batch_cancel_retransmission_timers(store, sequences).await;
        
        // 为每个序列号调度新定时器
        for &seq in sequences {
            let result = self.schedule_retransmission_timer(store, seq, rto).await;
            results.push((seq, result));
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