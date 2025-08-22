//! 数据包协调器 - 统一协调所有数据包管理组件
//! Packet Coordinator - Unified coordination of all packet management components
//!
//! 职责：
//! - 协调数据层、逻辑层的交互
//! - 提供统一的数据包管理接口
//! - 管理组件间的数据流和状态同步
//! - 执行复合操作和事务性操作

use super::super::{
    data::in_flight_store::{InFlightPacket, InFlightPacketStore, PacketState},
    logic::{retransmission::RetransmissionDecider, timer_event_handler::TimerEventHandler},
};
use crate::core::reliability::logic::retransmission::sack_processor::SackProcessor;
use crate::{
    core::endpoint::timing::TimeoutEvent,
    packet::{
        frame::{Frame, FrameType, RetransmissionContext},
        sack::SackRange,
    },
    timer::{
        actor::{ActorTimerId, SenderTimerActorHandle},
        event::{ConnectionId, TimerEventData},
    },
};
use bytes::Bytes;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, info, trace, warn};

/// Represents essential frame information for retransmission without storing complete header
/// 表示重传所需的基本帧信息，不存储完整的header
#[derive(Debug, Clone)]
pub struct RetransmissionFrameInfo {
    /// Frame type for reconstruction
    /// 用于重构的帧类型
    pub frame_type: FrameType,
    /// Original sequence number
    /// 原始序列号
    pub sequence_number: u32,
    /// Frame payload (empty for control frames)
    /// 帧载荷（控制帧为空）
    pub payload: Bytes,
    /// Additional data for specific frame types (e.g., challenge_data for PATH_CHALLENGE)
    /// 特定帧类型的附加数据（例如PATH_CHALLENGE的challenge_data）
    pub additional_data: Option<u64>,
}

impl RetransmissionFrameInfo {
    /// Creates retransmission info from a frame, extracting only essential information
    /// 从帧创建重传信息，只提取必要信息
    pub fn from_frame(frame: &Frame) -> Option<Self> {
        let frame_type = frame.frame_type()?; // Return None if frame type is not retransmittable
        let (payload, additional_data, sequence_number) = match frame {
            Frame::Push { header, payload } => (payload.clone(), None, header.sequence_number),
            Frame::Syn { .. } => (
                Bytes::new(),
                None,
                0, // SYN frames don't have sequence numbers in our design
            ),
            Frame::SynAck { .. } => (
                Bytes::new(),
                None,
                0, // SYN-ACK frames don't have sequence numbers in our design
            ),
            Frame::Fin { header } => (Bytes::new(), None, header.sequence_number),
            Frame::PathChallenge {
                header,
                challenge_data,
            } => (Bytes::new(), Some(*challenge_data), header.sequence_number),
            Frame::PathResponse {
                header,
                challenge_data,
            } => (Bytes::new(), Some(*challenge_data), header.sequence_number),
            // ACK and PING frames should not be stored for retransmission
            _ => return None,
        };
        Some(Self {
            frame_type,
            sequence_number,
            payload,
            additional_data,
        })
    }

    /// Reconstructs a complete frame with fresh header information
    /// 使用新鲜的header信息重构完整帧
    pub fn reconstruct_frame(&self, context: &RetransmissionContext, now: Instant) -> Frame {
        let current_timestamp = context.current_timestamp(now);
        match self.frame_type {
            FrameType::Push => Frame::new_push(
                context.current_peer_cid,
                self.sequence_number,
                context.recv_next_sequence,
                context.recv_window_size,
                current_timestamp,
                self.payload.clone(),
            ),
            FrameType::Syn => Frame::new_syn(
                context.protocol_version,
                context.local_cid,
                context.current_peer_cid,
            ),
            FrameType::SynAck => Frame::new_syn_ack(
                context.protocol_version,
                context.local_cid,
                context.current_peer_cid,
            ),
            FrameType::Fin => Frame::new_fin(
                context.current_peer_cid,
                self.sequence_number,
                current_timestamp,
                context.recv_next_sequence,
                context.recv_window_size,
            ),
            FrameType::PathChallenge => Frame::new_path_challenge(
                context.current_peer_cid,
                self.sequence_number,
                current_timestamp,
                self.additional_data.unwrap_or(0),
            ),
            FrameType::PathResponse => Frame::new_path_response(
                context.current_peer_cid,
                self.sequence_number,
                current_timestamp,
                self.additional_data.unwrap_or(0),
            ),
        }
    }
}
/// 综合处理结果
/// Comprehensive processing result
#[derive(Debug)]
pub struct ComprehensiveResult {
    /// 需要重传的帧
    /// Frames to retransmit
    pub frames_to_retransmit: Vec<Frame>,

