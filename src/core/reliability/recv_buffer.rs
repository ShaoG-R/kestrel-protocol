//! Manages the receiving of data, including handling out-of-order packets,
//! reassembly, and generating SACK ranges.
//!
//! 管理数据的接收，包括处理乱序数据包、重组和生成SACK范围。

use crate::packet::sack::SackRange;
use bytes::Bytes;
use std::collections::{btree_map::Entry, BTreeMap};

/// Represents the content of a received packet in the buffer.
///
/// 代表接收缓冲区中已接收数据包的内容。
#[derive(Debug, Clone)]
pub enum PacketOrFin {
    /// A regular data packet.
    ///
    /// 普通数据包。
    Push(Bytes),
    /// A signal indicating the end of the stream.
    ///
    /// 表示流结束的信号。
    Fin,
}

/// Manages incoming data.
#[derive(Debug)]
pub struct ReceiveBuffer {
    /// The next sequence number we expect to receive for contiguous data.
    next_sequence: u32,
    /// Stores out-of-order packets.
    received: BTreeMap<u32, PacketOrFin>,
    /// The maximum number of packets to buffer.
    capacity: usize,
    /// Becomes true once a FIN has been processed by `reassemble`.
    fin_reached: bool,
}

impl Default for ReceiveBuffer {
    fn default() -> Self {
        Self {
            next_sequence: 0,
            received: BTreeMap::new(),
            capacity: 256, // In packets
            fin_reached: false,
        }
    }
}

