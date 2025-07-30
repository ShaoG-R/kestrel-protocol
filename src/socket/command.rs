//! Commands used by the socket actor and sender task.

use crate::{
    config::Config,
    core::stream::Stream,
    error::Result,
};
use std::{net::SocketAddr};
use tokio::sync::oneshot;


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
    /// Command from an endpoint to signal that it has terminated and should be removed.
    ///
    /// 来自端点的命令，表示它已经终止，应该被移除。
    RemoveConnection { cid: u32 },
    /// Command from the public API to get the current local address.
    /// 来自公共API的命令，用于获取当前本地地址。
    GetLocalAddr {
        response_tx: oneshot::Sender<Result<SocketAddr>>,
    },
} 