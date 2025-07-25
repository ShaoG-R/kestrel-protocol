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
use tokio::{
    io::AsyncWriteExt,
    sync::{mpsc, Mutex},
};

/// A mock `UdpSocket` for testing the `SocketActor`.
/// This version uses tokio::sync::Mutex to be Send-safe across .await points.
#[derive(Debug)]
struct MockSocket {
    local_addr: SocketAddr,
    packet_rx: Arc<Mutex<mpsc::Receiver<(Vec<u8>, SocketAddr)>>>,
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
        // The actor should not send directly, but via the SenderTask.
        // This method being called would indicate a design flaw.
        unimplemented!("Actor should not call send_to directly")
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

/// A comprehensive test harness for the `SocketActor`.
struct ActorTestHarness {
    accept_rx: mpsc::Receiver<(Stream, SocketAddr)>,
    incoming_packet_tx: mpsc::Sender<(Vec<u8>, SocketAddr)>,
    outgoing_cmd_rx: mpsc::Receiver<SenderTaskCommand<MockSocket>>,
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
            outgoing_cmd_rx,
            actor_handle,
        }
    }

    async fn send_syn(&self, from_addr: SocketAddr, source_cid: u32) {
        let syn_frame = Frame::Syn {
            header: LongHeader {
                command: Command::Syn,
                protocol_version: Config::default().protocol_version,
                destination_cid: 0,
                source_cid,
            },
            payload: Bytes::new(),
        };
        let mut buffer = Vec::new();
        syn_frame.encode(&mut buffer);
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

    // 5. Verification (Part 2): The Actor must send a `SendCommand` to the SenderTask
    // with the `remote_addr` correctly set to the client's address.
    let sender_command =
        tokio::time::timeout(std::time::Duration::from_secs(1), harness.outgoing_cmd_rx.recv())
            .await
            .expect("Actor did not dispatch a command to the SenderTask")
            .unwrap();

    match sender_command {
        SenderTaskCommand::Send(send_command) => {
            assert_eq!(
                send_command.remote_addr, client_addr,
                "CRITICAL: Actor dispatched SendCommand with the wrong remote address!"
            );

            // Optional: check if the first frame is indeed a SYN-ACK
            assert!(
                matches!(send_command.frames.get(0), Some(Frame::SynAck { .. })),
                "Expected the first frame to be a SYN-ACK"
            );
        }
        _ => panic!("Expected SenderTaskCommand::Send, but got something else"),
    }
} 