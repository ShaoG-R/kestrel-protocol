//! 数据包定时器管理器 - 将重传定时器与数据包直接绑定
//! Packet Timer Manager - Direct binding of retransmission timers with packets
//!
//! 这个模块实现了一个新的重传管理机制，其中每个需要重传的数据包
//! 都直接携带自己的定时器信息，避免了复杂的间接映射关系。
//!
//! This module implements a new retransmission management mechanism where
//! each packet that needs retransmission directly carries its own timer
//! information, avoiding complex indirect mapping relationships.

use crate::{core::reliability::retransmission::RetransmissionFrameInfo, timer::{
    actor::{ActorTimerId, SenderTimerActorHandle},
    event::{ConnectionId, TimerEventData},
    task::types::SenderTimerRegistration,
}};
use crate::packet::frame::{Frame, RetransmissionContext};
use crate::core::endpoint::timing::TimeoutEvent;
use std::collections::BTreeMap;
use tokio::{sync::mpsc, time::{Duration, Instant}};
use tracing::{debug, trace, warn};

/// 带定时器的在途数据包
/// In-flight packet with timer
#[derive(Debug, Clone)]
pub struct InFlightPacketWithTimer {
    /// 数据包的重传信息
    /// Packet retransmission info
    pub frame_info: RetransmissionFrameInfo,
    
    /// 最后发送时间
    /// Last sent time
    pub last_sent_at: Instant,
    
    /// 重传次数
    /// Retransmission count
    pub retx_count: u32,
    
    /// 最大重传次数
    /// Maximum retransmission count
    pub max_retx_count: u32,
    
    /// 当前的定时器ID（如果有）
    /// Current timer ID (if any)
    pub timer_id: Option<ActorTimerId>,
    
    /// 是否已调度定时器
    /// Whether timer is scheduled
    pub timer_scheduled: bool,
}

impl InFlightPacketWithTimer {
    /// 创建新的在途数据包
    /// Create new in-flight packet
    pub fn new(frame: &Frame, now: Instant, max_retx_count: u32) -> Option<Self> {
        if let Some(frame_info) = RetransmissionFrameInfo::from_frame(frame) {
            Some(Self {
                frame_info,
                last_sent_at: now,
                retx_count: 0,
                max_retx_count,
                timer_id: None,
                timer_scheduled: false,
            })
        } else {
            None
        }
    }
    
    /// 检查是否需要重传
    /// Check if retransmission is needed
    pub fn needs_retransmission(&self, rto: Duration, now: Instant) -> bool {
        now.duration_since(self.last_sent_at) >= rto && self.retx_count < self.max_retx_count
    }
    
    /// 检查是否已达到最大重传次数
    /// Check if maximum retransmission count reached
    pub fn is_max_retx_reached(&self) -> bool {
        self.retx_count >= self.max_retx_count
    }
    
    /// 执行重传，返回重构的帧
    /// Perform retransmission, return reconstructed frame
    pub fn retransmit(&mut self, context: &RetransmissionContext, now: Instant) -> Frame {
        self.retx_count += 1;
        self.last_sent_at = now;
        self.timer_scheduled = false; // 需要重新调度定时器
        
        self.frame_info.reconstruct_frame(context, now)
    }
}

/// 数据包定时器管理器
/// Packet timer manager
#[derive(Debug)]
pub struct PacketTimerManager {
    /// 连接ID
    /// Connection ID
    connection_id: ConnectionId,
    
    /// 定时器Actor句柄
    /// Timer actor handle
    timer_actor: SenderTimerActorHandle,
    
    /// 定时器事件发送通道
    /// Timer event sender channel
    timeout_tx: mpsc::Sender<TimerEventData<TimeoutEvent>>,
    
    /// 在途数据包映射：序列号 -> 数据包
    /// In-flight packets map: sequence number -> packet
    in_flight_packets: BTreeMap<u32, InFlightPacketWithTimer>,
    
    /// 定时器ID到序列号的反向映射
    /// Reverse mapping: timer ID -> sequence number
    timer_to_sequence: BTreeMap<ActorTimerId, u32>,
}

impl PacketTimerManager {
    /// 创建新的数据包定时器管理器
    /// Create new packet timer manager
    pub fn new(
        connection_id: ConnectionId, 
        timer_actor: SenderTimerActorHandle,
        timeout_tx: mpsc::Sender<TimerEventData<TimeoutEvent>>,
    ) -> Self {
        Self {
            connection_id,
            timer_actor,
            timeout_tx,
            in_flight_packets: BTreeMap::new(),
            timer_to_sequence: BTreeMap::new(),
        }
    }
    
