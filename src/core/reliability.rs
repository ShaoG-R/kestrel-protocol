//! The reliability layer.
//!
//! This layer is responsible for sequencing, acknowledgments, retransmissions (RTO),
//! SACK processing, and reordering. It provides an interface for sending and
//! receiving reliable data blocks.
//!
//! 可靠性层。
//!
//! 该层负责序列化、确认、重传（RTO）、SACK处理和重排序。
//! 它提供了一个发送和接收可靠数据块的接口。

use crate::config::Config;
use crate::packet::frame::Frame;
use crate::packet::sack::SackRange;
use bytes::{Bytes, BytesMut};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Duration;
use tokio::time::Instant;

const DEFAULT_RECV_BUFFER_CAPACITY: usize = 256; // In packets
const DEFAULT_SEND_BUFFER_CAPACITY_BYTES: usize = 1024 * 1024; // 1 MB

// --- Public API ---

/// The reliability layer for a connection.
///
/// 连接的可靠性层。
pub struct ReliabilityLayer {
    send_buffer: SendBuffer,
    recv_buffer: ReceiveBuffer,
    rto_estimator: RttEstimator,
    sequence_number_counter: u32,
    /// A flag indicating that an ACK frame should be generated and sent at the next opportunity.
    ack_pending: bool,
    /// Counts the number of packets received that should trigger an ACK.
    ack_eliciting_packets_since_last_ack: u16,
    /// The configuration for this connection.
    config: Config,
    /// The time the connection was started.
    start_time: Instant,
}

impl ReliabilityLayer {
    pub fn new(config: Config) -> Self {
        Self {
            send_buffer: SendBuffer::new(),
            recv_buffer: ReceiveBuffer::new(),
            rto_estimator: RttEstimator::new(config.initial_rto),
            sequence_number_counter: 0,
            ack_pending: false,
            ack_eliciting_packets_since_last_ack: 0,
            config,
            start_time: Instant::now(),
        }
    }

    /// Handles an incoming ACK frame, processing SACK ranges and fast retransmissions.
    ///
    /// Returns a tuple of:
    /// 1. A boolean indicating if a packet loss was detected.
    /// 2. The RTT sample from this ACK.
    /// 3. A list of frames to be fast-retransmitted.
    ///
    /// 处理传入的ACK帧，处理SACK范围和快速重传。
    /// 返回一个元组：
    /// 1. 一个布尔值，指示是否检测到丢包。
    /// 2. 本次ACK的RTT样本。
    /// 3. 需要快速重传的帧列表。
    pub fn handle_ack(
        &mut self,
        sack_ranges: Vec<SackRange>,
    ) -> (bool, Option<Duration>, Vec<Frame>) {
        if sack_ranges.is_empty() {
            return (false, None, Vec::new());
        }

        let mut loss_detected = false;
        let mut rtt_sample = None;
        let mut frames_to_fast_retx = Vec::new();

        let mut acked_seq_numbers = BTreeSet::new();
        for range in sack_ranges {
            for seq in range.start..=range.end {
                acked_seq_numbers.insert(seq);
            }
        }

        let highest_acked_seq = *acked_seq_numbers.iter().next_back().unwrap();
        let now = Instant::now();

        self.send_buffer.in_flight.retain(|packet| {
            if let Some(seq) = packet.frame.sequence_number() {
                if acked_seq_numbers.contains(&seq) {
                    // This packet is acknowledged. Update RTO and remove it.
                    let rtt = now.duration_since(packet.last_sent_at);
                    self.rto_estimator.update(rtt, self.config.min_rto);
                    rtt_sample = Some(rtt);
                    return false; // Remove from in_flight
                }
            }
            true // Keep in in_flight
        });

        // After removing acknowledged packets, check for fast retransmissions
        for packet in self.send_buffer.iter_in_flight_mut() {
            if let Some(seq) = packet.frame.sequence_number() {
                if seq < highest_acked_seq {
                    packet.fast_retx_count += 1;
                    if packet.fast_retx_count >= self.config.fast_retx_threshold {
                        frames_to_fast_retx.push(packet.frame.clone());
                        packet.last_sent_at = now;
                        packet.fast_retx_count = 0; // Reset count
                        loss_detected = true;
                    }
                }
            }
        }

        (loss_detected, rtt_sample, frames_to_fast_retx)
    }

