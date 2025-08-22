//! 通信通道管理模块 - 管理端点的所有通信通道
//! Communication Channels Management Module - Manages all communication channels for endpoints
//!
//! 该模块封装了端点使用的所有异步通信通道，包括网络通信、用户流通信和控制命令通道。
//! 通过集中管理这些通道，我们可以更好地控制端点的通信模式和生命周期。
//!
//! This module encapsulates all asynchronous communication channels used by endpoints,
//! including network communication, user stream communication, and control command channels.
//! By centrally managing these channels, we can better control endpoint communication
//! patterns and lifecycle.

use crate::{
    core::endpoint::timing::TimeoutEvent,
    core::endpoint::types::command::StreamCommand,
    packet::frame::Frame,
    socket::TransportCommand,
    socket::{SocketActorCommand, Transport},
    timer::event::TimerEventData,
};
use bytes::Bytes;
use std::net::SocketAddr;
use tokio::sync::mpsc;

/// 通信通道管理器
/// Communication channels manager
pub struct ChannelManager<T: Transport> {
    /// 网络数据接收通道
    /// Network data receive channel
    pub(crate) receiver: mpsc::Receiver<(Frame, SocketAddr)>,

    /// 网络数据发送通道
    /// Network data send channel
    pub(crate) sender: mpsc::Sender<TransportCommand<T>>,

    /// Socket控制命令发送通道
    /// Socket control command send channel
    pub(crate) command_tx: mpsc::Sender<SocketActorCommand>,

    /// 用户流命令接收通道
    /// User stream command receive channel
    pub(crate) rx_from_stream: mpsc::Receiver<StreamCommand>,

    /// 用户流数据发送通道
    /// User stream data send channel
    pub(crate) tx_to_stream: Option<mpsc::Sender<Vec<Bytes>>>,

    /// 定时器事件接收通道 - 事件驱动架构的核心
    /// Timer event receive channel - core of event-driven architecture
    pub(crate) timer_event_rx: mpsc::Receiver<TimerEventData<TimeoutEvent>>,
}

impl<T: Transport> ChannelManager<T> {
    /// 创建新的通道管理器
    /// Create new channel manager
    pub fn new(
        receiver: mpsc::Receiver<(Frame, SocketAddr)>,
        sender: mpsc::Sender<TransportCommand<T>>,
        command_tx: mpsc::Sender<SocketActorCommand>,
        rx_from_stream: mpsc::Receiver<StreamCommand>,
        tx_to_stream: Option<mpsc::Sender<Vec<Bytes>>>,
        timer_event_rx: mpsc::Receiver<TimerEventData<TimeoutEvent>>,
    ) -> Self {
        Self {
            receiver,
            sender,
            command_tx,
            rx_from_stream,
            tx_to_stream,
            timer_event_rx,
        }
    }

    /// 获取网络数据接收通道的可变引用
    /// Get mutable reference to network data receive channel
    pub fn receiver_mut(&mut self) -> &mut mpsc::Receiver<(Frame, SocketAddr)> {
        &mut self.receiver
    }

    /// 获取网络数据发送通道的引用
    /// Get reference to network data send channel
    pub fn sender(&self) -> &mpsc::Sender<TransportCommand<T>> {
        &self.sender
    }

    /// 获取Socket控制命令发送通道的引用
    /// Get reference to socket control command send channel
    pub fn command_tx(&self) -> &mpsc::Sender<SocketActorCommand> {
        &self.command_tx
    }

    /// 获取用户流命令接收通道的可变引用
    /// Get mutable reference to user stream command receive channel
    pub fn rx_from_stream_mut(&mut self) -> &mut mpsc::Receiver<StreamCommand> {
        &mut self.rx_from_stream
    }

    /// 获取用户流数据发送通道的引用
    /// Get reference to user stream data send channel
    pub fn tx_to_stream(&self) -> &Option<mpsc::Sender<Vec<Bytes>>> {
        &self.tx_to_stream
    }

    /// 获取用户流数据发送通道的可变引用
    /// Get mutable reference to user stream data send channel
    pub fn tx_to_stream_mut(&mut self) -> &mut Option<mpsc::Sender<Vec<Bytes>>> {
        &mut self.tx_to_stream
    }

    /// 关闭用户流数据发送通道
    /// Close user stream data send channel
    pub fn close_user_stream(&mut self) {
        if let Some(tx) = self.tx_to_stream.take() {
            drop(tx); // 这会关闭通道，向用户发送EOF信号
        }
    }

    /// 检查用户流数据发送通道是否已关闭
    /// Check if user stream data send channel is closed
    pub fn is_user_stream_closed(&self) -> bool {
        self.tx_to_stream.is_none()
    }

    /// 尝试向用户流发送数据
    /// Try to send data to user stream
    pub async fn send_to_user_stream(&mut self, data: Vec<Bytes>) -> Result<(), ()> {
        if let Some(tx) = &self.tx_to_stream {
            match tx.send(data).await {
                Ok(()) => Ok(()),
                Err(_) => {
                    // 发送失败，说明接收端已关闭，清理发送端
                    // Send failed, receiver is closed, clean up sender
                    self.tx_to_stream = None;
                    Err(())
                }
            }
        } else {
            Err(())
        }
    }

    /// 尝试接收网络数据
    /// Try to receive network data
    pub async fn recv_network_data(&mut self) -> Option<(Frame, SocketAddr)> {
        self.receiver.recv().await
    }

