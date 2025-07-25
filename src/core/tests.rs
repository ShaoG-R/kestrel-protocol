//! Integration-style tests for the `Endpoint` worker, using a simulated network.

use super::endpoint::{Endpoint, StreamCommand};
use crate::{
    config::Config,
    error::Result,
    packet::frame::Frame,
    socket::{AsyncUdpSocket, SenderTaskCommand, SocketCommand},
};
use async_trait::async_trait;
use bytes::Bytes;
use std::{
    collections::VecDeque,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc;

// --- Mock Network Infrastructure ---

/// A mock UDP socket that uses shared queues to simulate a network link.
/// It allows two `MockUdpSocket` instances to send packets to each other.
#[derive(Clone)]
struct MockUdpSocket {
    local_addr: SocketAddr,
    // Packets sent to this socket are pushed here by the peer.
    recv_queue: Arc<Mutex<VecDeque<(Bytes, SocketAddr)>>>,
    // This socket sends packets by pushing them to the peer's recv_queue.
    peer_recv_queue: Arc<Mutex<VecDeque<(Bytes, SocketAddr)>>>,
    // The filter is applied on SEND. Return true to keep the packet, false to drop.
    packet_tx_filter: Arc<dyn Fn(&Frame) -> bool + Send + Sync>,
    sent_packets_count: Arc<AtomicUsize>,
}

#[async_trait]
impl AsyncUdpSocket for MockUdpSocket {
    async fn send_to(&self, buf: &[u8], _target: SocketAddr) -> Result<usize> {
        // A datagram might contain multiple frames. For test simplicity, we only check the first.
        if let Some(frame) = Frame::decode(buf) {
            if !(self.packet_tx_filter)(&frame) {
                // Packet dropped by the filter.
                return Ok(buf.len());
            }
        }

        self.sent_packets_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // When this socket sends, the peer receives. The sender address is our local address.
        self.peer_recv_queue
            .lock()
            .unwrap()
            .push_back((Bytes::copy_from_slice(buf), self.local_addr));
        Ok(buf.len())
    }

    async fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)> {
        // Loop until a packet is available in our receive queue.
        loop {
            if let Some((data, addr)) = self.recv_queue.lock().unwrap().pop_front() {
                let len = data.len();
                buf[..len].copy_from_slice(&data);
                return Ok((len, addr));
            }
            // Yield to allow other tasks to run.
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.local_addr)
    }
}

/// A handle to an `Endpoint` running in a test, providing access to its user-facing channels.
struct EndpointHarness {
    /// To simulate the user application sending commands (e.g., data) to the Endpoint.
    pub tx_to_endpoint_user: mpsc::Sender<StreamCommand>,
    /// To capture reassembled data that the Endpoint makes available to the user application.
    pub rx_from_endpoint_user: mpsc::Receiver<Vec<Bytes>>,
}

/// Spawns an `Endpoint` and the necessary relay tasks to connect it to a `MockUdpSocket`.
fn spawn_endpoint(
    mut endpoint: Endpoint<MockUdpSocket>,
    socket: MockUdpSocket,
    mut sender_task_rx: mpsc::Receiver<SenderTaskCommand<MockUdpSocket>>,
    tx_to_endpoint_network: mpsc::Sender<(Frame, SocketAddr)>,
) {
    // --- Sender Relay Task ---
    // The Endpoint sends `SenderTaskCommand`s to this task, which then uses the mock socket.
    let socket_clone = socket.clone();
    tokio::spawn(async move {
        while let Some(SenderTaskCommand::Send(cmd)) = sender_task_rx.recv().await {
            let mut send_buf = Vec::new();
            for frame in cmd.frames {
                frame.encode(&mut send_buf);
            }
            if !send_buf.is_empty() {
                socket_clone
                    .send_to(&send_buf, cmd.remote_addr)
                    .await
                    .unwrap();
            }
        }
    });

    // --- Receiver Relay Task ---
    // This task uses the mock socket to receive packets and forwards them to the Endpoint.
    tokio::spawn(async move {
        let mut recv_buf = [0u8; 2048];
        loop {
            if let Ok((len, src_addr)) = socket.recv_from(&mut recv_buf).await {
                if let Some(frame) = Frame::decode(&recv_buf[..len]) {
                    if tx_to_endpoint_network.send((frame, src_addr)).await.is_err() {
                        break; // Endpoint closed
                    }
                }
            }
        }
    });

    // --- Main Endpoint Task ---
    tokio::spawn(async move {
        let _ = endpoint.run().await;
    });
}

