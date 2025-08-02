//! Unified Retransmission Manager
//!
//! This module provides a unified interface for managing both SACK-based
//! reliable transmission and simple timeout-based retransmission.
//!
//! 统一重传管理器
//!
//! 此模块提供统一接口来管理基于SACK的可靠传输和基于超时的简单重传。

pub mod rtt;
mod sack_manager;
mod simple_retx_manager;

pub use sack_manager::RetransmissionFrameInfo;
use self::{sack_manager::SackManager, simple_retx_manager::SimpleRetransmissionManager};
use crate::packet::frame::{FrameType, RetransmissionContext};

use crate::{
    config::Config,
    packet::{
        frame::{Frame, ReliabilityMode},
        sack::SackRange,
    },
};
use std::time::Duration;
use tokio::time::Instant;
use tracing::{debug, trace};

/// Result of processing an ACK with both SACK and simple retransmission.
/// 使用SACK和简单重传处理ACK的结果。
#[derive(Debug)]
pub struct RetransmissionProcessResult {
    /// Frames that need to be retransmitted.
    /// 需要重传的帧。
    pub frames_to_retransmit: Vec<Frame>,
    /// RTT samples from SACK-managed packets.
    /// 来自SACK管理数据包的RTT样本。
    pub rtt_samples: Vec<Duration>,
    /// Sequence numbers that were newly acknowledged.
    /// 新确认的序列号。
    pub newly_acked_sequences: Vec<u32>,
}

/// Unified retransmission manager that handles both SACK and simple retransmission.
/// 处理SACK和简单重传的统一重传管理器。
#[derive(Debug)]
pub struct RetransmissionManager {
    /// SACK manager for reliable transmission.
    /// 用于可靠传输的SACK管理器。
    sack_manager: SackManager,
    /// Simple retransmission manager.
    /// 简单重传管理器。
    simple_retx_manager: SimpleRetransmissionManager,
    /// Configuration for determining retransmission modes.
    /// 用于确定重传模式的配置。
    config: Config,
}

impl RetransmissionManager {
    /// Creates a new unified retransmission manager.
    /// 创建新的统一重传管理器。
    pub fn new(config: Config) -> Self {
        let sack_manager = SackManager::new(
            config.reliability.fast_retx_threshold,
            config.reliability.ack_threshold as u32,
        );
        let simple_retx_manager = SimpleRetransmissionManager::new();

        Self {
            sack_manager,
            simple_retx_manager,
            config,
        }
    }

    /// Adds a packet to the appropriate retransmission tracking.
    /// 将数据包添加到适当的重传跟踪。
    pub fn add_in_flight_packet(&mut self, frame: Frame, now: Instant) {
        let reliability_mode = frame.reliability_mode_with_config(&self.config);

        match reliability_mode {
            ReliabilityMode::Reliable => {
                trace!(
                    seq = frame.sequence_number(),
                    "Adding frame to SACK-based retransmission"
                );
                self.sack_manager.add_in_flight_packet(frame, now);
            }
            ReliabilityMode::SimpleRetransmit { .. } => {
                trace!(
                    seq = frame.sequence_number(),
                    "Adding frame to simple retransmission"
                );
                self.simple_retx_manager.add_packet(frame, now);
            }
            ReliabilityMode::BestEffort => {
                trace!(
                    seq = frame.sequence_number(),
                    "Frame uses best-effort delivery, not tracking for retransmission"
                );
                // No tracking needed for best-effort frames
            }
        }
    }

