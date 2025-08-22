//! Minimal test to diagnose potential deadlocks in tokio::net::UdpSocket::bind.

pub mod common;

use common::harness::TestHarness;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tracing::Instrument;

#[tokio::test]
async fn test_udp_socket_bind() {
    tracing::info!("--- Minimal Tokio UDP Bind Test ---");
    tracing::info!("Attempting to bind UDP socket...");

    let addr = "127.0.0.1:0"; // Port 0 asks the OS for any available port.

    // 使用超时来防止无限等待
    let bind_result =
        tokio::time::timeout(Duration::from_secs(5), tokio::net::UdpSocket::bind(addr)).await;

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
    // 1. Setup server using the harness
    let TestHarness {
        server_addr,
        mut server_listener,
        ..
    } = TestHarness::new().await;

    // 2. Setup client using the harness
    let client = TestHarness::create_client().await;

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
    // 1. Setup using the harness
    let TestHarness {
        server_addr,
        mut server_listener,
        ..
    } = TestHarness::new().await;
    let client = TestHarness::create_client().await;

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
    server_writer
        .write_all(server_msg)
        .await
        .expect("Server write should succeed");

    let mut client_buf = vec![0; server_msg.len()];
    tracing::info!("[Client] Reading message from server...");
    client_reader
        .read_exact(&mut client_buf)
        .await
        .expect("Client read should succeed");
    assert_eq!(&client_buf, server_msg);
    tracing::info!("[Client] Received correct message from server.");

    // Client -> Server
    let client_msg = b"hello from client";
    tracing::info!("[Client] Sending message to server...");
    client_writer
        .write_all(client_msg)
        .await
        .expect("Client write should succeed");

    let mut server_buf = vec![0; client_msg.len()];
    tracing::info!("[Server] Reading message from client...");
    server_reader
        .read_exact(&mut server_buf)
        .await
        .expect("Server read should succeed");
    assert_eq!(&server_buf, client_msg);
    tracing::info!("[Server] Received correct message from client.");

    // 4. Graceful shutdown
    client_writer
        .shutdown()
        .await
        .expect("Client shutdown should succeed");
    let n = server_reader
        .read(&mut vec![0; 1])
        .await
        .expect("Server read should detect EOF");
    assert_eq!(n, 0, "Server should detect clean shutdown.");
    tracing::info!("[Test] CID handshake and data transfer successful.");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_concurrent_connections_cid_isolation() {
    tracing::info!("[Test] Starting CID isolation test for concurrent connections...");

    // 1. Setup server using the harness
    let TestHarness {
        server_addr,
        mut server_listener,
        ..
    } = TestHarness::new().await;

    // 2. The server task accepts connections and spawns a handler for each.
    let server_handle = tokio::spawn(async move {
        tracing::info!("[Server] Ready to accept 2 connections.");
        let mut handlers = Vec::new();
        for i in 0..2 {
            let (stream, remote_addr) = server_listener.accept().await.unwrap();
            tracing::info!(
                "[Server] Accepted connection #{} from {}",
                i + 1,
                remote_addr
            );

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

                // C. Based on the identity, read the rest of the message until the client
                // sends a FIN (indicated by read_to_end finishing).
                let ident_str = String::from_utf8(id_buf).unwrap();
                let (expected_msg, response_msg) = if ident_str == "ident_A" {
                    (b"from clientA".to_vec(), b"server ack A")
                } else {
                    (b"from clientB".to_vec(), b"server ack B")
                };

                let mut msg_buf = Vec::new();
                reader
                    .read_to_end(&mut msg_buf)
                    .await
                    .expect("Failed to read client message");
                assert_eq!(msg_buf, expected_msg);

                // D. Now that we've received everything, send the final ack.
                writer.write_all(response_msg).await.unwrap();

                // E. Gracefully shutdown the connection from the server side.
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
    let client_a = TestHarness::create_client().await;
    let client_b = TestHarness::create_client().await;

    let clients = [client_a, client_b];
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

            // C. Shutdown our writer to send a FIN signal.
            // This tells the server we are done sending data.
            writer.shutdown().await.unwrap();
            tracing::info!("[Client {}] Writer shut down.", id);

            // D. Wait for the server's final, unique acknowledgment.
            let expected_ack = format!("server ack {}", id);
            let mut ack_buf = Vec::new();
            reader
                .read_to_end(&mut ack_buf)
                .await
                .expect("Failed to read server ack");
            assert_eq!(ack_buf, expected_ack.as_bytes());
            tracing::info!("[Client {}] Received correct final ack.", id);
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

#[tokio::test(flavor = "multi_thread", worker_threads = 32)]
async fn test_high_concurrency_1rtt_connections() {
    const NUM_CLIENTS: usize = 1000;
    tracing::info!(
        "[Test] Starting high concurrency 1-RTT test with {} clients...",
        NUM_CLIENTS
    );

    // 1. Setup server using the harness
    let TestHarness {
        server_addr,
        mut server_listener,
        ..
    } = TestHarness::new().await;

    // 2. The server task accepts connections and spawns a handler for each.
    let server_handle = tokio::spawn(async move {
        tracing::info!("[Server] Ready to accept {} connections.", NUM_CLIENTS);
        let mut handlers = Vec::new();
        for i in 0..NUM_CLIENTS {
            let (stream, remote_addr) = server_listener.accept().await.unwrap();
            tracing::info!(
                "[Server] Accepted connection #{} from {}",
                i + 1,
                remote_addr
            );

            // Spawn an independent task to handle this specific client connection.
            let handler = tokio::spawn(
                async move {
                    let (mut reader, mut writer) = tokio::io::split(stream);

                    // A. Trigger SYN-ACK by writing a fixed-size welcome message.
                    // This is the core of the 1-RTT handshake from the server side.
                    const WELCOME_MSG: &[u8] = b"WELCOME";
                    writer
                        .write_all(WELCOME_MSG)
                        .await
                        .expect("Server handler failed to write welcome");
                    tracing::info!("Sent WELCOME.");

                    // B. Read the client's full message. This read will complete when
                    // the client shuts down its writing side, sending a FIN.
                    let mut client_msg_buf = Vec::new();
                    reader
                        .read_to_end(&mut client_msg_buf)
                        .await
                        .expect("Failed to read client message");
                    let client_msg_str = String::from_utf8_lossy(&client_msg_buf);
                    assert!(client_msg_str.starts_with("Message from client"));
                    tracing::info!("Read message: '{}'", client_msg_str);

                    // C. Send a final, fixed-size response.
                    const GOODBYE_MSG: &[u8] = b"GOODBYE";
                    writer
                        .write_all(GOODBYE_MSG)
                        .await
                        .expect("Server handler failed to write goodbye");

                    // D. Gracefully shutdown the connection from the server side.
                    writer
                        .shutdown()
                        .await
                        .expect("Server handler failed to shutdown");
                    tracing::info!("Finished.");
                }
                .instrument(tracing::info_span!(
                    "handler",
                    side = "server",
                    id = i,
                    addr = %remote_addr
                )),
            );
            handlers.push(handler);
        }

        // Wait for all client handlers to complete successfully.
        for handler in handlers {
            handler.await.unwrap();
        }
        tracing::info!("[Server] All {} client handlers finished.", NUM_CLIENTS);
    });

    // 3. Setup and connect N clients concurrently
    let mut client_handles = Vec::new();
    for i in 0..NUM_CLIENTS {
        let client_socket = TestHarness::create_client().await;
        let client_handle = tokio::spawn(
            async move {
                tracing::info!("Connecting to server...");
                let stream = client_socket.connect(server_addr).await.unwrap();
                let (mut reader, mut writer) = tokio::io::split(stream);

                // A. Read the server's fixed-size welcome message. This confirms
                // the 1-RTT handshake is complete.
                const WELCOME_MSG: &[u8] = b"WELCOME";
                let mut welcome_buf = vec![0; WELCOME_MSG.len()];
                reader
                    .read_exact(&mut welcome_buf)
                    .await
                    .expect("Client failed to read welcome message");
                assert_eq!(&welcome_buf, WELCOME_MSG);
                tracing::info!("Received WELCOME.");

                // B. Send a unique message.
                let msg = format!("Message from client {}", i);
                writer
                    .write_all(msg.as_bytes())
                    .await
                    .expect("Client failed to write message");
                tracing::info!("Sent message.");

                // C. Shutdown the writer to send a FIN. This signals to the server
                // that we are done sending data.
                writer
                    .shutdown()
                    .await
                    .expect("Client failed to shutdown writer");
                tracing::info!("Writer shut down.");

                // D. Wait for the server's final acknowledgment. This read will complete
                // when the server shuts down its writer.
                const GOODBYE_MSG: &[u8] = b"GOODBYE";
                let mut final_buf = Vec::new();
                reader
                    .read_to_end(&mut final_buf)
                    .await
                    .expect("Failed to read server's final response");
                assert_eq!(final_buf, GOODBYE_MSG);
                tracing::info!("Received GOODBYE and connection closed.");
            }
            .instrument(tracing::info_span!("handler", side = "client", id = i)),
        );
        client_handles.push(client_handle);
    }

    // Wait for all top-level tasks to complete
    for handle in client_handles {
        handle.await.unwrap();
    }
    server_handle.await.unwrap();

    tracing::info!(
        "[Test] High concurrency 1-RTT test with {} clients passed.",
        NUM_CLIENTS
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 32)]
async fn test_high_concurrency_large_transfer() {
    const NUM_CLIENTS: usize = 1000; // Reduced from 1000 to manage test duration with larger payloads
    const TRANSFER_SIZE: usize = 1024; // 1KB
    tracing::info!(
        "[Test] Starting high concurrency large transfer test with {} clients, {}B each way...",
        NUM_CLIENTS,
        TRANSFER_SIZE
    );

    // 1. Setup server using the harness
    let TestHarness {
        server_addr,
        mut server_listener,
        ..
    } = TestHarness::new().await;

    // 2. The server task accepts connections and spawns a handler for each.
    let server_handle = tokio::spawn(async move {
        tracing::info!("[Server] Ready to accept {} connections.", NUM_CLIENTS);
        let mut handlers = Vec::new();
        for i in 0..NUM_CLIENTS {
            let (stream, remote_addr) = server_listener.accept().await.unwrap();
            tracing::info!(
                "[Server] Accepted connection #{} from {}",
                i + 1,
                remote_addr
            );

            // Spawn an independent task to handle this specific client connection.
            let handler = tokio::spawn(
                async move {
                    let (mut reader, mut writer) = tokio::io::split(stream);

                    // A. Trigger SYN-ACK by writing a large payload.
                    let server_welcome_payload = vec![b'S'; TRANSFER_SIZE];
                    writer
                        .write_all(&server_welcome_payload)
                        .await
                        .expect("Server handler failed to write welcome");
                    tracing::info!("Sent welcome payload.");

                    // B. Read the client's full message.
                    let mut client_msg_buf = Vec::new();
                    reader
                        .read_to_end(&mut client_msg_buf)
                        .await
                        .expect("Failed to read client message");
                    assert_eq!(client_msg_buf.len(), TRANSFER_SIZE);
                    
                    // Verify data integrity: all bytes should be the same
                    // We don't care which client this came from, just that the data is consistent
                    if !client_msg_buf.is_empty() {
                        let pattern = client_msg_buf[0];
                        let mut corrupted_bytes = 0;
                        
                        for &byte in &client_msg_buf {
                            if byte != pattern {
                                corrupted_bytes += 1;
                            }
                        }
                        
                        if corrupted_bytes > 0 {
                            panic!(
                                "Data corruption detected in server handler {}: expected all bytes to be {}, but {} out of {} bytes were different",
                                i, pattern, corrupted_bytes, client_msg_buf.len()
                            );
                        }
                        
                        tracing::info!(
                            "Server handler {} received valid message with pattern {}, {} bytes",
                            i, pattern, client_msg_buf.len()
                        );
                    } else {
                        panic!("Server handler {} received empty message", i);
                    }
                    tracing::info!("Read {}B message.", client_msg_buf.len());

                    // C. Send a final, large response.
                    let server_goodbye_payload = vec![b'G'; TRANSFER_SIZE];
                    writer
                        .write_all(&server_goodbye_payload)
                        .await
                        .expect("Server handler failed to write goodbye");
                    // D. Gracefully shutdown the connection from the server side.
                    writer
                        .shutdown()
                        .await
                        .expect("Server handler failed to shutdown");
                    tracing::info!("Finished.");
                }
                .instrument(tracing::info_span!(
                    "handler",
                    side = "server",
                    id = i,
                    addr = %remote_addr
                )),
            );
            handlers.push(handler);
        }

        // Wait for all client handlers to complete successfully.
        for handler in handlers {
            handler.await.unwrap();
        }
        tracing::info!("[Server] All {} client handlers finished.", NUM_CLIENTS);
    });

    // 3. Setup and connect N clients concurrently
    let mut client_handles = Vec::new();
    for i in 0..NUM_CLIENTS {
        let client_socket = TestHarness::create_client().await;
        let client_handle = tokio::spawn(
            async move {
                tracing::info!("Connecting to server...");
                let stream = client_socket.connect(server_addr).await.unwrap();
                let (mut reader, mut writer) = tokio::io::split(stream);

                // A. Read the server's large welcome message.
                let server_welcome_payload = vec![b'S'; TRANSFER_SIZE];
                let mut welcome_buf = vec![0; TRANSFER_SIZE];
                reader
                    .read_exact(&mut welcome_buf)
                    .await
                    .expect("Client failed to read welcome message");
                assert_eq!(welcome_buf, server_welcome_payload);
                tracing::info!("Received welcome payload.");

                // B. Send a unique, large message.
                // Each client sends a different pattern to test data integrity
                let client_pattern = (i % 256) as u8;
                let client_payload = vec![client_pattern; TRANSFER_SIZE];

                writer
                    .write_all(&client_payload)
                    .await
                    .expect("Client failed to write message");
                tracing::info!("Sent message with pattern {}.", client_pattern);

                // C. Add a small delay before shutdown to ensure data is transmitted
                tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

                // Shutdown the writer to send a FIN.
                writer
                    .shutdown()
                    .await
                    .expect("Client failed to shutdown writer");
                tracing::info!("Writer shut down.");

                // D. Wait for the server's final large acknowledgment.
                let server_goodbye_payload = vec![b'G'; TRANSFER_SIZE];
                let mut final_buf = Vec::new();
                reader
                    .read_to_end(&mut final_buf)
                    .await
                    .expect("Failed to read server's final response");
                assert_eq!(final_buf, server_goodbye_payload);
                tracing::info!("Received goodbye and connection closed.");
            }
            .instrument(tracing::info_span!("handler", side = "client", id = i)),
        );
        client_handles.push(client_handle);
    }

    // Wait for all top-level tasks to complete
    for handle in client_handles {
        handle.await.unwrap();
    }
    server_handle.await.unwrap();

    tracing::info!(
        "[Test] High concurrency large transfer test with {} clients passed.",
        NUM_CLIENTS
    );
}
