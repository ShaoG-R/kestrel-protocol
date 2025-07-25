//! Integration-style tests for the `Endpoint` worker, using a simulated network.

use super::endpoint::{Endpoint, StreamCommand};
use super::test_utils::*;
use crate::config::Config;
use crate::packet::frame::Frame;
use crate::socket::SenderTaskCommand;
use bytes::Bytes;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

// --- Multi-client Test Infrastructure ---

/// A central hub to simulate a server's UDP socket that can handle multiple clients.
/// It creates interconnected mock sockets for a multi-client test scenario.
struct TestNetHub {
    server_addr: SocketAddr,
    /// A single queue that simulates the server's single listening port.
    /// All clients send packets to this queue.
    server_ingress_queue: Arc<Mutex<VecDeque<(Bytes, SocketAddr)>>>,
    /// Maps a client's address to its dedicated receive queue for server responses.
    client_egress_queues: Arc<Mutex<HashMap<SocketAddr, Arc<Mutex<VecDeque<(Bytes, SocketAddr)>>>>>>,
}

impl TestNetHub {
    fn new(server_addr: SocketAddr) -> Self {
        Self {
            server_addr,
            server_ingress_queue: Arc::new(Mutex::new(VecDeque::new())),
            client_egress_queues: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Creates a socket for a client.
    fn new_client_socket(&self, client_addr: SocketAddr) -> MockUdpSocket {
        let client_recv_queue = Arc::new(Mutex::new(VecDeque::new()));
        self.client_egress_queues
            .lock()
            .unwrap()
            .insert(client_addr, client_recv_queue.clone());

        MockUdpSocket {
            local_addr: client_addr,
            recv_queue: client_recv_queue,
            peer_recv_queue: self.server_ingress_queue.clone(), // Sends to the server
            packet_tx_filter: Arc::new(|_| true),
            sent_packets_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Creates a socket for a server-side endpoint.
    fn new_server_socket_for_client(&self, client_addr: SocketAddr) -> MockUdpSocket {
        let client_queue = self
            .client_egress_queues
            .lock()
            .unwrap()
            .get(&client_addr)
            .cloned()
            .unwrap();

        MockUdpSocket {
            local_addr: self.server_addr,
            // Server endpoints don't receive directly from a shared queue in this model.
            // The demultiplexer task below handles that.
            recv_queue: Arc::new(Mutex::new(VecDeque::new())),
            peer_recv_queue: client_queue, // Sends to a specific client
            packet_tx_filter: Arc::new(|_| true),
            sent_packets_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Spawns the demultiplexer task.
    /// This task reads from the central server ingress queue and forwards frames
    /// to the correct server-side endpoint based on the source address.
    fn spawn_demultiplexer(
        &self,
        mut endpoint_senders: HashMap<SocketAddr, mpsc::Sender<(Frame, SocketAddr)>>,
    ) {
        let server_ingress_queue = self.server_ingress_queue.clone();
        tokio::spawn(async move {
            loop {
                // Lock, pop, and immediately unlock to avoid holding the guard across `.await`.
                let packet = server_ingress_queue.lock().unwrap().pop_front();

                if let Some((data, src_addr)) = packet {
                    // --- DEBUG LOG: Print raw bytes received by the server-side hub ---
                    let mut hasher = Sha256::new();
                    hasher.update(&data);
                    let hash = hasher.finalize();
                    println!(
                        "[SERVER DEMUX] RECV FROM {} -> len: {}, hash: {:x}",
                        src_addr,
                        data.len(),
                        hash
                    );
                    // --- END DEBUG LOG ---

                    let mut cursor = &data[..];
                    while !cursor.is_empty() {
                        if let Some(frame) = Frame::decode(&mut cursor) {
                            if let Some(sender) = endpoint_senders.get_mut(&src_addr) {
                                if sender.send((frame, src_addr)).await.is_err() {
                                    // Endpoint is gone, remove it to prevent further sends.
                                    endpoint_senders.remove(&src_addr);
                                }
                            }
                        } else {
                            // Could not decode further, stop processing this datagram.
                            break;
                        }
                    }
                } else {
                    // If no packet, yield to avoid busy-waiting.
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            }
        });
    }
}

// --- Tests ---

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_core_multiple_clients_concurrently() {
    const NUM_CLIENTS: usize = 50;
    let server_addr: SocketAddr = "127.0.0.1:8000".parse().unwrap();

    let hub = Arc::new(TestNetHub::new(server_addr));
    let mut server_endpoint_senders = HashMap::new();
    let mut server_handles = Vec::new();
    let mut client_handles = Vec::new();

    // --- Setup all clients and server endpoints within a single, unified loop ---
    for i in 0..NUM_CLIENTS {
        let client_addr: SocketAddr = format!("127.0.0.1:{}", 9000 + i).parse().unwrap();
        let client_cid = (i + 1) as u32;
        let server_cid = (100 + i + 1) as u32;

        // Sockets must be created before spawning tasks that use them.
        let client_socket = hub.new_client_socket(client_addr);
        let server_socket = hub.new_server_socket_for_client(client_addr);

        // --- Setup Server-side Endpoint ---
        let (tx_to_endpoint_network, rx_from_demux) = mpsc::channel(128);
        server_endpoint_senders.insert(client_addr, tx_to_endpoint_network.clone());
        let (sender_task_tx, server_sender_task_rx) = mpsc::channel(128);
        let (server_command_tx, _) = mpsc::channel(128);
        let (server_endpoint, tx_to_server_user, rx_from_server_user) = Endpoint::new_server(
            Config::default(),
            client_addr,
            server_cid,
            client_cid,
            rx_from_demux,
            sender_task_tx.clone(),
            server_command_tx,
        );
        server_handles.push((rx_from_server_user, tx_to_server_user.clone()));

        spawn_endpoint(
            server_endpoint,
            server_socket,
            server_sender_task_rx,
            tx_to_endpoint_network,
        );

        // --- Setup and Spawn Client Task ---
        let client_handle = tokio::spawn(async move {
            let (tx_to_endpoint_network, rx_from_socket) = mpsc::channel(128);
            let (sender_task_tx, client_sender_task_rx) = mpsc::channel(128);
            let (client_command_tx, _) = mpsc::channel(128);

            let (mut endpoint, tx_to_client_user, mut rx_from_client_user) = Endpoint::new_client(
                Config::default(),
                server_addr,
                client_cid,
                rx_from_socket,
                sender_task_tx,
                client_command_tx,
                None,
            );
            endpoint.set_peer_cid(server_cid);
            spawn_endpoint(
                endpoint,
                client_socket,
                client_sender_task_rx,
                tx_to_endpoint_network,
            );

            // Establish connection by having server send SYN-ACK.
            tx_to_server_user
                .send(StreamCommand::SendData(Bytes::from_static(b"init")))
                .await
                .unwrap();
            let _ = rx_from_client_user.recv().await.unwrap(); // Client receives "init"

            // Client sends its unique data, now a larger payload.
            let payload = vec![i as u8; 10 * 1024]; // 10 KB
            let msg = Bytes::from(payload);

            // --- DEBUG LOG: Print bytes being sent from the client side ---
            let mut hasher = Sha256::new();
            hasher.update(&msg);
            let hash = hasher.finalize();
            println!("[CLIENT {}] SENDING PAYLOAD -> len: {}, hash: {:x}", i, msg.len(), hash);
            // --- END DEBUG LOG ---

            tx_to_client_user
                .send(StreamCommand::SendData(msg.clone()))
                .await
                .unwrap();

            msg
        });
        client_handles.push(client_handle);
    }

    // --- Run the network demultiplexer ---
    hub.spawn_demultiplexer(server_endpoint_senders);

    // --- Verification ---
    let mut server_verification_handles = Vec::new();
    for (i, (mut rx_from_user, _)) in server_handles.into_iter().enumerate() {
        let handle = tokio::spawn(async move {
            // Server should not receive its own "init" message.
            // It should only receive the unique data from the client.
            const EXPECTED_LEN: usize = 10 * 1024;
            let mut received_data = Vec::new();

            // Loop to receive all chunks until the total expected length is reached.
            while received_data.len() < EXPECTED_LEN {
                let chunks = tokio::time::timeout(
                    Duration::from_secs(15), // Generous timeout for 50 clients with large data
                    rx_from_user.recv(),
                )
                .await
                .unwrap_or_else(|_| panic!("Server for client {} timed out receiving data", i))
                .unwrap_or_else(|| {
                    panic!(
                        "Server for client {} channel closed unexpectedly. Received {} of {} bytes.",
                        i,
                        received_data.len(),
                        EXPECTED_LEN
                    )
                });

                for chunk in chunks {
                    received_data.extend_from_slice(&chunk);
                }
            }

            let expected_payload = vec![i as u8; EXPECTED_LEN];
            assert_eq!(
                received_data.len(),
                EXPECTED_LEN,
                "Server for client {} did not receive the correct amount of data.",
                i
            );
            assert_eq!(
                received_data, expected_payload,
                "Server for client {} received corrupted data.",
                i
            );
        });
        server_verification_handles.push(handle);
    }

    // Wait for all tasks to complete
    for handle in client_handles {
        handle.await.unwrap();
    }
    for handle in server_verification_handles {
        handle.await.unwrap();
    }
}

#[tokio::test]
async fn test_connect_and_send_data() {
    let (mut client, mut server) = setup_client_server_pair();

    // --- Establish Connection ---
    // The client sends SYN automatically. We trigger the server to send SYN-ACK
    // by having the application "accept" the connection by writing to it.
    server
        .tx_to_endpoint_user
        .send(StreamCommand::SendData(Bytes::new()))
        .await
        .unwrap();

    // Give time for the handshake (SYN -> SYN-ACK -> ACK) to complete.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // --- Actual Test ---
    // 1. Client sends data.
    let data_to_send = Bytes::from_static(b"hello server!");
    client
        .tx_to_endpoint_user
        .send(StreamCommand::SendData(data_to_send.clone()))
        .await
        .unwrap();

    // 2. Server should receive the data.
    let received_chunks = tokio::time::timeout(
        Duration::from_millis(200),
        server.rx_from_endpoint_user.recv(),
    )
    .await
    .expect("Server should receive data")
    .unwrap();

    assert_eq!(received_chunks.len(), 1);
    assert_eq!(received_chunks[0], data_to_send);

    // 3. Server sends a response.
    let response_data = Bytes::from_static(b"hello client!");
    server
        .tx_to_endpoint_user
        .send(StreamCommand::SendData(response_data.clone()))
        .await
        .unwrap();

    // 4. Client should receive the response.
    let received_chunks_client = tokio::time::timeout(
        Duration::from_millis(200),
        client.rx_from_endpoint_user.recv(),
    )
    .await
    .expect("Client should receive response")
    .unwrap();

    assert_eq!(received_chunks_client.len(), 1);
    assert_eq!(received_chunks_client[0], response_data);
}

#[tokio::test]
async fn test_data_flow_with_acks() {
    let (mut client, mut server) = setup_client_server_pair();

    // --- Establish Connection ---
    server
        .tx_to_endpoint_user
        .send(StreamCommand::SendData(Bytes::new()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Send a few packets from client to server
    let mut client_sent_data = Vec::new();
    for i in 0..5 {
        let data = Bytes::from(format!("c-packet-{}", i));
        client_sent_data.extend_from_slice(&data);
        client
            .tx_to_endpoint_user
            .send(StreamCommand::SendData(data))
            .await
            .unwrap();
    }

    // Receive them on the server to ensure they all arrived by checking total bytes.
    let mut server_recv_data = Vec::new();
    while server_recv_data.len() < client_sent_data.len() {
        let chunks = tokio::time::timeout(
            Duration::from_millis(200),
            server.rx_from_endpoint_user.recv(),
        )
        .await
        .expect("Server should receive data")
        .unwrap();
        for chunk in chunks {
            server_recv_data.extend_from_slice(&chunk);
        }
    }
    assert_eq!(server_recv_data, client_sent_data);

    // Now send from server to client
    let mut server_sent_data = Vec::new();
    for i in 0..3 {
        let data = Bytes::from(format!("s-packet-{}", i));
        server_sent_data.extend_from_slice(&data);
        server
            .tx_to_endpoint_user
            .send(StreamCommand::SendData(data))
            .await
            .unwrap();
    }

    // Receive them on the client by checking total bytes.
    let mut client_recv_data = Vec::new();
    while client_recv_data.len() < server_sent_data.len() {
        let chunks = tokio::time::timeout(
            Duration::from_millis(200),
            client.rx_from_endpoint_user.recv(),
        )
        .await
        .expect("Client should receive data")
        .unwrap();
        for chunk in chunks {
            client_recv_data.extend_from_slice(&chunk);
        }
    }
    assert_eq!(client_recv_data, server_sent_data);
}

#[tokio::test]
async fn test_endpoint_rto_retransmission() {
    let mut client_config = Config::default();
    client_config.initial_rto = Duration::from_millis(100);
    client_config.min_rto = Duration::from_millis(100);

    // Filter to drop all ACK packets sent from the server.
    let server_tx_filter = Arc::new(|frame: &Frame| -> bool { !matches!(frame, Frame::Ack { .. }) });
    // Client filter allows all packets.
    let client_tx_filter = Arc::new(|_: &Frame| -> bool { true });

    let (mut client, mut server, client_sent_count, _server_sent_count) =
        setup_client_server_with_filter(
            client_config,
            Config::default(),
            client_tx_filter,
            server_tx_filter,
        );

    // Establish connection.
    server
        .tx_to_endpoint_user
        .send(StreamCommand::SendData(Bytes::new()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    // Drain the empty data from the client side that was used to trigger the SYN-ACK.
    let _ = client.rx_from_endpoint_user.try_recv();

    // Client sends data.
    let data_to_send = Bytes::from_static(b"rto test data");
    client
        .tx_to_endpoint_user
        .send(StreamCommand::SendData(data_to_send.clone()))
        .await
        .unwrap();

    // Server should receive the packet the first time.
    let mut received_data_1 = Vec::new();
    let chunks = tokio::time::timeout(
        Duration::from_millis(50),
        server.rx_from_endpoint_user.recv(),
    )
    .await
    .expect("Server should receive the first packet transmission")
    .unwrap();
    for chunk in chunks {
        received_data_1.extend_from_slice(&chunk);
    }
    assert_eq!(received_data_1, data_to_send);

    // At this point, the client has sent some number of packets for the handshake and the data.
    // Let's capture this count. We need a small delay to ensure the PUSH is sent.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let packets_sent_before_rto = client_sent_count.load(Ordering::Relaxed);
    if packets_sent_before_rto == 0 {
        panic!("Expected at least the PUSH packet to be sent. Got 0");
    }

    // Now, wait for the RTO to expire. The client should retransmit the PUSH.
    tokio::time::sleep(Duration::from_millis(200)).await; // RTO is 100ms

    let packets_sent_after_rto = client_sent_count.load(Ordering::Relaxed);

    assert_eq!(
        packets_sent_after_rto,
        packets_sent_before_rto + 1,
        "Client should have retransmitted exactly one packet after RTO timeout"
    );

    // Verify the server does NOT receive the data again at the application layer.
    let retransmit_recv_result =
        tokio::time::timeout(Duration::from_millis(50), server.rx_from_endpoint_user.recv()).await;
    assert!(
        retransmit_recv_result.is_err(),
        "Server should not receive the retransmitted data at the application layer"
    );
}

#[tokio::test]
async fn test_endpoint_fast_retransmission() {
    // Filter to drop the PUSH packet with sequence number 1, just once.
    let packet_to_drop_seq = 1;
    let packet_has_been_dropped = Arc::new(AtomicBool::new(false));
    let client_tx_filter = Arc::new(move |frame: &Frame| -> bool {
        if let Frame::Push { header, .. } = frame {
            if header.sequence_number == packet_to_drop_seq
                && !packet_has_been_dropped.swap(true, Ordering::Relaxed)
            {
                return false; // Drop packet
            }
        }
        true // Keep all other packets
    });
    let server_tx_filter = Arc::new(|_: &Frame| -> bool { true });

    let (mut client, mut server, _, _) = setup_client_server_with_filter(
        Config::default(),
        Config::default(),
        client_tx_filter,
        server_tx_filter,
    );

    // Establish connection.
    server
        .tx_to_endpoint_user
        .send(StreamCommand::SendData(Bytes::new()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let _ = client.rx_from_endpoint_user.try_recv();

    // Client sends 5 packets. Packet with seq=1 will be dropped by the filter.
    let mut sent_data = Vec::new();
    for i in 0..5 {
        let data = Bytes::from(format!("packet-{}", i));
        sent_data.push(data.clone());
        client
            .tx_to_endpoint_user
            .send(StreamCommand::SendData(data))
            .await
            .unwrap();
    }

    // Server should receive packets 0, 2, 3, 4 first, then the retransmitted packet 1.
    // The reassemble buffer will wait for packet 1 before delivering anything to the user.
    let mut all_received_data = Vec::new();
    let total_len: usize = sent_data.iter().map(|d| d.len()).sum();

    while all_received_data.len() < total_len {
        let chunks =
            tokio::time::timeout(Duration::from_millis(200), server.rx_from_endpoint_user.recv())
                .await
                .expect("Server should eventually receive all data")
                .unwrap();
        for chunk in chunks {
            all_received_data.extend_from_slice(&chunk);
        }
    }

    // Verify that all data was received correctly and in order.
    let expected_data: Vec<u8> = sent_data.into_iter().flatten().collect();
    assert_eq!(all_received_data, expected_data);
}

#[tokio::test]
async fn test_connection_migration() {
    let mut harness = setup_server_harness();
    let old_addr = harness.client_addr;
    let new_addr: std::net::SocketAddr = "127.0.0.1:9999".parse().unwrap();

    // 1. Manually establish the connection from the old_addr
    // The server is in SynReceived state initially. Sending data will trigger SYN-ACK.
    harness
        .tx_to_endpoint_user
        .send(StreamCommand::SendData(Bytes::from_static(b"initial data")))
        .await
        .unwrap();

    // The server should send a SYN-ACK back to the client. We'll just drain this for now.
    let syn_ack_cmd = harness.rx_from_endpoint_network.recv().await.unwrap();
    if let SenderTaskCommand::Send(cmd) = syn_ack_cmd {
        assert!(matches!(cmd.frames[0], Frame::SynAck { .. }));
        assert_eq!(cmd.remote_addr, old_addr);
    } else {
        panic!("Expected a Send command");
    }

    // 2. Simulate a PUSH packet arriving from a new address. This should trigger path validation.
    let push_from_new_addr = Frame::Push {
        header: crate::packet::header::ShortHeader {
            command: crate::packet::command::Command::Push,
            connection_id: 2, // The server's CID
            sequence_number: 10,
            recv_window_size: 1024,
            timestamp: 0,
            recv_next_sequence: 0,
        },
        payload: Bytes::from_static(b"data from new address"),
    };
    harness
        .tx_to_endpoint_network
        .send((push_from_new_addr, new_addr))
        .await
        .unwrap();

    // 3. The server should respond with two packets in some order:
    //    - A PATH_CHALLENGE to the new address.
    //    - An ACK for the PUSH, sent to the *old*, validated address.
    let cmd1_enum = harness.rx_from_endpoint_network.recv().await.unwrap();
    let cmd2_enum = harness.rx_from_endpoint_network.recv().await.unwrap();
    let cmd1 = match cmd1_enum {
        SenderTaskCommand::Send(c) => c,
        _ => panic!("Expected Send"),
    };
    let cmd2 = match cmd2_enum {
        SenderTaskCommand::Send(c) => c,
        _ => panic!("Expected Send"),
    };

    let (challenge_cmd, ack_cmd) = if matches!(cmd1.frames[0], Frame::PathChallenge { .. }) {
        (cmd1, cmd2)
    } else {
        (cmd2, cmd1)
    };

    // Verify the PATH_CHALLENGE packet
    assert_eq!(challenge_cmd.remote_addr, new_addr, "Challenge should be sent to the new address");
    let challenge_data = if let Frame::PathChallenge { challenge_data, .. } = challenge_cmd.frames[0] {
        challenge_data
    } else {
        panic!("Expected a PathChallenge frame, got {:?}", challenge_cmd.frames[0]);
    };

    // Verify the ACK packet
    assert_eq!(ack_cmd.remote_addr, old_addr, "ACK should be sent to the old address");
    assert!(matches!(ack_cmd.frames[0], Frame::Ack { .. }));

    // 4. Simulate the client sending a PATH_RESPONSE back.
    let response_frame = crate::core::endpoint::frame_factory::create_path_response_frame(
        2,   // Server's CID
        999, // Sequence number is not critical for this test
        tokio::time::Instant::now(), // Timestamp also not critical
        challenge_data,
    );
    harness
        .tx_to_endpoint_network
        .send((response_frame, new_addr))
        .await
        .unwrap();

    // 5. The endpoint should now be migrated. To confirm, send data from the user side
    //    and verify it gets sent to the new address.
    harness
        .tx_to_endpoint_user
        .send(StreamCommand::SendData(Bytes::from_static(b"data after migration")))
        .await
        .unwrap();

    let push_after_migration_cmd =
        tokio::time::timeout(Duration::from_millis(100), harness.rx_from_endpoint_network.recv())
            .await
            .expect("should receive a PUSH after migration")
            .unwrap();

    if let SenderTaskCommand::Send(cmd) = push_after_migration_cmd {
        assert_eq!(cmd.remote_addr, new_addr, "PUSH should now be sent to the new, migrated address");
        assert!(matches!(cmd.frames[0], Frame::Push { .. }));
    } else {
        panic!("Expected a Send command");
    }
} 