    /// 添加数据包到在途管理
    /// Add packet to in-flight management
    pub async fn add_packet(&mut self, frame: &Frame, now: Instant, rto: Duration) -> bool {
        if let Some(seq) = frame.sequence_number() {
            if let Some(packet) = InFlightPacketWithTimer::new(frame, now, 5) {
                // 先添加数据包到映射中
                // First add packet to mapping
                self.in_flight_packets.insert(seq, packet);
                
                // 然后调度重传定时器
                // Then schedule retransmission timer
                match self.schedule_retransmission_timer(seq, rto).await {
                    Ok(timer_id) => {
                        // 更新数据包的定时器信息
                        // Update packet timer info
                        if let Some(packet) = self.in_flight_packets.get_mut(&seq) {
                            packet.timer_id = Some(timer_id);
                            packet.timer_scheduled = true;
                        }
                        self.timer_to_sequence.insert(timer_id, seq);
                        
                        trace!(
                            seq = seq,
                            timer_id = timer_id,
                            rto = ?rto,
                            "Added packet with retransmission timer"
                        );
                    }
                    Err(e) => {
                        warn!(seq = seq, error = e, "Failed to schedule retransmission timer for packet");
                        // 即使定时器调度失败，我们仍然跟踪数据包
                        // Even if timer scheduling fails, we still track the packet
                    }
                }
                
                return true;

            }
        }
        false
    }
    
    /// 处理定时器超时事件
    /// Handle timer timeout event
    pub async fn handle_timer_timeout(
        &mut self,
        timer_id: ActorTimerId,
        context: &RetransmissionContext,
        rto: Duration,
    ) -> Option<Frame> {
        // 通过定时器ID找到对应的序列号
        // Find sequence number by timer ID
        if let Some(&seq) = self.timer_to_sequence.get(&timer_id) {
            // 先移除定时器映射
            // First remove timer mapping
            self.timer_to_sequence.remove(&timer_id);
            
            if let Some(packet) = self.in_flight_packets.get_mut(&seq) {
                let now = Instant::now();
                
                // 清除数据包的定时器信息
                // Clear packet timer info
                packet.timer_id = None;
                packet.timer_scheduled = false;
                
                if packet.needs_retransmission(rto, now) {
                    // 执行重传
                    // Perform retransmission 
                    let retx_frame = packet.retransmit(context, now);
                    let retx_count = packet.retx_count;
                    let max_retx = packet.max_retx_count;
                    let should_reschedule = !packet.is_max_retx_reached();
                    
                    debug!(
                        seq = seq,
                        retx_count = retx_count,
                        max_retx = max_retx,
                        "Packet retransmitted due to timeout"
                    );
                    
                    // 如果还未达到最大重传次数，重新调度定时器
                    // If max retransmissions not reached, reschedule timer
                    if should_reschedule {
                        match self.schedule_retransmission_timer(seq, rto).await {
                            Ok(new_timer_id) => {
                                // 重新获取数据包引用更新定时器信息
                                // Get packet reference again to update timer info
                                if let Some(packet) = self.in_flight_packets.get_mut(&seq) {
                                    packet.timer_id = Some(new_timer_id);
                                    packet.timer_scheduled = true;
                                }
                                self.timer_to_sequence.insert(new_timer_id, seq);
                                
                                trace!(
                                    seq = seq,
                                    new_timer_id = new_timer_id,
                                    "Rescheduled retransmission timer after retransmission"
                                );
                            }
                            Err(e) => {
                                warn!(seq = seq, error = e, "Failed to reschedule retransmission timer");
                            }
                        }
                    }
                    
                    return Some(retx_frame);
                } else if packet.is_max_retx_reached() {
                    // 达到最大重传次数，放弃该数据包
                    // Max retransmissions reached, give up on this packet
                    warn!(seq = seq, retx_count = packet.retx_count, "Packet dropped after max retransmissions");
                    self.in_flight_packets.remove(&seq);
                }
            }
        }
        
        None
    }
    
    /// 确认数据包，移除重传跟踪
    /// Acknowledge packet, remove from retransmission tracking
    pub async fn acknowledge_packet(&mut self, seq: u32) -> bool {
        if let Some(packet) = self.in_flight_packets.remove(&seq) {
            // 取消相关的定时器
            // Cancel associated timer
            if let Some(timer_id) = packet.timer_id {
                self.timer_to_sequence.remove(&timer_id);
                self.timer_actor.cancel_timer_by_id(timer_id).await;
                
                trace!(
                    seq = seq,
                    timer_id = timer_id,
                    "Acknowledged packet and cancelled timer"
                );
            }
            return true;
        }
        false
    }
    
    /// 批量确认数据包
    /// Batch acknowledge packets
    pub async fn batch_acknowledge_packets(&mut self, sequences: &[u32]) -> usize {
        let mut acked_count = 0;
        let mut timers_to_cancel = Vec::new();
        
        for &seq in sequences {
            if let Some(packet) = self.in_flight_packets.remove(&seq) {
                if let Some(timer_id) = packet.timer_id {
                    self.timer_to_sequence.remove(&timer_id);
                    timers_to_cancel.push(timer_id);
                }
                acked_count += 1;
            }
        }
        
        // 批量取消定时器
        // Batch cancel timers
        if !timers_to_cancel.is_empty() {
            let cancelled_count = self.timer_actor.batch_cancel_timers_by_ids(timers_to_cancel).await;
            trace!(
                acked_packets = acked_count,
                cancelled_timers = cancelled_count,
                "Batch acknowledged packets and cancelled timers"
            );
        }
        
        acked_count
    }
    