    /// Processes an incoming ACK with both SACK and simple retransmission logic.
    /// 使用SACK和简单重传逻辑处理传入的ACK。
    pub fn process_ack(
        &mut self,
        recv_next_seq: u32,
        sack_ranges: &[SackRange],
        now: Instant,
        context: &RetransmissionContext,
    ) -> RetransmissionProcessResult {
        // Process SACK-managed packets
        // 处理SACK管理的数据包
        let sack_result = self.sack_manager.process_ack(recv_next_seq, sack_ranges, now, context);

        // Process simple retransmission packets
        // 处理简单重传数据包
        let mut simple_acked = self.simple_retx_manager.acknowledge_up_to(recv_next_seq);
        let sack_acked = self
            .simple_retx_manager
            .acknowledge_sack_ranges(sack_ranges);
        simple_acked.extend(sack_acked);

        // Combine results
        // 合并结果
        let sack_acked_count = sack_result.newly_acked_sequences.len();
        let simple_acked_count = simple_acked.len();

        let mut all_newly_acked = sack_result.newly_acked_sequences;
        all_newly_acked.extend(simple_acked);

        debug!(
            cumulative_ack = recv_next_seq,
            sack_ranges_count = sack_ranges.len(),
            sack_acked_count,
            simple_acked_count,
            rtt_samples_count = sack_result.rtt_samples.len(),
            "Processed ACK with unified retransmission manager"
        );

        RetransmissionProcessResult {
            frames_to_retransmit: sack_result.frames_to_retransmit,
            rtt_samples: sack_result.rtt_samples,
            newly_acked_sequences: all_newly_acked,
        }
    }

    /// Checks for retransmissions in both SACK and simple managers with frame reconstruction.
    /// 检查SACK和简单管理器中的重传，并重构帧。
    pub fn check_for_retransmissions(
        &mut self, 
        rto: Duration, 
        now: Instant, 
        context: &RetransmissionContext,
    ) -> Vec<Frame> {
        let mut frames_to_retx = Vec::new();

        // Check SACK-based retransmissions with frame reconstruction
        // 检查基于SACK的重传并重构帧
        let sack_retx = self.sack_manager.check_for_rto(rto, now, context);
        frames_to_retx.extend(sack_retx);

        // Check simple retransmissions with frame reconstruction
        // 检查简单重传并使用帧重构
        let simple_retx = self.simple_retx_manager.check_for_retransmissions(now, context);
        frames_to_retx.extend(simple_retx);

        if !frames_to_retx.is_empty() {
            debug!(
                retx_count = frames_to_retx.len(),
                current_peer_cid = context.current_peer_cid,
                "Retransmissions triggered by unified manager with frame reconstruction"
            );
        }

        frames_to_retx
    }

    /// Returns the earliest deadline for any retransmission check.
    /// 返回任何重传检查的最早截止时间。
    pub fn next_retransmission_deadline(&self, rto: Duration) -> Option<Instant> {
        let sack_deadline = self.sack_manager.next_rto_deadline(rto);
        let simple_deadline = self.simple_retx_manager.next_retransmission_deadline();

        match (sack_deadline, simple_deadline) {
            (Some(sack), Some(simple)) => Some(sack.min(simple)),
            (Some(sack), None) => Some(sack),
            (None, Some(simple)) => Some(simple),
            (None, None) => None,
        }
    }

    /// Returns the total number of packets in flight across both managers.
    /// 返回两个管理器中在途数据包的总数。
    pub fn total_in_flight_count(&self) -> usize {
        self.sack_manager.in_flight_count() + self.simple_retx_manager.in_flight_count()
    }

    /// Returns the number of packets managed by SACK (for congestion control).
    /// 返回由SACK管理的数据包数量（用于拥塞控制）。
    pub fn sack_in_flight_count(&self) -> usize {
        self.sack_manager.in_flight_count()
    }

    /// Checks if both managers have no packets in flight.
    /// 检查两个管理器是否都没有在途数据包。
    pub fn is_all_in_flight_empty(&self) -> bool {
        self.sack_manager.is_in_flight_empty() && self.simple_retx_manager.is_empty()
    }

    /// Checks if SACK manager has no packets in flight.
    /// 检查SACK管理器是否没有在途数据包。
    pub fn is_sack_in_flight_empty(&self) -> bool {
        self.sack_manager.is_in_flight_empty()
    }

    /// Checks if a FIN frame is in flight in either manager.
    /// 检查任一管理器中是否有FIN帧在途。
    pub fn has_fin_in_flight(&self) -> bool {
        self.sack_manager.has_fin_in_flight()
            || self
                .simple_retx_manager
                .has_frame_type_in_flight(FrameType::Fin)
    }

    /// Increments the ACK-eliciting packet counter (delegated to SACK manager).
    /// 增加ACK触发数据包计数器（委托给SACK管理器）。
    pub fn on_ack_eliciting_packet_received(&mut self) {
        self.sack_manager.on_ack_eliciting_packet_received();
    }

