//! 包含协议的顶层Socket接口。
//! Contains the top-level Socket interface for the protocol.

use crate::packet::frame::Frame;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
// TODO: We'll need a way to send frames to connection tasks.
// 我们需要一种方式将帧发送到连接任务。
// use tokio::sync::mpsc;

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
    // TODO: We'll need a concurrent map to store connection senders.
    // 我们需要一个并发安全的Map来存储连接的发送端。
    // connections: Arc<DashMap<SocketAddr, mpsc::Sender<Frame>>>,
}

impl ReliableUdpSocket {
    /// Creates a new `ReliableUdpSocket` and binds it to the given address.
    ///
    /// 创建一个新的 `ReliableUdpSocket` 并将其绑定到给定的地址。
    pub async fn new(addr: SocketAddr) -> std::io::Result<Self> {
        let socket = UdpSocket::bind(addr).await?;
        let socket = Arc::new(socket);

        Ok(Self { socket })
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
                    // TODO: Proper error logging
                    // 需要合适的错误日志
                    eprintln!("Failed to receive from socket: {}", e);
                    continue;
                }
            };

            let data = &recv_buf[..len];

            // 尝试将收到的字节解码成一个帧
            // Try to decode the received bytes into a frame
            let frame = match Frame::decode(data) {
                Some(frame) => frame,
                None => {
                    // TODO: Proper logging for invalid packets
                    // 需要为无效包提供合适的日志
                    eprintln!("Received an invalid packet from {}", remote_addr);
                    continue;
                }
            };

            // TODO: Dispatch the frame to the corresponding connection task
            // 将帧分发到对应的连接任务
            println!("Received frame: {:?} from {}", frame, remote_addr);
        }
    }
}
