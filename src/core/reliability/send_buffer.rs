//! Manages the sending of data, including buffering, packetizing, and tracking
//! in-flight packets.
//!
//! 管理数据的发送，包括缓冲、打包和跟踪在途数据包。

use crate::packet::frame::Frame;
use crate::packet::sack::SackRange;
use super::sack_manager::{SackManager, SackInFlightPacket};
use bytes::Bytes;
use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;
use tokio::time::Instant;
use tracing::debug;

/// A packet that has been sent but not yet acknowledged (in-flight).
/// This is now an alias for the SACK manager's version.
pub type InFlightPacket = SackInFlightPacket;

/// Manages outgoing data.
#[derive(Debug)]
pub struct SendBuffer {
    /// A queue of `Bytes` objects waiting to be packetized. This approach avoids
    /// copying data into a single large buffer. Each `Bytes` object is a separate
    /// block of data from a `write` call.
    stream_buffer: VecDeque<Bytes>,
    /// The total size of all `Bytes` objects in `stream_buffer`.
    stream_buffer_size: usize,
    /// Queue of packets that have been sent but not yet acknowledged, keyed by sequence number.
    in_flight: BTreeMap<u32, InFlightPacket>,
    /// Capacity of the stream buffer in bytes.
    stream_buffer_capacity: usize,
}

impl SendBuffer {
    /// Creates a new `SendBuffer`.
    pub fn new(capacity_bytes: usize) -> Self {
        Self {
            stream_buffer: VecDeque::new(),
            stream_buffer_size: 0,
            in_flight: BTreeMap::new(),
            stream_buffer_capacity: capacity_bytes,
        }
    }

    /// Writes data to the stream buffer. Returns the number of bytes written.
    /// This implementation is zero-copy; it just adds the `Bytes` object to a queue.
    ///
    /// 将数据写入流缓冲区。返回写入的字节数。
    /// 这个实现是零拷贝的；它只是将 `Bytes` 对象添加到一个队列中。
    pub fn write_to_stream(&mut self, buf: Bytes) -> usize {
        let space_available = self
            .stream_buffer_capacity
            .saturating_sub(self.stream_buffer_size);
        if space_available == 0 {
            return 0;
        }

        let bytes_to_write = std::cmp::min(buf.len(), space_available);
        if bytes_to_write < buf.len() {
            let chunk = buf.slice(..bytes_to_write);
            self.stream_buffer.push_back(chunk);
        } else {
            self.stream_buffer.push_back(buf);
        }
        self.stream_buffer_size += bytes_to_write;
        bytes_to_write
    }

    /// Creates a data chunk for a new packet from the stream buffer.
    pub fn create_chunk(&mut self, max_size: usize) -> Option<Bytes> {
        let first_chunk = self.stream_buffer.front_mut()?;
        let chunk_size = std::cmp::min(first_chunk.len(), max_size);

        if chunk_size == 0 {
            // This can happen if the front chunk is empty for some reason.
            self.stream_buffer.pop_front();
            return self.create_chunk(max_size); // Try again with the next one.
        }

        let chunk = if chunk_size >= first_chunk.len() {
            // The whole chunk is being taken, so we can pop it.
            self.stream_buffer.pop_front().unwrap()
        } else {
            // Only a part of the chunk is taken.
            first_chunk.split_to(chunk_size)
        };

        self.stream_buffer_size -= chunk.len();
        Some(chunk)
    }

    /// Takes all data from the stream buffer.
    ///
    /// 取出流缓冲区中的所有数据。
    pub fn take_stream_buffer(&mut self) -> impl Iterator<Item = Bytes> {
        self.stream_buffer_size = 0;
        self.stream_buffer.drain(..)
    }

    /// Checks if the stream buffer is empty.
    ///
    /// 检查流缓冲区是否为空。
    pub fn is_stream_buffer_empty(&self) -> bool {
        self.stream_buffer.is_empty()
    }

    /// Adds a packet to the in-flight queue.
    ///
    /// 将数据包添加到在途队列中。
    pub fn add_in_flight(&mut self, frame: Frame, now: Instant) {
        let seq = frame
            .sequence_number()
            .expect("Cannot add frame with no sequence number to in-flight buffer");
        let packet = SackInFlightPacket {
            last_sent_at: now,
            frame,
            fast_retx_count: 0,
        };
        self.in_flight.insert(seq, packet);
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
            .values()
            .any(|p| matches!(p.frame, Frame::Fin { .. }))
    }

