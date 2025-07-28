//! The user-facing API, including the `ReliableUdpSocket` handle and `ConnectionListener`.

use super::{
    actor::SocketActor,
    command::{SenderTaskCommand, SocketActorCommand},
    draining::DrainingPool,
    sender::sender_task,
    traits::BindableUdpSocket,
};
use crate::{
    config::Config,
    core::stream::Stream,
    error::{Error, Result},
    socket::zerortt::InitialData,
};
use std::{marker::PhantomData, net::SocketAddr, sync::Arc};
use tokio::sync::{mpsc, oneshot};
use tracing::info;

/// A listener for incoming reliable UDP connections.
///
/// This struct is created by the `ReliableUdpSocket::bind` method and can be used
/// to accept new incoming connections.
///
/// 用于传入可靠UDP连接的监听器。
///
/// 此结构体由 `ReliableUdpSocket::bind` 方法创建，可用于接受新的传入连接。
#[derive(Debug)]
pub struct Listener {
    pub(crate) accept_rx: mpsc::Receiver<(Stream, SocketAddr)>,
}

impl Listener {
    /// Waits for a new incoming connection.
    ///
    /// This method will block until a new connection is established.
    ///
    /// 等待一个新的传入连接。
    ///
    /// 此方法将阻塞直到一个新连接建立。
    pub async fn accept(&mut self) -> Result<(Stream, SocketAddr)> {
        self.accept_rx
            .recv()
            .await
            .ok_or(crate::error::Error::ChannelClosed)
    }
}

/// A handle to the `ReliableUdpSocket` actor.
///
/// This is the main entry point for the protocol. It is a lightweight handle
/// that sends commands to the central `SocketActor` task for processing.
///
/// `ReliableUdpSocket` actor的句柄。
///
/// 这是协议的主要入口点。它是一个轻量级的句柄，将命令发送到中央 `SocketActor` 任务进行处理。
pub struct ReliableUdpSocket<S: BindableUdpSocket> {
    pub(crate) command_tx: mpsc::Sender<SocketActorCommand>,
    _marker: PhantomData<S>,
}

impl<S: BindableUdpSocket> Clone for ReliableUdpSocket<S> {
    fn clone(&self) -> Self {
        Self {
            command_tx: self.command_tx.clone(),
            _marker: PhantomData,
        }
    }
}

impl<S: BindableUdpSocket> ReliableUdpSocket<S> {
    /// Creates a new `ReliableUdpSocket` and binds it to the given address.
    ///
    /// This function spawns the central `SocketActor` task which manages all
    /// state and connections, and returns a handle to communicate with it.
    ///
    /// 创建一个新的 `ReliableUdpSocket` 并将其绑定到给定的地址。
    ///
    /// 此函数会生成中央 `SocketActor` 任务，该任务管理所有状态和连接，并返回一个与其通信的句柄。
    pub async fn bind(addr: SocketAddr) -> Result<(Self, Listener)> {
        let socket = Arc::new(S::bind(addr).await?);

        // Create channel for actor commands.
        let (command_tx, command_rx) = mpsc::channel(128);

        // Create channel for sending packets.
        let (send_tx, send_rx) = mpsc::channel::<SenderTaskCommand<S>>(1024);

        // Create channel for accepting new connections.
        let (accept_tx, accept_rx) = mpsc::channel(128);
        let listener = Listener { accept_rx };

        // Spawn the sender task.
        let socket_clone = socket.clone();
        tokio::spawn(sender_task(socket_clone, send_rx));

        let config = Arc::new(Config::default());

        let mut actor = SocketActor {
            socket,
            connections: std::collections::HashMap::new(),
            addr_to_cid: std::collections::HashMap::new(),
            draining_pool: DrainingPool::new(config.connection.drain_timeout),
            config: config.clone(),
            send_tx,
            accept_tx,
            command_rx,
            command_tx: command_tx.clone(),
        };

        info!(addr = ?actor.socket.local_addr().ok(), "ReliableUdpSocket actor created and running");

        // Spawn the actor task.
        tokio::spawn(async move {
            actor.run().await;
        });

        let handle = Self {
            command_tx,
            _marker: PhantomData,
        };

        Ok((handle, listener))
    }

    /// Rebinds the underlying UDP socket to a new local address.
    ///
    /// Sends a command to the actor to perform the rebinding.
    ///
    /// 将底层UDP套接字重新绑定到一个新的本地地址。
    ///
    /// 向actor发送命令以执行重新绑定。
    pub async fn rebind(&self, new_local_addr: SocketAddr) -> Result<()> {
        let (response_tx, response_rx) = oneshot::channel();
        let cmd = SocketActorCommand::Rebind {
            new_local_addr,
            response_tx,
        };
        self.command_tx
            .send(cmd)
            .await
            .map_err(|_| Error::ChannelClosed)?;
        response_rx.await.map_err(|_| Error::ChannelClosed)?
    }

    /// Establishes a new reliable connection to the given remote address.
    ///
    /// This is a convenience method that uses the default configuration and sends
    /// no 0-RTT data. For more control, see [`connect_with_config`](#method.connect_with_config).
    ///
    /// 建立一个到指定远程地址的新的可靠连接。
    ///
    /// 这是一个使用默认配置且不发送0-RTT数据的便捷方法。
    /// 如需更多控制，请参阅 [`connect_with_config`](#method.connect_with_config)。
    pub async fn connect(&self, remote_addr: SocketAddr) -> Result<Stream> {
        self.connect_with_config(remote_addr, Config::default(), None)
            .await
    }

    /// Establishes a new reliable connection to the given remote address with
    /// custom configuration and optional 0-RTT data.
    ///
    /// The `initial_data` must be created using [`InitialData::new`], which
    /// validates that the data will fit into a single initial UDP packet.
    /// This enforces the single-packet constraint for 0-RTT at the API level.
    ///
    /// 使用自定义配置和可选的0-RTT数据，建立一个到指定远程地址的新的可靠连接。
    ///
    /// `initial_data` 必须使用 [`InitialData::new`] 创建，该函数会验证数据
    /// 能否装入单个初始UDP包中。这在API层面强制实施了0-RTT的单包约束。
    pub async fn connect_with_config(
        &self,
        remote_addr: SocketAddr,
        config: Config,
        initial_data: Option<InitialData>,
    ) -> Result<Stream> {
        let (response_tx, response_rx) = oneshot::channel();
        let cmd = SocketActorCommand::Connect {
            remote_addr,
            config,
            initial_data: initial_data.map(|d| d.into_bytes()),
            response_tx,
        };
        self.command_tx
            .send(cmd)
            .await
            .map_err(|_| Error::ChannelClosed)?;
        response_rx.await.map_err(|_| Error::ChannelClosed)?
    }
} 