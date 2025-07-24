//! 定义了用于可靠传输的发送和接收缓冲区。
//! Defines the send and receive buffers for reliable transmission.

use crate::packet::frame::Frame;
use crate::packet::sack::SackRange;
use bytes::Bytes;
use std::collections::{BTreeMap, VecDeque};
use std::time::Instant;

const DEFAULT_RECV_BUFFER_CAPACITY: usize = 256; // In packets

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
#[derive(Debug, Default)]
pub struct SendBuffer {
    /// A queue of frames that haven't been sent yet.
    /// 尚未发送的帧队列。
    to_send: VecDeque<Frame>,
    /// A queue of packets that are in-flight (sent but not yet acknowledged).
    /// This queue is kept sorted by sequence number.
    /// 在途的包队列（已发送但未被确认）。
    /// 此队列按序列号排序。
    pub(crate) in_flight: VecDeque<InFlightPacket>,
}

impl SendBuffer {
    pub fn new() -> Self {
        Self::default()
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

    /// Checks if there are any frames queued to be sent.
    /// 检查是否有任何帧在排队等待发送。
    pub fn is_empty(&self) -> bool {
        self.to_send.is_empty()
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

    /// Reads a contiguous block of data starting from the `next_sequence`.
    /// Returns the combined payload and advances the `next_sequence`.
    ///
    /// 读取从 `next_sequence` 开始的连续数据块。
    /// 返回组合的载荷并推进 `next_sequence`。
    pub fn read(&mut self) -> Option<Bytes> {
        // This is not yet a complete implementation for a stream-like API,
        // but it correctly assembles contiguous packets.
        let mut contiguous_payloads = Vec::new();
        let mut last_seq = self.next_sequence;

        while let Some((seq, payload)) = self.received.first_key_value() {
            if *seq == last_seq {
                contiguous_payloads.push(self.received.pop_first().unwrap().1);
                last_seq += 1;
            } else {
                break;
            }
        }

        self.next_sequence = last_seq;

        if contiguous_payloads.is_empty() {
            None
        } else {
            // This is a simplification. For a real stream, we'd handle
            // partial reads and buffer concatenation more carefully.
            Some(Bytes::from(contiguous_payloads.concat()))
        }
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
