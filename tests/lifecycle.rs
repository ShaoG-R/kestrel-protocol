//! Minimal test to diagnose potential deadlocks in tokio::net::UdpSocket::bind.

use std::sync::Once;
use std::time::Duration;
use kestrel_protocol::socket::ReliableUdpSocket;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Helper to initialize tracing for tests.
fn init_tracing() {
    static TRACING_INIT: Once = Once::new();
    TRACING_INIT.call_once(|| {
        tracing_subscriber::fmt()
            .with_env_filter("trace")
            .with_test_writer()
            .init();
    });
}

#[tokio::test]
async fn test_udp_socket_bind() {
    init_tracing();
    tracing::info!("--- Minimal Tokio UDP Bind Test ---");
    tracing::info!("Attempting to bind UDP socket...");

    let addr = "127.0.0.1:0"; // Port 0 asks the OS for any available port.
    
    // 使用超时来防止无限等待
    let bind_result = tokio::time::timeout(
        Duration::from_secs(5), 
        tokio::net::UdpSocket::bind(addr)
    ).await;
    
    match bind_result {
        Ok(Ok(socket)) => {
            let local_addr = socket.local_addr().unwrap();
            tracing::info!("Successfully bound UDP socket to address: {:?}", local_addr);
        }
        Ok(Err(e)) => {
            tracing::error!("Failed to bind UDP socket: {}", e);
            panic!("Failed to bind UDP socket: {}", e);
        }
        Err(_) => {
            tracing::error!("UDP socket bind operation timed out after 5 seconds");
            panic!("UDP socket bind operation timed out");
        }
    }

    tracing::info!("--- Test Finished ---");
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

    // 4. Server sends, client receives. This triggers the SYN-ACK from the server.
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


    // 5. Client sends, server receives
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

    // 6. Client closes gracefully
    client_writer
        .shutdown()
        .await
        .expect("Client shutdown should succeed");

    // 7. Server should detect end-of-file
    let mut final_buf = vec![0; 10];
    let n = server_reader
        .read(&mut final_buf)
        .await
        .expect("Server should read EOF");
    assert_eq!(n, 0, "Server should detect connection close (read 0 bytes)");
} 