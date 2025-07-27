//! Simple Retransmission Manager
//!
//! This module handles simple timeout-based retransmission for frames that don't
//! require the full complexity of SACK-based reliability. This includes control
//! frames (SYN, SYN-ACK, FIN) and handshake data frames.
//!
//! 简单重传管理器
//!
//! 此模块处理不需要基于SACK的完整可靠性复杂性的帧的简单超时重传。
//! 这包括控制帧（SYN、SYN-ACK、FIN）和握手数据帧。

use crate::packet::frame::{Frame, ReliabilityMode};
use std::collections::BTreeMap;
use tokio::time::{Duration, Instant};
use tracing::{debug, trace};

/// Represents a packet managed by simple retransmission.
/// 表示由简单重传管理的数据包。
#[derive(Debug, Clone)]
pub struct SimpleInFlightPacket {
    /// The frame to be retransmitted.
    /// 要重传的帧。
    pub frame: Frame,
    /// When this packet was last sent.
    /// 此数据包最后发送的时间。
    pub last_sent_at: Instant,
    /// Current retry count.
    /// 当前重试次数。
    pub retry_count: u8,
    /// Maximum number of retries allowed.
    /// 允许的最大重试次数。
    pub max_retries: u8,
    /// Interval between retries.
    /// 重试间隔。
    pub retry_interval: Duration,
}

/// Simple retransmission manager for frames that don't use SACK.
/// 不使用SACK的帧的简单重传管理器。
#[derive(Debug)]
pub struct SimpleRetransmissionManager {
    /// In-flight packets managed by simple retransmission, keyed by sequence number.
    /// 由简单重传管理的在途数据包，按序列号索引。
    in_flight_simple: BTreeMap<u32, SimpleInFlightPacket>,
}

impl SimpleRetransmissionManager {
    /// Creates a new simple retransmission manager.
    /// 创建新的简单重传管理器。
    pub fn new() -> Self {
        Self {
            in_flight_simple: BTreeMap::new(),
        }
    }

    /// Adds a packet to simple retransmission tracking.
    /// 将数据包添加到简单重传跟踪。
    pub fn add_packet(&mut self, frame: Frame, now: Instant) {
        if let Some(seq) = frame.sequence_number() {
            let reliability_mode = frame.reliability_mode();

            if let ReliabilityMode::SimpleRetransmit {
                max_retries,
                retry_interval,
            } = reliability_mode
            {
                let packet = SimpleInFlightPacket {
                    frame,
                    last_sent_at: now,
                    retry_count: 0,
                    max_retries,
                    retry_interval,
                };

                trace!(
                    seq,
                    max_retries,
                    retry_interval_ms = retry_interval.as_millis(),
                    "Added packet to simple retransmission tracking"
                );

                self.in_flight_simple.insert(seq, packet);
            }
        }
    }

    /// Acknowledges packets up to the given sequence number.
    /// 确认到给定序列号的数据包。
    pub fn acknowledge_up_to(&mut self, recv_next_seq: u32) -> Vec<u32> {
        let mut acked_sequences = Vec::new();

        // Remove all packets with sequence numbers less than recv_next_seq
        // 移除所有序列号小于recv_next_seq的数据包
        let keys_to_remove: Vec<u32> = self
            .in_flight_simple
            .range(..recv_next_seq)
            .map(|(&seq, _)| seq)
            .collect();

        for seq in keys_to_remove {
            if self.in_flight_simple.remove(&seq).is_some() {
                acked_sequences.push(seq);
                trace!(seq, "Packet acknowledged by cumulative ACK");
            }
        }

        acked_sequences
    }

    /// Acknowledges packets in the given SACK ranges.
    /// 确认给定SACK范围内的数据包。
    pub fn acknowledge_sack_ranges(
        &mut self,
        sack_ranges: &[crate::packet::sack::SackRange],
    ) -> Vec<u32> {
        let mut acked_sequences = Vec::new();

        for range in sack_ranges {
            for seq in range.start..=range.end {
                if self.in_flight_simple.remove(&seq).is_some() {
                    acked_sequences.push(seq);
                    trace!(seq, "Packet acknowledged by SACK");
                }
            }
        }

        acked_sequences
    }

    /// Checks for packets that need retransmission and returns them.
    /// 检查需要重传的数据包并返回它们。
    pub fn check_for_retransmissions(&mut self, now: Instant) -> Vec<Frame> {
        let mut frames_to_retx = Vec::new();
        let mut expired_sequences = Vec::new();

        for (&seq, packet) in self.in_flight_simple.iter_mut() {
            if now.duration_since(packet.last_sent_at) >= packet.retry_interval {
                if packet.retry_count < packet.max_retries {
                    // Retransmit the packet
                    // 重传数据包
                    frames_to_retx.push(packet.frame.clone());
                    packet.last_sent_at = now;
                    packet.retry_count += 1;

                    debug!(
                        seq,
                        retry_count = packet.retry_count,
                        max_retries = packet.max_retries,
                        "Simple retransmission triggered"
                    );
                } else {
                    // Max retries reached, give up
                    // 达到最大重试次数，放弃
                    expired_sequences.push(seq);
                    debug!(
                        seq,
                        max_retries = packet.max_retries,
                        "Simple retransmission max retries reached, giving up"
                    );
                }
            }
        }

        // Remove expired packets
        // 移除过期的数据包
        for seq in expired_sequences {
            self.in_flight_simple.remove(&seq);
        }

        frames_to_retx
    }

