//! 包含协议的顶层Socket接口。
//! Contains the top-level Socket interface for the protocol.

use crate::config::Config;
use crate::core::endpoint::Endpoint;
use crate::core::stream::Stream;
use crate::error::Result;
use crate::packet::frame::Frame;
use dashmap::DashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use async_trait::async_trait;
use tracing::{debug, error, info, warn};

/// An asynchronous UDP socket interface.
///
/// This trait allows for abstracting over the underlying UDP socket implementation,
/// enabling custom socket implementations for testing or other purposes.
///
/// 异步UDP套接字接口。
///
/// 此trait允许对底层UDP套接字实现进行抽象，从而可以为测试或其他目的自定义套接字实现。
#[async_trait]
pub trait AsyncUdpSocket: Send + Sync + 'static {
    /// Sends data on the socket to the given address.
    async fn send_to(&self, buf: &[u8], target: SocketAddr) -> Result<usize>;

    /// Receives a single datagram on the socket.
    async fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)>;

    /// Returns the local address that this socket is bound to.
    fn local_addr(&self) -> Result<SocketAddr>;
}

#[async_trait]
impl AsyncUdpSocket for UdpSocket {
    async fn send_to(&self, buf: &[u8], target: SocketAddr) -> Result<usize> {
        UdpSocket::send_to(self, buf, target).await.map_err(Into::into)
    }

    async fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)> {
        UdpSocket::recv_from(self, buf).await.map_err(Into::into)
    }

    fn local_addr(&self) -> Result<SocketAddr> {
        UdpSocket::local_addr(self).map_err(Into::into)
    }
}

/// A command for the central socket sender task.
/// 用于中央套接字发送任务的命令。
#[derive(Debug)]
pub struct SendCommand {
    /// The destination address.
    /// 目标地址。
    pub remote_addr: SocketAddr,
    /// The frames to send.
    /// 要发送的帧。
    pub frames: Vec<Frame>,
}

/// A listener for incoming reliable UDP connections.
///
/// This is created by `ReliableUdpSocket::bind` and can be used to accept
/// new incoming connections.
///
/// 用于传入可靠UDP连接的监听器。
///
/// 这是由 `ReliableUdpSocket::bind` 创建的，可以用来接受新的传入连接。
pub struct ConnectionListener {
    /// A receiver for newly accepted connections.
    /// 用于新接受连接的接收端。
    accept_rx: mpsc::Receiver<(Stream, SocketAddr)>,
}

