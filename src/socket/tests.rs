//! Unit tests for the `socket` module, specifically for the `SocketActor`.
//! `socket` 模块的单元测试，特别是针对 `SocketActor`。

use super::{actor::SocketActor, command::*, traits::*};
use crate::{
    config::Config,
    core::stream::Stream,
    error::{Error, Result},
    packet::{command::Command, frame::Frame, header::LongHeader},
};
use async_trait::async_trait;
use bytes::Bytes;
use std::{collections::HashMap, net::SocketAddr, sync::Arc};
use tokio::sync::{mpsc, Mutex};

/// A mock `UdpSocket` for testing the `SocketActor`.
/// This version uses tokio::sync::Mutex to be Send-safe across .await points.
#[derive(Debug)]
struct MockSocket {
    local_addr: SocketAddr,
    packet_rx: Arc<Mutex<mpsc::Receiver<(Vec<u8>, SocketAddr)>>>,
    _packet_tx: mpsc::Sender<SenderTaskCommand<Self>>,
}

#[async_trait]
impl AsyncUdpSocket for MockSocket {
    async fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)> {
        let mut rx = self.packet_rx.lock().await;
        match rx.recv().await {
            Some((data, addr)) => {
                let len = data.len();
                buf[..len].copy_from_slice(&data);
                Ok((len, addr))
            }
            None => Err(Error::ChannelClosed),
        }
    }

    async fn send_to(&self, _buf: &[u8], _target: SocketAddr) -> Result<usize> {
        unimplemented!("Actor should not call send_to directly, but send commands to SenderTask")
    }

    fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.local_addr)
    }
}

#[async_trait]
impl BindableUdpSocket for MockSocket {
    async fn bind(_addr: SocketAddr) -> Result<Self> {
        unreachable!("MockSocket is created manually for tests")
    }
}

/// Test harness for the `SocketActor`.
struct ActorTestHarness {
    accept_rx: mpsc::Receiver<(Stream, SocketAddr)>,
    incoming_packet_tx: mpsc::Sender<(Vec<u8>, SocketAddr)>,
    _outgoing_cmd_rx: mpsc::Receiver<SenderTaskCommand<MockSocket>>,
    actor_handle: tokio::task::JoinHandle<()>,
}

impl ActorTestHarness {
    fn new() -> Self {
        let (command_tx, command_rx) = mpsc::channel(128);
        let (accept_tx, accept_rx) = mpsc::channel(128);
        let (send_tx, outgoing_cmd_rx) = mpsc::channel(128);
        let (incoming_packet_tx, incoming_packet_rx) = mpsc::channel(128);

        let mock_socket = Arc::new(MockSocket {
            local_addr: "127.0.0.1:9999".parse().unwrap(),
            packet_rx: Arc::new(Mutex::new(incoming_packet_rx)),
            _packet_tx: send_tx.clone(),
        });

        let mut actor = SocketActor {
            socket: mock_socket,
            connections: HashMap::new(),
            addr_to_cid: HashMap::new(),
            send_tx,
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
            _outgoing_cmd_rx: outgoing_cmd_rx,
            actor_handle,
        }
    }

    async fn send_syn(&self, from_addr: SocketAddr, source_cid: u32) {
        let syn_packet = Frame::Syn {
            header: LongHeader {
                command: Command::Syn,
                protocol_version: Config::default().protocol_version,
                destination_cid: 0,
                source_cid,
            },
            payload: Bytes::new(),
        };
        let mut buffer = Vec::new();
        Frame::from(syn_packet).encode(&mut buffer);
        self.incoming_packet_tx
            .send((buffer, from_addr))
            .await
            .unwrap();
    }
}

impl Drop for ActorTestHarness {
    fn drop(&mut self) {
        self.actor_handle.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_actor_concurrent_syn_handling_does_not_block() {
    // 1. Setup
    let mut harness = ActorTestHarness::new();
    let client_a_addr: SocketAddr = "127.0.0.1:1001".parse().unwrap();
    let client_b_addr: SocketAddr = "127.0.0.1:1002".parse().unwrap();

    // 2. Action: Simulate two SYN packets arriving back-to-back.
    harness.send_syn(client_a_addr, 100).await;
    harness.send_syn(client_b_addr, 200).await;

    // 3. Verification:
    // The actor should accept both connections and send two streams to the listener
    // without blocking, even if the listener doesn't immediately `accept()`.
    // The use of `try_send` in the actor is critical here.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await; // Give actor time to process

    let mut accepted_addrs = std::collections::HashSet::new();
    // Use `try_recv` to drain the channel without blocking the test.
    while let Ok((_, addr)) = harness.accept_rx.try_recv() {
        accepted_addrs.insert(addr);
    }

    // Verify that we accepted connections from both clients.
    let mut expected_addrs = std::collections::HashSet::new();
    expected_addrs.insert(client_a_addr);
    expected_addrs.insert(client_b_addr);

    assert_eq!(
        accepted_addrs.len(),
        2,
        "Actor should have processed both SYN packets"
    );
    assert_eq!(
        accepted_addrs, expected_addrs,
        "The actor did not correctly accept both concurrent connections."
    );
} 