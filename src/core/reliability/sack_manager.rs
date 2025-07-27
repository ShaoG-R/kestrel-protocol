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

/// Represents a packet in flight with SACK-related metadata
#[derive(Debug, Clone)]
pub struct SackInFlightPacket {
    pub last_sent_at: Instant,
    pub frame: Frame,
    pub fast_retx_count: u16,
}

/// Centralized SACK manager that handles all SACK-related operations
#[derive(Debug)]
pub struct SackManager {
    /// Configuration for fast retransmission threshold
    fast_retx_threshold: u16,
}

impl SackManager {
    /// Creates a new SACK manager
    pub fn new(fast_retx_threshold: u16) -> Self {
        Self {
            fast_retx_threshold,
        }
    }

    /// Generates SACK ranges from a receive buffer state
    /// 
    /// This method takes the out-of-order packets and generates
    /// continuous ranges for SACK information.
    pub fn generate_sack_ranges(
        &self,
        received_packets: &BTreeMap<u32, bool>, // seq -> is_received
        next_expected_seq: u32,
    ) -> Vec<SackRange> {
        let mut ranges = Vec::new();
        let mut current_range: Option<SackRange> = None;

        // Only consider packets beyond the next expected sequence
        for (&seq, &is_received) in received_packets.range(next_expected_seq..) {
            if !is_received {
                continue;
            }

            match current_range.as_mut() {
                Some(range) => {
                    if seq == range.end + 1 {
                        // Extend current range
                        range.end = seq;
                    } else {
                        // Gap detected, finalize current range and start new one
                        ranges.push(range.clone());
                        current_range = Some(SackRange {
                            start: seq,
                            end: seq,
                        });
                    }
                }
                None => {
                    // Start first range
                    current_range = Some(SackRange {
                        start: seq,
                        end: seq,
                    });
                }
            }
        }

        if let Some(range) = current_range {
            ranges.push(range);
        }

        ranges
    }

    /// Processes incoming ACK with SACK information
    /// 
    /// This is the core SACK processing logic that:
    /// 1. Handles cumulative ACK
    /// 2. Processes SACK ranges
    /// 3. Identifies packets for fast retransmission
    /// 4. Calculates RTT samples
    pub fn process_ack(
        &self,
        in_flight_packets: &mut BTreeMap<u32, SackInFlightPacket>,
        recv_next_seq: u32,
        sack_ranges: &[SackRange],
        now: Instant,
    ) -> SackProcessResult {
        let mut rtt_samples = Vec::new();
        let mut newly_acked_sequences = Vec::new();

        // Step 1: Process cumulative ACK
        let mut cumulative_acked_keys = Vec::new();
        for (&seq, packet) in in_flight_packets.iter() {
            if seq < recv_next_seq {
                cumulative_acked_keys.push(seq);
                rtt_samples.push(now.saturating_duration_since(packet.last_sent_at));
                newly_acked_sequences.push(seq);
            } else {
                break; // BTreeMap is sorted
            }
        }

        for key in cumulative_acked_keys {
            in_flight_packets.remove(&key);
        }

        // Step 2: Process SACK ranges
        let mut sack_acked_sequences = Vec::new();
        for range in sack_ranges {
            for seq in range.start..=range.end {
                if let Some(packet) = in_flight_packets.remove(&seq) {
                    rtt_samples.push(now.saturating_duration_since(packet.last_sent_at));
                    newly_acked_sequences.push(seq);
                    sack_acked_sequences.push(seq);
                }
            }
        }

        // Step 3: Check for fast retransmission
        let frames_to_retransmit = self.check_fast_retransmission(
            in_flight_packets,
            &sack_acked_sequences,
            now,
        );

        SackProcessResult {
            frames_to_retransmit,
            rtt_samples,
            newly_acked_sequences,
        }
    }

