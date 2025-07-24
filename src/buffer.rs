//! 定义了用于可靠传输的发送和接收缓冲区。
//! Defines the send and receive buffers for reliable transmission.

use crate::packet::frame::Frame;
use crate::packet::sack::SackRange;
use bytes::{Bytes, BytesMut};
use std::collections::{BTreeMap, VecDeque};
use std::time::Instant;

const DEFAULT_RECV_BUFFER_CAPACITY: usize = 256; // In packets
const DEFAULT_SEND_BUFFER_CAPACITY_BYTES: usize = 1024 * 1024; // 1 MB

/// A packet that has been sent but not yet acknowledged (in-flight).
/// 一个已发送但尚未确认的包（在途）。
#[derive(Debug)]
pub struct InFlightPacket {
    /// The time the packet was last sent. Used for RTO calculation.
    /// 包最后一次发送的时间。用于RTO计算。
    pub last_sent_at: Instant,
    /// The actual frame that was sent.
    /// 发送的实际帧。
    pub frame: Frame,
    /// Counter for fast retransmission. Incremented each time an ACK for a later
    /// packet is received.
    /// 用于快速重传的计数器。每当收到一个更高序列号包的ACK时递增。
    pub fast_retx_count: u16,
}

/// Manages outgoing data, tracking which packets have been sent and acknowledged.
/// 管理待发送的数据，追踪哪些包已被发送和确认。
#[derive(Debug)]
pub struct SendBuffer {
    /// A queue of frames that haven't been sent yet.
    /// 尚未发送的帧队列。
    to_send: VecDeque<Frame>,
    /// A queue of packets that are in-flight (sent but not yet acknowledged).
    /// This queue is kept sorted by sequence number.
    /// 在途的包队列（已发送但未被确认）。
    /// 此队列按序列号排序。
    pub(crate) in_flight: VecDeque<InFlightPacket>,
    /// A buffer for stream data waiting to be packetized.
    /// 等待打包的流数据缓冲区。
    stream_buffer: VecDeque<u8>,
    /// The capacity of the stream buffer in bytes.
    /// 流缓冲区的容量（以字节为单位）。
    stream_buffer_capacity: usize,
}

impl Default for SendBuffer {
    fn default() -> Self {
        Self {
            to_send: VecDeque::new(),
            in_flight: VecDeque::new(),
            stream_buffer: VecDeque::new(),
            stream_buffer_capacity: DEFAULT_SEND_BUFFER_CAPACITY_BYTES,
        }
    }
}

impl SendBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Writes data to the stream buffer. Returns the number of bytes written.
    /// 将数据写入流缓冲区。返回写入的字节数。
    pub fn write_to_stream(&mut self, buf: &[u8]) -> usize {
        let space_available = self
            .stream_buffer_capacity
            .saturating_sub(self.stream_buffer.len());
        let bytes_to_write = std::cmp::min(buf.len(), space_available);
        self.stream_buffer.extend(&buf[..bytes_to_write]);
        bytes_to_write
    }

    /// Creates a data chunk of up to `max_size` from the stream buffer.
    /// 从流缓冲区创建一个最大为 `max_size` 的数据块。
    pub fn create_chunk(&mut self, max_size: usize) -> Option<Bytes> {
        let chunk_size = std::cmp::min(self.stream_buffer.len(), max_size);
        if chunk_size == 0 {
            return None;
        }
        Some(self.stream_buffer.drain(..chunk_size).collect())
    }

    /// Queues a frame to be sent for the first time.
    /// 将一个帧加入发送队列，用于首次发送。
    pub fn queue_frame(&mut self, frame: Frame) {
        self.to_send.push_back(frame);
    }

    /// Retrieves the next frame to be sent from the `to_send` queue.
    /// This does NOT mark it as in-flight yet. The caller is responsible for
    /// calling `add_in_flight` after the packet is successfully sent.
    ///
    /// 从 `to_send` 队列中获取下一个要发送的帧。
    /// 这还不会将其标记为在途。调用者负责在包成功发送后调用 `add_in_flight`。
    pub fn pop_next_frame(&mut self) -> Option<Frame> {
        self.to_send.pop_front()
    }

    /// Adds a sent frame to the in-flight tracking list.
    /// 将一个已发送的帧添加到在途跟踪列表。
    pub fn add_in_flight(&mut self, frame: Frame, now: Instant) {
        self.in_flight.push_back(InFlightPacket {
            last_sent_at: now,
            frame,
            fast_retx_count: 0,
        });
    }

    /// Iterates mutably over the in-flight packets. Useful for updating send times.
    /// 对在途数据包进行可变迭代。用于更新发送时间。
    pub fn iter_in_flight_mut(&mut self) -> std::collections::vec_deque::IterMut<'_, InFlightPacket> {
        self.in_flight.iter_mut()
    }

    /// Checks if there are any frames queued to be sent or stream data to be packetized.
    /// 检查是否有任何帧在排队等待发送或有流数据待打包。
    pub fn has_data_to_send(&self) -> bool {
        !self.to_send.is_empty() || !self.stream_buffer.is_empty()
    }
}

