//! The user-facing API, including the `ReliableUdpSocket` handle and `ConnectionListener`.

use super::{
    actor::SocketActor,
    command::{SenderTaskCommand, SocketActorCommand},
    sender::sender_task,
    traits::BindableUdpSocket,
};
use crate::{
    config::Config,
    core::stream::Stream,
    error::{Error, Result},
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

        let mut actor = SocketActor {
            socket,
            connections: std::collections::HashMap::new(),
            addr_to_cid: std::collections::HashMap::new(),
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
    /// This will create a new `Stream` instance and spawn its event loop.
    /// The connection can be used immediately to write 0-RTT data.
    ///
    /// 建立一个到指定远程地址的新的可靠连接。
    ///
    /// 这会创建一个新的 `Stream` 实例并生成其事件循环。
    /// 该连接可以立即用于写入0-RTT数据。
    pub async fn connect(&self, remote_addr: SocketAddr) -> Result<Stream> {
        self.connect_with_config(remote_addr, Config::default(), None)
            .await
    }

    /// Establishes a new reliable connection with 0-RTT data.
    ///
    /// The size of `initial_data` must not exceed `max_payload_size` from the config.
    ///
    /// 建立一个携带0-RTT数据的新的可靠连接。
    ///
    /// `initial_data` 的大小不能超过配置中的 `max_payload_size`。
    pub async fn connect_with_0rtt(
        &self,
        remote_addr: SocketAddr,
        initial_data: &[u8],
    ) -> Result<Stream> {
        self.connect_with_config(
            remote_addr,
            Config::default(),
            Some(bytes::Bytes::copy_from_slice(initial_data)),
        )
        .await
    }

    /// Establishes a new reliable connection to the given remote address with custom configuration.
    ///
    /// 使用自定义配置建立一个到指定远程地址的新的可靠连接。
    pub async fn connect_with_config(
        &self,
        remote_addr: SocketAddr,
        config: Config,
        initial_data: Option<bytes::Bytes>,
    ) -> Result<Stream> {
        if let Some(data) = &initial_data {
            if data.len() > config.max_payload_size {
                return Err(crate::error::Error::MessageTooLarge);
            }
        }
        let (response_tx, response_rx) = oneshot::channel();
        let cmd = SocketActorCommand::Connect {
            remote_addr,
            config,
            initial_data,
            response_tx,
        };
        self.command_tx
            .send(cmd)
            .await
            .map_err(|_| Error::ChannelClosed)?;
        response_rx.await.map_err(|_| Error::ChannelClosed)?
    }
} 