    /// Returns the deadline for the next RTO event.
    ///
    /// 返回下一个RTO事件的截止时间。
    pub fn next_rto_deadline(&self) -> Option<Instant> {
        self.send_buffer
            .in_flight
            .front()
            .map(|p| p.last_sent_at + self.rto_estimator.rto())
    }

    /// Checks for any in-flight packets that have timed out and retransmits them.
    ///
    /// Returns a tuple of:
    /// 1. A boolean indicating if an RTO occurred.
    /// 2. A list of frames to be retransmitted.
    ///
    /// 检查是否有任何在途数据包已超时并进行重传。
    /// 返回一个元组：
    /// 1. 一个布尔值，指示是否发生了RTO。
    /// 2. 需要重传的帧列表。
    pub fn check_for_retransmissions(&mut self) -> (bool, Vec<Frame>) {
        let rto = self.rto_estimator.rto();
        let now = Instant::now();
        let mut frames_to_resend = Vec::new();
        let mut rto_occured = false;

        for packet in self.send_buffer.iter_in_flight_mut() {
            if now.duration_since(packet.last_sent_at) > rto {
                // RTO exceeded. Collect the frame to resend it later.
                frames_to_resend.push(packet.frame.clone());
                // Update the time it was sent.
                packet.last_sent_at = now;
                rto_occured = true;
            }
        }

        (rto_occured, frames_to_resend)
    }

    /// Receives a data payload for a given sequence number.
    ///
    /// This should be called when a PUSH frame is received. It will update the
    /// internal state to track that an ACK is needed.
    ///
    /// 为给定的序列号接收一个数据载荷。
    ///
    /// 当收到PUSH帧时应调用此方法。它会更新内部状态以跟踪需要一个ACK。
    pub fn receive_push(&mut self, sequence_number: u32, payload: Bytes) {
        self.recv_buffer.receive(sequence_number, payload);
        self.ack_pending = true;
        self.ack_eliciting_packets_since_last_ack += 1;
    }

    /// Tries to reassemble contiguous packets into a single `Bytes` object.
    ///
    /// 尝试将连续的数据包重组成一个单独的 `Bytes` 对象。
    pub fn reassemble(&mut self) -> Option<Bytes> {
        self.recv_buffer.reassemble()
    }

    /// Returns the next sequence number to be used for an outgoing frame.
    ///
    /// 返回用于出站帧的下一个序列号。
    pub fn next_sequence_number(&mut self) -> u32 {
        let seq = self.sequence_number_counter;
        self.sequence_number_counter += 1;
        seq
    }

    /// Checks if a standalone ACK should be sent immediately based on the ACK threshold.
    ///
    /// 根据ACK阈值检查是否应立即发送一个独立的ACK。
    pub fn should_send_standalone_ack(&self) -> bool {
        self.ack_eliciting_packets_since_last_ack >= self.config.ack_threshold
            && !self.recv_buffer.get_sack_ranges().is_empty()
    }

    /// Gathers the necessary information to build an ACK frame.
    ///
    /// 收集构建ACK帧所需的信息。
    pub fn get_ack_info(&self) -> (Vec<SackRange>, u32, u16) {
        (
            self.recv_buffer.get_sack_ranges(),
            self.recv_buffer.next_sequence(),
            self.recv_buffer.window_size(),
        )
    }

    /// Notifies the layer that an ACK has been sent, resetting pending counters.
    ///
    /// 通知层一个ACK已被发送，重置待处理的计数器。
    pub fn on_ack_sent(&mut self) {
        self.ack_pending = false;
        self.ack_eliciting_packets_since_last_ack = 0;
    }

    /// Writes data to the stream buffer. Returns the number of bytes written.
    ///
    /// 将数据写入流缓冲区。返回写入的字节数。
    pub fn write_to_stream(&mut self, buf: &[u8]) -> usize {
        self.send_buffer.write_to_stream(buf)
    }

    /// Checks if the in-flight buffer is empty.
    pub fn is_in_flight_empty(&self) -> bool {
        self.send_buffer.in_flight.is_empty()
    }

    /// Checks if a FIN frame is already in the in-flight queue.
    pub fn has_fin_in_flight(&self) -> bool {
        self.send_buffer
            .in_flight
            .iter()
            .any(|p| matches!(p.frame, Frame::Fin { .. }))
    }

    /// A special method to add a FIN frame directly to the in-flight queue.
    pub fn add_fin_to_in_flight(&mut self, fin_frame: Frame) {
        self.send_buffer.add_in_flight(fin_frame, Instant::now());
    }

