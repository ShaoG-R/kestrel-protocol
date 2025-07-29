//! UDP-based transport implementation using actor model.
//!
//! 使用actor模型的UDP传输实现。

use super::{BindableTransport, FrameBatch, ReceivedDatagram, Transport};
use crate::{
    error::Result,
    packet::frame::Frame,
};
use async_trait::async_trait;
use std::net::SocketAddr;
use tokio::{net::UdpSocket, sync::{mpsc, oneshot}};
use tracing::{debug, warn};
use std::sync::Arc;

/// Commands for the UDP transport actor.
///
/// UDP传输actor的命令。
#[derive(Debug)]
enum UdpTransportCommand {
    /// Send frames to a remote address.
    /// 向远程地址发送帧。
    Send {
        batch: FrameBatch,
        response_tx: oneshot::Sender<Result<()>>,
    },
    /// Get the local address.
    /// 获取本地地址。
    GetLocalAddr {
        response_tx: oneshot::Sender<Result<SocketAddr>>,
    },
    /// Rebind to a new address.
    /// 重新绑定到新地址。
    Rebind {
        new_addr: SocketAddr,
        response_tx: oneshot::Sender<Result<SocketAddr>>,
    },
    /// Shutdown the actor gracefully.
    /// 优雅地关闭actor。
    Shutdown,
}

/// UDP-based transport implementation using actor model.
///
/// This transport uses an actor pattern for send operations and direct
/// socket access for receive operations to avoid deadlocks.
///
/// 使用actor模式的基于UDP的传输实现。
///
/// 此传输对发送操作使用actor模式，对接收操作使用直接套接字访问以避免死锁。
#[derive(Debug)]
pub struct UdpTransport {
    send_command_tx: mpsc::Sender<UdpTransportCommand>,
    recv_socket: Arc<UdpSocket>,
    local_addr: SocketAddr,
}

/// Actor that manages the UDP socket send operations.
///
/// 管理UDP套接字发送操作的actor。
struct UdpTransportSendActor {
    socket: Arc<UdpSocket>,
    command_rx: mpsc::Receiver<UdpTransportCommand>,
}

impl UdpTransportSendActor {
    /// Runs the actor's main event loop.
    ///
    /// 运行actor的主事件循环。
    async fn run(mut self) {
        while let Some(command) = self.command_rx.recv().await {
            match command {
                UdpTransportCommand::Send { batch, response_tx } => {
                    let result = self.handle_send(batch).await;
                    let _ = response_tx.send(result);
                }
                UdpTransportCommand::GetLocalAddr { response_tx } => {
                    let result = self.socket.local_addr().map_err(Into::into);
                    let _ = response_tx.send(result);
                }
                UdpTransportCommand::Rebind { new_addr, response_tx } => {
                    let result = self.handle_rebind(new_addr).await;
                    let _ = response_tx.send(result);
                }
                UdpTransportCommand::Shutdown => {
                    debug!("UDP transport actor shutting down gracefully");
                    break;
                }
            }
        }
        debug!("UDP transport actor has shut down");
    }

    /// Handles sending frames.
    ///
    /// 处理发送帧。
    async fn handle_send(&self, batch: FrameBatch) -> Result<()> {
        if batch.frames.is_empty() {
            return Ok(());
        }

        let buffer = Self::serialize_frames(&batch.frames);
        debug!(
            addr = %batch.remote_addr,
            frame_count = batch.frames.len(),
            bytes = buffer.len(),
            "Sending UDP datagram"
        );

        self.socket.send_to(&buffer, batch.remote_addr).await?;
        Ok(())
    }



    /// Handles rebinding to a new address.
    ///
    /// 处理重新绑定到新地址。
    async fn handle_rebind(&mut self, new_addr: SocketAddr) -> Result<SocketAddr> {
        let new_socket = UdpSocket::bind(new_addr).await?;
        let actual_addr = new_socket.local_addr()?;
        self.socket = Arc::new(new_socket);
        debug!(addr = %actual_addr, "UDP socket rebound to new address");
        Ok(actual_addr)
    }

    /// Serializes frames into a buffer for transmission.
    ///
    /// 将帧序列化到缓冲区以便传输。
    #[inline]
    fn serialize_frames(frames: &[Frame]) -> Vec<u8> {
        let mut buffer = Vec::with_capacity(2048);
        for frame in frames {
            frame.encode(&mut buffer);
        }
        buffer
    }

    /// Deserializes frames from a received buffer.
    ///
    /// 从接收的缓冲区反序列化帧。
    #[inline]
    fn deserialize_frames(buffer: &[u8]) -> Vec<Frame> {
        let mut frames = Vec::new();
        let mut cursor = buffer;
        let original_len = buffer.len();
        
        while !cursor.is_empty() {
            let remaining_before = cursor.len();
            match Frame::decode(&mut cursor) {
                Some(frame) => {
                    frames.push(frame);
                }
                None => {
                    let consumed = remaining_before - cursor.len();
                    warn!(
                        total_bytes = original_len,
                        consumed_bytes = original_len - remaining_before,
                        failed_at_byte = consumed,
                        remaining_bytes = cursor.len(),
                        "Failed to decode frame from buffer, stopping deserialization"
                    );
                    break;
                }
            }
        }
        
        frames
    }
}

