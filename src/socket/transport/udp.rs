//! UDP-based transport implementation using an actor model and a buffered receiver.
//!
//! 使用actor模型和带缓冲接收器的UDP传输实现。

use super::{BindableTransport, FrameBatch, ReceivedDatagram, Transport};
use crate::{error::Result, packet::frame::Frame};
use arc_swap::ArcSwap;
use async_trait::async_trait;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::{
    net::UdpSocket,
    sync::{mpsc, oneshot, watch},
};
use tracing::{debug, error, warn};

/// Commands for the UDP transport actor (send-side).
///
/// UDP传输actor的命令 (发送侧)。
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
}

/// UDP-based transport implementation.
///
/// This design uses three main components for full decoupling:
/// 1.  **Send Actor (`UdpTransportSendActor`)**: Manages all send operations and socket rebinding
///     via a command channel. This serializes write access to the socket.
/// 2.  **Receive Task (`receiver_task`)**: A dedicated asynchronous task that continuously polls
///     the UDP socket for incoming datagrams and pushes them into a lock-free queue.
/// 3.  **Shared Socket (`Arc<ArcSwap<UdpSocket>>`)**: Allows both the send actor and receive task
///     to safely access the underlying `UdpSocket` and allows it to be atomically replaced
///     during a rebind operation without locks.
///
/// 使用actor模式的基于UDP的传输实现。
///
/// 此设计使用三个主要组件实现完全解耦：
/// 1. **发送Actor (`UdpTransportSendActor`)**: 通过命令通道管理所有发送操作和套接字重绑定。
/// 2. **接收任务 (`receiver_task`)**: 一个专用的异步任务，持续轮询UDP套接字以获取传入的数据报，并将其推入无锁队列。
/// 3. **共享套接字 (`Arc<ArcSwap<UdpSocket>>`)**: 允许发送actor和接收任务安全地访问底层`UdpSocket`，并允许在重绑定操作期间原子地替换它，而无需锁。
#[derive(Debug)]
pub struct UdpTransport {
    send_command_tx: mpsc::Sender<UdpTransportCommand>,
    // The receiving end of the datagram buffer.
    // 数据报缓冲区的接收端。
    datagram_rx: async_channel::Receiver<ReceivedDatagram>,
    // A watch channel to signal shutdown to the receiver task.
    // 用于向接收器任务发送关闭信号的watch通道。
    shutdown_tx: Arc<watch::Sender<()>>,
    // Cache local address for immediate access, updated on rebind.
    // 缓存本地地址以便立即访问，在重绑定时更新。
    local_addr: Arc<ArcSwap<SocketAddr>>,
}

/// Actor that manages the UDP socket send operations.
///
/// 管理UDP套接字发送操作的actor。
struct UdpTransportSendActor {
    // Reference to the same ArcSwap used by UdpTransport's receiver.
    // UdpTransport接收器使用的相同ArcSwap的引用。
    shared_socket: Arc<ArcSwap<UdpSocket>>,
    command_rx: mpsc::Receiver<UdpTransportCommand>,
    // A reference to the cached local address to update it on rebind.
    // 对缓存的本地地址的引用，以便在重绑定时更新它。
    local_addr: Arc<ArcSwap<SocketAddr>>,
}

impl UdpTransportSendActor {
    /// Runs the actor's main event loop.
    ///
    /// 运行actor的主事件循环。
    async fn run(mut self) {
        debug!("UDP transport send actor started");
        while let Some(command) = self.command_rx.recv().await {
            match command {
                UdpTransportCommand::Send { batch, response_tx } => {
                    let result = self.handle_send(batch).await;
                    let _ = response_tx.send(result);
                }
                UdpTransportCommand::GetLocalAddr { response_tx } => {
                    let result = self.shared_socket.load().local_addr().map_err(Into::into);
                    let _ = response_tx.send(result);
                }
                UdpTransportCommand::Rebind {
                    new_addr,
                    response_tx,
                } => {
                    let result = self.handle_rebind(new_addr).await;
                    let _ = response_tx.send(result);
                }
            }
        }
        debug!("UDP transport send actor has shut down");
    }