    /// 新确认的序列号
    /// Newly acknowledged sequence numbers
    pub newly_acked_sequences: Vec<u32>,

    /// RTT样本
    /// RTT samples
    pub rtt_samples: Vec<Duration>,

    /// 已处理的数据包数量
    /// Number of processed packets
    pub processed_packet_count: usize,
}

/// 数据包协调器
/// Packet coordinator
#[derive(Debug)]
pub struct PacketCoordinator {
    /// 数据存储层
    /// Data storage layer
    store: InFlightPacketStore,

    /// SACK处理器
    /// SACK processor
    sack_processor: SackProcessor,

    /// 重传决策器
    /// Retransmission decider
    retransmission_decider: RetransmissionDecider,

    /// 定时器事件处理器
    /// Timer event handler
    timer_event_handler: TimerEventHandler,
}

impl PacketCoordinator {
    /// 创建新的数据包协调器
    /// Create new packet coordinator
    pub fn new(
        connection_id: ConnectionId,
        timer_actor: SenderTimerActorHandle,
        timeout_tx: mpsc::Sender<TimerEventData<TimeoutEvent>>,
        fast_retx_threshold: u8,
        max_retx_count: u8,
    ) -> Self {
        Self {
            store: InFlightPacketStore::new(),
            sack_processor: SackProcessor::new(fast_retx_threshold),
            retransmission_decider: RetransmissionDecider::new(max_retx_count),
            timer_event_handler: TimerEventHandler::new(connection_id, timer_actor, timeout_tx),
        }
    }

    /// 添加数据包到管理
    /// Add packet to management
    pub async fn add_packet(&mut self, frame: &Frame, now: Instant, rto: Duration) -> bool {
        if let Some(seq) = frame.sequence_number() {
            if let Some(frame_info) = RetransmissionFrameInfo::from_frame(frame) {
                // 创建在途数据包
                let packet = InFlightPacket {
                    frame_info,
                    last_sent_at: now,
                    retx_count: 0,
                    max_retx_count: 5, // 可配置
                    timer_id: None,
                    state: PacketState::Sent,
                    fast_retx_count: 0,
                };

                // 添加到存储
                self.store.add_packet(seq, packet);

                // 调度重传定时器
                match self
                    .timer_event_handler
                    .schedule_retransmission_timer(&mut self.store, seq, rto)
                    .await
                {
                    Ok(timer_id) => {
                        trace!(
                            seq = seq,
                            timer_id = timer_id,
                            rto_ms = rto.as_millis(),
                            "Successfully added packet with timer"
                        );
                        return true;
                    }
                    Err(e) => {
                        warn!(
                            seq = seq,
                            error = e,
                            "Failed to schedule timer for packet, but keeping packet"
                        );
                        // 即使定时器失败，仍然保留数据包
                        return true;
                    }
                }
            }
        }
        false
    }

    /// 处理ACK并获取综合结果
    /// Process ACK and get comprehensive result
    pub async fn process_ack_comprehensive(
        &mut self,
        recv_next_seq: u32,
        sack_ranges: Vec<SackRange>,
        now: Instant,
        context: &RetransmissionContext,
    ) -> ComprehensiveResult {
        // 步骤1：使用SACK处理器处理ACK
        let sack_result =
            self.sack_processor
                .process_sack(&mut self.store, recv_next_seq, &sack_ranges, now);

        // 步骤2：收集所有需要确认的序列号（优化：避免不必要的克隆）
        let all_acked: Vec<u32> = sack_result
            .cumulative_acked
            .iter()
            .chain(sack_result.sack_acked.iter())
            .copied()
            .collect();

        // 步骤3：批量移除已确认的数据包并取消定时器
        let removed_packets = self.store.batch_remove_packets(&all_acked);
        if !removed_packets.is_empty() {
            // 直接使用all_acked而不是重新提取序列号
            self.timer_event_handler
                .batch_cancel_retransmission_timers(&mut self.store, &all_acked)
                .await;
        }

        // 步骤4：处理快速重传
        let mut fast_retx_frames = Vec::new();
        if !sack_result.fast_retx_candidates.is_empty() {
            // 更新快速重传候选状态
            self.sack_processor
                .update_fast_retx_candidates(&mut self.store, &sack_result.fast_retx_candidates);

            // 执行快速重传
            let fast_retx_decision = self.retransmission_decider.handle_fast_retransmission(
                &mut self.store,
                &sack_result.fast_retx_candidates,
                context,
                now,
            );

            fast_retx_frames = fast_retx_decision.frames_to_retransmit;
        }

        let result = ComprehensiveResult {
            frames_to_retransmit: fast_retx_frames,
            newly_acked_sequences: all_acked,
            rtt_samples: sack_result.rtt_samples,
            processed_packet_count: removed_packets.len(),
        };

        debug!(
            cumulative_acked = sack_result.cumulative_acked.len(),
            sack_acked = sack_result.sack_acked.len(),
            fast_retx_frames = result.frames_to_retransmit.len(),
            rtt_samples = result.rtt_samples.len(),
            "ACK processing completed"
        );

        result
    }