/// Sets up a connected pair of (client, server) endpoints for integration testing.
fn setup_client_server_pair() -> (EndpointHarness, EndpointHarness) {
    let client_config = Config::default();
    let server_config = Config::default();
    let client_tx_filter = Arc::new(|_: &Frame| -> bool { true });
    let server_tx_filter = Arc::new(|_: &Frame| -> bool { true });

    // The counters are not needed for this simple setup, so we just ignore them.
    let (client_harness, server_harness, _, _) = setup_client_server_with_filter(
        client_config,
        server_config,
        client_tx_filter,
        server_tx_filter,
    );
    (client_harness, server_harness)
}

/// Sets up a connected pair of (client, server) endpoints with network simulation filters.
fn setup_client_server_with_filter(
    client_config: Config,
    server_config: Config,
    client_tx_filter: Arc<dyn Fn(&Frame) -> bool + Send + Sync>,
    server_tx_filter: Arc<dyn Fn(&Frame) -> bool + Send + Sync>,
) -> (
    EndpointHarness,
    EndpointHarness,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
) {
    let client_addr: SocketAddr = "127.0.0.1:1234".parse().unwrap();
    let server_addr: SocketAddr = "127.0.0.1:5678".parse().unwrap();

    // Create the shared "network" queues
    let client_recv_queue = Arc::new(Mutex::new(VecDeque::new()));
    let server_recv_queue = Arc::new(Mutex::new(VecDeque::new()));

    let client_sent_count = Arc::new(AtomicUsize::new(0));
    let server_sent_count = Arc::new(AtomicUsize::new(0));

    // Create sockets that are linked to each other's queues
    let client_socket = MockUdpSocket {
        local_addr: client_addr,
        recv_queue: client_recv_queue.clone(),
        peer_recv_queue: server_recv_queue.clone(),
        packet_tx_filter: client_tx_filter,
        sent_packets_count: client_sent_count.clone(),
    };
    let server_socket = MockUdpSocket {
        local_addr: server_addr,
        recv_queue: server_recv_queue,
        peer_recv_queue: client_recv_queue,
        packet_tx_filter: server_tx_filter,
        sent_packets_count: server_sent_count.clone(),
    };

    // --- Setup Client ---
    let (client_harness, client_peer_cid) = {
        let local_cid = 1;
        let peer_cid = 2; // Pre-determined for the test
        let (tx_to_endpoint_network, rx_from_socket) = mpsc::channel(128);
        let (sender_task_tx, sender_task_rx) = mpsc::channel(128);
        let (socket_command_tx, _) = mpsc::channel::<SocketCommand>(128);

        let (mut endpoint, tx_to_user, rx_from_user) = Endpoint::new_client(
            client_config,
            server_addr,
            local_cid,
            rx_from_socket,
            sender_task_tx.clone(),
            socket_command_tx.clone(),
            None,
        );
        endpoint.set_peer_cid(peer_cid);

        spawn_endpoint(
            endpoint,
            client_socket,
            sender_task_rx,
            tx_to_endpoint_network,
        );

        let harness = EndpointHarness {
            tx_to_endpoint_user: tx_to_user,
            rx_from_endpoint_user: rx_from_user,
        };
        (harness, local_cid)
    };

    // --- Setup Server ---
    let server_harness = {
        let local_cid = 2;
        let (tx_to_endpoint_network, rx_from_socket) = mpsc::channel(128);
        let (sender_task_tx, sender_task_rx) = mpsc::channel(128);
        let (socket_command_tx, _) = mpsc::channel::<SocketCommand>(128);

        let (endpoint, tx_to_user, rx_from_user) = Endpoint::new_server(
            server_config,
            client_addr,
            local_cid,
            client_peer_cid,
            rx_from_socket,
            sender_task_tx.clone(),
            socket_command_tx,
        );

        spawn_endpoint(
            endpoint,
            server_socket,
            sender_task_rx,
            tx_to_endpoint_network,
        );

        EndpointHarness {
            tx_to_endpoint_user: tx_to_user,
            rx_from_endpoint_user: rx_from_user,
        }
    };

    (
        client_harness,
        server_harness,
        client_sent_count,
        server_sent_count,
    )
}

// --- Tests ---

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
    tokio::time::sleep(Duration::from_millis(10)).await;
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
    use std::sync::atomic::{AtomicBool, Ordering};
    let client_config = Config::default();
    let server_config = Config::default();

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
        client_config,
        server_config,
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

/*
// NOTE: Connection migration requires a more advanced network simulation that can
// handle address changes, which is beyond the scope of the current filter-based model.
// This test will be implemented in a future step with an upgraded mock network.
#[tokio::test]
async fn test_connection_migration() {
    // ...
}
*/ 