    /// Returns the number of packets currently in flight.
    pub fn in_flight_count(&self) -> usize {
        self.send_buffer.in_flight.len()
    }

    /// Takes data from the stream buffer, packetizes it into PUSH frames,
    /// and adds them to the in-flight queue.
    ///
    /// This method is a placeholder and needs to be integrated with congestion
    /// and flow control from the `Endpoint`.
    ///
    /// 从流缓冲区获取数据，将其打包成PUSH帧，并添加到在途队列。
    ///
    /// 这个方法是一个占位符，需要与`Endpoint`的拥塞和流量控制集成。
    pub fn packetize_stream_data(
        &mut self,
        peer_cid: u32,
        peer_recv_window: u16,
        next_sequence_to_ack: u32,
        now: Instant,
    ) -> Vec<Frame> {
        let mut frames = Vec::new();
        while let Some(chunk) = self
            .send_buffer
            .create_chunk(self.config.max_payload_size)
        {
            let push_header = crate::packet::header::ShortHeader {
                command: crate::packet::command::Command::Push,
                connection_id: peer_cid,
                recv_window_size: peer_recv_window,
                recv_next_sequence: next_sequence_to_ack,
                timestamp: now.duration_since(self.start_time).as_millis() as u32,
                sequence_number: self.next_sequence_number(),
            };

            let frame = Frame::Push {
                header: push_header,
                payload: chunk,
            };
            frames.push(frame.clone());
            self.send_buffer.add_in_flight(frame, now);
        }
        frames
    }
}

// --- Internal Implementation Details ---
// The following structs are internal to the reliability layer.
// 下面的结构体是可靠性层的内部实现细节。

/// An estimator for the round-trip time (RTT).
/// RTT 估算器。
#[derive(Debug)]
struct RttEstimator {
    srtt: Duration,
    rttvar: Duration,
    rto: Duration,
}

impl RttEstimator {
    fn new(initial_rto: Duration) -> Self {
        Self {
            srtt: Duration::from_secs(0),
            rttvar: Duration::from_secs(0),
            rto: initial_rto,
        }
    }

    fn rto(&self) -> Duration {
        self.rto
    }

    fn update(&mut self, rtt: Duration, min_rto: Duration) {
        if self.srtt == Duration::from_secs(0) {
            self.srtt = rtt;
            self.rttvar = rtt / 2;
        } else {
            let delta = if self.srtt > rtt {
                self.srtt - rtt
            } else {
                rtt - self.srtt
            };
            self.rttvar = (3 * self.rttvar + delta) / 4;
            self.srtt = (7 * self.srtt + rtt) / 8;
        }
        self.rto = (self.srtt + 4 * self.rttvar).max(min_rto);
    }
}

/// A packet that has been sent but not yet acknowledged (in-flight).
#[derive(Debug)]
struct InFlightPacket {
    pub last_sent_at: Instant,
    pub frame: Frame,
    pub fast_retx_count: u16,
}

/// Manages outgoing data.
#[derive(Debug)]
struct SendBuffer {
    in_flight: VecDeque<InFlightPacket>,
    stream_buffer: VecDeque<u8>,
    stream_buffer_capacity: usize,
}

impl Default for SendBuffer {
    fn default() -> Self {
        Self {
            in_flight: VecDeque::new(),
            stream_buffer: VecDeque::new(),
            stream_buffer_capacity: DEFAULT_SEND_BUFFER_CAPACITY_BYTES,
        }
    }
}

impl SendBuffer {
    fn new() -> Self {
        Self::default()
    }

    fn write_to_stream(&mut self, buf: &[u8]) -> usize {
        let space_available = self
            .stream_buffer_capacity
            .saturating_sub(self.stream_buffer.len());
        let bytes_to_write = std::cmp::min(buf.len(), space_available);
        self.stream_buffer.extend(&buf[..bytes_to_write]);
        bytes_to_write
    }

    fn create_chunk(&mut self, max_size: usize) -> Option<Bytes> {
        let chunk_size = std::cmp::min(self.stream_buffer.len(), max_size);
        if chunk_size == 0 {
            return None;
        }
        Some(self.stream_buffer.drain(..chunk_size).collect())
    }

    fn add_in_flight(&mut self, frame: Frame, now: Instant) {
        self.in_flight.push_back(InFlightPacket {
            last_sent_at: now,
            frame,
            fast_retx_count: 0,
        });
    }