impl UdpTransport {
    /// Creates a new UDP transport from an existing socket.
    ///
    /// 从现有套接字创建新的UDP传输。
    pub fn from_socket(socket: UdpSocket) -> Result<Self> {
        let local_addr = socket.local_addr()?;
        
        // Create command channel for the send actor
        let (send_command_tx, command_rx) = mpsc::channel(1024);
        
        // Use the same socket for both send and recv operations
        let socket = Arc::new(socket);
        let recv_socket = socket.clone();
        let send_socket = socket;
        
        let actor = UdpTransportSendActor {
            socket: send_socket,
            command_rx,
        };
        
        tokio::spawn(async move {
            actor.run().await;
        });

        Ok(Self {
            send_command_tx,
            recv_socket,
            local_addr,
        })
    }

    /// Creates a new UDP transport and binds it to the specified address.
    ///
    /// 创建新的UDP传输并绑定到指定地址。
    pub async fn new(addr: SocketAddr) -> Result<Self> {
        let socket = UdpSocket::bind(addr).await?;
        Self::from_socket(socket)
    }

    /// Shuts down the transport actor gracefully.
    ///
    /// 优雅地关闭传输actor。
    pub async fn shutdown(&self) {
        let _ = self.send_command_tx.send(UdpTransportCommand::Shutdown).await;
    }

    /// Deserializes frames from a received buffer.
    ///
    /// 从接收的缓冲区反序列化帧。
    #[inline]
    fn deserialize_frames(buffer: &[u8]) -> Vec<Frame> {
        let mut frames = Vec::new();
        let mut cursor = buffer;
        let original_len = buffer.len();
        
        while !cursor.is_empty() {
            let remaining_before = cursor.len();
            match Frame::decode(&mut cursor) {
                Some(frame) => {
                    frames.push(frame);
                }
                None => {
                    let consumed = remaining_before - cursor.len();
                    warn!(
                        total_bytes = original_len,
                        consumed_bytes = original_len - remaining_before,
                        failed_at_byte = consumed,
                        remaining_bytes = cursor.len(),
                        "Failed to decode frame from buffer, stopping deserialization"
                    );
                    break;
                }
            }
        }
        
        frames
    }
}



#[async_trait]
impl Transport for UdpTransport {
    async fn send_frames(&self, batch: FrameBatch) -> Result<()> {
        let (response_tx, response_rx) = oneshot::channel();
        
        let command = UdpTransportCommand::Send {
            batch,
            response_tx,
        };
        
        if let Err(_) = self.send_command_tx.send(command).await {
            return Err(crate::error::Error::ChannelClosed);
        }
        
        response_rx.await.map_err(|_| crate::error::Error::ChannelClosed)?
    }

    async fn recv_frames(&self) -> Result<ReceivedDatagram> {
        // Direct socket access for recv operations to avoid deadlock
        // 直接访问套接字进行接收操作以避免死锁
        let mut buffer = [0u8; 2048]; // Max UDP packet size
        let (len, remote_addr) = self.recv_socket.recv_from(&mut buffer).await?;

        // Create an owned copy of the received data
        let datagram_buf = buffer[..len].to_vec();
        let frames = Self::deserialize_frames(&datagram_buf);

        debug!(
            addr = %remote_addr,
            frame_count = frames.len(),
            bytes = len,
            "Received UDP datagram"
        );

        Ok(ReceivedDatagram {
            remote_addr,
            frames,
        })
    }

    fn local_addr(&self) -> Result<SocketAddr> {
        // Return cached local address for immediate access
        // 返回缓存的本地地址以便立即访问
        Ok(self.local_addr)
    }
}

#[async_trait]
impl BindableTransport for UdpTransport {
    async fn bind(addr: SocketAddr) -> Result<Self> {
        let socket = UdpSocket::bind(addr).await?;
        Self::from_socket(socket)
    }

    async fn rebind(&mut self, new_addr: SocketAddr) -> Result<()> {
        let (response_tx, response_rx) = oneshot::channel();
        
        let command = UdpTransportCommand::Rebind {
            new_addr,
            response_tx,
        };
        
        if let Err(_) = self.send_command_tx.send(command).await {
            return Err(crate::error::Error::ChannelClosed);
        }
        
        let actual_addr = response_rx.await.map_err(|_| crate::error::Error::ChannelClosed)??;
        
        // Update cached local address with the actual bound address
        // 使用实际绑定的地址更新缓存的本地地址
        self.local_addr = actual_addr;
        
        // Also update the recv socket
        // 同时更新接收套接字
        let new_recv_socket = UdpSocket::bind(actual_addr).await?;
        self.recv_socket = Arc::new(new_recv_socket);
        
        Ok(())
    }
}

impl Drop for UdpTransport {
    fn drop(&mut self) {
        // Send shutdown command in a non-blocking way
        // 以非阻塞方式发送关闭命令
        let _ = self.send_command_tx.try_send(UdpTransportCommand::Shutdown);
    }
}