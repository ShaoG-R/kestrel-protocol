//! Manages the sending of data, including buffering, packetizing, and tracking
//! in-flight packets.
//!
//! 管理数据的发送，包括缓冲、打包和跟踪在途数据包。

use crate::packet::frame::Frame;
use crate::packet::sack::SackRange;
use bytes::{Bytes, BytesMut};
use std::collections::{BTreeSet, VecDeque};
use tokio::time::Instant;

const DEFAULT_SEND_BUFFER_CAPACITY_BYTES: usize = 1024 * 1024; // 1 MB

/// A packet that has been sent but not yet acknowledged (in-flight).
#[derive(Debug, Clone)]
pub struct InFlightPacket {
    pub last_sent_at: Instant,
    pub frame: Frame,
    pub fast_retx_count: u16,
}

/// Manages outgoing data.
#[derive(Debug)]
pub struct SendBuffer {
    /// Buffer for user data waiting to be packetized.
    stream_buffer: BytesMut,
    /// Queue of packets that have been sent but not yet acknowledged.
    in_flight: VecDeque<InFlightPacket>,
    /// Capacity of the stream buffer in bytes.
    stream_buffer_capacity: usize,
}

impl Default for SendBuffer {
    fn default() -> Self {
        Self {
            stream_buffer: BytesMut::new(),
            in_flight: VecDeque::new(),
            stream_buffer_capacity: DEFAULT_SEND_BUFFER_CAPACITY_BYTES,
        }
    }
}

impl SendBuffer {
    /// Creates a new `SendBuffer`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Writes data to the stream buffer. Returns the number of bytes written.
    pub fn write_to_stream(&mut self, buf: &[u8]) -> usize {
        let space_available = self
            .stream_buffer_capacity
            .saturating_sub(self.stream_buffer.len());
        let bytes_to_write = std::cmp::min(buf.len(), space_available);
        self.stream_buffer.extend_from_slice(&buf[..bytes_to_write]);
        bytes_to_write
    }

    /// Creates a data chunk for a new packet from the stream buffer.
    pub fn create_chunk(&mut self, max_size: usize) -> Option<Bytes> {
        let chunk_size = std::cmp::min(self.stream_buffer.len(), max_size);
        if chunk_size == 0 {
            return None;
        }
        Some(self.stream_buffer.split_to(chunk_size).freeze())
    }

    /// Takes all data from the stream buffer.
    ///
    /// 取出流缓冲区中的所有数据。
    pub fn take_stream_buffer(&mut self) -> Bytes {
        self.stream_buffer.split_to(self.stream_buffer.len()).freeze()
    }

    /// Checks if the stream buffer is empty.
    ///
    /// 检查流缓冲区是否为空。
    pub fn is_stream_buffer_empty(&self) -> bool {
        self.stream_buffer.is_empty()
    }

    /// Adds a packet to the in-flight queue.
    pub fn add_in_flight(&mut self, frame: Frame, now: Instant) {
        self.in_flight.push_back(InFlightPacket {
            last_sent_at: now,
            frame,
            fast_retx_count: 0,
        });
    }

    /// Returns the number of packets currently in flight.
    pub fn in_flight_count(&self) -> usize {
        self.in_flight.len()
    }

    /// Checks if the in-flight buffer is empty.
    pub fn is_in_flight_empty(&self) -> bool {
        self.in_flight.is_empty()
    }

    /// Checks if a FIN frame is already in the in-flight queue.
    pub fn has_fin_in_flight(&self) -> bool {
        self.in_flight
            .iter()
            .any(|p| matches!(p.frame, Frame::Fin { .. }))
    }

