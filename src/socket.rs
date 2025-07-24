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

use tracing::{debug, error, info, warn};

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
pub struct ReliableUdpSocket {
    socket: Arc<UdpSocket>,
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

impl ReliableUdpSocket {
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
        self.connect_with_config(remote_addr, Config::default())
            .await
    }

    /// Establishes a new reliable connection to the given remote address with custom configuration.
    ///
    /// 使用自定义配置建立一个到指定远程地址的新的可靠连接。
    pub async fn connect_with_config(
        &self,
        remote_addr: SocketAddr,
        config: Config,
    ) -> Result<Stream> {
        let local_cid = rand::random();

        let (tx_to_endpoint, rx_from_socket) = mpsc::channel(128);

        // Create a new Endpoint and get the stream handles
        let (mut endpoint, tx_to_stream_handle, rx_from_stream_handle) = Endpoint::new_client(
            config,
            remote_addr,
            local_cid,
            rx_from_socket,
            self.send_tx.clone(),
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



#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::command::Command;
    use bytes::Bytes;
    use std::sync::Once;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Helper to initialize tracing for tests.
    fn init_tracing() {
        static TRACING_INIT: Once = Once::new();
        TRACING_INIT.call_once(|| {
            tracing_subscriber::fmt()
                .with_env_filter("protocol=trace")
                .with_test_writer()
                .init();
        });
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_bind_and_log() {
        init_tracing();
        info!("--- Starting test_bind_and_log ---");
        let addr = "127.0.0.1:0".parse().unwrap(); // Use port 0 to get a random available port
        let bind_result = ReliableUdpSocket::bind(addr).await;
        assert!(bind_result.is_ok());
        info!("--- Finished test_bind_and_log ---");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_server_accept() {
        // 1. Setup a listener socket
        init_tracing();
        let listener_addr = "127.0.0.1:9999".parse().unwrap();
        let (socket, mut listener) = ReliableUdpSocket::bind(listener_addr).await.unwrap();
        let socket_arc = Arc::new(socket);

        // 2. Spawn the listener's run loop
        let socket_run = socket_arc.clone();
        let run_handle = tokio::spawn(async move {
            socket_run.run().await;
        });

        // 3. Setup a "client" socket to send a SYN
        let client_addr = "127.0.0.1:1111".parse().unwrap();
        let client_socket = UdpSocket::bind(client_addr).await.unwrap();

        // 4. Create and send a SYN packet
        let syn_header = crate::packet::header::LongHeader {
            command: Command::Syn,
            protocol_version: 1, // Use a matching version for the test
            destination_cid: 0, // Destination is unknown initially
            source_cid: 1234,   // Client's chosen CID
        };
        let syn_frame = Frame::Syn {
            header: syn_header,
            payload: Bytes::new(),
        };
        let mut send_buf = Vec::new();
        syn_frame.encode(&mut send_buf);
        client_socket
            .send_to(&send_buf, listener_addr)
            .await
            .unwrap();

        // 5. Call accept() on the listener.
        // It should unblock and return a connection.
        // Use a timeout to prevent test from hanging.
        let accept_result =
            tokio::time::timeout(Duration::from_secs(1), listener.accept()).await;

        assert!(accept_result.is_ok(), "accept() timed out");
        let (conn, remote_addr) = accept_result.unwrap().unwrap();

        // 6. Verify the remote address is the client's address
        assert_eq!(remote_addr, client_addr);
        // A simple check to see if the connection handle is usable
        assert!(!conn.is_closed());

        // 7. Cleanup
        run_handle.abort();
    }

    
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_full_connection_lifecycle() {
        init_tracing();

        // 1. Setup server
        let server_addr = "127.0.0.1:9998".parse().unwrap();
        let (server_socket, mut server_listener) =
            ReliableUdpSocket::bind(server_addr).await.unwrap();
        let server = Arc::new(server_socket);
        let server_run = server.clone();
        tokio::spawn(async move { server_run.run().await });

        // 2. Setup client
        let client_addr = "127.0.0.1:9997".parse().unwrap();
        let (client_socket, _client_listener) =
            ReliableUdpSocket::bind(client_addr).await.unwrap();
        let client = Arc::new(client_socket);
        let client_run = client.clone();
        tokio::spawn(async move { client_run.run().await });

        // Channel to sync the server's accepted stream with the main test task.
        let (server_stream_tx, mut server_stream_rx) = mpsc::channel(1);

        // 3. Concurrently connect and accept
        let server_handle = tokio::spawn(async move {
            let (stream, _addr) = server_listener.accept().await.unwrap();
            server_stream_tx.send(stream).await.unwrap();
        });

        let client_stream = client.connect(server_addr).await.unwrap();
        let server_stream = server_stream_rx.recv().await.unwrap();
        server_handle.await.unwrap();

        let (mut client_reader, mut client_writer) = tokio::io::split(client_stream);
        let (mut server_reader, mut server_writer) = tokio::io::split(server_stream);

        // 4. Client sends, server receives
        let client_msg = b"message from client";
        client_writer
            .write_all(client_msg)
            .await
            .expect("Client write should succeed");

        let mut server_buf = vec![0; client_msg.len()];
        server_reader
            .read_exact(&mut server_buf)
            .await
            .expect("Server read should succeed");
        assert_eq!(&server_buf, client_msg);

        // 5. Server sends, client receives
        let server_msg = b"response from server";
        server_writer
            .write_all(server_msg)
            .await
            .expect("Server write should succeed");

        let mut client_buf = vec![0; server_msg.len()];
        client_reader
            .read_exact(&mut client_buf)
            .await
            .expect("Client read should succeed");
        assert_eq!(&client_buf, server_msg);

        // 6. Client closes
        drop(client_writer);
        drop(client_reader);

        // 7. Server should detect end-of-file
        let mut final_buf = vec![0; 10];
        let n = server_reader
            .read(&mut final_buf)
            .await
            .expect("Server should read EOF");
        assert_eq!(n, 0, "Server should detect connection close (read 0 bytes)");
    }

}