    fn iter_in_flight_mut(&mut self) -> std::collections::vec_deque::IterMut<'_, InFlightPacket> {
        self.in_flight.iter_mut()
    }

}

/// Manages incoming data.
#[derive(Debug)]
struct ReceiveBuffer {
    next_sequence: u32,
    received: BTreeMap<u32, Bytes>,
    capacity: usize,
}

impl Default for ReceiveBuffer {
    fn default() -> Self {
        Self {
            next_sequence: 0,
            received: BTreeMap::new(),
            capacity: DEFAULT_RECV_BUFFER_CAPACITY,
        }
    }
}

impl ReceiveBuffer {
    fn new() -> Self {
        Self::default()
    }

    fn window_size(&self) -> u16 {
        (self.capacity.saturating_sub(self.received.len())) as u16
    }

    fn next_sequence(&self) -> u32 {
        self.next_sequence
    }

    fn receive(&mut self, sequence_number: u32, payload: Bytes) {
        if sequence_number >= self.next_sequence {
            self.received.insert(sequence_number, payload);
        }
    }

    fn reassemble(&mut self) -> Option<Bytes> {
        let mut reassembled_data = BytesMut::new();
        while let Some(payload) = self.try_pop_next_contiguous() {
            reassembled_data.extend_from_slice(&payload);
        }
        if reassembled_data.is_empty() {
            None
        } else {
            Some(reassembled_data.freeze())
        }
    }

    fn try_pop_next_contiguous(&mut self) -> Option<Bytes> {
        if let Some((seq, _payload)) = self.received.first_key_value() {
            if *seq == self.next_sequence {
                self.next_sequence += 1;
                return Some(self.received.pop_first().unwrap().1);
            }
        }
        None
    }

