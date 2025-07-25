//! Tests for connection state management, especially in concurrent or racy conditions.

use crate::socket::ReliableUdpSocket;
use std::{net::SocketAddr, time::Duration};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::UdpSocket};
use tracing::info;

/// Test that a server can handle a new connection from an address that was
/// just used by a different connection. This validates that the `addr_to_cid`
/// temporary mapping is correctly cleaned up.
///
/// 测试服务器是否可以处理来自刚刚被不同连接使用过的地址的新连接。
/// 这验证了 `addr_to_cid` 临时映射是否被正确清理。
#[tokio::test(start_paused = true)]
async fn test_address_reuse_after_connection_close() {
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

    // Give the actor tasks time to process the connection removal.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // 4. Second connection attempt from the same client socket.
    // This will use the same local address as the first connection.
    info!("Connecting for the second time from the same local socket...");
    let mut client_stream_b = client.connect(server_addr).await.unwrap();
    info!("Second connection established.");
    client_stream_b.write_all(b"hello from client B").await.unwrap();
    client_stream_b.flush().await.unwrap();
    info!("Data sent on second connection.");

    // 5. Wait for server to finish its checks.
    
    server_task.await.unwrap();
} 