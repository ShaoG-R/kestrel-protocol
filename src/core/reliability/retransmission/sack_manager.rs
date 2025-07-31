//! SACK (Selective Acknowledgment) Manager
//! 
//! This module centralizes all SACK-related logic including:
//! - SACK range generation from receive buffer
//! - SACK information processing for send buffer
//! - SACK encoding/decoding coordination
//! - SACK-based retransmission logic
//!
//! SACK（选择性确认）管理器
//!
//! 此模块集中了所有SACK相关逻辑，包括：
//! - 从接收缓冲区生成SACK范围
//! - 为发送缓冲区处理SACK信息
//! - SACK编解码协调
//! - 基于SACK的重传逻辑

use crate::packet::{frame::Frame, sack::SackRange};
use bytes::Bytes;
use std::collections::BTreeMap;
use tokio::time::{Duration, Instant};
use tracing::{debug, trace};

/// Represents the result of processing an ACK with SACK information
#[derive(Debug)]
pub struct SackProcessResult {
    /// Frames that need to be retransmitted due to fast retransmission
    pub frames_to_retransmit: Vec<Frame>,
    /// RTT samples calculated from newly acknowledged packets
    pub rtt_samples: Vec<Duration>,
    /// Sequence numbers that were newly acknowledged
    pub newly_acked_sequences: Vec<u32>,
}

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

/// Frame type enum for retransmission reconstruction
/// 用于重传重构的帧类型枚举
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameType {
    Push,
    Syn,
    SynAck,
    Fin,
    PathChallenge,
    PathResponse,
}

impl RetransmissionFrameInfo {
    /// Creates retransmission info from a frame, extracting only essential information
    /// 从帧创建重传信息，只提取必要信息
    pub fn from_frame(frame: &Frame) -> Self {
        let (frame_type, payload, additional_data, sequence_number) = match frame {
            Frame::Push { header, payload } => (
                FrameType::Push,
                payload.clone(),
                None,
                header.sequence_number,
            ),
            Frame::Syn { .. } => (
                FrameType::Syn,
                Bytes::new(),
                None,
                0, // SYN frames don't have sequence numbers in our design
            ),
            Frame::SynAck { .. } => (
                FrameType::SynAck,
                Bytes::new(),
                None,
                0, // SYN-ACK frames don't have sequence numbers in our design
            ),
            Frame::Fin { header } => (
                FrameType::Fin,
                Bytes::new(),
                None,
                header.sequence_number,
            ),
            Frame::PathChallenge { header, challenge_data } => (
                FrameType::PathChallenge,
                Bytes::new(),
                Some(*challenge_data),
                header.sequence_number,
            ),
            Frame::PathResponse { header, challenge_data } => (
                FrameType::PathResponse,
                Bytes::new(),
                Some(*challenge_data),
                header.sequence_number,
            ),
            // ACK and PING frames should not be stored for retransmission
            _ => panic!("Unsupported frame type for retransmission storage"),
        };

        Self {
            frame_type,
            sequence_number,
            payload,
            additional_data,
        }
    }

    /// Reconstructs a complete frame with fresh header information
    /// 使用新鲜的header信息重构完整帧
    pub fn reconstruct_frame(
        &self,
        current_peer_cid: u32,
        protocol_version: u8,
        local_cid: u32,
        recv_next_sequence: u32,
        recv_window_size: u16,
        current_timestamp: u32,
    ) -> Frame {
        match self.frame_type {
            FrameType::Push => {
                Frame::new_push(
                    current_peer_cid,
                    self.sequence_number,
                    recv_next_sequence,
                    recv_window_size,
                    current_timestamp,
                    self.payload.clone(),
                )
            }
            FrameType::Syn => {
                Frame::new_syn(protocol_version, local_cid, current_peer_cid)
            }
            FrameType::SynAck => {
                Frame::new_syn_ack(protocol_version, local_cid, current_peer_cid)
            }
            FrameType::Fin => {
                Frame::new_fin(
                    current_peer_cid,
                    self.sequence_number,
                    current_timestamp,
                    recv_next_sequence,
                    recv_window_size,
                )
            }
            FrameType::PathChallenge => {
                Frame::new_path_challenge(
                    current_peer_cid,
                    self.sequence_number,
                    current_timestamp,
                    self.additional_data.unwrap_or(0),
                )
            }
            FrameType::PathResponse => {
                Frame::new_path_response(
                    current_peer_cid,
                    self.sequence_number,
                    current_timestamp,
                    self.additional_data.unwrap_or(0),
                )
            }
        }
    }
}

