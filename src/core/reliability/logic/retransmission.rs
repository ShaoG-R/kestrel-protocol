//! 重传决策器 - 专门处理重传决策逻辑
//! Retransmission Decider - Specialized retransmission decision logic
//!
//! 职责：
//! - 决定哪些数据包需要重传
//! - 重构重传帧
//! - 管理重传次数和策略
//! - 丢包检测和处理

use super::super::data::in_flight_store::{InFlightPacket, InFlightPacketStore, PacketState};
use crate::packet::frame::{Frame, RetransmissionContext};
use std::time::Duration;
use tokio::time::Instant;
use tracing::{debug, trace, warn};

pub mod sack_processor;

/// 重传决策结果
/// Retransmission decision result
#[derive(Debug)]
pub struct RetransmissionDecision {
    /// 需要重传的帧
    /// Frames to retransmit
    pub frames_to_retransmit: Vec<Frame>,

    /// 需要丢弃的数据包（达到最大重传次数）
    /// Packets to drop (max retransmissions reached)
    pub packets_to_drop: Vec<u32>,

    /// 需要重新调度定时器的数据包
    /// Packets that need timer rescheduling
    pub packets_to_reschedule: Vec<u32>,
}

/// 重传类型
/// Retransmission type
#[derive(Debug, Clone, PartialEq)]
pub enum RetransmissionType {
    /// 超时重传
    /// Timeout retransmission
    Timeout,
    /// 快速重传
    /// Fast retransmission
    FastRetx,
}

/// 重传决策器
/// Retransmission decider
#[derive(Debug)]
pub struct RetransmissionDecider {
    /// 最大重传次数
    /// Maximum retransmission count
    max_retx_count: u8,
}

impl RetransmissionDecider {
    /// 创建新的重传决策器
    /// Create new retransmission decider
    pub fn new(max_retx_count: u8) -> Self {
        Self { max_retx_count }
    }

    /// 处理超时重传
    /// Handle timeout retransmission
    pub fn handle_timeout_retransmission(
        &self,
        store: &mut InFlightPacketStore,
        seq: u32,
        context: &RetransmissionContext,
        _rto: Duration,
        now: Instant,
    ) -> RetransmissionDecision {
        let mut decision = RetransmissionDecision {
            frames_to_retransmit: Vec::new(),
            packets_to_drop: Vec::new(),
            packets_to_reschedule: Vec::new(),
        };

        if let Some(packet) = store.get_packet_mut(seq) {
            // 定时器事件已明确表明该数据包的RTO已到期。
            // 为避免由于RTO在事件到达与处理之间动态变化而造成的误判，
            // 这里不再重复进行“是否超时”的时间条件检查，仅保留状态与次数检查。
            if self.should_retransmit_on_timer_event(packet) {
                // 执行重传
                let retx_frame = self.perform_retransmission(packet, context, now);
                decision.frames_to_retransmit.push(retx_frame);

                if packet.retx_count >= self.max_retx_count {
                    // 达到最大重传次数，标记为丢弃
                    decision.packets_to_drop.push(seq);
                    warn!(
                        seq = seq,
                        retx_count = packet.retx_count,
                        "Packet dropped after max retransmissions"
                    );
                } else {
                    // 需要重新调度定时器
                    decision.packets_to_reschedule.push(seq);
                }

                debug!(
                    seq = seq,
                    retx_count = packet.retx_count,
                    max_retx = self.max_retx_count,
                    "Timeout retransmission performed"
                );
            }
        }

        decision
    }

    /// 处理快速重传
    /// Handle fast retransmission
    pub fn handle_fast_retransmission(
        &self,
        store: &mut InFlightPacketStore,
        candidates: &[u32],
        context: &RetransmissionContext,
        now: Instant,
    ) -> RetransmissionDecision {
        let mut decision = RetransmissionDecision {
            frames_to_retransmit: Vec::new(),
            packets_to_drop: Vec::new(),
            packets_to_reschedule: Vec::new(),
        };

        for &seq in candidates {
            if let Some(packet) = store.get_packet_mut(seq) {
                // 快速重传不需要检查RTO，直接重传
                if packet.state == PacketState::FastRetxCandidate {
                    let retx_frame = self.perform_retransmission(packet, context, now);
                    decision.frames_to_retransmit.push(retx_frame);

                    // 更新状态为需要重传
                    packet.state = PacketState::NeedsRetx;

                    if packet.retx_count >= self.max_retx_count {
                        decision.packets_to_drop.push(seq);
                        warn!(
                            seq = seq,
                            retx_count = packet.retx_count,
                            "Fast retransmitted packet dropped after max retransmissions"
                        );
                    } else {
                        decision.packets_to_reschedule.push(seq);
                    }

                    debug!(
                        seq = seq,
                        retx_count = packet.retx_count,
                        "Fast retransmission performed"
                    );
                }
            }
        }

        decision
    }

