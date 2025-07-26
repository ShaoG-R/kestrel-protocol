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
        ReliableUdpSocket::<tokio::net::UdpSocket>::bind(server_addr).await.unwrap();
    let server = Arc::new(server_socket);
    let _server_run = server.clone();
    // The .run() method is no longer needed, as the actor is spawned in .bind()
    // tokio::spawn(async move { server_run.run().await });

    // 2. Setup client
    let client_addr = "127.0.0.1:9997".parse().unwrap();
    let (client_socket, _client_listener) =
        ReliableUdpSocket::<tokio::net::UdpSocket>::bind(client_addr).await.unwrap();
    let client = Arc::new(client_socket);
    let _client_run = client.clone();
    // The .run() method is no longer needed, as the actor is spawned in .bind()
    // tokio::spawn(async move { client_run.run().await });

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_cid_handshake_and_data_transfer() {
    init_tracing();

    // 1. Setup server & client
    let server_addr = "127.0.0.1:9996".parse().unwrap();
    let (server_socket, mut server_listener) =
        ReliableUdpSocket::<tokio::net::UdpSocket>::bind(server_addr).await.unwrap();
    let _ = Arc::new(server_socket);
    
    let client_addr = "127.0.0.1:9995".parse().unwrap();
    let (client_socket, _client_listener) =
        ReliableUdpSocket::<tokio::net::UdpSocket>::bind(client_addr).await.unwrap();
    let client = Arc::new(client_socket);

    // Channel to sync the server's accepted stream with the main test task.
    let (server_stream_tx, mut server_stream_rx) = mpsc::channel(1);

    // 2. Concurrently connect and accept
    // This is where the SYN and SYN-ACK packets are exchanged, completing the CID handshake.
    let server_handle = tokio::spawn(async move {
        tracing::info!("[Server] Waiting for connection...");
        let (stream, remote_addr) = server_listener.accept().await.unwrap();
        tracing::info!("[Server] Accepted connection from {}", remote_addr);
        server_stream_tx.send(stream).await.unwrap();
    });

    tracing::info!("[Client] Connecting to server at {}...", server_addr);
    let client_stream = client.connect(server_addr).await.unwrap();
    tracing::info!("[Client] Connection established.");

    let server_stream = server_stream_rx.recv().await.unwrap();
    server_handle.await.unwrap();

    let (mut client_reader, mut client_writer) = tokio::io::split(client_stream);
    let (mut server_reader, mut server_writer) = tokio::io::split(server_stream);

    // 3. Verify data transfer, which relies on correct CID routing
    // Per protocol design, server MUST write first to trigger SYN-ACK.
    
    // Server -> Client
    let server_msg = b"hello from server";
    tracing::info!("[Server] Sending message to client...");
    server_writer.write_all(server_msg).await.expect("Server write should succeed");

    let mut client_buf = vec![0; server_msg.len()];
    tracing::info!("[Client] Reading message from server...");
    client_reader.read_exact(&mut client_buf).await.expect("Client read should succeed");
    assert_eq!(&client_buf, server_msg);
    tracing::info!("[Client] Received correct message from server.");

    // Client -> Server
    let client_msg = b"hello from client";
    tracing::info!("[Client] Sending message to server...");
    client_writer.write_all(client_msg).await.expect("Client write should succeed");

    let mut server_buf = vec![0; client_msg.len()];
    tracing::info!("[Server] Reading message from client...");
    server_reader.read_exact(&mut server_buf).await.expect("Server read should succeed");
    assert_eq!(&server_buf, client_msg);
    tracing::info!("[Server] Received correct message from client.");

    // 4. Graceful shutdown
    client_writer.shutdown().await.expect("Client shutdown should succeed");
    let n = server_reader.read(&mut vec![0; 1]).await.expect("Server read should detect EOF");
    assert_eq!(n, 0, "Server should detect clean shutdown.");
    tracing::info!("[Test] CID handshake and data transfer successful.");
} 

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_concurrent_connections_cid_isolation() {
    init_tracing();
    tracing::info!("[Test] Starting CID isolation test for concurrent connections...");

    // 1. Setup server
    let server_addr = "127.0.0.1:9994".parse().unwrap();
    let (_server_socket, mut server_listener) =
        ReliableUdpSocket::<tokio::net::UdpSocket>::bind(server_addr).await.unwrap();

    // 2. The server task accepts connections and spawns a handler for each.
    let server_handle = tokio::spawn(async move {
        tracing::info!("[Server] Ready to accept 2 connections.");
        let mut handlers = Vec::new();
        for i in 0..2 {
            let (stream, remote_addr) = server_listener.accept().await.unwrap();
            tracing::info!("[Server] Accepted connection #{} from {}", i + 1, remote_addr);
            
            // Spawn an independent task to handle this specific client connection.
            let handler = tokio::spawn(async move {
                let (mut reader, mut writer) = tokio::io::split(stream);

                // A. DEADLOCK BREAKER: Trigger SYN-ACK by writing a probe.
                // This is required by the protocol design.
                writer.write_all(b"probe").await.unwrap();
                tracing::info!("[Server Handler for {}] Sent probe.", remote_addr);

                // B. Read the client's identity and its message.
                let mut id_buf = vec![0; 7]; // e.g., "ident_A"
                reader
                    .read_exact(&mut id_buf)
                    .await
                    .expect("Failed to read client identity");
                tracing::info!(
                    "[Server Handler for {}] Read identity: {}",
                    remote_addr,
                    String::from_utf8_lossy(&id_buf)
                );

                // C. Based on the identity, read the correct message and send the correct ack.
                let ident_str = String::from_utf8(id_buf).unwrap();
                if ident_str == "ident_A" {
                    let mut msg_buf = vec![0; 12]; // "from clientA"
                    reader.read_exact(&mut msg_buf).await.unwrap();
                    assert_eq!(&msg_buf, b"from clientA");
                    writer.write_all(b"server ack A").await.unwrap();
                } else if ident_str == "ident_B" {
                    let mut msg_buf = vec![0; 12]; // "from clientB"
                    reader.read_exact(&mut msg_buf).await.unwrap();
                    assert_eq!(&msg_buf, b"from clientB");
                    writer.write_all(b"server ack B").await.unwrap();
                } else {
                    panic!("Unknown client identity received: {:?}", ident_str);
                }
                
                // D. Gracefully shutdown the connection from the server side.
                writer
                    .shutdown()
                    .await
                    .expect("Server handler failed to shutdown");

                tracing::info!("[Server Handler for {}] Finished.", remote_addr);
            });
            handlers.push(handler);
        }

        // Wait for both client handlers to complete successfully.
        for handler in handlers {
            handler.await.unwrap();
        }
        tracing::info!("[Server] All client handlers finished.");
    });

    // 3. Setup and connect two clients concurrently
    let client_a_addr = "127.0.0.1:9993".parse().unwrap();
    let (client_a_socket, _) =
        ReliableUdpSocket::<tokio::net::UdpSocket>::bind(client_a_addr)
            .await
            .unwrap();
    let client_b_addr = "127.0.0.1:9992".parse().unwrap();
    let (client_b_socket, _) =
        ReliableUdpSocket::<tokio::net::UdpSocket>::bind(client_b_addr)
            .await
            .unwrap();

    let clients = [client_a_socket, client_b_socket];
    let client_ids = ['A', 'B'];

    let mut client_handles = Vec::new();
    for i in 0..2 {
        let client_socket = clients[i].clone();
        let id = client_ids[i];
        let client_handle = tokio::spawn(async move {
            tracing::info!("[Client {}] Connecting to server...", id);
            let stream = client_socket.connect(server_addr).await.unwrap();
            let (mut reader, mut writer) = tokio::io::split(stream);
            
            // A. Read the server's initial "probe" required for handshake.
            let mut probe_buf = vec![0; 5];
            reader.read_exact(&mut probe_buf).await.unwrap();
            assert_eq!(&probe_buf, b"probe");
            tracing::info!("[Client {}] Probe received.", id);

            // B. Send identity and unique message.
            let ident = format!("ident_{}", id);
            let msg = format!("from client{}", id);
            writer.write_all(ident.as_bytes()).await.unwrap();
            writer.write_all(msg.as_bytes()).await.unwrap();
            tracing::info!("[Client {}] Sent identity and message.", id);

            // C. Wait for the server's final, unique acknowledgment.
            let expected_ack = format!("server ack {}", id);
            let mut ack_buf = vec![0; expected_ack.len()];
            reader.read_exact(&mut ack_buf).await.unwrap();
            assert_eq!(ack_buf, expected_ack.as_bytes());
            tracing::info!("[Client {}] Received correct final ack.", id);

            // D. Gracefully shutdown the connection from the client side.
            // This is crucial for the test to terminate cleanly.
            writer.shutdown().await.unwrap();
            tracing::info!("[Client {}] Shutdown complete.", id);
        });
        client_handles.push(client_handle);
    }

    // Wait for all top-level tasks to complete
    server_handle.await.unwrap();
    for client_handle in client_handles {
        client_handle.await.unwrap();
    }
    
    tracing::info!("[Test] CID isolation test passed.");
} 