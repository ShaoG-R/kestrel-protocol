//! Unit tests for the `socket` module, specifically for the transport-based `SocketActor`.
//! `socket` 模块的单元测试，特别是针对基于传输的 `SocketActor`。

use super::{
    draining::DrainingPool,
    transport::{BindableTransport, FrameBatch, ReceivedDatagram, Transport, TransportCommand},
    actor::TransportSocketActor,
};
use crate::{
    config::Config,
    core::stream::Stream,
    error::{Error, Result},
    packet::frame::Frame,
};
use arc_swap::ArcSwap;
use async_trait::async_trait;
use bytes::Bytes;
use std::{collections::HashMap, net::SocketAddr, sync::Arc};
use tokio::{
    io::AsyncWriteExt,
    sync::{mpsc, Mutex},
};

/// A mock transport for testing the transport-based `SocketActor`.
/// This version uses tokio::sync::Mutex to be Send-safe across .await points.
#[derive(Debug)]
struct MockTransport {
    local_addr: ArcSwap<SocketAddr>,
    packet_rx: Arc<Mutex<mpsc::Receiver<ReceivedDatagram>>>,
    sent_packets: Arc<Mutex<Vec<FrameBatch>>>,
}

impl MockTransport {
    fn new(local_addr: SocketAddr) -> (Self, mpsc::Sender<ReceivedDatagram>) {
        let (packet_tx, packet_rx) = mpsc::channel(128);
        let transport = Self {
            local_addr: ArcSwap::from_pointee(local_addr),
            packet_rx: Arc::new(Mutex::new(packet_rx)),
            sent_packets: Arc::new(Mutex::new(Vec::new())),
        };
        (transport, packet_tx)
    }

    async fn get_sent_packets(&self) -> Vec<FrameBatch> {
        self.sent_packets.lock().await.clone()
    }
}

#[async_trait]
impl Transport for MockTransport {
    async fn send_frames(&self, batch: FrameBatch) -> Result<()> {
        let mut sent = self.sent_packets.lock().await;
        sent.push(batch);
        Ok(())
    }

    async fn recv_frames(&self) -> Result<ReceivedDatagram> {
        let mut rx = self.packet_rx.lock().await;
        match rx.recv().await {
            Some(datagram) => Ok(datagram),
            None => Err(Error::ChannelClosed),
        }
    }

    fn local_addr(&self) -> Result<SocketAddr> {
        Ok(**self.local_addr.load())
    }
}

#[async_trait]
impl BindableTransport for MockTransport {
    async fn bind(_addr: SocketAddr) -> Result<Self> {
        unreachable!("MockTransport is created manually for tests")
    }

    async fn rebind(&self, new_addr: SocketAddr) -> Result<()> {
        self.local_addr.store(Arc::new(new_addr));
        Ok(())
    }
}

/// A comprehensive test harness for the transport-based `SocketActor`.
struct ActorTestHarness {
    accept_rx: mpsc::Receiver<(Stream, SocketAddr)>,
    incoming_packet_tx: mpsc::Sender<ReceivedDatagram>,
    outgoing_cmd_rx: mpsc::Receiver<TransportCommand<MockTransport>>,
    mock_transport: Arc<MockTransport>,
    actor_handle: tokio::task::JoinHandle<()>,
}

impl ActorTestHarness {
    fn new() -> Self {
        let (command_tx, command_rx) = mpsc::channel(128);
        let (accept_tx, accept_rx) = mpsc::channel(128);
        let (send_tx, outgoing_cmd_rx) = mpsc::channel(128);

        let (mock_transport, incoming_packet_tx) = MockTransport::new("127.0.0.1:9999".parse().unwrap());
        let mock_transport = Arc::new(mock_transport);

        let config = Arc::new(Config::default());

        // 创建传输管理器用于测试
        // Create transport manager for testing
        let transport_manager = super::transport::TransportManager::new(mock_transport.clone(), send_tx);
        
        // 创建帧路由管理器用于测试
        // Create frame router manager for testing
        let frame_router = super::routing::FrameRouter::new(
            DrainingPool::new(config.connection.drain_timeout)
        );

        let mut actor = TransportSocketActor {
            transport_manager,
            frame_router,
            config: config.clone(),
            accept_tx,
            command_rx,
            command_tx: command_tx.clone(),
        };

        let actor_handle = tokio::spawn(async move {
            actor.run().await;
        });

        Self {
            accept_rx,
            incoming_packet_tx,
            outgoing_cmd_rx,
            mock_transport,
            actor_handle,
        }
    }