    /// Resets the ACK-eliciting packet counter (delegated to SACK manager).
    /// 重置ACK触发数据包计数器（委托给SACK管理器）。
    pub fn on_ack_sent(&mut self) {
        self.sack_manager.on_ack_sent();
    }

    /// Determines if a standalone ACK should be sent.
    /// 确定是否应发送独立ACK。
    pub fn should_send_standalone_ack(&self, sack_ranges: &[SackRange]) -> bool {
        self.sack_manager.should_send_standalone_ack(sack_ranges)
    }

    /// Encodes SACK ranges (delegated to SACK manager).
    /// 编码SACK范围（委托给SACK管理器）。
    pub fn encode_sack_ranges(&self, ranges: &[SackRange]) -> bytes::Bytes {
        self.sack_manager.encode_sack_ranges(ranges)
    }

    /// Decodes SACK ranges (delegated to SACK manager).
    /// 解码SACK范围（委托给SACK管理器）。
    pub fn decode_sack_ranges(&self, payload: bytes::Bytes) -> Vec<SackRange> {
        self.sack_manager.decode_sack_ranges(payload)
    }

    /// Clears all in-flight packets from both managers.
    ///
    /// 从两个管理器中清除所有在途数据包。
    pub fn clear(&mut self) {
        self.sack_manager.clear();
        self.simple_retx_manager.clear();
        debug!("Cleared all in-flight packets from both SACK and simple retransmission managers.");
    }

    /// 获取下一个重传超时的截止时间
    /// Get the deadline for the next retransmission timeout
    ///
    /// 该方法计算所有重传相关超时中最早的截止时间，用于事件循环的等待时间优化。
    /// 这是分层超时管理架构中重传层的截止时间计算接口。
    ///
    /// This method calculates the earliest deadline among all retransmission-related
    /// timeouts, used for optimizing event loop wait times. This is the deadline
    /// calculation interface for the retransmission layer in layered timeout management.
    pub fn next_retransmission_timeout_deadline(&self, rto: Duration) -> Option<Instant> {
        // 委托给现有的方法
        // Delegate to existing method
        self.next_retransmission_deadline(rto)
    }

