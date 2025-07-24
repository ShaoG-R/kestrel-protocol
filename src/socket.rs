//! 包含协议的顶层Socket接口。
//! Contains the top-level Socket interface for the protocol.

use crate::connection::{self, Connection, SendCommand};
use crate::error::Result;
use crate::packet::frame::Frame;
use dashmap::DashMap;
use rand::RngCore;
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
    pub async fn new(addr: SocketAddr) -> Result<Self> {
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

    /// Establishes a new reliable connection to the given remote address.
    ///
    /// This will create a new `Connection` instance and spawn its event loop.
    /// The connection can be used immediately to write 0-RTT data.
    ///
    /// 建立一个到指定远程地址的新的可靠连接。
    ///
    /// 这会创建一个新的 `Connection` 实例并生成其事件循环。
    /// 该连接可以立即用于写入0-RTT数据。
    pub async fn connect(&self, remote_addr: SocketAddr) -> Result<Connection> {
        let mut rng = rand::rng();
        let conn_id = rng.next_u32();

        let (tx_to_worker, rx_from_worker) = mpsc::channel(128);

        // Create a new connection in the `Connecting` state.
        let (mut worker, handle) = connection::ConnectionWorker::new(
            remote_addr,
            conn_id,
            connection::State::Connecting,
            self.send_tx.clone(),
            rx_from_worker,
        );

        // Spawn a new task for the connection to run in.
        tokio::spawn(async move {
            if let Err(e) = worker.run().await {
                eprintln!("Connection to {} closed with error: {}", remote_addr, e);
            }
        });

        // Insert the sender into the map so that incoming packets can be routed.
        self.connections.insert(remote_addr, tx_to_worker);

        Ok(handle)
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
                    // Log the error but continue running. A single recv error is not fatal.
                    eprintln!("Failed to receive from socket: {}", e);
                    continue;
                }
            };

            let data = &recv_buf[..len];
            let frame = match Frame::decode(data) {
                Some(frame) => frame,
                None => {
                    // Log and drop invalid packets.
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
            if let Frame::Syn {
                header,
                payload: _,
            } = &frame
            {
                println!("Received SYN from {}, creating new connection.", remote_addr);
                let conn_id = header.connection_id;

                let (tx_to_worker, rx_from_worker) = mpsc::channel(128);

                let (mut worker, _handle) = connection::ConnectionWorker::new(
                    remote_addr,
                    conn_id,
                    connection::State::Connecting,
                    self.send_tx.clone(),
                    rx_from_worker,
                );

                // Spawn a new task for the connection to run in.
                tokio::spawn(async move {
                    if let Err(e) = worker.run().await {
                        eprintln!("Connection to {} closed with error: {}", remote_addr, e);
                    }
                });

                // Store the sender so we can route future packets to it.
                // 存储发送端，以便我们将来的包可以路由给它。
                self.connections.insert(remote_addr, tx_to_worker.clone());

                // Send the first frame to the newly created connection.
                if tx_to_worker.send(frame).await.is_err() {
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
            // A single send error is not fatal to the whole socket.
            eprintln!(
                "Failed to send packet to {}: {}",
                cmd.remote_addr, e
            );
        }
    }
}