    fn get_sack_ranges(&self) -> Vec<SackRange> {
        let mut ranges = Vec::new();
        let mut current_range: Option<SackRange> = None;

        for &seq in self.received.keys() {
            match current_range.as_mut() {
                Some(range) => {
                    if seq == range.end + 1 {
                        range.end = seq;
                    } else {
                        ranges.push(range.clone());
                        current_range = Some(SackRange {
                            start: seq,
                            end: seq,
                        });
                    }
                }
                None => {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::packet::header::ShortHeader;
    use bytes::Bytes;
    use std::time::Duration;
    use tokio::time::Instant;

    fn test_config() -> Config {
        Config {
            fast_retx_threshold: 2,
            initial_rto: Duration::from_millis(50),
            ..Default::default()
        }
    }

    fn create_push_frame(seq: u32) -> Frame {
        Frame::Push {
            header: ShortHeader {
                command: crate::packet::command::Command::Push,
                connection_id: 1,
                recv_window_size: 10,
                timestamp: 0,
                sequence_number: seq,
                recv_next_sequence: 0,
            },
            payload: Bytes::from(format!("packet-{}", seq)),
        }
    }

    #[test]
    fn test_receive_in_order_and_reassemble() {
        let mut reliability = ReliabilityLayer::new(test_config());

        // Receive two packets in order
        reliability.receive_push(0, Bytes::from("hello"));
        reliability.receive_push(1, Bytes::from(" world"));

        // Reassemble them
        let data = reliability.reassemble().unwrap();
        assert_eq!(data, "hello world");

        // Should be nothing left to reassemble
        assert!(reliability.reassemble().is_none());

        // next_sequence should be updated
        assert_eq!(reliability.recv_buffer.next_sequence(), 2);
    }

    #[test]
    fn test_receive_out_of_order_and_reassemble() {
        let mut reliability = ReliabilityLayer::new(test_config());

        // Receive packet 1, then packet 0
        reliability.receive_push(1, Bytes::from("world"));

        // Nothing should be reassembled yet, because 0 is missing
        assert!(reliability.reassemble().is_none());
        assert_eq!(reliability.recv_buffer.next_sequence(), 0);

        reliability.receive_push(0, Bytes::from("hello "));

        // Now reassemble
        let data = reliability.reassemble().unwrap();
        assert_eq!(data, "hello world");
        assert_eq!(reliability.recv_buffer.next_sequence(), 2);
    }

    #[test]
    fn test_sack_range_generation() {
        let mut reliability = ReliabilityLayer::new(test_config());

        // No packets received yet
        assert!(reliability.get_ack_info().0.is_empty());

        // Receive discontinuous packets
        reliability.receive_push(0, Bytes::new());
        reliability.receive_push(1, Bytes::new());
        reliability.receive_push(3, Bytes::new());
        reliability.receive_push(4, Bytes::new());
        reliability.receive_push(6, Bytes::new());

        // Reassemble will pull out 0 and 1
        let _ = reliability.reassemble();
        assert_eq!(reliability.recv_buffer.next_sequence(), 2);

        // Check SACK ranges for what's left (3-4, 6)
        let (sack_ranges, next_ack, _) = reliability.get_ack_info();
        assert_eq!(next_ack, 2);
        assert_eq!(
            sack_ranges,
            vec![
                SackRange { start: 3, end: 4 },
                SackRange { start: 6, end: 6 }
            ]
        );
    }

    #[tokio::test]
    async fn test_handle_ack_and_rtt_update() {
        let mut reliability = ReliabilityLayer::new(test_config());

        // Simulate sending two packets
        let now = Instant::now();
        reliability
            .send_buffer
            .add_in_flight(create_push_frame(0), now);
        reliability
            .send_buffer
            .add_in_flight(create_push_frame(1), now);

        assert_eq!(reliability.in_flight_count(), 2);

        tokio::time::pause();
        tokio::time::advance(Duration::from_millis(100)).await;

        // Receive an ACK for packet 0
        let sack_ranges = vec![SackRange { start: 0, end: 0 }];
        let (loss, rtt, fast_retx) = reliability.handle_ack(sack_ranges);

        assert!(!loss);
        assert!(rtt.is_some());
        assert!(rtt.unwrap() >= Duration::from_millis(100));
        assert!(fast_retx.is_empty());

        // Only packet 1 should be in flight
        assert_eq!(reliability.in_flight_count(), 1);
        let in_flight_seq = reliability
            .send_buffer
            .in_flight
            .front()
            .unwrap()
            .frame
            .sequence_number()
            .unwrap();
        assert_eq!(in_flight_seq, 1);
    }

    #[test]
    fn test_fast_retransmission() {
        let mut reliability = ReliabilityLayer::new(test_config());
        let now = Instant::now();

        // Send packets 0, 1, 2, 3
        for i in 0..=3 {
            reliability
                .send_buffer
                .add_in_flight(create_push_frame(i), now);
        }

        // ACK for 0 removes it.
        reliability.handle_ack(vec![SackRange { start: 0, end: 0 }]);
        assert_eq!(reliability.in_flight_count(), 3); // 1, 2, 3 left

        // 1. Receive ACK for packet 2. This implies packet 1 was lost.
        // `in_flight` contains [1, 2, 3]. Highest acked is 2. Packet 1 is before it.
        // fast_retx_count for packet 1 becomes 1.
        let (loss, _, retx) = reliability.handle_ack(vec![SackRange { start: 2, end: 2 }]);
        assert!(!loss);
        assert!(retx.is_empty());
        assert_eq!(
            reliability
                .send_buffer
                .in_flight
                .front()
                .unwrap()
                .fast_retx_count,
            1
        );

        // 2. Receive ACK for packet 3.
        // `in_flight` contains [1, 3]. Highest acked is 3. Packet 1 is before it.
        // fast_retx_count for packet 1 becomes 2. Threshold is met.
        let (loss, _, retx) = reliability.handle_ack(vec![SackRange { start: 3, end: 3 }]);
        assert!(loss);
        assert_eq!(retx.len(), 1);
        assert_eq!(retx[0].sequence_number().unwrap(), 1);

        // fast_retx_count for packet 1 should be reset
        assert_eq!(
            reliability
                .send_buffer
                .in_flight
                .front()
                .unwrap()
                .fast_retx_count,
            0
        );
    }

    #[tokio::test]
    async fn test_rto_retransmission() {
        let mut reliability = ReliabilityLayer::new(test_config());

        // Send a packet
        let frame0 = create_push_frame(0);
        reliability
            .send_buffer
            .add_in_flight(frame0.clone(), Instant::now());

        tokio::time::pause();

        // Advance time just before RTO
        tokio::time::advance(Duration::from_millis(49)).await;
        let (rto_occured, retx) = reliability.check_for_retransmissions();
        assert!(!rto_occured);
        assert!(retx.is_empty());

        // Advance time past RTO
        tokio::time::advance(Duration::from_millis(2)).await; // Total 51ms
        let (rto_occured, retx) = reliability.check_for_retransmissions();
        assert!(rto_occured);
        assert_eq!(retx.len(), 1);
        assert_eq!(retx[0].sequence_number().unwrap(), 0);
    }
} 