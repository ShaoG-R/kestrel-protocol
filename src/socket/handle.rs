//! The user-facing API using the transport layer.

use super::{
    command::SocketActorCommand,
    event_loop::SocketEventLoop,
    transport::{transport_sender_task, BindableTransport, TransportCommand},
};
use crate::{
    config::Config,
    core::stream::Stream,
    error::{Error, Result},
};
use std::{marker::PhantomData, net::SocketAddr, sync::Arc};
use tokio::sync::{mpsc, oneshot};
use tracing::info;
use initial_data::InitialData;
use crate::socket::event_loop::draining::DrainingPool;
use crate::socket::event_loop::routing::FrameRouter;
use crate::socket::event_loop::session_coordinator::SocketSessionCoordinator;

pub mod initial_data;

/// A listener for incoming reliable UDP connections using transport layer.
///
/// 使用传输层的传入可靠UDP连接监听器。
#[derive(Debug)]
pub struct TransportListener {
    pub(crate) accept_rx: mpsc::Receiver<(Stream, SocketAddr)>,
}

impl TransportListener {
    /// Waits for a new incoming connection.
    ///
    /// 等待一个新的传入连接。
    pub async fn accept(&mut self) -> Result<(Stream, SocketAddr)> {
        self.accept_rx
            .recv()
            .await
            .ok_or(crate::error::Error::ChannelClosed)
    }
}

/// A handle to the transport-based `ReliableUdpSocket` actor.
///
/// 基于传输的 `ReliableUdpSocket` actor的句柄。
pub struct TransportReliableUdpSocket<T: BindableTransport> {
    pub(crate) command_tx: mpsc::Sender<SocketActorCommand>,
    _marker: PhantomData<T>,
}

impl<T: BindableTransport> Clone for TransportReliableUdpSocket<T> {
    fn clone(&self) -> Self {
        Self {
            command_tx: self.command_tx.clone(),
            _marker: PhantomData,
        }
    }
}

impl<T: BindableTransport> TransportReliableUdpSocket<T> {
    /// Creates a new transport-based `ReliableUdpSocket` and binds it to the given address.
    ///
    /// 创建一个新的基于传输的 `ReliableUdpSocket` 并将其绑定到给定的地址。
    pub async fn bind(addr: SocketAddr) -> Result<(Self, TransportListener)> {
        let transport = Arc::new(T::bind(addr).await?);

        // Create channel for actor commands
        let (command_tx, command_rx) = mpsc::channel(128);

        // Create channel for transport commands
        let (send_tx, send_rx) = mpsc::channel::<TransportCommand<T>>(1024);

        // Create channel for accepting new connections
        let (accept_tx, accept_rx) = mpsc::channel(128);
        let listener = TransportListener { accept_rx };

        // Spawn the transport sender task
        let transport_clone = transport.clone();
        tokio::spawn(transport_sender_task(transport_clone, send_rx));

        let config = Arc::new(Config::default());

        // 创建传输管理器
        // Create transport manager
        let transport_manager = super::transport::TransportManager::new(transport, send_tx);
        
        // 创建帧路由管理器
        // Create frame router manager
        let frame_router = FrameRouter::new(
            DrainingPool::new(config.connection.drain_timeout)
        );

        // 创建会话协调器
        // Create session coordinator
        let session_coordinator = SocketSessionCoordinator::new(
            transport_manager,
            frame_router,
            config.clone(),
            accept_tx,
            command_tx.clone(),
        );

        let mut actor = SocketEventLoop {
            session_coordinator,
            command_rx,
        };

        info!(addr = ?actor.session_coordinator.transport_manager().local_addr().ok(), "TransportReliableUdpSocket actor created and running");

        // Spawn the actor task
        tokio::spawn(async move {
            actor.run().await;
        });

        let handle = Self {
            command_tx,
            _marker: PhantomData,
        };

        Ok((handle, listener))
    }

    /// Rebinds the underlying transport to a new local address.
    ///
    /// 将底层传输重新绑定到新的本地地址。
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
    /// 建立一个到指定远程地址的新的可靠连接。
    pub async fn connect(&self, remote_addr: SocketAddr) -> Result<Stream> {
        self.connect_with_config(remote_addr, Config::default(), None)
            .await
    }

    /// Establishes a new reliable connection with custom configuration and optional 0-RTT data.
    ///
    /// 使用自定义配置和可选的0-RTT数据建立新的可靠连接。
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