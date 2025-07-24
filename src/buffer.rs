//! 定义了用于可靠传输的发送和接收缓冲区。
//! Defines the send and receive buffers for reliable transmission.

use crate::packet::frame::Frame;
use std::collections::VecDeque;

/// Manages outgoing data, tracking which packets have been sent and acknowledged.
/// 管理待发送的数据，追踪哪些包已被发送和确认。
#[derive(Debug, Default)]
pub struct SendBuffer {
    /// A queue of frames to be sent.
    /// 待发送的帧队列。
    to_send: VecDeque<Frame>,
    // TODO: We will also need a way to track packets that are in-flight (sent but not yet acked).
    // 我们还需要追踪已发送但未被确认的包。
}

impl SendBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues a frame to be sent.
    /// 将一个帧加入发送队列。
    pub fn queue_frame(&mut self, frame: Frame) {
        self.to_send.push_back(frame);
    }

    /// Retrieves the next frame to be sent.
    /// 获取下一个待发送的帧。
    pub fn pop_next_frame(&mut self) -> Option<Frame> {
        self.to_send.pop_front()
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
