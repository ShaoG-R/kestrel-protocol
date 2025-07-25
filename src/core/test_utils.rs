//! Common testing infrastructure for core endpoint tests.

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
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::mpsc;

// --- Mock Network Infrastructure ---

/// A mock UDP socket that uses shared queues to simulate a network link.
/// It allows two `MockUdpSocket` instances to send packets to each other.
#[derive(Clone)]
pub struct MockUdpSocket {
    pub local_addr: SocketAddr,
    // Packets sent to this socket are pushed here by the peer.
    pub recv_queue: Arc<Mutex<VecDeque<(Bytes, SocketAddr)>>>,
    // This socket sends packets by pushing them to the peer's recv_queue.
    pub peer_recv_queue: Arc<Mutex<VecDeque<(Bytes, SocketAddr)>>>,
    // The filter is applied on SEND. Return true to keep the packet, false to drop.
    pub packet_tx_filter: Arc<dyn Fn(&Frame) -> bool + Send + Sync>,
    pub sent_packets_count: Arc<AtomicUsize>,
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

        self.sent_packets_count.fetch_add(1, Ordering::Relaxed);

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
pub struct EndpointHarness {
    /// To simulate the user application sending commands (e.g., data) to the Endpoint.
    pub tx_to_endpoint_user: mpsc::Sender<StreamCommand>,
    /// To capture reassembled data that the Endpoint makes available to the user application.
    pub rx_from_endpoint_user: mpsc::Receiver<Vec<Bytes>>,
}

/// A harness for testing a server-side `Endpoint` in isolation.
/// It provides direct access to the "network" and "user" channels.
pub struct ServerTestHarness {
    /// To send user commands (e.g., data) to the Endpoint.
    pub tx_to_endpoint_user: mpsc::Sender<StreamCommand>,
    /// To receive data from the Endpoint that would go to the user.
    pub rx_from_endpoint_user: mpsc::Receiver<Vec<Bytes>>,
    /// To send frames to the Endpoint, simulating network ingress.
    pub tx_to_endpoint_network: mpsc::Sender<(Frame, SocketAddr)>,
    /// To receive commands from the Endpoint that would go to the network.
    pub rx_from_endpoint_network: mpsc::Receiver<SenderTaskCommand<MockUdpSocket>>,
    /// The address of the "client" that the server is connected to.
    pub client_addr: SocketAddr,
}

/// Spawns an `Endpoint` and the necessary relay tasks to connect it to a `MockUdpSocket`.
pub fn spawn_endpoint(
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
pub fn setup_client_server_pair() -> (EndpointHarness, EndpointHarness) {
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
pub fn setup_client_server_with_filter(
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

/// Sets up a server-side `Endpoint` for isolated testing.
///
/// This does NOT spawn the relay tasks, allowing the test to act as the network
/// by directly using the `tx_to_endpoint_network` and `rx_from_endpoint_network` channels.
pub fn setup_server_harness() -> ServerTestHarness {
    let _server_addr: SocketAddr = "127.0.0.1:5678".parse().unwrap();
    let client_addr: SocketAddr = "127.0.0.1:1234".parse().unwrap(); // "old" client addr
    let config = Config::default();

    let (tx_to_endpoint_network, rx_from_socket) = mpsc::channel(128);
    let (sender_task_tx, sender_task_rx) = mpsc::channel(128);
    let (socket_command_tx, _socket_command_rx) = mpsc::channel::<SocketCommand>(128);

    let server_cid = 2;
    let client_cid = 1;

    let (mut endpoint, tx_to_user, rx_from_user) = Endpoint::new_server(
        config,
        client_addr,
        server_cid,
        client_cid,
        rx_from_socket,
        sender_task_tx,
        socket_command_tx,
    );

    // Unlike other test setups, we only spawn the main endpoint task.
    // The test itself will drive the network channels.
    tokio::spawn(async move {
        let _ = endpoint.run().await;
    });

    ServerTestHarness {
        tx_to_endpoint_user: tx_to_user,
        rx_from_endpoint_user: rx_from_user,
        tx_to_endpoint_network,
        rx_from_endpoint_network: sender_task_rx,
        client_addr,
    }
} 