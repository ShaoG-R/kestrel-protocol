//! 包含协议的顶层Socket接口。
//! Contains the top-level Socket interface for the protocol.

use crate::connection::Connection;
use crate::packet::frame::Frame;
use dashmap::DashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

/// A reliable UDP socket.
///
/// This is the main entry point for the protocol. It is responsible for
/// binding a UDP socket and managing all the connections.
///
/// 一个可靠的UDP套接字。
///
/// 这是协议的主要入口点。它负责绑定一个UDP套接字并管理所有的连接。
pub struct ReliableUdpSocket {
    socket: Arc<UdpSocket>,
    /// A map of all active connections, mapping a remote address to a sender
    /// that can send frames to the connection's task.
    /// 所有活动连接的映射，将远程地址映射到一个可以向连接任务发送帧的发送端。
    connections: Arc<DashMap<SocketAddr, mpsc::Sender<Frame>>>,
}

impl ReliableUdpSocket {
    /// Creates a new `ReliableUdpSocket` and binds it to the given address.
    ///
    /// 创建一个新的 `ReliableUdpSocket` 并将其绑定到给定的地址。
    pub async fn new(addr: SocketAddr) -> std::io::Result<Self> {
        let socket = UdpSocket::bind(addr).await?;
        let socket = Arc::new(socket);
        let connections = Arc::new(DashMap::new());

        Ok(Self {
            socket,
            connections,
        })
    }

    /// Runs the socket's main loop to receive and dispatch packets.
    /// This should be spawned in a separate task.
    ///
    /// 运行套接字的主循环以接收和分发包。
    /// 这个方法应该在一个单独的任务中被spawn。
    pub async fn run(&self) {
        let mut recv_buf = [0u8; 2048]; // Max UDP packet size

        loop {
            let (len, remote_addr) = match self.socket.recv_from(&mut recv_buf).await {
                Ok(val) => val,
                Err(e) => {
                    eprintln!("Failed to receive from socket: {}", e);
                    continue;
                }
            };

            let data = &recv_buf[..len];
            let frame = match Frame::decode(data) {
                Some(frame) => frame,
                None => {
                    eprintln!("Received an invalid packet from {}", remote_addr);
                    continue;
                }
            };

            // 如果我们已经有这个连接的记录，直接发送
            if let Some(tx) = self.connections.get(&remote_addr) {
                if tx.send(frame).await.is_err() {
                    // This means the connection task has died. Remove it.
                    // 这意味着连接任务已经死亡。移除它。
                    self.connections.remove(&remote_addr);
                }
                continue;
            }

            // 如果是新连接 (目前我们简化为：任何我们不认识的地址发来的第一个包)
            // For a new connection (simplified for now as the first packet from an unknown address)
            // TODO: In Phase 2, we will only create a connection upon receiving a SYN packet.
            // 在第二阶段，我们将只在收到SYN包时创建连接。
            let (mut connection, tx) = Connection::new(remote_addr);

            // Spawn a new task for the connection to run in.
            // 为连接生成一个新任务来运行。
            tokio::spawn(async move {
                connection.run().await;
            });

            // Store the sender so we can route future packets to it.
            // 存储发送端，以便我们将来的包可以路由给它。
            self.connections.insert(remote_addr, tx.clone());

            // Send the first frame to the newly created connection.
            // 将第一个帧发送给新创建的连接。
            if tx.send(frame).await.is_err() {
                self.connections.remove(&remote_addr);
            }
        }
    }
}
