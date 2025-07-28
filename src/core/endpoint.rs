//! The endpoint of a connection, which is the "brain" of the new layered protocol.
//!
//! 连接的端点，是新分层协议的“大脑”。

pub mod core;
pub mod processing;
pub mod lifecycle;
pub mod builder;
pub mod types;
pub mod operations;

#[cfg(test)]
mod tests;

pub use types::command::StreamCommand;

use lifecycle::{ConnectionLifecycleManager, DefaultLifecycleManager};
use crate::{
    config::Config,
    core::reliability::ReliabilityLayer,
    socket::{AsyncUdpSocket, SenderTaskCommand, SocketActorCommand},
};
use bytes::Bytes;
use std::net::SocketAddr;
use tokio::{
    sync::mpsc,
    time::Instant,
};
use tracing::trace;
use types::state::ConnectionState;



/// A guard that ensures the connection is cleaned up in the `SocketActor`
/// when the `Endpoint` is dropped.
///
/// 一个哨兵结构，确保在 `Endpoint` 被丢弃时，其在 `SocketActor` 中的
/// 连接状态能够被清理。
struct ConnectionCleaner<S: AsyncUdpSocket> {
    cid: u32,
    command_tx: mpsc::Sender<SocketActorCommand>,
    _marker: std::marker::PhantomData<S>,
}

impl<S: AsyncUdpSocket> Drop for ConnectionCleaner<S> {
    fn drop(&mut self) {
        // Use `try_send` to avoid blocking in a drop implementation. This is a
        // "best-effort" cleanup. If the channel is full or closed, the actor
        // will eventually clean up the connection via timeout.
        if let Err(e) = self.command_tx.try_send(SocketActorCommand::RemoveConnection { cid: self.cid }) {
            trace!(cid = self.cid, "Failed to send remove command during drop: {}", e);
        }
    }
}

/// Represents one end of a reliable connection.
pub struct Endpoint<S: AsyncUdpSocket> {
    remote_addr: SocketAddr,
    local_cid: u32,
    peer_cid: u32,

    lifecycle_manager: DefaultLifecycleManager,
    start_time: Instant,
    reliability: ReliabilityLayer,
    peer_recv_window: u32,
    config: Config,
    last_recv_time: Instant,
    receiver: mpsc::Receiver<(crate::packet::frame::Frame, SocketAddr)>,
    sender: mpsc::Sender<SenderTaskCommand<S>>,
    command_tx: mpsc::Sender<SocketActorCommand>,
    rx_from_stream: mpsc::Receiver<StreamCommand>,
    tx_to_stream: Option<mpsc::Sender<Vec<Bytes>>>,
    /// Set to true when a FIN is received, indicating we should close the
    /// user stream once the receive buffer is drained.
    fin_pending_eof: bool,
}

impl<S: AsyncUdpSocket> Endpoint<S> {

    /// 获取生命周期管理器的引用
    /// Gets a reference to the lifecycle manager
    pub fn lifecycle_manager(&self) -> &DefaultLifecycleManager {
        &self.lifecycle_manager
    }

    /// 获取生命周期管理器的可变引用
    /// Gets a mutable reference to the lifecycle manager
    pub fn lifecycle_manager_mut(&mut self) -> &mut DefaultLifecycleManager {
        &mut self.lifecycle_manager
    }

    /// 通过生命周期管理器进行状态转换（新方法）
    /// Perform state transition through lifecycle manager (new method)
    pub fn transition_state(&mut self, new_state: ConnectionState) -> crate::error::Result<()> {
        // 只通过生命周期管理器验证和执行转换
        // Only validate and execute transition through lifecycle manager
        self.lifecycle_manager.transition_to(new_state)?;
        Ok(())
    }

    /// 检查是否可以发送数据（使用生命周期管理器）
    /// Check if data can be sent (using lifecycle manager)
    pub fn can_send_data(&self) -> bool {
        self.lifecycle_manager.can_send_data()
    }

    /// 检查是否可以接收数据（使用生命周期管理器）
    /// Check if data can be received (using lifecycle manager)  
    pub fn can_receive_data(&self) -> bool {
        self.lifecycle_manager.can_receive_data()
    }

    /// 开始优雅关闭（使用生命周期管理器）
    /// Start graceful shutdown (using lifecycle manager)
    pub fn begin_graceful_shutdown(&mut self) -> crate::error::Result<()> {
        self.lifecycle_manager.begin_graceful_shutdown()
    }

    /// 开始路径验证（使用生命周期管理器）
    /// Start path validation (using lifecycle manager)
    pub fn start_path_validation(
        &mut self,
        new_addr: SocketAddr,
        challenge_data: u64,
        notifier: tokio::sync::oneshot::Sender<crate::error::Result<()>>,
    ) -> crate::error::Result<()> {
        self.lifecycle_manager.start_path_validation(new_addr, challenge_data, notifier)
    }

    /// 完成路径验证（使用生命周期管理器）
    /// Complete path validation (using lifecycle manager)
    pub fn complete_path_validation(&mut self, success: bool) -> crate::error::Result<()> {
        self.lifecycle_manager.complete_path_validation(success)
    }

    /// 获取本地连接ID
    /// Gets the local connection ID
    pub fn local_cid(&self) -> u32 {
        self.local_cid
    }

    /// 获取对端连接ID
    /// Gets the peer connection ID
    pub fn peer_cid(&self) -> u32 {
        self.peer_cid
    }

    /// 设置对端连接ID
    /// Sets the peer connection ID
    pub fn set_peer_cid(&mut self, peer_cid: u32) {
        self.peer_cid = peer_cid;
    }

    /// 获取当前连接状态
    /// Gets the current connection state
    pub fn state(&self) -> &ConnectionState {
        self.lifecycle_manager.current_state()
    }



    /// 更新最后接收时间
    /// Updates the last receive time
    pub fn update_last_recv_time(&mut self, time: Instant) {
        self.last_recv_time = time;
    }

    /// 获取连接开始时间
    /// Gets the connection start time
    pub fn start_time(&self) -> Instant {
        self.start_time
    }

    /// 获取远程地址
    /// Gets the remote address
    pub fn remote_addr(&self) -> SocketAddr {
        self.remote_addr
    }

    /// 设置远程地址
    /// Sets the remote address
    pub fn set_remote_addr(&mut self, addr: SocketAddr) {
        self.remote_addr = addr;
    }

    /// 获取命令发送器
    /// Gets the command sender
    pub fn command_tx(&self) -> &mpsc::Sender<SocketActorCommand> {
        &self.command_tx
    }

    /// 获取可靠性层的可变引用
    /// Gets a mutable reference to the reliability layer
    pub fn reliability_mut(&mut self) -> &mut ReliabilityLayer {
        &mut self.reliability
    }

    /// 获取可靠性层的引用
    /// Gets a reference to the reliability layer
    pub fn reliability(&self) -> &ReliabilityLayer {
        &self.reliability
    }
}