    /// 检查RTO重传
    /// Check RTO retransmission
    pub fn check_rto_retransmission(
        &self,
        store: &mut InFlightPacketStore,
        context: &RetransmissionContext,
        rto: Duration,
        now: Instant,
    ) -> RetransmissionDecision {
        let mut decision = RetransmissionDecision {
            frames_to_retransmit: Vec::new(),
            packets_to_drop: Vec::new(),
            packets_to_reschedule: Vec::new(),
        };

        let all_sequences = store.get_all_sequences();

        for seq in all_sequences {
            if let Some(packet) = store.get_packet_mut(seq) {
                if self.should_retransmit_packet(packet, rto, now) {
                    let retx_frame = self.perform_retransmission(packet, context, now);
                    decision.frames_to_retransmit.push(retx_frame);

                    if packet.retx_count >= self.max_retx_count {
                        decision.packets_to_drop.push(seq);
                    } else {
                        decision.packets_to_reschedule.push(seq);
                    }
                }
            }
        }

        if !decision.frames_to_retransmit.is_empty() {
            debug!(
                retx_count = decision.frames_to_retransmit.len(),
                drop_count = decision.packets_to_drop.len(),
                "RTO retransmission check completed"
            );
        }

        decision
    }

    /// 判断数据包是否应该重传
    /// Determine if packet should be retransmitted
    fn should_retransmit_packet(
        &self,
        packet: &InFlightPacket,
        rto: Duration,
        now: Instant,
    ) -> bool {
        // 检查是否超时且未达到最大重传次数
        let is_timeout = now.duration_since(packet.last_sent_at) >= rto;
        let can_retransmit = packet.retx_count < self.max_retx_count;
        let is_sent_state =
            packet.state == PacketState::Sent || packet.state == PacketState::NeedsRetx;

        is_timeout && can_retransmit && is_sent_state
    }

    /// 针对定时器事件的重传判断（不重复时间条件检查）
    /// Retransmission decision for timer events (without re-checking time condition)
    fn should_retransmit_on_timer_event(&self, packet: &InFlightPacket) -> bool {
        let can_retransmit = packet.retx_count < self.max_retx_count;
        let is_sent_state =
            packet.state == PacketState::Sent || packet.state == PacketState::NeedsRetx;
        can_retransmit && is_sent_state
    }

    /// 执行重传
    /// Perform retransmission
    fn perform_retransmission(
        &self,
        packet: &mut InFlightPacket,
        context: &RetransmissionContext,
        now: Instant,
    ) -> Frame {
        // 增加重传次数
        packet.retx_count += 1;
        packet.last_sent_at = now;
        packet.state = PacketState::Sent;

        // 重构帧
        packet.frame_info.reconstruct_frame(context, now)
    }

    /// 清理丢弃的数据包
    /// Clean up dropped packets
    pub fn cleanup_dropped_packets(
        &self,
        store: &mut InFlightPacketStore,
        dropped_sequences: &[u32],
    ) {
        for &seq in dropped_sequences {
            store.remove_packet(seq);
            trace!(seq = seq, "Dropped packet removed from store");
        }

        if !dropped_sequences.is_empty() {
            debug!(
                dropped_count = dropped_sequences.len(),
                "Cleaned up dropped packets"
            );
        }
    }

    /// 获取需要重传的数据包数量
    /// Get count of packets needing retransmission
    pub fn get_retx_needed_count(&self, store: &InFlightPacketStore) -> usize {
        store.get_packets_by_state(PacketState::NeedsRetx).len()
    }

    /// 获取快速重传候选数量
    /// Get fast retransmission candidate count
    pub fn get_fast_retx_candidate_count(&self, store: &InFlightPacketStore) -> usize {
        store
            .get_packets_by_state(PacketState::FastRetxCandidate)
            .len()
    }
}