    /// 获取在途数据包数量
    /// Get in-flight packet count
    pub fn in_flight_count(&self) -> usize {
        self.in_flight_packets.len()
    }
    
    /// 检查是否为空
    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.in_flight_packets.is_empty()
    }
    
    /// 获取所有在途数据包的序列号
    /// Get all in-flight packet sequence numbers
    pub fn get_in_flight_sequences(&self) -> Vec<u32> {
        self.in_flight_packets.keys().copied().collect()
    }
    
    /// 清理所有在途数据包
    /// Clear all in-flight packets
    pub async fn clear(&mut self) {
        let timer_ids: Vec<ActorTimerId> = self.timer_to_sequence.keys().copied().collect();
        
        if !timer_ids.is_empty() {
            let cancelled_count = self.timer_actor.batch_cancel_timers_by_ids(timer_ids).await;
            debug!(cancelled_timers = cancelled_count, "Cleared all in-flight packets and timers");
        }
        
        self.in_flight_packets.clear();
        self.timer_to_sequence.clear();
    }
    
    /// 调度重传定时器（使用新的基于数据包的超时事件）
    /// Schedule retransmission timer (using new packet-based timeout event)
    async fn schedule_retransmission_timer(&mut self, seq: u32, rto: Duration) -> Result<ActorTimerId, String> {
        // 使用新的基于数据包的重传超时定时器注册，传递正确的回调通道
        // Use new packet-based retransmission timeout timer registration with correct callback channel
        // 使用PacketRetransmissionTimeout事件
        // Use PacketRetransmissionTimeout event
        let timeout_event = TimeoutEvent::PacketRetransmissionTimeout {
            sequence_number: seq,
            timer_id: 0, // 这里先设为0，实际的timer_id会由Actor分配
        };
        
        // 创建定时器注册
        // Create timer registration
        let registration = SenderTimerRegistration::with_sender(
            self.connection_id,
            rto,
            timeout_event,
            self.timeout_tx.clone(),
        );
        
        match self.timer_actor.register_timer(registration).await {
            Ok(timer_id) => {
                trace!(
                    seq = seq,
                    timer_id = timer_id,
                    "Scheduled packet retransmission timer with new event structure"
                );
                Ok(timer_id)
            }
            Err(e) => {
                warn!(seq = seq, error = e, "Failed to schedule packet retransmission timer");
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timer::start_hybrid_timer_task;
    use tokio::time::Duration;
    
    #[tokio::test]
    async fn test_packet_timer_manager_basic() {
        let timer_handle = start_hybrid_timer_task();
        let timer_actor = crate::timer::actor::start_sender_timer_actor(timer_handle.clone(), None);
        let (timeout_tx, _timeout_rx) = mpsc::channel(32);
        let mut manager = PacketTimerManager::new(123, timer_actor, timeout_tx);
        
        // 创建测试帧
        let frame = Frame::new_push(100, 1, 1, 1024, 0, b"test".to_vec().into());
        let now = Instant::now();
        let rto = Duration::from_millis(100);
        
        // 添加数据包
        let added = manager.add_packet(&frame, now, rto).await;
        assert!(added);
        assert_eq!(manager.in_flight_count(), 1);
        
        // 确认数据包
        let acked = manager.acknowledge_packet(1).await;
        assert!(acked);
        assert_eq!(manager.in_flight_count(), 0);
        
        timer_handle.shutdown().await.unwrap();
    }
    
    #[tokio::test]
    async fn test_packet_timer_manager_retransmission() {
        let timer_handle = start_hybrid_timer_task();
        let timer_actor = crate::timer::actor::start_sender_timer_actor(timer_handle.clone(), None);
        let (timeout_tx, _timeout_rx) = mpsc::channel(32);
        let mut manager = PacketTimerManager::new(123, timer_actor, timeout_tx);
        
        // 创建测试帧和上下文
        let frame = Frame::new_push(100, 1, 1, 1024, 0, b"test".to_vec().into());
        let now = Instant::now();
        let rto = Duration::from_millis(10); // 短超时便于测试
        
        let _context = RetransmissionContext {
            local_cid: 100,
            current_peer_cid: 200,
            protocol_version: 1,
            recv_next_sequence: 1,
            recv_window_size: 1024,
            start_time: now,
        };
        
        // 添加数据包
        manager.add_packet(&frame, now, rto).await;
        
        // 等待定时器触发
        tokio::time::sleep(Duration::from_millis(20)).await;
        
        // 注意：在实际测试中，我们需要模拟定时器事件的触发
        // 这里只是验证数据结构的正确性
        assert_eq!(manager.in_flight_count(), 1);
        
        timer_handle.shutdown().await.unwrap();
    }
}