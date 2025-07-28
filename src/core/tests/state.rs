//! Tests for connection state management, especially in concurrent or racy conditions.

use crate::socket::ReliableUdpSocket;
use std::{net::SocketAddr, time::Duration};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::UdpSocket};
use tracing::info;

use crate::{core::test_utils::init_tracing, config::Config};


/// Test that a server can handle a new connection from an address that was
/// just used by a different connection. This validates that the `addr_to_cid`
/// temporary mapping is correctly cleaned up.
///
/// 测试服务器是否可以处理来自刚刚被不同连接使用过的地址的新连接。
/// 这验证了 `addr_to_cid` 临时映射是否被正确清理。
#[tokio::test(start_paused = true)]
async fn test_address_reuse_after_connection_close() {
    init_tracing();
    // 1. Setup a server listener by first grabbing a known available port.
    let temp_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let server_addr: SocketAddr = temp_socket.local_addr().unwrap();
    drop(temp_socket); // Release the address so our socket can bind to it.

    let (_, mut listener) =
        ReliableUdpSocket::<UdpSocket>::bind(server_addr)
            .await
            .unwrap();

    let server_task = tokio::spawn(async move {
        // Accept the first connection.
        let (mut server_stream_a, _) = listener.accept().await.unwrap();
        info!("Accepted first connection.");
        // It immediately closes, so reading should yield 0 bytes (EOF).
        let mut buf = vec![0; 10];
        assert_eq!(
            server_stream_a.read(&mut buf).await.unwrap(),
            0,
            "Server stream A should be closed"
        );
        info!("First connection terminated on server.");

        // Accept the second connection.
        let (mut server_stream_b, _) = listener.accept().await.unwrap();
        info!("Accepted second connection.");
        
        // Per protocol design, the server must write something to trigger the
        // SYN-ACK and complete the handshake. An empty write is sufficient.
        server_stream_b.write_all(b"1").await.unwrap();
        server_stream_b.flush().await.unwrap();
        info!("Sent empty write to trigger SYN-ACK for connection B.");

        let mut buf = vec![0; 20];
        let len = server_stream_b.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..len], b"hello from client B");
        info!("Second connection on server verified.");
    });

    // 2. Setup a client socket.
    let (client, _) =
        ReliableUdpSocket::<UdpSocket>::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();

    // 3. First connection attempt: connect and immediately close.
    info!("Connecting for the first time...");
    let mut client_stream_a = client.connect(server_addr).await.unwrap();
    info!("First connection established.");
    client_stream_a.shutdown().await.unwrap();
    info!("First connection shut down from client side.");

    // Wait for a short time, much less than the drain timeout, to allow the
    // server to process the shutdown and move the first connection's CID to the
    // draining state.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // 4. Second connection attempt from the same client socket.
    // This will use the same local address as the first connection. This must
    // succeed because the connection is identified by CID, not address.
    info!("Connecting for the second time from the same local socket...");
    let mut client_stream_b = client.connect(server_addr).await.unwrap();
    info!("Second connection established.");
    client_stream_b.write_all(b"hello from client B").await.unwrap();
    client_stream_b.flush().await.unwrap();
    info!("Data sent on second connection.");

    // 5. Wait for server to finish its checks.
    
    server_task.await.unwrap();
} 

/// Test that a CID is eventually fully removed from the draining state,
/// allowing it to be potentially reused later (though unlikely).
/// This test verifies that the periodic cleanup task in the SocketActor works.
///
/// 测试CID最终会从冷却状态中完全移除，从而可能在以后被重用（尽管不太可能）。
/// 此测试验证了SocketActor中的周期性清理任务是否正常工作。
#[tokio::test(start_paused = true)]
async fn test_address_reuse_after_cid_drained() {
    init_tracing();
    let config = Config::default();

    // 1. Setup server
    let temp_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let server_addr: SocketAddr = temp_socket.local_addr().unwrap();
    drop(temp_socket);

    let (_, mut listener) =
        ReliableUdpSocket::<UdpSocket>::bind(server_addr)
            .await
            .unwrap();

    let server_task = tokio::spawn(async move {
        // Accept and close connection A.
        // The client shuts down before the handshake fully completes on the server,
        // so the server-side endpoint will just time out. We accept it and
        // immediately drop it to simulate the user moving on.
        info!("[Server] Waiting for conn A");
        let (stream_a, _) = listener.accept().await.unwrap();
        drop(stream_a);
        info!("[Server] Conn A accepted and dropped.");

        // Accept connection B
        info!("[Server] Waiting for conn B");
        let (mut stream_b, _) = listener.accept().await.unwrap();
        
        // Per protocol design, the server must write something to trigger the
        // SYN-ACK and complete the handshake. We send our "pong" to do this.
        stream_b.write_all(b"pong").await.unwrap();
        
        // Now that the handshake is complete, we can read the client's "ping".
        let mut buf = vec![0; 10];
        let len = stream_b.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..len], b"ping");
        info!("[Server] Conn B verified");
    });

    // 2. Setup client
    let (client, _) =
        ReliableUdpSocket::<UdpSocket>::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();

    // 3. Connection A: connect and immediately close.
    info!("[Client] Connecting A...");
    let mut client_stream_a = client.connect(server_addr).await.unwrap();
    client_stream_a.shutdown().await.unwrap();
    info!("[Client] Conn A shut down.");

    // 4. Wait for a time LONGER than the drain timeout + cleanup interval
    // to ensure the CID has been fully purged from the server actor.
    info!("[Client] Waiting for CID to be drained on server...");
    tokio::time::sleep(config.connection.drain_timeout + config.connection.draining_cleanup_interval).await;
    
    // 5. Connection B: Connect again from the same socket.
    info!("[Client] Connecting B...");
    let mut client_stream_b = client.connect(server_addr).await.unwrap();
    client_stream_b.write_all(b"ping").await.unwrap();
    let mut buf = vec![0; 10];
    let len = client_stream_b.read(&mut buf).await.unwrap();
    assert_eq!(&buf[..len], b"pong");
    info!("[Client] Conn B verified.");

    server_task.await.unwrap();
} 