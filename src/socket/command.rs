//! Commands used by the socket actor and sender task.

use crate::{
    config::Config,
    core::stream::Stream,
    error::Result,
    packet::frame::Frame,
};
use std::{net::SocketAddr, sync::Arc};
use tokio::sync::oneshot;

use super::traits::AsyncUdpSocket;

/// A command for the central socket sender task.
/// 用于中央套接字发送任务的命令。
#[derive(Debug)]
pub struct SendCommand {
    /// The destination address.
    /// 目标地址。
    pub remote_addr: SocketAddr,
    /// The frames to send.
    /// 要发送的帧。
    pub frames: Vec<Frame>,
}

/// Commands for the central socket sender task.
///
/// 用于中央套接字发送任务的命令。
#[derive(Debug)]
pub enum SenderTaskCommand<S: AsyncUdpSocket> {
    /// Send a batch of frames to a remote address.
    Send(SendCommand),
    /// Swap the underlying socket.
    SwapSocket(Arc<S>),
}

/// Commands sent to the `SocketActor`.
///
/// This enum encapsulates all operations that can be performed on the socket,
/// including handling API calls from the user and internal commands from endpoints.
///
/// 发送到 `SocketActor` 的命令。
///
/// 此枚举封装了可在套接字上执行的所有操作，包括处理来自用户的API调用和来自端点的内部命令。
#[derive(Debug)]
pub enum SocketActorCommand {
    /// Command from the public API to establish a new connection.
    /// 来自公共API的命令，用于建立一个新连接。
    Connect {
        remote_addr: SocketAddr,
        config: Config,
        initial_data: Option<bytes::Bytes>,
        response_tx: oneshot::Sender<Result<Stream>>,
    },
    /// Command from the public API to rebind the socket to a new address.
    /// 来自公共API的命令，用于将套接字重新绑定到新地址。
    Rebind {
        new_local_addr: SocketAddr,
        response_tx: oneshot::Sender<Result<()>>,
    },
    /// Internal command from an `Endpoint` task to update its address mapping.
    /// 来自 `Endpoint` 任务的内部命令，用于更新其地址映射。
    UpdateAddr { cid: u32, new_addr: SocketAddr },
} 