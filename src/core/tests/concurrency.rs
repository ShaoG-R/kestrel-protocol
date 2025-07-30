//! Tests for multi-client concurrency scenarios.

use crate::{
    config::Config,
    core::{
        endpoint::{Endpoint, StreamCommand},
        test_utils::{spawn_endpoint, MockTransport},
    },
    packet::frame::Frame,
    socket::transport::ReceivedDatagram,
};
use bytes::Bytes;
// use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    net::SocketAddr,
    sync::{
        atomic::AtomicUsize,
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::mpsc;

// --- Multi-client Test Infrastructure ---

/// A central hub to simulate a server's UDP socket that can handle multiple clients.
/// It creates interconnected mock transports for a multi-client test scenario.
struct TestNetHub {
    server_addr: SocketAddr,
    /// A single queue that simulates the server's single listening port.
    /// All clients send datagrams to this queue.
    server_ingress_queue: Arc<Mutex<VecDeque<ReceivedDatagram>>>,
    /// Maps a client's address to its dedicated receive queue for server responses.
    client_egress_queues: Arc<Mutex<HashMap<SocketAddr, Arc<Mutex<VecDeque<ReceivedDatagram>>>>>>,
}

impl TestNetHub {
    fn new(server_addr: SocketAddr) -> Self {
        Self {
            server_addr,
            server_ingress_queue: Arc::new(Mutex::new(VecDeque::new())),
            client_egress_queues: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Creates a transport for a client.
    fn new_client_transport(&self, client_addr: SocketAddr) -> MockTransport {
        let client_recv_queue = Arc::new(Mutex::new(VecDeque::new()));
        self.client_egress_queues
            .lock()
            .unwrap()
            .insert(client_addr, client_recv_queue.clone());

        MockTransport {
            local_addr: client_addr,
            recv_queue: client_recv_queue,
            peer_recv_queue: self.server_ingress_queue.clone(), // Sends to the server
            packet_tx_filter: Arc::new(|_| true),
            sent_packets_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Creates a transport for a server-side endpoint.
    fn new_server_transport_for_client(&self, client_addr: SocketAddr) -> MockTransport {
        let client_queue = self
            .client_egress_queues
            .lock()
            .unwrap()
            .get(&client_addr)
            .cloned()
            .unwrap();

        MockTransport {
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
                let datagram = server_ingress_queue.lock().unwrap().pop_front();

                if let Some(datagram) = datagram {
                    // --- DEBUG LOG: Print datagram received by the server-side hub ---
                    // let total_bytes: usize = datagram.frames.iter()
                    //     .map(|f| {
                    //         let mut buf = Vec::new();
                    //         f.encode(&mut buf);
                    //         buf.len()
                    //     })
                    //     .sum();
                    // let mut hasher = Sha256::new();
                    // hasher.update(&total_bytes.to_be_bytes());
                    // let hash = hasher.finalize();
                    // println!(
                    //     "[SERVER DEMUX] RECV FROM {} -> frame_count: {}, total_bytes: {}, hash: {:x}",
                    //     datagram.remote_addr,
                    //     datagram.frames.len(),
                    //     total_bytes,
                    //     hash
                    // );
                    // --- END DEBUG LOG ---

                    for frame in datagram.frames {
                        if let Some(sender) = endpoint_senders.get_mut(&datagram.remote_addr) {
                            if sender.send((frame, datagram.remote_addr)).await.is_err() {
                                // Endpoint is gone, remove it to prevent further sends.
                                endpoint_senders.remove(&datagram.remote_addr);
                            }
                        }
                    }
                } else {
                    // If no datagram, yield to avoid busy-waiting.
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            }
        });
    }
}

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

        // Transports must be created before spawning tasks that use them.
        let client_transport = hub.new_client_transport(client_addr);
        let server_transport = hub.new_server_transport_for_client(client_addr);

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
            server_transport,
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
                client_transport,
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
            // let mut hasher = Sha256::new();
            // hasher.update(&msg);
            // let hash = hasher.finalize();
            // println!("[CLIENT {}] SENDING PAYLOAD -> len: {}, hash: {:x}", i, msg.len(), hash);
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