    /// 处理定时器超时事件
    /// Handle timer timeout event
    pub async fn handle_timer_timeout(
        &mut self,
        timer_id: ActorTimerId,
        context: &RetransmissionContext,
        rto: Duration,
        now: Instant,
    ) -> Option<Frame> {
        // 步骤1：使用定时器事件处理器处理超时
        let timer_result = self
            .timer_event_handler
            .handle_timer_timeout(&mut self.store, timer_id);

        if let Some(seq) = timer_result.sequence_number {
            if timer_result.needs_retransmission_handling {
                // 步骤2：使用重传决策器处理重传
                let retx_decision = self.retransmission_decider.handle_timeout_retransmission(
                    &mut self.store,
                    seq,
                    context,
                    rto,
                    now,
                );

                // 步骤3：处理决策结果
                if !retx_decision.packets_to_drop.is_empty() {
                    // 清理达到最大重传次数的数据包
                    self.retransmission_decider
                        .cleanup_dropped_packets(&mut self.store, &retx_decision.packets_to_drop);
                }

                if !retx_decision.packets_to_reschedule.is_empty() {
                    // 重新调度需要继续重传的数据包
                    for &reschedule_seq in &retx_decision.packets_to_reschedule {
                        if let Err(e) = self
                            .timer_event_handler
                            .reschedule_retransmission_timer(&mut self.store, reschedule_seq, rto)
                            .await
                        {
                            warn!(
                                seq = reschedule_seq,
                                error = e,
                                "Failed to reschedule timer after retransmission"
                            );
                        }
                    }
                }

                // 返回第一个需要重传的帧
                return retx_decision.frames_to_retransmit.into_iter().next();
            }
        }

        None
    }

    /// 检查RTO重传
    /// Check RTO retransmission  
    pub async fn check_rto_retransmission(
        &mut self,
        context: &RetransmissionContext,
        rto: Duration,
        now: Instant,
    ) -> Vec<Frame> {
        let retx_decision = self.retransmission_decider.check_rto_retransmission(
            &mut self.store,
            context,
            rto,
            now,
        );

        // 处理丢弃的数据包
        if !retx_decision.packets_to_drop.is_empty() {
            self.retransmission_decider
                .cleanup_dropped_packets(&mut self.store, &retx_decision.packets_to_drop);
        }

        // 重新调度需要继续的定时器
        if !retx_decision.packets_to_reschedule.is_empty() {
            for &seq in &retx_decision.packets_to_reschedule {
                if let Err(e) = self
                    .timer_event_handler
                    .reschedule_retransmission_timer(&mut self.store, seq, rto)
                    .await
                {
                    warn!(
                        seq = seq,
                        error = e,
                        "Failed to reschedule timer after RTO check"
                    );
                }
            }
        }

        retx_decision.frames_to_retransmit
    }

    /// 获取在途数据包数量
    /// Get in-flight packet count
    pub fn in_flight_count(&self) -> usize {
        self.store.count()
    }

    /// 检查是否为空
    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }

    /// 清理所有数据包和定时器
    /// Clean up all packets and timers
    pub async fn cleanup_all(&mut self) {
        // 清理所有定时器
        self.timer_event_handler
            .cleanup_all_timers(&mut self.store)
            .await;

        // 清理所有数据包
        self.store.clear();

        info!("Cleaned up all packets and timers");
    }

    /// 获取统计信息
    /// Get statistics
    pub fn get_statistics(&self) -> PacketCoordinatorStats {
        PacketCoordinatorStats {
            total_in_flight: self.store.count(),
            needs_retx_count: self
                .retransmission_decider
                .get_retx_needed_count(&self.store),
            fast_retx_candidates: self
                .retransmission_decider
                .get_fast_retx_candidate_count(&self.store),
            active_timers: self.timer_event_handler.get_active_timer_count(&self.store),
        }
    }
}

/// 数据包协调器统计信息
/// Packet coordinator statistics
#[derive(Debug, Clone)]
pub struct PacketCoordinatorStats {
    /// 总在途数据包数
    /// Total in-flight packets
    pub total_in_flight: usize,

    /// 需要重传的数据包数
    /// Packets needing retransmission
    pub needs_retx_count: usize,

    /// 快速重传候选数
    /// Fast retransmission candidates
    pub fast_retx_candidates: usize,

    /// 活跃定时器数
    /// Active timers
    pub active_timers: usize,
}
