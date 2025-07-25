//! The endpoint of a connection, which is the "brain" of the new layered protocol.
//!
//! 连接的端点，是新分层协议的“大脑”。

mod command;
mod constructors;
pub mod frame_factory;
mod logic;
mod sending;
pub mod state;

pub use command::StreamCommand;

use self::state::ConnectionState;
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
    state: ConnectionState,
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
}

impl<S: AsyncUdpSocket> Endpoint<S> {
    /// Sets the peer's connection ID.
    /// This is primarily used for testing setups where CIDs are pre-determined.
    #[cfg(test)]
    pub fn set_peer_cid(&mut self, peer_cid: u32) {
        self.peer_cid = peer_cid;
    }
}