    /// Returns the deadline for the next retransmission check.
    /// 返回下次重传检查的截止时间。
    pub fn next_retransmission_deadline(&self) -> Option<Instant> {
        self.in_flight_simple
            .values()
            .map(|packet| packet.last_sent_at + packet.retry_interval)
            .min()
    }

    /// Returns the number of packets currently being tracked.
    /// 返回当前跟踪的数据包数量。
    pub fn in_flight_count(&self) -> usize {
        self.in_flight_simple.len()
    }

    /// Checks if there are no packets being tracked.
    /// 检查是否没有正在跟踪的数据包。
    pub fn is_empty(&self) -> bool {
        self.in_flight_simple.is_empty()
    }

    /// Checks if a specific frame type is in flight.
    /// 检查特定帧类型是否在途。
    pub fn has_frame_type_in_flight<F>(&self, predicate: F) -> bool
    where
        F: Fn(&Frame) -> bool,
    {
        self.in_flight_simple
            .values()
            .any(|packet| predicate(&packet.frame))
    }

    /// Removes all packets from tracking (used for connection cleanup).
    /// 从跟踪中移除所有数据包（用于连接清理）。
    pub fn clear(&mut self) {
        self.in_flight_simple.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::frame::Frame;

    fn create_test_fin_frame(seq: u32) -> Frame {
        Frame::new_fin(1, seq, 0, 0, 0)
    }

    #[test]
    fn test_add_and_acknowledge_packet() {
        let mut manager = SimpleRetransmissionManager::new();
        let now = Instant::now();

        // Add a FIN frame (uses simple retransmission)
        let frame = create_test_fin_frame(0);
        manager.add_packet(frame, now);
        assert_eq!(manager.in_flight_count(), 1);

        // Acknowledge it
        let acked = manager.acknowledge_up_to(1);
        assert_eq!(acked, vec![0]);
        assert_eq!(manager.in_flight_count(), 0);
    }

    #[test]
    fn test_retransmission_logic() {
        let mut manager = SimpleRetransmissionManager::new();
        let now = Instant::now();

        // Add a FIN frame (has sequence number, uses simple retransmission)
        let frame = create_test_fin_frame(1);
        manager.add_packet(frame, now);

        // Should have added the packet
        assert_eq!(manager.in_flight_count(), 1);

        // No retransmission needed immediately
        let frames = manager.check_for_retransmissions(now);
        assert!(frames.is_empty());

        // After retry interval, should trigger retransmission
        let later = now + Duration::from_millis(600); // > 500ms retry interval
        let frames = manager.check_for_retransmissions(later);
        assert_eq!(frames.len(), 1);

        // Check that retry count was incremented
        if let Some(packet) = manager.in_flight_simple.values().next() {
            assert_eq!(packet.retry_count, 1);
        }
    }

    #[test]
    fn test_max_retries_reached() {
        let mut manager = SimpleRetransmissionManager::new();
        let now = Instant::now();

        // Add a FIN frame (has sequence number, uses simple retransmission)
        let frame = create_test_fin_frame(1);
        manager.add_packet(frame, now);

        // Should have added the packet
        assert_eq!(manager.in_flight_count(), 1);

        // Simulate multiple retransmissions until max is reached
        let mut current_time = now;
        for i in 1..=5 {
            current_time = current_time + Duration::from_millis(600);
            let frames = manager.check_for_retransmissions(current_time);
            assert_eq!(frames.len(), 1, "Retry {}", i);
        }

        // After max retries, should give up
        current_time = current_time + Duration::from_millis(600);
        let frames = manager.check_for_retransmissions(current_time);
        assert!(frames.is_empty());
        assert_eq!(manager.in_flight_count(), 0);
    }

    #[test]
    fn test_sack_acknowledgment() {
        let mut manager = SimpleRetransmissionManager::new();
        let now = Instant::now();

        // Add multiple FIN frames (use simple retransmission)
        for i in 0..3 {
            let frame = create_test_fin_frame(i);
            manager.add_packet(frame, now);
        }
        assert_eq!(manager.in_flight_count(), 3);

        // SACK acknowledge frames 1 and 2
        let sack_ranges = vec![crate::packet::sack::SackRange { start: 1, end: 2 }];
        let acked = manager.acknowledge_sack_ranges(&sack_ranges);
        assert_eq!(acked, vec![1, 2]);
        assert_eq!(manager.in_flight_count(), 1);
    }

    #[test]
    fn test_next_retransmission_deadline() {
        let mut manager = SimpleRetransmissionManager::new();
        let now = Instant::now();

        // No packets, no deadline
        assert!(manager.next_retransmission_deadline().is_none());

        // Add a packet
        let frame = create_test_fin_frame(1);
        manager.add_packet(frame, now);

        // Should have a deadline
        let deadline = manager.next_retransmission_deadline();
        assert!(deadline.is_some());
        assert!(deadline.unwrap() > now);
    }
}
