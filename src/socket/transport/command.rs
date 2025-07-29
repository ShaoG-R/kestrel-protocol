//! Commands for transport layer operations.
//!
//! 传输层操作的命令。

use super::{FrameBatch, Transport};
use std::sync::Arc;

/// Commands for the transport sender task.
///
/// 传输发送任务的命令。
#[derive(Debug)]
pub enum TransportCommand<T: Transport> {
    /// Send a batch of frames.
    /// 发送一批帧。
    Send(FrameBatch),
    /// Swap the underlying transport.
    /// 交换底层传输。
    SwapTransport(Arc<T>),
}