    /// Processes SACK information using the centralized SACK manager.
    /// Returns a tuple of (frames_to_retransmit, rtt_samples).
    pub fn handle_ack(
        &mut self,
        recv_next_seq: u32,
        sack_ranges: &[SackRange],
        fast_retx_threshold: u16,
        now: Instant,
    ) -> (Vec<Frame>, Vec<Duration>) {
        // Create a temporary SACK manager for this operation
        // In a future refactor, this could be passed in as a parameter
        let sack_manager = SackManager::new(fast_retx_threshold);
        
        let result = sack_manager.process_ack(
            &mut self.in_flight,
            recv_next_seq,
            sack_ranges,
            now,
        );

        (result.frames_to_retransmit, result.rtt_samples)
    }

    /// Checks for packets that have timed out based on the RTO.
    pub fn check_for_rto(&mut self, rto: std::time::Duration, now: Instant) -> Vec<Frame> {
        let mut frames_to_resend = Vec::new();
        for packet in self.in_flight.values_mut() {
            if now.duration_since(packet.last_sent_at) > rto {
                debug!(
                    seq = packet.frame.sequence_number().unwrap_or(u32::MAX),
                    "RTO retransmission triggered"
                );
                frames_to_resend.push(packet.frame.clone());
                packet.last_sent_at = now;
            }
        }
        frames_to_resend
    }

    /// Returns the deadline for the next RTO event.
    pub fn next_rto_deadline(&self, rto: std::time::Duration) -> Option<Instant> {
        self.in_flight.values().next().map(|p| p.last_sent_at + rto)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn create_push_frame(seq: u32) -> Frame {
        let payload = Bytes::from(format!("packet-{}", seq));
        Frame::new_push(1, seq, 0, 10, 0, payload)
    }

    fn create_test_send_buffer() -> SendBuffer {
        SendBuffer::new(1024 * 1024)
    }

    #[test]
    fn test_stream_buffering_and_chunking() {
        let mut buffer = create_test_send_buffer();
        let data1 = Bytes::from_static(b"hello world");
        let data2 = Bytes::from_static(b", this is a test");

        assert_eq!(buffer.write_to_stream(data1), 11);
        assert_eq!(buffer.write_to_stream(data2), 16);
        assert_eq!(buffer.stream_buffer_size, 27);

        // First chunk should come from the first `Bytes` object.
        let chunk1 = buffer.create_chunk(5).unwrap();
        assert_eq!(chunk1, "hello");
        assert_eq!(buffer.stream_buffer_size, 22);
        assert_eq!(buffer.stream_buffer.front().unwrap(), " world");

        // Second chunk finishes off the first `Bytes` object.
        let chunk2 = buffer.create_chunk(10).unwrap();
        assert_eq!(chunk2, " world");
        assert_eq!(buffer.stream_buffer_size, 16);
        assert!(buffer
            .stream_buffer
            .front()
            .unwrap()
            .eq(", this is a test"));

        // Third chunk takes the entire second `Bytes` object.
        let chunk3 = buffer.create_chunk(100).unwrap();
        assert_eq!(chunk3, ", this is a test");
        assert_eq!(buffer.stream_buffer_size, 0);

        assert!(buffer.create_chunk(10).is_none());
        assert!(buffer.is_stream_buffer_empty());
    }

    #[tokio::test]
    async fn test_fast_retransmission() {
        let mut buffer = create_test_send_buffer();
        let now = Instant::now();
        let threshold = 2; // Let's use 2 for this test.

        for i in 0..=3 {
            buffer.add_in_flight(create_push_frame(i), now);
        }

        // ACK for 0. This should remove packet 0.
        let (retx, rtts) = buffer.handle_ack(1, &[], threshold, now);
        assert!(retx.is_empty());
        assert_eq!(rtts.len(), 1);
        assert!(!buffer.in_flight.contains_key(&0));
        assert_eq!(buffer.in_flight.first_key_value().unwrap().0, &1);

        // Receive ACK for packet 2, implies 1 is lost. fast_retx_count for packet 1 becomes 1.
        let (retx1, _) = buffer.handle_ack(1, &[SackRange { start: 2, end: 2 }], threshold, now);
        assert!(retx1.is_empty());
        assert!(!buffer.in_flight.contains_key(&2));
        assert_eq!(buffer.in_flight.get(&1).unwrap().fast_retx_count, 1);

        // Receive ACK for packet 3. fast_retx_count for packet 1 becomes 2. Threshold met.
        let (retx2, _) = buffer.handle_ack(1, &[SackRange { start: 3, end: 3 }], threshold, now);
        assert_eq!(retx2.len(), 1, "Should retransmit packet 1");
        assert_eq!(retx2[0].sequence_number().unwrap(), 1);

        // After retransmission, the count should be reset. Packet 3 should be gone.
        assert!(!buffer.in_flight.contains_key(&3));
        assert_eq!(buffer.in_flight.get(&1).unwrap().fast_retx_count, 0);
    }

    #[tokio::test]
    async fn test_rto_retransmission() {
        let mut buffer = create_test_send_buffer();
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