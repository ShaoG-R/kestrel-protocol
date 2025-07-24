//! Manages the receiving of data, including handling out-of-order packets,
//! reassembly, and generating SACK ranges.
//!
//! 管理数据的接收，包括处理乱序数据包、重组和生成SACK范围。

use crate::packet::sack::SackRange;
use bytes::{Bytes, BytesMut};
use std::collections::BTreeMap;

const DEFAULT_RECV_BUFFER_CAPACITY: usize = 256; // In packets

/// Manages incoming data.
#[derive(Debug)]
pub struct ReceiveBuffer {
    /// The next sequence number we expect to receive for contiguous data.
    next_sequence: u32,
    /// Stores out-of-order packets.
    received: BTreeMap<u32, Bytes>,
    /// The maximum number of packets to buffer.
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
    /// Creates a new `ReceiveBuffer`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the size of the available receive window in packets.
    ///
    /// 返回可用接收窗口的大小（以数据包为单位）。
    pub fn window_size(&self) -> u16 {
        (self.capacity.saturating_sub(self.received.len())) as u16
    }

    /// Returns the sequence number of the next contiguous packet expected.
    ///
    /// 返回期望的下一个连续数据包的序列号。
    pub fn next_sequence(&self) -> u32 {
        self.next_sequence
    }

    /// Receives a packet payload for a given sequence number.
    ///
    /// 为给定的序列号接收一个数据包有效载荷。
    ///
    pub fn receive(&mut self, sequence_number: u32, payload: Bytes) {
        // We only care about packets that are at or after the next expected sequence.
        // Duplicates of already processed packets are ignored.
        if sequence_number >= self.next_sequence {
            // Use entry API to avoid overwriting existing packets (duplicates).
            self.received.entry(sequence_number).or_insert(payload);
        }
    }

    /// Tries to reassemble contiguous packets into a single `Bytes` object.
    ///
    /// 尝试将连续的数据包重组成一个单独的 `Bytes` 对象。
    pub fn reassemble(&mut self) -> Option<Bytes> {
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

    /// Checks for and removes the next contiguous packet from the buffer.
    fn try_pop_next_contiguous(&mut self) -> Option<Bytes> {
        if let Some((&seq, _)) = self.received.first_key_value() {
            if seq == self.next_sequence {
                // The next expected packet is here. Pop it.
                self.next_sequence += 1;
                return self.received.pop_first().map(|(_, payload)| payload);
            }
        }
        None
    }

    /// Processes all contiguous packets at the start of the buffer, advancing
    /// `next_sequence`. This is critical for acknowledging packets like FIN
    /// that don't carry data and thus might not trigger a `reassemble` call.
    pub fn process_next_contiguous(&mut self) {
        while self.received.first_key_value().map_or(false, |(&s, _)| s == self.next_sequence) {
            self.received.pop_first();
            self.next_sequence += 1;
        }
    }

    /// Generates a vector of SACK ranges based on the currently buffered packets.
    ///
    /// 根据当前缓冲的数据包生成一个 SACK 范围的向量。
    pub fn get_sack_ranges(&self) -> Vec<SackRange> {
        let mut ranges = Vec::new();
        let mut current_range: Option<SackRange> = None;

        for &seq in self.received.keys() {
            match current_range.as_mut() {
                Some(range) => {
                    if seq == range.end + 1 {
                        // This packet is contiguous with the current range, extend it.
                        range.end = seq;
                    } else {
                        // Gap detected. Finalize the current range and start a new one.
                        ranges.push(range.clone());
                        current_range = Some(SackRange {
                            start: seq,
                            end: seq,
                        });
                    }
                }
                None => {
                    // Start the first range.
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
    use bytes::Bytes;

    #[test]

    fn test_receive_in_order_and_reassemble() {
        let mut buffer = ReceiveBuffer::new();

        buffer.receive(0, Bytes::from("hello"));
        buffer.receive(1, Bytes::from(" world"));

        let data = buffer.reassemble().unwrap();
        assert_eq!(data, "hello world");
        assert!(buffer.reassemble().is_none());
        assert_eq!(buffer.next_sequence(), 2);
    }

    #[test]
    fn test_receive_out_of_order_and_reassemble() {
        let mut buffer = ReceiveBuffer::new();

        buffer.receive(1, Bytes::from("world"));
        assert!(buffer.reassemble().is_none());
        assert_eq!(buffer.next_sequence(), 0);

        buffer.receive(0, Bytes::from("hello "));
        let data = buffer.reassemble().unwrap();
        assert_eq!(data, "hello world");
        assert_eq!(buffer.next_sequence(), 2);
    }

    #[test]
    fn test_receive_duplicate_and_old_packets() {
        let mut buffer = ReceiveBuffer::new();

        buffer.receive(0, Bytes::from("one"));
        buffer.receive(1, Bytes::from("two"));
        assert_eq!(buffer.reassemble().unwrap(), "onetwo");
        assert_eq!(buffer.next_sequence(), 2);

        // Receive an old packet (already processed)
        buffer.receive(0, Bytes::from("ignored"));
        assert!(buffer.received.is_empty());
        assert!(buffer.reassemble().is_none());

        // Receive a future packet, then a duplicate of it
        buffer.receive(3, Bytes::from("three"));
        assert_eq!(buffer.received.len(), 1);
        buffer.receive(3, Bytes::from("ignored duplicate"));
        assert_eq!(buffer.received.len(), 1);
        assert_eq!(buffer.received.get(&3).unwrap(), "three");
    }

    #[test]
    fn test_sack_range_generation() {
        let mut buffer = ReceiveBuffer::new();
        assert!(buffer.get_sack_ranges().is_empty());

        // Receive discontinuous packets
        buffer.receive(0, Bytes::new());
        buffer.receive(1, Bytes::new());
        buffer.receive(3, Bytes::new());
        buffer.receive(4, Bytes::new());
        buffer.receive(6, Bytes::new());

        // Reassemble contiguous part
        let _ = buffer.reassemble();
        assert_eq!(buffer.next_sequence(), 2);

        // Check SACK ranges for what's left
        let sack_ranges = buffer.get_sack_ranges();
        assert_eq!(
            sack_ranges,
            vec![
                SackRange { start: 3, end: 4 },
                SackRange { start: 6, end: 6 }
            ]
        );
    }

    #[test]
    fn test_sack_range_complex() {
        let mut buffer = ReceiveBuffer::new();
        let received_seqs = [0, 1, 5, 6, 7, 10, 12, 13, 15];
        for &seq in &received_seqs {
            buffer.receive(seq, Bytes::new());
        }

        let _ = buffer.reassemble();
        assert_eq!(buffer.next_sequence(), 2);

        let sack_ranges = buffer.get_sack_ranges();
        assert_eq!(
            sack_ranges,
            vec![
                SackRange { start: 5, end: 7 },
                SackRange { start: 10, end: 10 },
                SackRange { start: 12, end: 13 },
                SackRange { start: 15, end: 15 },
            ]
        );
    }
} 