/// Represents a packet in flight with SACK-related metadata
#[derive(Debug, Clone)]
pub struct SackInFlightPacket {
    pub last_sent_at: Instant,
    pub frame_info: RetransmissionFrameInfo,
    pub fast_retx_count: u16,
}

/// Centralized SACK manager that handles all SACK-related operations
#[derive(Debug)]
pub struct SackManager {
    /// Configuration for fast retransmission threshold
    fast_retx_threshold: u16,
    /// In-flight packets managed by SACK manager
    in_flight_packets: BTreeMap<u32, SackInFlightPacket>,
    /// ACK threshold for standalone ACK decisions
    ack_threshold: u32,
    /// Counter for ACK-eliciting packets since last ACK
    ack_eliciting_packets_count: u32,
}

impl SackManager {
    /// Creates a new SACK manager
    pub fn new(fast_retx_threshold: u16, ack_threshold: u32) -> Self {
        Self {
            fast_retx_threshold,
            in_flight_packets: BTreeMap::new(),
            ack_threshold,
            ack_eliciting_packets_count: 0,
        }
    }

    /// Adds a packet to the in-flight tracking
    pub fn add_in_flight_packet(&mut self, frame: Frame, now: Instant) {
        if let Some(seq) = frame.sequence_number() {
            let packet = SackInFlightPacket {
                last_sent_at: now,
                frame_info: RetransmissionFrameInfo::from_frame(&frame),
                fast_retx_count: 0,
            };
            self.in_flight_packets.insert(seq, packet);
        }
    }

    /// Returns the number of packets currently in flight
    pub fn in_flight_count(&self) -> usize {
        self.in_flight_packets.len()
    }

    /// Checks if the in-flight buffer is empty
    pub fn is_in_flight_empty(&self) -> bool {
        self.in_flight_packets.is_empty()
    }

    /// Checks if a FIN frame is already in the in-flight queue
    pub fn has_fin_in_flight(&self) -> bool {
        self.in_flight_packets
            .values()
            .any(|p| p.frame_info.frame_type == FrameType::Fin)
    }

    /// Increments the ACK-eliciting packet counter
    pub fn on_ack_eliciting_packet_received(&mut self) {
        self.ack_eliciting_packets_count += 1;
    }

    /// Resets the ACK-eliciting packet counter (called when ACK is sent)
    pub fn on_ack_sent(&mut self) {
        self.ack_eliciting_packets_count = 0;
    }

    /// Checks for RTO retransmissions with frame reconstruction
    /// 检查RTO重传并重构帧
    pub fn check_for_rto(
        &mut self, 
        rto: Duration, 
        now: Instant, 
        start_time: Instant,
        current_peer_cid: u32,
        protocol_version: u8,
        local_cid: u32,
        recv_next_sequence: u32,
        recv_window_size: u16
    ) -> Vec<Frame> {
        let mut frames_to_resend = Vec::new();
        let current_timestamp = now.duration_since(start_time).as_millis() as u32;
        
        for packet in self.in_flight_packets.values_mut() {
            if now.duration_since(packet.last_sent_at) > rto {
                debug!(
                    seq = packet.frame_info.sequence_number,
                    frame_type = ?packet.frame_info.frame_type,
                    updated_cid = current_peer_cid,
                    "RTO retransmission triggered - reconstructing frame with fresh header"
                );
                
                // Reconstruct frame with fresh header information
                // 使用新鲜的header信息重构帧
                let reconstructed_frame = packet.frame_info.reconstruct_frame(
                    current_peer_cid,
                    protocol_version,
                    local_cid,
                    recv_next_sequence,
                    recv_window_size,
                    current_timestamp,
                );
                
                frames_to_resend.push(reconstructed_frame);
                packet.last_sent_at = now;
            }
        }
        frames_to_resend
    }

    /// Returns the deadline for the next RTO event
    pub fn next_rto_deadline(&self, rto: Duration) -> Option<Instant> {
        self.in_flight_packets
            .values()
            .map(|p| p.last_sent_at + rto)
            .min()
    }