    /// 尝试非阻塞接收网络数据
    /// Try to receive network data non-blocking
    pub fn try_recv_network_data(
        &mut self,
    ) -> Result<(Frame, SocketAddr), mpsc::error::TryRecvError> {
        self.receiver.try_recv()
    }

    /// 尝试接收用户流命令
    /// Try to receive user stream command
    pub async fn recv_stream_command(&mut self) -> Option<StreamCommand> {
        self.rx_from_stream.recv().await
    }

    /// 尝试非阻塞接收用户流命令
    /// Try to receive user stream command non-blocking
    pub fn try_recv_stream_command(&mut self) -> Result<StreamCommand, mpsc::error::TryRecvError> {
        self.rx_from_stream.try_recv()
    }

    /// 尝试接收定时器事件
    /// Try to receive timer event
    pub async fn recv_timer_event(&mut self) -> Option<TimerEventData<TimeoutEvent>> {
        self.timer_event_rx.recv().await
    }

    /// 尝试非阻塞接收定时器事件
    /// Try to receive timer event non-blocking
    pub fn try_recv_timer_event(
        &mut self,
    ) -> Result<TimerEventData<TimeoutEvent>, mpsc::error::TryRecvError> {
        self.timer_event_rx.try_recv()
    }

    /// 获取定时器事件接收通道的可变引用
    /// Get mutable reference to timer event receive channel
    pub fn timer_event_rx_mut(&mut self) -> &mut mpsc::Receiver<TimerEventData<TimeoutEvent>> {
        &mut self.timer_event_rx
    }

    /// 检查是否所有输入通道都已关闭
    /// Check if all input channels are closed
    pub fn are_input_channels_closed(&self) -> bool {
        self.receiver.is_closed()
            && self.rx_from_stream.is_closed()
            && self.timer_event_rx.is_closed()
    }
}

impl<T: Transport> std::fmt::Debug for ChannelManager<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelManager")
            .field("receiver_closed", &self.receiver.is_closed())
            .field("rx_from_stream_closed", &self.rx_from_stream.is_closed())
            .field("timer_event_rx_closed", &self.timer_event_rx.is_closed())
            .field("tx_to_stream_available", &self.tx_to_stream.is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use crate::socket::UdpTransport;

    use super::*;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn test_channel_manager_creation() {
        let (_frame_tx, frame_rx) = mpsc::channel(10);
        let (sender_tx, _sender_rx) = mpsc::channel(10);
        let (cmd_tx, _cmd_rx) = mpsc::channel(10);
        let (_stream_cmd_tx, stream_cmd_rx) = mpsc::channel(10);
        let (to_stream_tx, _to_stream_rx) = mpsc::channel(10);
        let (_timer_tx, timer_rx) = mpsc::channel(10);

        let manager = ChannelManager::<UdpTransport>::new(
            frame_rx,
            sender_tx,
            cmd_tx,
            stream_cmd_rx,
            Some(to_stream_tx),
            timer_rx,
        );

        assert!(!manager.is_user_stream_closed());
        assert!(!manager.are_input_channels_closed());
    }

    #[tokio::test]
    async fn test_user_stream_operations() {
        let (_frame_tx, frame_rx) = mpsc::channel(10);
        let (sender_tx, _sender_rx) = mpsc::channel(10);
        let (cmd_tx, _cmd_rx) = mpsc::channel(10);
        let (_stream_cmd_tx, stream_cmd_rx) = mpsc::channel(10);
        let (to_stream_tx, mut to_stream_rx) = mpsc::channel(10);
        let (_timer_tx, timer_rx) = mpsc::channel(10);

        let mut manager = ChannelManager::<UdpTransport>::new(
            frame_rx,
            sender_tx,
            cmd_tx,
            stream_cmd_rx,
            Some(to_stream_tx),
            timer_rx,
        );

        // 测试发送数据到用户流
        let test_data = vec![Bytes::from("test data")];
        assert!(manager.send_to_user_stream(test_data.clone()).await.is_ok());

        // 验证数据被正确接收
        let received = to_stream_rx.recv().await.unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0], test_data[0]);

        // 测试关闭用户流
        manager.close_user_stream();
        assert!(manager.is_user_stream_closed());

        // 关闭后发送应该失败
        assert!(
            manager
                .send_to_user_stream(vec![Bytes::from("fail")])
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn test_input_channel_status() {
        let (frame_tx, frame_rx) = mpsc::channel(10);
        let (sender_tx, _sender_rx) = mpsc::channel(10);
        let (cmd_tx, _cmd_rx) = mpsc::channel(10);
        let (stream_cmd_tx, stream_cmd_rx) = mpsc::channel(10);
        let (timer_tx, timer_rx) = mpsc::channel(10);

        let manager = ChannelManager::<UdpTransport>::new(
            frame_rx,
            sender_tx,
            cmd_tx,
            stream_cmd_rx,
            None,
            timer_rx,
        );

        // 初始状态下通道应该是开放的
        assert!(!manager.are_input_channels_closed());

        // 关闭发送端后检查状态
        drop(frame_tx);
        drop(stream_cmd_tx);
        drop(timer_tx);

        // 注意：这个测试可能需要一些时间让通道状态更新
        tokio::task::yield_now().await;
    }
}