    /// Processes SACK information, removing acknowledged packets and identifying
    /// packets that need to be fast-retransmitted.
    pub fn handle_ack(
        &mut self,
        sack_ranges: &[SackRange],
        fast_retx_threshold: u16,
        now: Instant,
    ) -> Vec<Frame> {
        if sack_ranges.is_empty() {
            return Vec::new();
        }

        let mut acked_seq_numbers = BTreeSet::new();
        for range in sack_ranges {
            for seq in range.start..=range.end {
                acked_seq_numbers.insert(seq);
            }
        }

        let highest_acked_seq = *acked_seq_numbers.iter().next_back().unwrap();

        // Remove acknowledged packets from the in-flight queue.
        self.in_flight
            .retain(|p| !p.frame.sequence_number().map_or(false, |s| acked_seq_numbers.contains(&s)));

        // Identify frames for fast retransmission.
        let mut frames_to_fast_retx = Vec::new();
        for packet in self.in_flight.iter_mut() {
            if let Some(seq) = packet.frame.sequence_number() {
                if seq < highest_acked_seq {
                    packet.fast_retx_count += 1;
                    if packet.fast_retx_count >= fast_retx_threshold {
                        frames_to_fast_retx.push(packet.frame.clone());
                        packet.last_sent_at = now;
                        packet.fast_retx_count = 0; // Reset after retransmission
                    }
                }
            }
        }
        frames_to_fast_retx
    }

    /// Checks for packets that have timed out based on the RTO.
    pub fn check_for_rto(&mut self, rto: std::time::Duration, now: Instant) -> Vec<Frame> {
        let mut frames_to_resend = Vec::new();
        for packet in self.in_flight.iter_mut() {
            if now.duration_since(packet.last_sent_at) > rto {
                frames_to_resend.push(packet.frame.clone());
                packet.last_sent_at = now;
            }
        }
        frames_to_resend
    }

    /// Returns the deadline for the next RTO event.
    pub fn next_rto_deadline(&self, rto: std::time::Duration) -> Option<Instant> {
        self.in_flight.front().map(|p| p.last_sent_at + rto)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::header::ShortHeader;
    use std::time::Duration;

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
    fn test_stream_buffering_and_chunking() {
        let mut buffer = SendBuffer::new();
        let data = b"hello world, this is a test";
        
        assert_eq!(buffer.write_to_stream(data), data.len());
        
        let chunk1 = buffer.create_chunk(5).unwrap();
        assert_eq!(chunk1, "hello");

        let chunk2 = buffer.create_chunk(10).unwrap();
        assert_eq!(chunk2, " world, th");
        
        let chunk3 = buffer.create_chunk(100).unwrap();
        assert_eq!(chunk3, "is is a test");

        assert!(buffer.create_chunk(10).is_none());
    }

    #[tokio::test]
    async fn test_fast_retransmission() {
        let mut buffer = SendBuffer::new();
        let now = Instant::now();

        for i in 0..=3 {
            buffer.add_in_flight(create_push_frame(i), now);
        }

        // ACK for 0 removes it.
        buffer.handle_ack(&[SackRange { start: 0, end: 0 }], 2, now);
        assert_eq!(buffer.in_flight_count(), 3);

        // Receive ACK for packet 2, implies 1 is lost. fast_retx_count for packet 1 becomes 1.
        let retx1 = buffer.handle_ack(&[SackRange { start: 2, end: 2 }], 2, now);
        assert!(retx1.is_empty());
        assert_eq!(buffer.in_flight.front().unwrap().fast_retx_count, 1);

        // Receive ACK for packet 3. fast_retx_count for packet 1 becomes 2. Threshold met.
        let retx2 = buffer.handle_ack(&[SackRange { start: 3, end: 3 }], 2, now);
        assert_eq!(retx2.len(), 1);
        assert_eq!(retx2[0].sequence_number().unwrap(), 1);
        assert_eq!(buffer.in_flight.front().unwrap().fast_retx_count, 0); // Reset
    }

    #[tokio::test]
    async fn test_rto_retransmission() {
        let mut buffer = SendBuffer::new();
        let rto = Duration::from_millis(100);

        buffer.add_in_flight(create_push_frame(0), Instant::now());
        tokio::time::pause();
        tokio::time::advance(Duration::from_millis(50)).await;

        // No RTO yet
        let retx1 = buffer.check_for_rto(rto, Instant::now());
        assert!(retx1.is_empty());

        // RTO expires
        tokio::time::advance(Duration::from_millis(60)).await;
        let retx2 = buffer.check_for_rto(rto, Instant::now());
        assert_eq!(retx2.len(), 1);
        assert_eq!(retx2[0].sequence_number().unwrap(), 0);
    }
} 