/// Manages incoming data, reordering out-of-order packets and managing acknowledgments.
/// 管理传入的数据，重排乱序的包并管理确认。
#[derive(Debug)]
pub struct ReceiveBuffer {
    /// The next sequence number we are expecting to deliver to the application.
    /// 我们期望交付给应用程序的下一个序列号。
    next_sequence: u32,
    /// A map of received packets that are waiting to be reordered and delivered.
    /// The key is the sequence number.
    /// 已接收但等待重排和交付的包的映射。
    /// 键是序列号。
    received: BTreeMap<u32, Bytes>,
    /// The total capacity of the buffer in packets.
    /// 缓冲区的总容量（以包为单位）。
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
    pub fn new() -> Self {
        Self::default()
    }

    /// Calculates the available window size in packets.
    /// 计算可用的窗口大小（以包为单位）。
    pub fn window_size(&self) -> u16 {
        (self.capacity.saturating_sub(self.received.len())) as u16
    }

    /// Gets the next sequence number that the buffer is expecting.
    /// This is used for the `recv_next_sequence` field in outgoing headers.
    ///
    /// 获取缓冲区期望的下一个序列号。
    /// 这用于出站头中的 `recv_next_sequence` 字段。
    pub fn next_sequence(&self) -> u32 {
        self.next_sequence
    }

    /// Receives a data payload for a given sequence number.
    /// 为给定的序列号接收一个数据载荷。
    pub fn receive(&mut self, sequence_number: u32, payload: Bytes) {
        // Don't insert if it's already been delivered.
        if sequence_number >= self.next_sequence {
            self.received.insert(sequence_number, payload);
        }
    }

    /// Tries to reassemble contiguous packets into a single `Bytes` object.
    /// Returns `Some(Bytes)` if any new data was reassembled.
    ///
    /// 尝试将连续的数据包重组成一个单独的 `Bytes` 对象。
    /// 如果有任何新数据被重组，则返回 `Some(Bytes)`。
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

    /// Checks for the next contiguous packet, removes it from the buffer,
    /// advances the sequence number, and returns its payload.
    ///
    /// 检查下一个连续的数据包，将其从缓冲区中移除，
    /// 推进序列号，并返回其载荷。
    fn try_pop_next_contiguous(&mut self) -> Option<Bytes> {
        if let Some((seq, _payload)) = self.received.first_key_value() {
            if *seq == self.next_sequence {
                self.next_sequence += 1;
                // Use remove_entry to get ownership of the key and value
                return Some(self.received.pop_first().unwrap().1);
            }
        }
        None
    }