    /// Processes incoming ACK with SACK information
    /// 
    /// This is the core SACK processing logic that:
    /// 1. Handles cumulative ACK
    /// 2. Processes SACK ranges
    /// 3. Identifies packets for fast retransmission
    /// 4. Calculates RTT samples
    pub fn process_ack(
        &mut self,
        recv_next_seq: u32,
        sack_ranges: &[SackRange],
        now: Instant,
        start_time: Instant,
        current_peer_cid: u32,
        protocol_version: u8,
        local_cid: u32,
        recv_window_size: u16,
    ) -> SackProcessResult {
        let mut rtt_samples = Vec::new();
        let mut newly_acked_sequences = Vec::new();

        // Step 1: Process cumulative ACK
        let mut cumulative_acked_keys = Vec::new();
        for (&seq, packet) in self.in_flight_packets.iter() {
            if seq < recv_next_seq {
                cumulative_acked_keys.push(seq);
                rtt_samples.push(now.saturating_duration_since(packet.last_sent_at));
                newly_acked_sequences.push(seq);
            } else {
                break; // BTreeMap is sorted
            }
        }

        for key in cumulative_acked_keys {
            self.in_flight_packets.remove(&key);
        }

        // Step 2: Process SACK ranges
        let mut sack_acked_sequences = Vec::new();
        for range in sack_ranges {
            for seq in range.start..=range.end {
                if let Some(packet) = self.in_flight_packets.remove(&seq) {
                    rtt_samples.push(now.saturating_duration_since(packet.last_sent_at));
                    newly_acked_sequences.push(seq);
                    sack_acked_sequences.push(seq);
                }
            }
        }

        // Step 3: Check for fast retransmission with frame reconstruction
        // 检查快速重传并使用帧重构
        let frames_to_retransmit = if !sack_acked_sequences.is_empty() {
            self.check_fast_retransmission(
                &sack_acked_sequences,
                now,
                start_time,
                current_peer_cid,
                protocol_version,
                local_cid,
                recv_next_seq,
                recv_window_size,
            )
        } else {
            Vec::new()
        };

        SackProcessResult {
            frames_to_retransmit,
            rtt_samples,
            newly_acked_sequences,
        }
    }

    /// Checks for packets that need fast retransmission based on SACK information with frame reconstruction
    /// 基于SACK信息检查需要快速重传的数据包，支持帧重构
    fn check_fast_retransmission(
        &mut self,
        sack_acked_sequences: &[u32],
        now: Instant,
        start_time: Instant,
        current_peer_cid: u32,
        protocol_version: u8,
        local_cid: u32,
        recv_next_sequence: u32,
        recv_window_size: u16,
    ) -> Vec<Frame> {
        let mut frames_to_retransmit = Vec::new();
        let current_timestamp = now.duration_since(start_time).as_millis() as u32;

        // Find the highest sequence number that was SACKed in this ACK
        let highest_sacked = sack_acked_sequences.iter().max().copied();

        if let Some(highest_sacked_seq) = highest_sacked {
            trace!(
                highest_sacked = highest_sacked_seq,
                "Checking for fast retransmission against in-flight packets"
            );

            // Check all in-flight packets with sequence numbers less than the highest SACKed
            let mut keys_to_modify = Vec::new();
            for (&seq, _) in self.in_flight_packets.range(..highest_sacked_seq) {
                keys_to_modify.push(seq);
            }

            // Increment fast retransmission counters and trigger retransmission if threshold is met
            for seq in keys_to_modify {
                if let Some(packet) = self.in_flight_packets.get_mut(&seq) {
                    let old_count = packet.fast_retx_count;
                    packet.fast_retx_count += 1;

                    trace!(
                        seq,
                        old_count,
                        new_count = packet.fast_retx_count,
                        threshold = self.fast_retx_threshold,
                        "Packet skipped by SACK, incrementing fast retransmission count"
                    );

                    if packet.fast_retx_count >= self.fast_retx_threshold {
                        debug!(
                            seq,
                            frame_type = ?packet.frame_info.frame_type,
                            "Fast retransmission triggered by SACK - reconstructing frame with fresh header"
                        );
                        
                        // Reconstruct frame with fresh header information
                        // 使用新鲜的header信息重构帧
                        let reconstructed_frame = packet.frame_info.reconstruct_frame(
                            current_peer_cid,
                            protocol_version,
                            local_cid,
                            recv_next_sequence,
                            recv_window_size,
                            current_timestamp,
                        );
                        frames_to_retransmit.push(reconstructed_frame);
                        
                        packet.last_sent_at = now;
                        packet.fast_retx_count = 0; // Reset after triggering
                    }
                }
            }
        }

        frames_to_retransmit
    }