    /// Handles sending frames.
    ///
    /// 处理发送帧。
    async fn handle_send(&self, batch: FrameBatch) -> Result<()> {
        if batch.frames.is_empty() {
            return Ok(());
        }

        // Load current socket atomically - no locks!
        // 原子加载当前套接字 - 无锁！
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
        debug!(?new_addr, "Attempting to rebind UDP socket");
        let new_socket = UdpSocket::bind(new_addr).await?;
        let actual_addr = new_socket.local_addr()?;

        // Atomically replace the socket. The receiver task will pick this up on its next loop.
        // 原子地替换套接字。接收任务将在其下一个循环中获取此更新。
        self.shared_socket.store(Arc::new(new_socket));

        // Atomically update the cached local address.
        // 原子地更新缓存的本地地址。
        self.local_addr.store(Arc::new(actual_addr));

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

/// The dedicated receiver task.
///
/// 专用的接收器任务。
async fn receiver_task(
    shared_socket: Arc<ArcSwap<UdpSocket>>,
    datagram_tx: async_channel::Sender<ReceivedDatagram>,
    mut shutdown_rx: watch::Receiver<()>,
) {
    debug!("UDP transport receiver task started");
    let mut buffer = [0u8; 2048]; // Max UDP packet size

    loop {
        // Load the current socket on each iteration to handle rebinds.
        // 在每次迭代时加载当前套接字以处理重绑定。
        let socket = shared_socket.load_full();

        tokio::select! {
            // Biased select to prefer shutdown signal.
            // 优先选择关闭信号。
            biased;

            _ = shutdown_rx.changed() => {
                debug!("Receiver task received shutdown signal");
                break;
            }
            result = socket.recv_from(&mut buffer) => {
                match result {
                    Ok((len, remote_addr)) => {
                        let datagram_buf = buffer[..len].to_vec();
                        let frames = deserialize_frames(&datagram_buf);

                        debug!(
                            addr = %remote_addr,
                            frame_count = frames.len(),
                            bytes = len,
                            "Received UDP datagram and pushed to buffer"
                        );

                        let received = ReceivedDatagram { remote_addr, frames };

                        // Use try_send for non-blocking behavior. If the bounded channel is full,
                        // it returns an error immediately, and we drop the packet.
                        // This prevents the network I/O task from blocking.
                        //
                        // 使用 try_send 实现非阻塞行为。如果带边界的通道已满，
                        // 它会立即返回错误，我们丢弃该数据包。
                        // 这可以防止网络I/O任务阻塞。
                        if let Err(e) = datagram_tx.try_send(received) {
                            warn!("Failed to push received datagram to buffer (likely full), dropping packet. Reason: {}", e);
                        }
                    }
                    Err(e) => {
                        // This can happen during rebind, which is normal.
                        // 这种情况在重绑定期间可能发生，是正常的。
                        error!("UDP recv_from error: {}", e);
                        // Avoid busy-looping on persistent errors.
                        // 避免在持续错误上产生忙循环。
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                }
            }
        }
    }
    debug!("UDP transport receiver task has shut down");
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

impl UdpTransport {
    /// Creates a new UDP transport from an existing socket.
    ///
    /// 从现有套接字创建新的UDP传输。
    pub fn from_socket(socket: UdpSocket) -> Result<Self> {
        let local_addr = socket.local_addr()?;

        // --- Channels and Shared State ---
        let (send_command_tx, command_rx) = mpsc::channel(1024);
        // This is a Single-Producer, Single-Consumer (SPSC) scenario, but async_channel
        // is used for its high performance and convenient `try_send` API.
        // A bounded channel is crucial for backpressure to prevent unbounded memory usage.
        //
        // 这是一个单生产者，单消费者（SPSC）的场景，但我们使用 async_channel
        // 是因为它具有高性能和方便的 `try_send` API。
        // 有界通道对于提供背压以防止无限的内存使用至关重要。
        let (datagram_tx, datagram_rx) = async_channel::bounded(1024);
        let (shutdown_tx, shutdown_rx) = watch::channel(());

        let shared_socket = Arc::new(ArcSwap::new(Arc::new(socket)));
        let cached_local_addr = Arc::new(ArcSwap::new(Arc::new(local_addr)));

        // --- Start Actor and Tasks ---
        let send_actor = UdpTransportSendActor {
            shared_socket: shared_socket.clone(),
            command_rx,
            local_addr: cached_local_addr.clone(),
        };
        tokio::spawn(send_actor.run());

        tokio::spawn(receiver_task(shared_socket, datagram_tx, shutdown_rx));

        Ok(Self {
            send_command_tx,
            datagram_rx,
            shutdown_tx: Arc::new(shutdown_tx),
            local_addr: cached_local_addr,
        })
    }

    /// Creates a new UDP transport and binds it to the specified address.
    ///
    /// 创建新的UDP传输并绑定到指定地址。
    pub async fn new(addr: SocketAddr) -> Result<Self> {
        let socket = UdpSocket::bind(addr).await?;
        Self::from_socket(socket)
    }

    /// Gets the local address by querying the actor directly, ensuring it's fresh from the socket.
    /// This is useful when you need to ensure you're getting the most up-to-date address.
    ///
    /// 通过直接查询actor获取本地地址，确保它是从套接字获取的最新地址。
    /// 当您需要确保获取最新地址时，这很有用。
    pub async fn fresh_local_addr(&self) -> Result<SocketAddr> {
        let (response_tx, response_rx) = oneshot::channel();
        let command = UdpTransportCommand::GetLocalAddr { response_tx };

        if self.send_command_tx.send(command).await.is_err() {
            return Err(crate::error::Error::ChannelClosed);
        }

        response_rx
            .await
            .map_err(|_| crate::error::Error::ChannelClosed)?
    }

    /// Shuts down the transport actor and receiver task gracefully.
    ///
    /// 优雅地关闭传输actor和接收器任务。
    pub fn shutdown(&self) {
        // The receiver will get this signal and shut down.
        // 接收器将收到此信号并关闭。
        let _ = self.shutdown_tx.send(());
        // The sender actor will shut down when the UdpTransport is dropped and
        // the send_command_tx channel closes.
        // 当UdpTransport被丢弃且send_command_tx通道关闭时，发送actor将关闭。
    }
}

#[async_trait]
impl Transport for UdpTransport {
    async fn send_frames(&self, batch: FrameBatch) -> Result<()> {
        let (response_tx, response_rx) = oneshot::channel();
        let command = UdpTransportCommand::Send { batch, response_tx };

        if self.send_command_tx.send(command).await.is_err() {
            return Err(crate::error::Error::ChannelClosed);
        }

        response_rx
            .await
            .map_err(|_| crate::error::Error::ChannelClosed)?
    }

    /// Asynchronously receives a datagram from the internal buffer.
    /// This method only requires an immutable reference `&self`.
    ///
    /// 从内部缓冲区异步接收数据报。
    /// 此方法仅需要不可变引用`&self`。
    async fn recv_frames(&self) -> Result<ReceivedDatagram> {
        match self.datagram_rx.recv().await {
            Ok(datagram) => Ok(datagram),
            Err(_) => Err(crate::error::Error::ChannelClosed),
        }
    }

    fn local_addr(&self) -> Result<SocketAddr> {
        // 大多数情况下直接使用缓存的地址即可，这样更高效
        // In most cases, use the cached address directly, which is more efficient
        Ok(**self.local_addr.load())
    }
}

#[async_trait]
impl BindableTransport for UdpTransport {
    async fn bind(addr: SocketAddr) -> Result<Self> {
        let socket = UdpSocket::bind(addr).await?;
        Self::from_socket(socket)
    }

    /// Rebinds the transport to a new address. This can be called concurrently
    /// with `send_frames` and `recv_frames`.
    ///
    /// 将传输重新绑定到新地址。这可以与`send_frames`和`recv_frames`并发调用。
    async fn rebind(&self, new_addr: SocketAddr) -> Result<()> {
        let (response_tx, response_rx) = oneshot::channel();
        let command = UdpTransportCommand::Rebind {
            new_addr,
            response_tx,
        };

        if self.send_command_tx.send(command).await.is_err() {
            return Err(crate::error::Error::ChannelClosed);
        }

        // Wait for the rebind to complete in the actor.
        // The actor will update the shared socket and the cached address.
        // 等待actor中的重绑定完成。
        // actor将更新共享套接字和缓存的地址。
        response_rx
            .await
            .map_err(|_| crate::error::Error::ChannelClosed)??;

        Ok(())
    }
}

impl Drop for UdpTransport {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use std::time::Duration;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_send_and_recv_decoupled() {
        let transport1_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let transport1 = UdpTransport::new(transport1_addr).await.unwrap();
        let addr1 = transport1.local_addr().unwrap();

        let transport2_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let transport2 = UdpTransport::new(transport2_addr).await.unwrap();
        let addr2 = transport2.local_addr().unwrap();

        // Transport 1 sends a packet to Transport 2
        let frame1 = Frame::new_push(1, 1, 0, 1024, 0, Bytes::from_static(b"hello 2"));
        let batch1 = FrameBatch {
            remote_addr: addr2,
            frames: vec![frame1],
        };
        transport1.send_frames(batch1).await.unwrap();

        // Transport 2 sends a packet to Transport 1
        let frame2 = Frame::new_push(2, 2, 0, 1024, 0, Bytes::from_static(b"hello 1"));
        let batch2 = FrameBatch {
            remote_addr: addr1,
            frames: vec![frame2],
        };
        transport2.send_frames(batch2).await.unwrap();

        // Concurrently receive on both transports
        let recv1 = transport1.recv_frames();
        let recv2 = transport2.recv_frames();

        let (res1, res2) = tokio::join!(recv1, recv2);

        // Check transport 1 received from 2
        let received1 = res1.unwrap();
        assert_eq!(received1.remote_addr, addr2);
        assert_eq!(received1.frames.len(), 1);

        // Check transport 2 received from 1
        let received2 = res2.unwrap();
        assert_eq!(received2.remote_addr, addr1);
        assert_eq!(received2.frames.len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_concurrent_send_recv_with_rebind() {
        // Transport that will be the sender and will be rebound
        let transport = UdpTransport::new("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let original_addr = transport.local_addr().unwrap();

        // The peer that will receive packets
        let peer_transport = UdpTransport::new("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let peer_addr = peer_transport.local_addr().unwrap();

        // Put both into Arcs to share with tasks
        let transport_arc = Arc::new(transport);
        let peer_transport_arc = Arc::new(peer_transport);

        // Task to send frames FROM transport_arc TO peer_transport_arc
        let send_task = {
            let transport = transport_arc.clone();
            tokio::spawn(async move {
                for i in 0..10 {
                    let frame = Frame::new_push(
                        i as u32,
                        1,
                        0,
                        1024,
                        0,
                        Bytes::from(format!("data {}", i)),
                    );
                    let batch = FrameBatch {
                        remote_addr: peer_addr,
                        frames: vec![frame],
                    };
                    if let Err(e) = transport.send_frames(batch).await {
                        error!("Send failed: {}", e);
                        break; // Stop sending on error
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
        };

        // Task to receive frames on peer_transport_arc
        let recv_task = {
            let peer_transport = peer_transport_arc.clone();
            tokio::spawn(async move {
                let mut received_count = 0;
                loop {
                    // Use a timeout to stop the loop gracefully
                    match tokio::time::timeout(Duration::from_secs(1), peer_transport.recv_frames())
                        .await
                    {
                        Ok(Ok(_)) => received_count += 1,
                        _ => break, // Break on error or timeout
                    }
                }
                received_count
            })
        };

        // Let some packets fly before rebinding
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Rebind the sender transport while sends are ongoing.
        // This now works because rebind takes &self and can be called on an Arc.
        let new_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        transport_arc.rebind(new_addr).await.unwrap();

        let final_addr = transport_arc.local_addr().unwrap();
        assert_ne!(
            original_addr, final_addr,
            "Address should have changed after rebind"
        );

        // Wait for tasks to complete
        send_task.await.unwrap();
        let received_count = recv_task.await.unwrap();

        assert!(received_count > 0, "Should have received some packets");
        println!("Received {} packets", received_count);

        // Shutdown transports
        transport_arc.shutdown();
        peer_transport_arc.shutdown();
    }

    #[tokio::test]
    async fn test_shutdown_stops_receiver() {
        let transport = UdpTransport::new("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let transport_arc = Arc::new(transport);

        let recv_handle = {
            let transport = transport_arc.clone();
            tokio::spawn(async move {
                // This should eventually fail when the channel is closed after shutdown.
                transport.recv_frames().await
            })
        };

        // Give the receiver a moment to start
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Shutdown the transport
        transport_arc.shutdown();

        // The recv_frames call should now unblock and return an error
        let result = tokio::time::timeout(Duration::from_secs(1), recv_handle).await;

        assert!(result.is_ok(), "recv_frames did not unblock after shutdown");
        let inner_result = result.unwrap();
        assert!(inner_result.is_ok(), "Join handle failed");
        let final_result = inner_result.unwrap();
        assert!(final_result.is_err(), "Expected channel closed error");
        assert!(matches!(
            final_result.unwrap_err(),
            crate::error::Error::ChannelClosed
        ));
    }

    #[tokio::test]
    async fn test_local_addr_methods() {
        let transport = UdpTransport::new("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();

        // Test the cached local_addr method
        let cached_addr = transport.local_addr().unwrap();
        assert!(cached_addr.port() > 0);

        // Test the fresh_local_addr method that uses GetLocalAddr command
        let fresh_addr = transport.fresh_local_addr().await.unwrap();
        assert_eq!(
            cached_addr, fresh_addr,
            "Cached and fresh addresses should match"
        );

        // Test that both addresses have the same IP and port
        assert_eq!(cached_addr.ip(), fresh_addr.ip());
        assert_eq!(cached_addr.port(), fresh_addr.port());
    }
}
