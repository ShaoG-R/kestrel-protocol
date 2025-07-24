//! 包含协议的顶层Socket接口。
//! Contains the top-level Socket interface for the protocol.

use crate::connection::{self, Connection, SendCommand};
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
    /// A sender to the central sending task.
    /// 到中央发送任务的发送端。
    send_tx: mpsc::Sender<SendCommand>,
}

impl ReliableUdpSocket {
    /// Creates a new `ReliableUdpSocket` and binds it to the given address.
    ///
    /// 创建一个新的 `ReliableUdpSocket` 并将其绑定到给定的地址。
    pub async fn new(addr: SocketAddr) -> std::io::Result<Self> {
        let socket = UdpSocket::bind(addr).await?;
        let socket = Arc::new(socket);
        let connections = Arc::new(DashMap::new());

        // Create a channel for sending packets.
        let (send_tx, send_rx) = mpsc::channel::<SendCommand>(1024);

        // Spawn the sender task.
        let socket_clone = socket.clone();
        tokio::spawn(sender_task(socket_clone, send_rx));

        Ok(Self {
            socket,
            connections,
            send_tx,
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

            // A new connection can only be initiated with a SYN packet.
            // 只有SYN包才能发起新连接。
            if let Frame::Syn { .. } = &frame {
                println!("Received SYN from {}, creating new connection.", remote_addr);

                let (mut connection, tx) = Connection::new(
                    remote_addr,
                    connection::State::Connecting,
                    self.send_tx.clone(),
                );

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
            } else {
                // Ignore packets from unknown addresses that are not SYN.
                // 忽略来自未知地址的非SYN包。
                println!(
                    "Ignoring non-SYN packet from unknown address {}: {:?}",
                    remote_addr, frame
                );
            }
        }
    }
}

/// The dedicated task for sending UDP packets.
/// This centralizes all writes to the socket.
///
/// 用于发送UDP包的专用任务。
/// 这将所有对套接字的写入操作集中起来。
async fn sender_task(socket: Arc<UdpSocket>, mut rx: mpsc::Receiver<SendCommand>) {
    let mut send_buf = Vec::with_capacity(2048);

    while let Some(cmd) = rx.recv().await {
        send_buf.clear();
        for frame in cmd.frames {
            frame.encode(&mut send_buf);
        }

        if send_buf.is_empty() {
            continue;
        }

        if let Err(e) = socket.send_to(&send_buf, cmd.remote_addr).await {
            eprintln!(
                "Failed to send packet to {}: {}",
                cmd.remote_addr, e
            );
        }
    }
}
