//! 定义了用于可靠传输的发送和接收缓冲区。
//! Defines the send and receive buffers for reliable transmission.

use crate::packet::frame::Frame;
use std::collections::VecDeque;
use std::time::Instant;

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
#[derive(Debug, Default)]
pub struct ReceiveBuffer {
    // TODO: We will store out-of-order packets here until they can be delivered to the application.
    // 我们将在这里存储乱序的包，直到它们可以被交付给应用程序。
}

impl ReceiveBuffer {
    pub fn new() -> Self {
        Self::default()
    }
}