    /// 检查是否有重传超时
    /// Check if there are retransmission timeouts
    ///
    /// 该方法检查是否有数据包需要重传，但不返回实际的帧数据。
    /// 这用于分离超时检查和重传帧获取的职责。
    ///
    /// This method checks if there are packets that need retransmission, but doesn't
    /// return the actual frame data. This is used to separate timeout checking from
    /// frame retrieval responsibilities.
    pub fn has_retransmission_timeout(&self, rto: Duration, now: Instant) -> bool {
        // 检查SACK管理器是否有超时的数据包
        // Check if SACK manager has timed out packets
        let sack_deadline = self.sack_manager.next_rto_deadline(rto);
        if let Some(deadline) = sack_deadline {
            if now >= deadline {
                return true;
            }
        }

        // 检查简单重传管理器是否有超时的数据包
        // Check if simple retransmission manager has timed out packets
        let simple_deadline = self.simple_retx_manager.next_retransmission_deadline();
        if let Some(deadline) = simple_deadline {
            if now >= deadline {
                return true;
            }
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::frame::Frame;
    use bytes::Bytes;
    
    // Helper function for testing retransmissions
    fn test_check_retransmissions(manager: &mut RetransmissionManager, rto: Duration, now: Instant, peer_cid: u32) -> Vec<Frame> {
        let context = RetransmissionContext::new(now, peer_cid, 1, 1, 0, 1024);
        manager.check_for_retransmissions(rto, now, &context)
    }
    
    fn create_test_config() -> Config {
        Config::default()
    }

    fn create_handshake_push_frame(seq: u32) -> Frame {
        Frame::new_push(
            1,
            seq,
            0,
            1024,
            0,
            Bytes::from(format!("handshake-{}", seq)),
        )
    }

    fn create_regular_push_frame(seq: u32) -> Frame {
        Frame::new_push(1, seq, 0, 1024, 0, Bytes::from(format!("regular-{}", seq)))
    }

    fn create_syn_frame() -> Frame {
        Frame::new_syn(1, 123, 0)
    }

    fn create_fin_frame(seq: u32) -> Frame {
        Frame::new_fin(1, seq, 0, 0, 0)
    }

    #[test]
    fn test_frame_routing_to_correct_manager() {
        let mut manager = RetransmissionManager::new(create_test_config());
        let now = Instant::now();

        // Add handshake data frame (now goes to SACK)
        let handshake_frame = create_handshake_push_frame(0);
        manager.add_in_flight_packet(handshake_frame, now);

        // Add regular data frame (should go to SACK)
        let regular_frame = create_regular_push_frame(10);
        manager.add_in_flight_packet(regular_frame, now);

        // Add control frame (should go to simple retransmission)
        let fin_frame = create_fin_frame(1);
        manager.add_in_flight_packet(fin_frame, now);

        // Check counts
        assert_eq!(manager.sack_in_flight_count(), 2); // Both PUSH frames
        assert_eq!(manager.simple_retx_manager.in_flight_count(), 1); // Only FIN
        assert_eq!(manager.total_in_flight_count(), 3);
    }

    #[test]
    fn test_unified_ack_processing() {
        let mut manager = RetransmissionManager::new(create_test_config());
        let now = Instant::now();

        // Add frames - both PUSH frames go to SACK now
        manager.add_in_flight_packet(create_handshake_push_frame(0), now);
        manager.add_in_flight_packet(create_regular_push_frame(10), now);

        // Process ACK that acknowledges both
        let context = RetransmissionContext::new(now, 12345, 1, 1, 1024, 1024);
        let result = manager.process_ack(11, &[], now, &context);

        // Should acknowledge both frames
        assert!(result.newly_acked_sequences.contains(&0)); // Handshake frame
        assert!(result.newly_acked_sequences.contains(&10)); // Regular frame
        assert_eq!(manager.total_in_flight_count(), 0);
    }

    #[test]
    fn test_unified_retransmission_check() {
        let mut manager = RetransmissionManager::new(create_test_config());
        let now = Instant::now();

        // Add frames that will need retransmission
        manager.add_in_flight_packet(create_fin_frame(1), now);
        manager.add_in_flight_packet(create_regular_push_frame(10), now);

        // Check immediately - no retransmissions yet
        let frames = test_check_retransmissions(&mut manager, Duration::from_secs(1), now, 12345);
        assert!(frames.is_empty());

        // Check after timeout - should trigger retransmissions
        let later = now + Duration::from_secs(2);
        let frames = test_check_retransmissions(&mut manager, Duration::from_secs(1), later, 12345);
        assert_eq!(frames.len(), 2); // Both frames should be retransmitted
    }

    #[test]
    fn test_layered_retransmission_disabled() {
        let mut config = create_test_config();
        config.reliability.enable_layered_retransmission = false;

        let mut manager = RetransmissionManager::new(config);
        let now = Instant::now();

        // Add handshake frame - should go to SACK when layered retransmission is disabled
        let handshake_frame = create_handshake_push_frame(0);
        manager.add_in_flight_packet(handshake_frame, now);

        // Should be in SACK manager, not simple retransmission
        assert_eq!(manager.sack_in_flight_count(), 1);
        assert_eq!(manager.simple_retx_manager.in_flight_count(), 0);
    }

    #[test]
    fn test_next_retransmission_deadline() {
        let mut manager = RetransmissionManager::new(create_test_config());
        let now = Instant::now();

        // No packets - no deadline
        assert!(
            manager
                .next_retransmission_deadline(Duration::from_secs(1))
                .is_none()
        );

        // Add packets to both managers
        manager.add_in_flight_packet(create_syn_frame(), now);
        manager.add_in_flight_packet(create_regular_push_frame(10), now);

        // Should have a deadline
        let deadline = manager.next_retransmission_deadline(Duration::from_secs(1));
        assert!(deadline.is_some());
        assert!(deadline.unwrap() > now);
    }

    #[test]
    fn test_clear() {
        let mut manager = RetransmissionManager::new(create_test_config());
        let now = Instant::now();

        // Add packets to both managers
        manager.add_in_flight_packet(create_fin_frame(1), now);
        manager.add_in_flight_packet(create_regular_push_frame(10), now);

        assert_eq!(manager.total_in_flight_count(), 2);
        manager.clear();
        assert_eq!(manager.total_in_flight_count(), 0);
        assert!(manager.is_all_in_flight_empty());
    }
}