impl ConnectionListener {
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

/// A reliable UDP socket.
///
/// This is the main entry point for the protocol. It is responsible for
/// binding a UDP socket and managing all the connections.
///
/// 一个可靠的UDP套接字。
///
/// 这是协议的主要入口点。它负责绑定一个UDP套接字并管理所有的连接。
pub struct ReliableUdpSocket<S: AsyncUdpSocket> {
    socket: Arc<S>,
    /// A map of all active connections, mapping a remote address to a sender
    /// that can send frames to the connection's task.
    /// 所有活动连接的映射，将远程地址映射到一个可以向连接任务发送帧的发送端。
    connections: Arc<DashMap<SocketAddr, mpsc::Sender<Frame>>>,
    /// A sender to the central sending task.
    /// 到中央发送任务的发送端。
    send_tx: mpsc::Sender<SendCommand>,
    /// A sender for newly accepted connections.
    /// 用于新接受连接的发送端。
    accept_tx: mpsc::Sender<(Stream, SocketAddr)>,
}

impl ReliableUdpSocket<UdpSocket> {
    /// Creates a new `ReliableUdpSocket` and binds it to the given address.
    ///
    /// Returns a `ReliableUdpSocket` for managing connections and a
    /// `ConnectionListener` for accepting them.
    ///
    /// 创建一个新的 `ReliableUdpSocket` 并将其绑定到给定的地址。
    ///
    /// 返回一个用于管理连接的 `ReliableUdpSocket` 和一个用于接受连接的
    /// `ConnectionListener`。
    pub async fn bind(addr: SocketAddr) -> Result<(Self, ConnectionListener)> {
        let socket = UdpSocket::bind(addr).await?;
        Self::with_socket(socket)
    }
}

impl<S: AsyncUdpSocket> ReliableUdpSocket<S> {
    /// Creates a new `ReliableUdpSocket` with a provided socket implementation.
    ///
    /// Returns a `ReliableUdpSocket` for managing connections and a
    /// `ConnectionListener` for accepting them.
    ///
    /// 使用提供的套接字实现创建一个新的 `ReliableUdpSocket`。
    ///
    /// 返回一个用于管理连接的 `ReliableUdpSocket` 和一个用于接受连接的
    /// `ConnectionListener`。
    pub fn with_socket(socket: S) -> Result<(Self, ConnectionListener)> {
        let socket = Arc::new(socket);
        let connections = Arc::new(DashMap::new());

        // Create a channel for sending packets.
        let (send_tx, send_rx) = mpsc::channel::<SendCommand>(1024);

        // Create a channel for accepting new connections.
        let (accept_tx, accept_rx) = mpsc::channel(128);

        // Spawn the sender task.
        let socket_clone = socket.clone();
        tokio::spawn(sender_task(socket_clone, send_rx));

        info!(addr = ?socket.local_addr().ok(), "ReliableUdpSocket created and running");
        let socket_handle = Self {
            socket,
            connections,
            send_tx,
            accept_tx,
        };
        let listener = ConnectionListener { accept_rx };

        Ok((socket_handle, listener))
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

        let local_cid = rand::random();

        let (tx_to_endpoint, rx_from_socket) = mpsc::channel(128);

        // Create a new Endpoint and get the stream handles
        let (mut endpoint, tx_to_stream_handle, rx_from_stream_handle) = Endpoint::new_client(
            config,
            remote_addr,
            local_cid,
            rx_from_socket,
            self.send_tx.clone(),
            initial_data,
        );

        // Spawn a new task for the connection to run in.
        tokio::spawn(async move {
            info!(addr = %remote_addr, "Spawning new endpoint task for outbound connection");
            if let Err(e) = endpoint.run().await {
                error!(addr = %remote_addr, "Endpoint closed with error: {}", e);
            }
        });

        // Insert the sender into the map so that incoming packets can be routed.
        self.connections.insert(remote_addr, tx_to_endpoint);

        // Create and return the user-facing Stream handle
        let stream = Stream::new(tx_to_stream_handle, rx_from_stream_handle);
        Ok(stream)
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
                    error!("Failed to receive from socket: {}", e);
                    continue;
                }
            };
            debug!(len, addr = %remote_addr, "Received UDP datagram");

            let data = &recv_buf[..len];
            let frame = match Frame::decode(data) {
                Some(frame) => frame,
                None => {
                    // Log and drop invalid packets.
                    warn!(addr = %remote_addr, "Received an invalid packet");
                    continue;
                }
            };

            // 如果我们已经有这个连接的记录，直接发送
            if let Some(tx) = self.connections.get(&remote_addr) {
                if tx.send(frame).await.is_err() {
                    // This means the connection task has died. Remove it.
                    // 这意味着连接任务已经死亡。移除它。
                    debug!(addr = %remote_addr, "Connection task appears to have died. Removing.");
                    self.connections.remove(&remote_addr);
                }
                continue;
            }

