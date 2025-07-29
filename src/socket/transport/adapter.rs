//! Adapter for bridging transport commands with legacy sender commands.
//!
//! 用于桥接传输命令与传统发送器命令的适配器。

use super::{FrameBatch, Transport, TransportCommand};
use crate::socket::SenderTaskCommand;
use crate::socket::traits::AsyncUdpSocket;
use tokio::sync::mpsc;
use tracing::error;

/// Adapter that converts legacy SenderTaskCommand to TransportCommand.
///
/// This allows existing Endpoint code to work with the new transport layer
/// without requiring immediate changes to the Endpoint implementation.
///
/// 将传统SenderTaskCommand转换为TransportCommand的适配器。
///
/// 这允许现有的Endpoint代码与新的传输层一起工作，
/// 而无需立即更改Endpoint实现。
pub struct TransportAdapter<S: AsyncUdpSocket, T: Transport> {
    transport_tx: mpsc::Sender<TransportCommand<T>>,
    _phantom: std::marker::PhantomData<S>,
}

impl<S: AsyncUdpSocket, T: Transport> TransportAdapter<S, T> {
    /// Creates a new transport adapter.
    ///
    /// 创建新的传输适配器。
    pub fn new(transport_tx: mpsc::Sender<TransportCommand<T>>) -> Self {
        Self {
            transport_tx,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Converts a SenderTaskCommand to a TransportCommand and sends it.
    ///
    /// 将SenderTaskCommand转换为TransportCommand并发送。
    pub async fn send(&self, command: SenderTaskCommand<S>) -> Result<(), mpsc::error::SendError<TransportCommand<T>>> {
        match command {
            SenderTaskCommand::Send(send_cmd) => {
                let batch = FrameBatch {
                    remote_addr: send_cmd.remote_addr,
                    frames: send_cmd.frames,
                };
                self.transport_tx.send(TransportCommand::Send(batch)).await
            }
            SenderTaskCommand::SwapSocket(_) => {
                // SwapSocket is handled differently in the transport layer
                // For now, we'll ignore this command as socket swapping
                // is handled at the transport level
                // SwapSocket在传输层中的处理方式不同
                // 现在我们忽略此命令，因为套接字交换在传输层处理
                Ok(())
            }
        }
    }
}

/// Creates a channel pair that bridges legacy sender commands to transport commands.
///
/// Returns (sender_for_endpoint, receiver_for_adapter_task).
/// The adapter task should be spawned to forward commands.
///
/// 创建桥接传统发送器命令到传输命令的通道对。
///
/// 返回 (endpoint用的发送器, 适配器任务用的接收器)。
/// 应该生成适配器任务来转发命令。
pub fn create_adapter_channel<S: AsyncUdpSocket, T: Transport>(
    _transport_tx: mpsc::Sender<TransportCommand<T>>,
) -> (mpsc::Sender<SenderTaskCommand<S>>, mpsc::Receiver<SenderTaskCommand<S>>) {
    let (tx, rx) = mpsc::channel(1024);
    (tx, rx)
}

/// Task that runs the adapter, converting legacy commands to transport commands.
///
/// 运行适配器的任务，将传统命令转换为传输命令。
pub async fn adapter_task<S: AsyncUdpSocket, T: Transport>(
    mut rx: mpsc::Receiver<SenderTaskCommand<S>>,
    transport_tx: mpsc::Sender<TransportCommand<T>>,
) {
    while let Some(command) = rx.recv().await {
        match command {
            SenderTaskCommand::Send(send_cmd) => {
                let batch = FrameBatch {
                    remote_addr: send_cmd.remote_addr,
                    frames: send_cmd.frames,
                };
                if let Err(e) = transport_tx.send(TransportCommand::Send(batch)).await {
                    error!("Failed to forward send command to transport: {}", e);
                    break;
                }
            }
            SenderTaskCommand::SwapSocket(_) => {
                // SwapSocket is handled at the transport level, ignore here
                // SwapSocket在传输层处理，这里忽略
            }
        }
    }
}