impl ReceiveBuffer {
    /// Creates a new `ReceiveBuffer`.
    pub fn new(capacity_packets: usize) -> Self {
        Self {
            next_sequence: 0,
            received: BTreeMap::new(),
            capacity: capacity_packets,
            fin_reached: false,
        }
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

    /// Checks if there are any buffered, out-of-order packets.
    pub fn is_empty(&self) -> bool {
        self.received.is_empty()
    }

    /// Receives a data packet payload for a given sequence number. Returns `true`
    /// if the packet was accepted, `false` if it was a duplicate or ignored.
    ///
    /// 为给定的序列号接收一个数据包有效载荷。如果数据包被接受，则返回 `true`，
    /// 如果是重复或被忽略，则返回 `false`。
    pub fn receive_push(&mut self, sequence_number: u32, payload: Bytes) -> bool {
        // 1. Ignore packets received after a FIN has been processed.
        // 2. Ignore old packets that have already been delivered.
        if self.fin_reached || sequence_number < self.next_sequence {
            return false;
        }

        // Use entry API to atomically check for existence and insert if vacant.
        // This handles duplicate packets correctly.
        match self.received.entry(sequence_number) {
            Entry::Vacant(entry) => {
                entry.insert(PacketOrFin::Push(payload));
                true
            }
            Entry::Occupied(_) => {
                // Duplicate packet, ignore.
                false
            }
        }
    }

    /// Receives a FIN signal for a given sequence number. Returns `true` if the
    /// signal was accepted, `false` if it was a duplicate or ignored.
    ///
    /// 为给定的序列号接收一个FIN信号。如果信号被接受，则返回 `true`，
    /// 如果是重复或被忽略，则返回 `false`。
    pub fn receive_fin(&mut self, sequence_number: u32) -> bool {
        // 1. Ignore packets received after a FIN has been processed.
        // 2. Ignore old packets that have already been delivered.
        if self.fin_reached || sequence_number < self.next_sequence {
            return false;
        }

        // Use entry API to atomically check for existence and insert if vacant.
        match self.received.entry(sequence_number) {
            Entry::Vacant(entry) => {
                entry.insert(PacketOrFin::Fin);
                true
            }
            Entry::Occupied(_) => {
                // Duplicate FIN, ignore.
                false
            }
        }
    }

    /// Tries to reassemble contiguous packets.
    ///
    /// Returns a tuple containing:
    /// 1. An `Option<Vec<Bytes>>` with the reassembled data payloads. `None` if no
    ///    contiguous data was available.
    /// 2. A `bool` that is `true` if a `FIN` packet was encountered during reassembly.
    ///
    /// 尝试重组连续的数据包。
    ///
    /// 返回一个元组，包含：
    /// 1. `Option<Vec<Bytes>>`，其中包含重组后的数据有效载荷。如果没有连续数据可用，则为`None`。
    /// 2. `bool`，如果在重组过程中遇到`FIN`数据包，则为`true`。
    pub fn reassemble(&mut self) -> (Option<Vec<Bytes>>, bool) {
        if self.fin_reached {
            // Once the FIN is processed, no more data can be reassembled.
            return (None, false);
        }

        let mut reassembled_data = Vec::new();
        let mut fin_seen = false;

        while let Some(packet) = self.try_pop_next_contiguous() {
            match packet {
                PacketOrFin::Push(payload) => {
                    reassembled_data.push(payload);
                }
                PacketOrFin::Fin => {
                    fin_seen = true;
                    self.fin_reached = true;
                    // Stop reassembly after encountering a FIN. Any subsequent packets
                    // are considered extraneous and should be discarded.
                    self.received.clear();
                    break;
                }
            }
        }

        let data_option = if reassembled_data.is_empty() {
            None
        } else {
            Some(reassembled_data)
        };

        (data_option, fin_seen)
    }

    /// Checks for and removes the next contiguous packet from the buffer.
    fn try_pop_next_contiguous(&mut self) -> Option<PacketOrFin> {
        if let Some((&seq, _)) = self.received.first_key_value() {
            if seq == self.next_sequence {
                // The next expected packet is here. Pop it.
                self.next_sequence += 1;
                return self.received.pop_first().map(|(_, packet)| packet);
            }
        }
        None
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

    fn create_test_recv_buffer() -> ReceiveBuffer {
        ReceiveBuffer::new(256)
    }

    #[test]
    fn test_receive_in_order_and_reassemble() {
        let mut buffer = create_test_recv_buffer();

        buffer.receive_push(0, Bytes::from("hello"));
        buffer.receive_push(1, Bytes::from(" world"));

        let (data_vec_opt, fin_seen) = buffer.reassemble();
        let data_vec = data_vec_opt.unwrap();
        let data: Bytes = data_vec.into_iter().flat_map(|b| b).collect();
        assert_eq!(data, "hello world");
        assert!(!fin_seen);
        assert!(buffer.reassemble().0.is_none());
        assert_eq!(buffer.next_sequence(), 2);
    }

    #[test]
    fn test_receive_out_of_order_and_reassemble() {
        let mut buffer = create_test_recv_buffer();

        buffer.receive_push(1, Bytes::from("world"));
        let (data, fin_seen) = buffer.reassemble();
        assert!(data.is_none());
        assert!(!fin_seen);
        assert_eq!(buffer.next_sequence(), 0);

        buffer.receive_push(0, Bytes::from("hello "));
        let (data_vec_opt, fin_seen) = buffer.reassemble();
        let data_vec = data_vec_opt.unwrap();
        let data: Bytes = data_vec.into_iter().flat_map(|b| b).collect();
        assert_eq!(data, "hello world");
        assert!(!fin_seen);
        assert_eq!(buffer.next_sequence(), 2);
    }

    #[test]
    fn test_receive_with_fin() {
        let mut buffer = create_test_recv_buffer();

        buffer.receive_push(0, Bytes::from("data"));
        buffer.receive_fin(1);
        buffer.receive_push(2, Bytes::from("more")); // Should be ignored after FIN

        let (data_vec_opt, fin_seen) = buffer.reassemble();
        assert!(fin_seen);
        let data: Bytes = data_vec_opt.unwrap().into_iter().flatten().collect();
        assert_eq!(data, "data");
        assert_eq!(buffer.next_sequence(), 2);

        // After the FIN is processed, the buffer should be empty of re-orderable packets
        // up to the FIN, and further reassembly should yield nothing.
        let (data_vec_opt_2, fin_seen_2) = buffer.reassemble();
        assert!(!fin_seen_2);
        assert!(data_vec_opt_2.is_none());
    }

    #[test]
    fn test_receive_duplicate_and_old_packets() {
        let mut buffer = create_test_recv_buffer();

        buffer.receive_push(0, Bytes::from("one"));
        buffer.receive_push(1, Bytes::from("two"));
        let (data_vec_opt, _) = buffer.reassemble();
        let data_vec = data_vec_opt.unwrap();
        let data: Bytes = data_vec.into_iter().flat_map(|b| b).collect();
        assert_eq!(data, "onetwo");
        assert_eq!(buffer.next_sequence(), 2);

        // Receive an old packet (already processed)
        buffer.receive_push(0, Bytes::from("ignored"));
        assert!(buffer.received.is_empty());
        assert!(buffer.reassemble().0.is_none());

        // Receive a future packet, then a duplicate of it
        buffer.receive_push(3, Bytes::from("three"));
        assert_eq!(buffer.received.len(), 1);
        buffer.receive_push(3, Bytes::from("ignored duplicate"));
        assert_eq!(buffer.received.len(), 1);
        assert!(matches!(
            buffer.received.get(&3).unwrap(),
            PacketOrFin::Push(b) if b == "three"
        ));
    }

    #[test]
    fn test_sack_range_generation() {
        let mut buffer = create_test_recv_buffer();
        assert!(buffer.get_sack_ranges().is_empty());

        // Receive discontinuous packets
        buffer.receive_push(0, Bytes::new());
        buffer.receive_push(1, Bytes::new());
        buffer.receive_push(3, Bytes::new());
        buffer.receive_push(4, Bytes::new());
        buffer.receive_push(6, Bytes::new());

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
        let mut buffer = create_test_recv_buffer();
        let received_seqs = [0, 1, 5, 6, 7, 10, 12, 13, 15];
        for &seq in &received_seqs {
            buffer.receive_push(seq, Bytes::new());
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