    /// Generates a list of SACK ranges based on the currently received packets.
    /// 根据当前接收到的包生成一个SACK范围列表。
    pub fn get_sack_ranges(&self) -> Vec<SackRange> {
        let mut ranges = Vec::new();
        let mut current_range: Option<SackRange> = None;

        for &seq in self.received.keys() {
            match current_range.as_mut() {
                Some(range) => {
                    if seq == range.end + 1 {
                        // Extend the current range
                        range.end = seq;
                    } else {
                        // End the current range and start a new one
                        ranges.push(range.clone());
                        current_range = Some(SackRange {
                            start: seq,
                            end: seq,
                        });
                    }
                }
                None => {
                    // Start a new range
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
    use crate::packet::command::Command;
    use crate::packet::header::ShortHeader;
    use bytes::Bytes;

    fn create_dummy_push_frame(seq: u32) -> Frame {
        Frame::Push {
            header: ShortHeader {
                command: Command::Push,
                connection_id: 1,
                recv_window_size: 100,
                timestamp: 0,
                sequence_number: seq,
                recv_next_sequence: 0,
            },
            payload: Bytes::from_static(b"hello"),
        }
    }

    #[test]
    fn send_buffer_write_and_chunk() {
        let mut send_buffer = SendBuffer::new();
        let data1 = b"hello world";
        let data2 = b"goodbye world";

        // Write data
        assert_eq!(send_buffer.write_to_stream(data1), data1.len());
        assert_eq!(send_buffer.write_to_stream(data2), data2.len());
        assert_eq!(
            send_buffer.stream_buffer.len(),
            data1.len() + data2.len()
        );

        // Create a chunk smaller than the total data
        let chunk1 = send_buffer.create_chunk(10).unwrap();
        assert_eq!(chunk1.as_ref(), &data1[..10]);
        assert_eq!(
            send_buffer.stream_buffer.len(),
            data1.len() + data2.len() - 10
        );

        // Create another chunk
        let chunk2 = send_buffer.create_chunk(100).unwrap(); // ask for more than is there
        let mut expected = Vec::new();
        expected.extend_from_slice(&data1[10..]);
        expected.extend_from_slice(data2);
        assert_eq!(chunk2.as_ref(), expected.as_slice());

        // Buffer should be empty now
        assert!(send_buffer.stream_buffer.is_empty());
        assert!(send_buffer.create_chunk(10).is_none());
    }

    #[test]
    fn send_buffer_capacity() {
        let mut send_buffer = SendBuffer::new();
        send_buffer.stream_buffer_capacity = 20;

        let data1 = &[0; 15];
        let data2 = &[1; 10];

        assert_eq!(send_buffer.write_to_stream(data1), 15);
        // Only 5 bytes of space left
        assert_eq!(send_buffer.write_to_stream(data2), 5);
        // Cant write more
        assert_eq!(send_buffer.write_to_stream(data2), 0);

        assert_eq!(send_buffer.stream_buffer.len(), 20);
    }

    #[test]
    fn send_buffer_has_data() {
        let mut send_buffer = SendBuffer::new();
        assert!(!send_buffer.has_data_to_send());

        send_buffer.write_to_stream(b"some data");
        assert!(send_buffer.has_data_to_send());
        send_buffer.create_chunk(100);
        assert!(!send_buffer.has_data_to_send());

        send_buffer.queue_frame(create_dummy_push_frame(1));
        assert!(send_buffer.has_data_to_send());
        send_buffer.pop_next_frame();
        assert!(!send_buffer.has_data_to_send());
    }

    #[test]
    fn receive_buffer_in_order() {
        let mut recv_buffer = ReceiveBuffer::new();
        assert_eq!(recv_buffer.next_sequence(), 0);

        let payload1 = Bytes::from_static(b"hello");
        let payload2 = Bytes::from_static(b" world");

        recv_buffer.receive(0, payload1.clone());
        let reassembled1 = recv_buffer.reassemble().unwrap();
        assert_eq!(reassembled1, payload1);
        assert_eq!(recv_buffer.next_sequence(), 1);

        recv_buffer.receive(1, payload2.clone());
        let reassembled2 = recv_buffer.reassemble().unwrap();
        assert_eq!(reassembled2, payload2);
        assert_eq!(recv_buffer.next_sequence(), 2);
    }

    #[test]
    fn receive_buffer_out_of_order() {
        let mut recv_buffer = ReceiveBuffer::new();
        let payload1 = Bytes::from_static(b"hello");
        let payload2 = Bytes::from_static(b" world");
        let payload3 = Bytes::from_static(b"!");

        // Receive 2, then 0, then 1
        recv_buffer.receive(2, payload3.clone());
        assert!(recv_buffer.reassemble().is_none()); // Nothing to reassemble yet

        recv_buffer.receive(0, payload1.clone());
        let reassembled1 = recv_buffer.reassemble().unwrap(); // Reassembles packet 0
        assert_eq!(reassembled1, payload1);
        assert_eq!(recv_buffer.next_sequence(), 1);

        // stream buffer is empty now, but packet 2 is still held
        assert!(recv_buffer.reassemble().is_none());

        recv_buffer.receive(1, payload2.clone());
        let reassembled2 = recv_buffer.reassemble().unwrap(); // Reassembles 1 and 2

        let mut expected_data = BytesMut::new();
        expected_data.extend_from_slice(&payload2);
        expected_data.extend_from_slice(&payload3);
        assert_eq!(reassembled2, expected_data.freeze());
        assert_eq!(recv_buffer.next_sequence(), 3);
    }

    #[test]
    fn receive_buffer_sack_ranges() {
        let mut recv_buffer = ReceiveBuffer::new();

        // No packets
        assert!(recv_buffer.get_sack_ranges().is_empty());

        // One block
        recv_buffer.receive(2, Bytes::new());
        recv_buffer.receive(3, Bytes::new());
        recv_buffer.receive(4, Bytes::new());
        assert_eq!(
            recv_buffer.get_sack_ranges(),
            vec![SackRange { start: 2, end: 4 }]
        );

        // Two disjoint blocks
        recv_buffer.receive(6, Bytes::new());
        recv_buffer.receive(7, Bytes::new());
        assert_eq!(
            recv_buffer.get_sack_ranges(),
            vec![SackRange { start: 2, end: 4 }, SackRange { start: 6, end: 7 }]
        );

        // Fill the gap
        recv_buffer.receive(5, Bytes::new());
        assert_eq!(
            recv_buffer.get_sack_ranges(),
            vec![SackRange { start: 2, end: 7 }]
        );
    }
}