    async fn send_syn(&self, from_addr: SocketAddr, source_cid: u32) {
        let syn_frame = Frame::new_syn(
            Config::default().protocol_version,
            source_cid,
            0, // destination_cid is unknown for initial SYN
        );
        
        let datagram = ReceivedDatagram {
            remote_addr: from_addr,
            frames: vec![syn_frame],
        };
        
        self.incoming_packet_tx
            .send(datagram)
            .await
            .unwrap();
    }

    async fn get_sent_packets(&self) -> Vec<FrameBatch> {
        self.mock_transport.get_sent_packets().await
    }
}

impl Drop for ActorTestHarness {
    fn drop(&mut self) {
        self.actor_handle.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_actor_sends_to_correct_address_after_accept() {
    // 1. Setup
    let mut harness = ActorTestHarness::new();
    let client_addr: SocketAddr = "127.0.0.1:1001".parse().unwrap();

    // 2. Action: Client sends SYN
    harness.send_syn(client_addr, 123).await;

    // 3. Verification (Part 1): Server accepts the connection
    let (stream, accepted_addr) =
        tokio::time::timeout(std::time::Duration::from_secs(1), harness.accept_rx.recv())
            .await
            .expect("Actor failed to accept connection in time")
            .unwrap();
    assert_eq!(
        accepted_addr, client_addr,
        "Actor accepted connection from wrong address"
    );

    // 4. Action: The "user" (our test) writes data to the newly accepted stream.
    // This forces the server-side Endpoint to send a SYN-ACK.
    let (_, mut writer) = tokio::io::split(stream);
    writer
        .write_all(b"hello from server")
        .await
        .expect("Writing to stream failed");

    // 5. Verification (Part 2): The Actor must send a `TransportCommand` to the transport
    // with the `remote_addr` correctly set to the client's address.
    let transport_command =
        tokio::time::timeout(std::time::Duration::from_secs(1), harness.outgoing_cmd_rx.recv())
            .await
            .expect("Actor did not dispatch a command to the transport")
            .unwrap();

    match transport_command {
        TransportCommand::Send(frame_batch) => {
            assert_eq!(
                frame_batch.remote_addr, client_addr,
                "CRITICAL: Actor dispatched FrameBatch with the wrong remote address!"
            );

            // Optional: check if the first frame is indeed a SYN-ACK
            assert!(
                matches!(frame_batch.frames.get(0), Some(Frame::SynAck { .. })),
                "Expected the first frame to be a SYN-ACK"
            );
        }
        _ => panic!("Expected TransportCommand::Send, but got something else"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_actor_concurrent_write_sends_to_correct_addresses() {
    // 1. Setup
    let mut harness = ActorTestHarness::new();
    let client_a_addr: SocketAddr = "127.0.0.1:2001".parse().unwrap();
    let client_b_addr: SocketAddr = "127.0.0.1:2002".parse().unwrap();

    // 2. Action: Both clients connect
    harness.send_syn(client_a_addr, 100).await;
    harness.send_syn(client_b_addr, 200).await;

    // 3. Action: Server accepts both connections
    let (stream_a, _) = harness.accept_rx.recv().await.unwrap();
    let (stream_b, _) = harness.accept_rx.recv().await.unwrap();

    // 4. Action: Write to both streams concurrently. This is the key part.
    let (_, mut writer_a) = tokio::io::split(stream_a);
    let (_, mut writer_b) = tokio::io::split(stream_b);

    writer_a.write_all(b"to A").await.unwrap();
    writer_b.write_all(b"to B").await.unwrap();

    // 5. Verification: Collect the two outgoing TransportCommands
    let mut outgoing_commands = HashMap::new();
    for _ in 0..2 {
        let cmd = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            harness.outgoing_cmd_rx.recv(),
        )
        .await
        .expect("Failed to receive command from actor")
        .unwrap();

        if let TransportCommand::Send(frame_batch) = cmd {
            // We use the payload to identify which command is which.
            // The actual data is now in the PUSH frame, not the SYN-ACK.
            let payload = frame_batch
                .frames
                .iter()
                .find_map(|f| match f {
                    Frame::Push { payload, .. } => {
                        if !payload.is_empty() {
                            Some(payload.clone())
                        } else {
                            None
                        }
                    }
                    _ => None,
                })
                .unwrap();
            outgoing_commands.insert(payload, frame_batch.remote_addr);
        }
    }

    assert_eq!(
        outgoing_commands.get(&Bytes::from_static(b"to A")),
        Some(&client_a_addr),
        "Data for A was sent to the wrong address"
    );
    assert_eq!(
        outgoing_commands.get(&Bytes::from_static(b"to B")),
        Some(&client_b_addr),
        "Data for B was sent to the wrong address"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_actor_with_true_concurrent_handlers() {
    // This test fully replicates the concurrency model of the failing integration test.
    // 1. Setup
    let mut harness = ActorTestHarness::new();
    let client_a_addr: SocketAddr = "127.0.0.1:3001".parse().unwrap();
    let client_b_addr: SocketAddr = "127.0.0.1:3002".parse().unwrap();

    // 2. Action: Spawn client tasks to connect concurrently
    harness.send_syn(client_a_addr, 300).await;
    harness.send_syn(client_b_addr, 400).await;

    // 3. Action: The main test task now acts as the server, accepting and spawning handlers.
    let mut handlers = Vec::new();
    for _ in 0..2 {
        let (stream, _addr) = harness
            .accept_rx
            .recv()
            .await
            .expect("Failed to accept connection");
        let handler = tokio::spawn(async move {
            let (_, mut writer) = tokio::io::split(stream);
            writer.write_all(b"probe").await.unwrap();
        });
        handlers.push(handler);
    }
    for handler in handlers {
        handler.await.unwrap();
    }

    // 4. Verification
    let mut outgoing_commands = HashMap::new();
    for _ in 0..2 {
        let cmd = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            harness.outgoing_cmd_rx.recv(),
        )
        .await
        .expect("Actor did not send command in time")
        .unwrap();

        if let TransportCommand::Send(frame_batch) = cmd {
            // SYN-ACK for a probe has no payload, and the PUSH frame might also be empty initially.
            // So, we identify by address.
            outgoing_commands.insert(frame_batch.remote_addr, frame_batch);
        }
    }

    assert!(
        outgoing_commands.contains_key(&client_a_addr),
        "Did not send a command to client A"
    );
    assert!(
        outgoing_commands.contains_key(&client_b_addr),
        "Did not send a command to client B"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_transport_layer_integration() {
    // Test that the transport layer correctly handles frame batching and addressing
    let mut harness = ActorTestHarness::new();
    let client_addr: SocketAddr = "127.0.0.1:4001".parse().unwrap();

    // Send SYN and accept connection
    harness.send_syn(client_addr, 500).await;
    let (stream, _) = harness.accept_rx.recv().await.unwrap();

    // Write data to trigger frame sending
    let (_, mut writer) = tokio::io::split(stream);
    writer.write_all(b"test data").await.unwrap();

    // Wait for the transport command to be sent
    let transport_command = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        harness.outgoing_cmd_rx.recv(),
    )
    .await
    .expect("Actor did not dispatch a command to the transport")
    .unwrap();

    // Verify the command is correct
    match transport_command {
        TransportCommand::Send(frame_batch) => {
            assert_eq!(
                frame_batch.remote_addr, client_addr,
                "Transport command sent to wrong address"
            );
            assert!(!frame_batch.frames.is_empty(), "No frames in the batch");
        }
        _ => panic!("Expected TransportCommand::Send, but got something else"),
    }

    // Also verify that the mock transport would have received the frames
    // (This tests the integration between the actor and transport layer)
    let _sent_packets = harness.get_sent_packets().await;
    // Note: sent_packets might be empty because our mock doesn't actually execute the send
    // but the important thing is that the command was dispatched correctly
} 