    /// Encodes SACK ranges into bytes for transmission
    pub fn encode_sack_ranges(&self, ranges: &[SackRange]) -> Bytes {
        use bytes::BytesMut;
        use crate::packet::sack::encode_sack_ranges;

        let mut payload = BytesMut::with_capacity(ranges.len() * 8);
        encode_sack_ranges(ranges, &mut payload);
        payload.freeze()
    }

    /// Decodes SACK ranges from received bytes
    pub fn decode_sack_ranges(&self, payload: Bytes) -> Vec<SackRange> {
        use crate::packet::sack::decode_sack_ranges;
        decode_sack_ranges(payload)
    }

    /// Determines if a standalone ACK should be sent based on SACK information
    pub fn should_send_standalone_ack(&self, sack_ranges: &[SackRange]) -> bool {
        self.ack_eliciting_packets_count >= self.ack_threshold && !sack_ranges.is_empty()
    }

    /// Clears all in-flight packets from tracking.
    ///
    /// 清除所有在途数据包的跟踪。
    pub fn clear(&mut self) {
        self.in_flight_packets.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::frame::Frame;
    use bytes::Bytes;

    fn create_test_push_frame(seq: u32) -> Frame {
        Frame::new_push(1, seq, 0, 1024, 0, Bytes::from(format!("data-{}", seq)))
    }

    #[test]
    fn test_process_ack_cumulative_only() {
        let mut manager = SackManager::new(3, 5);
        let now = Instant::now();
        let start_time = now;

        // Add packets 0, 1, 2 to in-flight
        for i in 0..3 {
            manager.add_in_flight_packet(create_test_push_frame(i), now);
        }

        // ACK up to sequence 2 (cumulative ACK for 0, 1)
        let result = manager.process_ack(2, &[], now, start_time, 12345, 1, 1, 1024);

        assert_eq!(result.newly_acked_sequences, vec![0, 1]);
        assert_eq!(result.rtt_samples.len(), 2);
        assert!(result.frames_to_retransmit.is_empty());
        assert_eq!(manager.in_flight_count(), 1); // Only packet 2 remains
    }

    #[test]
    fn test_process_ack_with_sack() {
        let mut manager = SackManager::new(3, 5);
        let now = Instant::now();
        let start_time = now;

        // Add packets 0, 1, 2, 3 to in-flight
        for i in 0..4 {
            manager.add_in_flight_packet(create_test_push_frame(i), now);
        }

        // Cumulative ACK for 0, SACK for 2 and 3
        let sack_ranges = vec![
            SackRange { start: 2, end: 3 },
        ];
        let result = manager.process_ack(1, &sack_ranges, now, start_time, 12345, 1, 1, 1024);

        assert_eq!(result.newly_acked_sequences, vec![0, 2, 3]);
        assert_eq!(result.rtt_samples.len(), 3);
        assert!(result.frames_to_retransmit.is_empty()); // No fast retx yet
        assert_eq!(manager.in_flight_count(), 1); // Only packet 1 remains
    }

    #[test]
    fn test_fast_retransmission() {
        let mut manager = SackManager::new(2, 5); // Lower threshold for testing
        let now = Instant::now();
        let start_time = now;

        // Add packets 0, 1, 2, 3 to in-flight
        for i in 0..4 {
            manager.add_in_flight_packet(create_test_push_frame(i), now);
        }

        // First SACK: ACK 0, SACK 2 (packet 1 missing)
        let result1 = manager.process_ack(1, &[SackRange { start: 2, end: 2 }], now, start_time, 12345, 1, 1, 1024);
        assert!(result1.frames_to_retransmit.is_empty()); // fast_retx_count = 1, below threshold
        assert_eq!(manager.in_flight_packets.get(&1).unwrap().fast_retx_count, 1);

        // Second SACK: SACK 3 (packet 1 still missing)
        let result2 = manager.process_ack(1, &[SackRange { start: 3, end: 3 }], now, start_time, 12345, 1, 1, 1024);
        assert_eq!(result2.frames_to_retransmit.len(), 1); // fast_retx_count = 2, meets threshold
        assert_eq!(result2.frames_to_retransmit[0].sequence_number().unwrap(), 1);
        assert_eq!(manager.in_flight_packets.get(&1).unwrap().fast_retx_count, 0); // Reset after retransmission
    }

    #[test]
    fn test_encode_decode_sack_ranges() {
        let manager = SackManager::new(3, 5);
        let original_ranges = vec![
            SackRange { start: 10, end: 15 },
            SackRange { start: 20, end: 25 },
        ];

        let encoded = manager.encode_sack_ranges(&original_ranges);
        let decoded = manager.decode_sack_ranges(encoded);

        assert_eq!(decoded, original_ranges);
    }

    #[test]
    fn test_should_send_standalone_ack() {
        let mut manager = SackManager::new(3, 5);
        
        // No SACK ranges, should not send ACK regardless of count
        manager.ack_eliciting_packets_count = 10;
        assert!(!manager.should_send_standalone_ack(&[]));
        
        // Has SACK ranges but count below threshold
        let ranges = vec![SackRange { start: 5, end: 10 }];
        manager.ack_eliciting_packets_count = 3;
        assert!(!manager.should_send_standalone_ack(&ranges));
        
        // Has SACK ranges and count meets threshold
        manager.ack_eliciting_packets_count = 5;
        assert!(manager.should_send_standalone_ack(&ranges));
        manager.ack_eliciting_packets_count = 10;
        assert!(manager.should_send_standalone_ack(&ranges));
    }

    #[test]
    fn test_clear() {
        let mut manager = SackManager::new(3, 5);
        let now = Instant::now();
        manager.add_in_flight_packet(create_test_push_frame(0), now);
        assert_eq!(manager.in_flight_count(), 1);
        manager.clear();
        assert_eq!(manager.in_flight_count(), 0);
    }

    #[test]
    fn test_fast_retransmission_frame_reconstruction() {
        let mut manager = SackManager::new(2, 5); // Lower threshold for testing
        let now = Instant::now();
        let start_time = now;

        // Add a PUSH packet to in-flight
        let original_frame = create_test_push_frame(1);
        manager.add_in_flight_packet(original_frame, now);

        // Add packets 0, 2 to trigger fast retransmission for packet 1
        manager.add_in_flight_packet(create_test_push_frame(0), now);
        manager.add_in_flight_packet(create_test_push_frame(2), now);

        // First SACK: ACK 1, SACK 2 (indicates packet 1 might be lost)
        let result1 = manager.process_ack(1, &[SackRange { start: 2, end: 2 }], now, start_time, 12345, 1, 1, 1024);
        assert!(result1.frames_to_retransmit.is_empty()); // Below threshold

        // Second SACK: SACK another higher sequence to trigger fast retransmission
        manager.add_in_flight_packet(create_test_push_frame(3), now);
        let result2 = manager.process_ack(1, &[SackRange { start: 3, end: 3 }], now, start_time, 12345, 1, 1, 1024);
        
        // Should trigger fast retransmission for packet 1
        assert_eq!(result2.frames_to_retransmit.len(), 1);
        let retx_frame = &result2.frames_to_retransmit[0];
        
        // Verify the retransmitted frame has the correct sequence number
        assert_eq!(retx_frame.sequence_number().unwrap(), 1);
        
        // Verify frame reconstruction works - the frame should be a PUSH frame
        match retx_frame {
            Frame::Push { header, payload } => {
                assert_eq!(header.sequence_number, 1);
                assert_eq!(header.recv_next_sequence, 1); // Should use the ACK info from process_ack
                // The connection ID should be updated with the real peer CID
                assert_eq!(header.connection_id, 12345); // Real peer CID from test parameters
                // Verify payload is preserved
                assert!(!payload.is_empty());
            },
            _ => panic!("Expected PUSH frame for fast retransmission"),
        }
        
        // Verify the fast retx count was reset
        assert_eq!(manager.in_flight_packets.get(&1).unwrap().fast_retx_count, 0);
    }
}