    /// Checks for packets that need fast retransmission based on SACK information
    fn check_fast_retransmission(
        &self,
        in_flight_packets: &mut BTreeMap<u32, SackInFlightPacket>,
        sack_acked_sequences: &[u32],
        now: Instant,
    ) -> Vec<Frame> {
        let mut frames_to_retransmit = Vec::new();

        // Find the highest sequence number that was SACKed in this ACK
        let highest_sacked = sack_acked_sequences.iter().max().copied();

        if let Some(highest_sacked_seq) = highest_sacked {
            trace!(
                highest_sacked = highest_sacked_seq,
                "Checking for fast retransmission against in-flight packets"
            );

            // Check all in-flight packets with sequence numbers less than the highest SACKed
            let mut keys_to_modify = Vec::new();
            for (&seq, _) in in_flight_packets.range(..highest_sacked_seq) {
                keys_to_modify.push(seq);
            }

            // Increment fast retransmission counters and trigger retransmission if threshold is met
            for seq in keys_to_modify {
                if let Some(packet) = in_flight_packets.get_mut(&seq) {
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
                        debug!(seq, "Fast retransmission triggered by SACK");
                        frames_to_retransmit.push(packet.frame.clone());
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
    pub fn should_send_standalone_ack(
        &self,
        sack_ranges: &[SackRange],
        ack_eliciting_packets_count: u32,
        ack_threshold: u32,
    ) -> bool {
        ack_eliciting_packets_count >= ack_threshold && !sack_ranges.is_empty()
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

    fn create_test_in_flight_packet(seq: u32, now: Instant) -> SackInFlightPacket {
        SackInFlightPacket {
            last_sent_at: now,
            frame: create_test_push_frame(seq),
            fast_retx_count: 0,
        }
    }

    #[test]
    fn test_generate_sack_ranges() {
        let manager = SackManager::new(3);
        let mut received_packets = BTreeMap::new();
        
        // Simulate received packets: 0, 1, 3, 4, 6
        received_packets.insert(0, true);
        received_packets.insert(1, true);
        received_packets.insert(2, false); // gap
        received_packets.insert(3, true);
        received_packets.insert(4, true);
        received_packets.insert(5, false); // gap
        received_packets.insert(6, true);

        // After processing 0 and 1, next expected is 2
        let ranges = manager.generate_sack_ranges(&received_packets, 2);
        
        assert_eq!(ranges, vec![
            SackRange { start: 3, end: 4 },
            SackRange { start: 6, end: 6 },
        ]);
    }

    #[test]
    fn test_process_ack_cumulative_only() {
        let manager = SackManager::new(3);
        let mut in_flight = BTreeMap::new();
        let now = Instant::now();

        // Add packets 0, 1, 2 to in-flight
        for i in 0..3 {
            in_flight.insert(i, create_test_in_flight_packet(i, now));
        }

        // ACK up to sequence 2 (cumulative ACK for 0, 1)
        let result = manager.process_ack(&mut in_flight, 2, &[], now);

        assert_eq!(result.newly_acked_sequences, vec![0, 1]);
        assert_eq!(result.rtt_samples.len(), 2);
        assert!(result.frames_to_retransmit.is_empty());
        assert_eq!(in_flight.len(), 1); // Only packet 2 remains
    }

    #[test]
    fn test_process_ack_with_sack() {
        let manager = SackManager::new(3);
        let mut in_flight = BTreeMap::new();
        let now = Instant::now();

        // Add packets 0, 1, 2, 3 to in-flight
        for i in 0..4 {
            in_flight.insert(i, create_test_in_flight_packet(i, now));
        }

        // Cumulative ACK for 0, SACK for 2 and 3
        let sack_ranges = vec![
            SackRange { start: 2, end: 3 },
        ];
        let result = manager.process_ack(&mut in_flight, 1, &sack_ranges, now);

        assert_eq!(result.newly_acked_sequences, vec![0, 2, 3]);
        assert_eq!(result.rtt_samples.len(), 3);
        assert!(result.frames_to_retransmit.is_empty()); // No fast retx yet
        assert_eq!(in_flight.len(), 1); // Only packet 1 remains
    }

    #[test]
    fn test_fast_retransmission() {
        let manager = SackManager::new(2); // Lower threshold for testing
        let mut in_flight = BTreeMap::new();
        let now = Instant::now();

        // Add packets 0, 1, 2, 3 to in-flight
        for i in 0..4 {
            in_flight.insert(i, create_test_in_flight_packet(i, now));
        }

        // First SACK: ACK 0, SACK 2 (packet 1 missing)
        let result1 = manager.process_ack(&mut in_flight, 1, &[SackRange { start: 2, end: 2 }], now);
        assert!(result1.frames_to_retransmit.is_empty()); // fast_retx_count = 1, below threshold
        assert_eq!(in_flight.get(&1).unwrap().fast_retx_count, 1);

        // Second SACK: SACK 3 (packet 1 still missing)
        let result2 = manager.process_ack(&mut in_flight, 1, &[SackRange { start: 3, end: 3 }], now);
        assert_eq!(result2.frames_to_retransmit.len(), 1); // fast_retx_count = 2, meets threshold
        assert_eq!(result2.frames_to_retransmit[0].sequence_number().unwrap(), 1);
        assert_eq!(in_flight.get(&1).unwrap().fast_retx_count, 0); // Reset after retransmission
    }

    #[test]
    fn test_encode_decode_sack_ranges() {
        let manager = SackManager::new(3);
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
        let manager = SackManager::new(3);
        
        // No SACK ranges, should not send ACK regardless of count
        assert!(!manager.should_send_standalone_ack(&[], 10, 5));
        
        // Has SACK ranges but count below threshold
        let ranges = vec![SackRange { start: 5, end: 10 }];
        assert!(!manager.should_send_standalone_ack(&ranges, 3, 5));
        
        // Has SACK ranges and count meets threshold
        assert!(manager.should_send_standalone_ack(&ranges, 5, 5));
        assert!(manager.should_send_standalone_ack(&ranges, 10, 5));
    }
}