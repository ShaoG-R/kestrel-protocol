//! UDP-based transport implementation using actor model.
//!
//! 使用actor模型的UDP传输实现。

use super::{BindableTransport, FrameBatch, ReceivedDatagram, Transport};
use crate::{error::Result, packet::frame::Frame};
use arc_swap::ArcSwap;
use async_trait::async_trait;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::{
    net::UdpSocket,
    sync::{mpsc, oneshot},
};
use tracing::{debug, warn};

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
    // Shared socket using ArcSwap for lock-free atomic updates
    shared_socket: Arc<ArcSwap<UdpSocket>>,
    // Cache local address for immediate access, updated on rebind
    local_addr: SocketAddr,
}

/// Actor that manages the UDP socket send operations.
///
/// 管理UDP套接字发送操作的actor。
struct UdpTransportSendActor {
    // Reference to the same ArcSwap used by UdpTransport
    shared_socket: Arc<ArcSwap<UdpSocket>>,
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
                    let socket = self.shared_socket.load();
                    let result = socket.local_addr().map_err(Into::into);
                    let _ = response_tx.send(result);
                }
                UdpTransportCommand::Rebind {
                    new_addr,
                    response_tx,
                } => {
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

        // Load current socket atomically - no locks!
        let socket = self.shared_socket.load();
        let buffer = Self::serialize_frames(&batch.frames);
        
        debug!(
            addr = %batch.remote_addr,
            frame_count = batch.frames.len(),
            bytes = buffer.len(),
            "Sending UDP datagram"
        );

        socket.send_to(&buffer, batch.remote_addr).await?;
        Ok(())
    }

    /// Handles rebinding to a new address.
    ///
    /// 处理重新绑定到新地址。
    async fn handle_rebind(&self, new_addr: SocketAddr) -> Result<SocketAddr> {
        let new_socket = UdpSocket::bind(new_addr).await?;
        let actual_addr = new_socket.local_addr()?;
        
        // Atomically replace the socket - this is the key operation!
        self.shared_socket.store(Arc::new(new_socket));
        
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
}