            // A new connection can only be initiated with a SYN packet from an unknown peer.
            // 只有来自未知对端的SYN包才能发起新连接。
            if let Frame::Syn { header, .. } = &frame {
                // TODO: Allow server-side configuration
                let config = Config::default();

                // Check for version compatibility.
                if header.protocol_version != config.protocol_version {
                    warn!(
                        addr = %remote_addr,
                        client_version = header.protocol_version,
                        server_version = config.protocol_version,
                        "Dropping SYN with incompatible protocol version."
                    );
                    continue;
                }

                info!(addr = %remote_addr, "Accepting new connection attempt.");
                // The client's CID is in the source_cid field. The destination is our future CID.
                let peer_cid = header.source_cid;
                let local_cid = rand::random();
                let (tx_to_endpoint, rx_from_socket) = mpsc::channel(128);

                let (mut endpoint, tx_to_stream_handle, rx_from_stream_handle) =
                    Endpoint::new_server(
                        config,
                        remote_addr,
                        local_cid,
                        peer_cid,
                        rx_from_socket,
                        self.send_tx.clone(),
                    );

                // Spawn a new task for the connection to run in.
                tokio::spawn(async move {
                    info!(addr = %remote_addr, "Spawning new endpoint task for inbound connection");
                    if let Err(e) = endpoint.run().await {
                        error!(addr = %remote_addr, "Endpoint closed with error: {}", e);
                    }
                });

                // Insert into map so future packets are routed.
                self.connections
                    .insert(remote_addr, tx_to_endpoint.clone());

                // Send the initial frame to the newly created connection.
                if tx_to_endpoint.send(frame.clone()).await.is_err() {
                    // Worker died immediately.
                    self.connections.remove(&remote_addr);
                    warn!(addr = %remote_addr, "Failed to send initial SYN to newly created worker.");
                    continue;
                }

                // Create the stream handle and send it to the user calling accept().
                let stream = Stream::new(tx_to_stream_handle, rx_from_stream_handle);
                if self.accept_tx.send((stream, remote_addr)).await.is_err() {
                    // No one is listening for new connections.
                    info!(
                        "No active listener on accept(), dropping new connection from {}",
                        remote_addr
                    );
                    self.connections.remove(&remote_addr); // Clean up
                }
            } else {
                // Ignore packets from unknown addresses that are not SYN.
                debug!(
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
async fn sender_task<S: AsyncUdpSocket>(socket: Arc<S>, mut rx: mpsc::Receiver<SendCommand>) {
    const MAX_BATCH_SIZE: usize = 64;
    let mut send_buf = Vec::with_capacity(2048);
    let mut commands = Vec::with_capacity(MAX_BATCH_SIZE);

    while let Some(first_cmd) = rx.recv().await {
        commands.push(first_cmd);

        // Try to drain the channel of any pending commands to process in a batch.
        while commands.len() < MAX_BATCH_SIZE {
            match rx.try_recv() {
                Ok(cmd) => commands.push(cmd),
                Err(mpsc::error::TryRecvError::Empty) => break, // No more commands for now.
                Err(mpsc::error::TryRecvError::Disconnected) => return, // Channel closed.
            }
        }

        for cmd in commands.drain(..) {
            send_buf.clear();
            // This is "packet coalescing" at the sender level.
            // Multiple frames for the same destination are encoded into a single UDP datagram.
            for frame in cmd.frames {
                frame.encode(&mut send_buf);
            }

            if send_buf.is_empty() {
                continue;
            }
            debug!(
                addr = %cmd.remote_addr,
                bytes = send_buf.len(),
                "sender_task sending datagram"
            );

            if let Err(e) = socket.send_to(&send_buf, cmd.remote_addr).await {
                // A single send error is not fatal to the whole socket.
                error!(
                    addr = %cmd.remote_addr,
                    "Failed to send packet: {}",
                    e
                );
            }
        }
    }
}



#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::command::Command;
    use bytes::Bytes;
    use std::time::Duration;
    use std::collections::VecDeque;
    use std::sync::{Mutex};
    use tokio::io::AsyncWriteExt;

    /// A mock UDP socket for testing purposes.
    struct MockUdpSocket {
        local_addr: SocketAddr,
        sent_packets: Arc<Mutex<Vec<(Bytes, SocketAddr)>>>,
        recv_queue: Arc<Mutex<VecDeque<(Bytes, SocketAddr)>>>,
    }

    impl MockUdpSocket {
        fn new(
            local_addr: SocketAddr,
            sent_packets: Arc<Mutex<Vec<(Bytes, SocketAddr)>>>,
            recv_queue: Arc<Mutex<VecDeque<(Bytes, SocketAddr)>>>,
        ) -> Self {
            Self {
                local_addr,
                sent_packets,
                recv_queue,
            }
        }
    }

    #[async_trait]
    impl AsyncUdpSocket for MockUdpSocket {
        async fn send_to(&self, buf: &[u8], target: SocketAddr) -> Result<usize> {
            let mut sent = self.sent_packets.lock().unwrap();
            sent.push((Bytes::copy_from_slice(buf), target));
            Ok(buf.len())
        }

        async fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)> {
            loop {
                let maybe_data = {
                    // Lock, check, and release inside this block
                    let mut queue = self.recv_queue.lock().unwrap();
                    queue.pop_front()
                };

                if let Some((data, addr)) = maybe_data {
                    let len = data.len();
                    buf[..len].copy_from_slice(&data);
                    return Ok((len, addr));
                }
                
                // If we're here, the queue was empty. Yield to the scheduler.
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }

        fn local_addr(&self) -> Result<SocketAddr> {
            Ok(self.local_addr)
        }
    }
    
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_server_accept() {
        // 1. Setup mock environment
        let listener_addr = "127.0.0.1:9999".parse().unwrap();
        let client_addr = "127.0.0.1:1111".parse().unwrap();

        let server_recv_queue = Arc::new(Mutex::new(VecDeque::new()));
        let server_sent_packets = Arc::new(Mutex::new(Vec::new()));

        let mock_socket = MockUdpSocket::new(
            listener_addr,
            server_sent_packets.clone(),
            server_recv_queue.clone(),
        );

        // 2. Setup the ReliableUdpSocket with the mock
        let (socket, mut listener) = ReliableUdpSocket::with_socket(mock_socket).unwrap();
        let socket_arc = Arc::new(socket);

        // 3. Spawn the listener's run loop
        let socket_run = socket_arc.clone();
        let run_handle = tokio::spawn(async move {
            socket_run.run().await;
        });

        // 4. Simulate a client sending a SYN packet by populating the mock's receive queue
        let syn_header = crate::packet::header::LongHeader {
            command: Command::Syn,
            protocol_version: 1, // Use a matching version for the test
            destination_cid: 0,  // Destination is unknown initially
            source_cid: 1234,    // Client's chosen CID
        };
        let syn_frame = Frame::Syn {
            header: syn_header,
            payload: Bytes::new(),
        };
        let mut send_buf = Vec::new();
        syn_frame.encode(&mut send_buf);

        server_recv_queue
            .lock()
            .unwrap()
            .push_back((Bytes::from(send_buf), client_addr));

        // 5. Call accept() on the listener.
        // It should unblock and return a connection.
        // Use a timeout to prevent test from hanging.
        let accept_result =
            tokio::time::timeout(Duration::from_secs(1), listener.accept()).await;

        assert!(accept_result.is_ok(), "accept() timed out");
        let (mut conn, remote_addr) = accept_result.unwrap().unwrap();

        // 6. Verify the remote address is the client's address
        assert_eq!(remote_addr, client_addr);
        assert!(!conn.is_closed());

        // 7. Simulate the application writing data back. Per design, this is what
        // triggers the server to send the SYN-ACK and establish the connection.
        conn.write_all(b"server-hello").await.unwrap();

        // 8. Verify that a SYN-ACK was sent back by the server
        // A little delay to allow the server task to process and send.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let sent_packets = server_sent_packets.lock().unwrap();
        assert_eq!(sent_packets.len(), 1, "Expected one packet to be sent");
        let (sent_data, sent_addr) = &sent_packets[0];
        assert_eq!(*sent_addr, client_addr);

        // Decode the frame to check if it's a SYN-ACK
        let response_frame = Frame::decode(sent_data).expect("Failed to decode response frame");
        assert!(
            matches!(response_frame, Frame::SynAck { .. }),
            "Expected SYN-ACK, got {:?}",
            response_frame
        );

        // 9. Cleanup
        run_handle.abort();
    }

    
}