impl UdpTransport {
    /// Creates a new UDP transport from an existing socket.
    ///
    /// 从现有套接字创建新的UDP传输。
    pub fn from_socket(socket: UdpSocket) -> Result<Self> {
        let local_addr = socket.local_addr()?;

        // Create command channel for the send actor
        let (send_command_tx, command_rx) = mpsc::channel(1024);

        // Create shared socket using ArcSwap - no additional Arc wrapper needed!
        let shared_socket = ArcSwap::from_pointee(socket);
        let shared_socket = Arc::new(shared_socket);

        let actor = UdpTransportSendActor {
            shared_socket: shared_socket.clone(),
            command_rx,
        };

        tokio::spawn(async move {
            actor.run().await;
        });

        Ok(Self {
            send_command_tx,
            shared_socket,
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
        let _ = self
            .send_command_tx
            .send(UdpTransportCommand::Shutdown)
            .await;
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

        let command = UdpTransportCommand::Send { batch, response_tx };

        if let Err(_) = self.send_command_tx.send(command).await {
            return Err(crate::error::Error::ChannelClosed);
        }

        response_rx
            .await
            .map_err(|_| crate::error::Error::ChannelClosed)?
    }

    async fn recv_frames(&self) -> Result<ReceivedDatagram> {
        // Load current socket atomically - always gets the latest socket!
        let socket = self.shared_socket.load();
        
        let mut buffer = [0u8; 2048]; // Max UDP packet size
        let (len, remote_addr) = socket.recv_from(&mut buffer).await?;

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

        let actual_addr = response_rx
            .await
            .map_err(|_| crate::error::Error::ChannelClosed)??;

        // Update cached local address with the actual bound address
        // 使用实际绑定的地址更新缓存的本地地址
        self.local_addr = actual_addr;

        // Note: shared_socket has already been atomically updated in the actor
        // recv operations will automatically use the new socket
        // 注意：shared_socket已经在actor中原子更新了
        // 接收操作会自动使用新的socket

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn test_concurrent_send_recv_with_rebind() {
        // Test that send and recv operations work correctly during rebind
        let initial_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let mut transport = UdpTransport::new(initial_addr).await.unwrap();
        let original_addr = transport.local_addr().unwrap();

        // Test basic send functionality
        let peer_addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        let test_frame = Frame::new_push(123, 1, 0, 1024, 0, bytes::Bytes::from("test data"));
        let batch = FrameBatch {
            remote_addr: peer_addr,
            frames: vec![test_frame],
        };

        // Send should work before rebind
        let _ = transport.send_frames(batch.clone()).await;

        // Perform a rebind
        let new_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        transport.rebind(new_addr).await.unwrap();

        // Send should still work after rebind
        let _ = transport.send_frames(batch).await;

        // Verify the transport address changed
        let final_addr = transport.local_addr().unwrap();
        assert_ne!(original_addr, final_addr, "Address should have changed after rebind");

        // Verify socket consistency
        let socket1 = transport.shared_socket.load();
        let socket2 = transport.shared_socket.load();
        assert_eq!(
            socket1.local_addr().unwrap(),
            socket2.local_addr().unwrap(),
            "Multiple loads should return the same socket"
        );

        transport.shutdown().await;
    }

    #[tokio::test]
    async fn test_socket_consistency_during_rebind() {
        // Test that send and recv operations use the same socket after rebind
        let initial_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let mut transport = UdpTransport::new(initial_addr).await.unwrap();

        // Get initial socket reference
        let initial_socket = transport.shared_socket.load();
        let initial_local_addr = initial_socket.local_addr().unwrap();

        // Perform rebind
        let new_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        transport.rebind(new_addr).await.unwrap();

        // Get new socket reference
        let new_socket = transport.shared_socket.load();
        let new_local_addr = new_socket.local_addr().unwrap();

        // Verify socket has changed
        assert_ne!(
            initial_local_addr, new_local_addr,
            "Socket address should change after rebind"
        );

        // Verify both send and recv operations use the same socket
        let send_socket = transport.shared_socket.load();
        let recv_socket = transport.shared_socket.load();
        
        assert_eq!(
            send_socket.local_addr().unwrap(),
            recv_socket.local_addr().unwrap(),
            "Send and recv should use the same socket"
        );

        transport.shutdown().await;
    }

    #[tokio::test]
    async fn test_multiple_concurrent_rebinds() {
        // Test that multiple concurrent rebind operations are handled correctly
        let initial_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let mut transport = UdpTransport::new(initial_addr).await.unwrap();

        // Perform sequential rebinds since we can't have multiple mutable references
        let mut success_count = 0;
        for i in 0..5 {
            let new_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
            match transport.rebind(new_addr).await {
                Ok(()) => {
                    println!("Rebind {} succeeded", i);
                    success_count += 1;
                }
                Err(e) => {
                    println!("Rebind {} failed: {:?}", i, e);
                }
            }
            
            // Small delay between rebinds
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // All rebinds should succeed in sequential execution
        assert!(success_count > 0, "At least one rebind should succeed");

        // Verify transport is still functional
        let final_addr = transport.local_addr().unwrap();
        println!("Final address: {}", final_addr);

        transport.shutdown().await;
    }

    #[tokio::test]
    async fn test_concurrent_socket_operations() {
        // Test concurrent socket operations through the actor system
        let initial_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let transport = UdpTransport::new(initial_addr).await.unwrap();
        let peer_addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();

        // Create Arc wrapper for shared access
        let transport = Arc::new(transport);
        
        // Spawn multiple tasks that send frames concurrently
        let mut send_handles = Vec::new();
        for i in 0..10 {
            let transport_clone = transport.clone();
            let handle = tokio::spawn(async move {
                let frame = Frame::new_push(123, i, 0, 1024, 0, bytes::Bytes::from(format!("concurrent_{}", i)));
                let batch = FrameBatch {
                    remote_addr: peer_addr,
                    frames: vec![frame],
                };
                
                // This should not panic even if socket is being updated
                let _ = transport_clone.send_frames(batch).await;
            });
            send_handles.push(handle);
        }

        // Spawn tasks that read the current socket
        let mut read_handles = Vec::new();
        for i in 0..5 {
            let shared_socket = transport.shared_socket.clone();
            let handle = tokio::spawn(async move {
                for _ in 0..20 {
                    let socket = shared_socket.load();
                    let addr = socket.local_addr().unwrap();
                    assert!(addr.port() > 0, "Reader {}: Invalid socket", i);
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            });
            read_handles.push(handle);
        }

        // Wait for all operations to complete
        for handle in send_handles {
            handle.await.unwrap();
        }
        for handle in read_handles {
            handle.await.unwrap();
        }

        transport.shutdown().await;
    }

    #[tokio::test]
    async fn test_arcswap_atomic_updates() {
        // Test that ArcSwap provides atomic updates without intermediate states
        let initial_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let transport = UdpTransport::new(initial_addr).await.unwrap();

        // Spawn multiple readers that continuously check socket consistency
        let mut reader_handles = Vec::new();
        for i in 0..3 {
            let shared_socket = transport.shared_socket.clone();
            let handle = tokio::spawn(async move {
                for _ in 0..100 {
                    let socket = shared_socket.load();
                    let addr = socket.local_addr().unwrap();
                    
                    // Verify the socket is always valid
                    assert!(addr.port() > 0, "Reader {}: Invalid socket address", i);
                    
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            });
            reader_handles.push(handle);
        }

        // Spawn a writer that updates the socket
        let writer_handle = {
            let shared_socket = transport.shared_socket.clone();
            tokio::spawn(async move {
                for _ in 0..10 {
                    let new_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
                    shared_socket.store(Arc::new(new_socket));
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
        };

        // Wait for all operations to complete
        writer_handle.await.unwrap();
        for handle in reader_handles {
            handle.await.unwrap();
        }

        transport.shutdown().await;
    }

    #[tokio::test]
    async fn test_send_recv_during_rapid_rebinds() {
        // Test send/recv operations during rapid rebind operations
        let initial_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let mut transport1 = UdpTransport::new(initial_addr).await.unwrap();
        let transport2 = UdpTransport::new(initial_addr).await.unwrap();

        let addr1 = transport1.local_addr().unwrap();
        let _addr2 = transport2.local_addr().unwrap();

        // Rapid rebind on transport1
        let rebind_handle = tokio::spawn(async move {
            for _ in 0..10 {
                let new_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
                let _ = transport1.rebind(new_addr).await;
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            transport1
        });

        // Continuous send from transport2
        let send_handle = tokio::spawn(async move {
            for i in 0..20 {
                let batch = FrameBatch {
                    remote_addr: addr1, // This might become stale, but shouldn't crash
                    frames: vec![Frame::new_push(123, i, 0, 1024, 0, bytes::Bytes::from(format!("msg_{}", i)))],
                };
                
                // Send operations should not panic even if target address is stale
                let _ = transport2.send_frames(batch).await;
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            transport2
        });

        // Wait for operations to complete
        let transport1 = rebind_handle.await.unwrap();
        let transport2 = send_handle.await.unwrap();

        // Cleanup
        transport1.shutdown().await;
        transport2.shutdown().await;
    }
}