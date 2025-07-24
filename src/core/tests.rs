//! Integration-style tests for the `Endpoint` worker.

use super::endpoint::{Endpoint, StreamCommand};
use crate::config::Config;
use crate::packet::frame::Frame;
use crate::packet::header::ShortHeader;
use crate::socket::SendCommand;
use bytes::Bytes;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::sync::mpsc;

/// A test harness that wraps an `Endpoint` and its communication channels.
struct TestHarness {
    /// To simulate frames coming from the network into the Endpoint.
    tx_to_endpoint_network: mpsc::Sender<Frame>,
    /// To capture frames/commands sent from the Endpoint to the network.
    rx_from_endpoint_network: mpsc::Receiver<SendCommand>,
    /// To simulate the user application sending commands (e.g., data) to the Endpoint.
    tx_to_endpoint_user: mpsc::Sender<StreamCommand>,
    /// To capture reassembled data that the Endpoint makes available to the user application.
    rx_from_endpoint_user: mpsc::Receiver<Bytes>,
}

/// Sets up an `Endpoint` with a given config and returns a harness to interact with it.
fn setup_endpoint_with_config(config: Config) -> TestHarness {
    let remote_addr: SocketAddr = "127.0.0.1:1234".parse().unwrap();
    let local_cid = 1;

    let (tx_to_endpoint_network, rx_from_socket) = mpsc::channel(128);
    let (tx_from_endpoint_network, rx_from_endpoint_network) = mpsc::channel(128);

    let (mut endpoint, tx_to_endpoint_user, rx_from_endpoint_user) = Endpoint::new_client(
        config,
        remote_addr,
        local_cid,
        rx_from_socket,
        tx_from_endpoint_network,
    );

    tokio::spawn(async move {
        let _ = endpoint.run().await;
    });

    TestHarness {
        tx_to_endpoint_network,
        rx_from_endpoint_network,
        tx_to_endpoint_user,
        rx_from_endpoint_user,
    }
}

/// Sets up an `Endpoint` in a spawned task and returns a harness to interact with it.
fn setup_endpoint() -> TestHarness {
    setup_endpoint_with_config(Config::default())
}

/// Helper to perform the initial client-side handshake.
/// Returns the server's chosen CID.
async fn establish_connection(harness: &mut TestHarness) -> u32 {
    let send_cmd = tokio::time::timeout(
        Duration::from_millis(100),
        harness.rx_from_endpoint_network.recv(),
    )
    .await
    .expect("should receive a SYN command")
    .expect("command should not be None");

    let our_cid = if let Frame::Syn { header, .. } = &send_cmd.frames[0] {
        header.source_cid
    } else {
        panic!("Expected a SYN frame");
    };

    let server_cid = 5678;
    let syn_ack_header = crate::packet::header::LongHeader {
        command: crate::packet::command::Command::SynAck,
        protocol_version: 1,
        destination_cid: our_cid,
        source_cid: server_cid,
    };
    let syn_ack_frame = Frame::SynAck {
        header: syn_ack_header,
    };
    harness
        .tx_to_endpoint_network
        .send(syn_ack_frame)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(10)).await;

    server_cid
}

#[tokio::test]
async fn test_endpoint_client_connect_and_send() {
    let mut harness = setup_endpoint();
    let server_cid = establish_connection(&mut harness).await;

    let data = Bytes::from_static(b"hello");
    harness
        .tx_to_endpoint_user
        .send(StreamCommand::SendData(data))
        .await
        .unwrap();

    let send_cmd_push = tokio::time::timeout(
        Duration::from_millis(100),
        harness.rx_from_endpoint_network.recv(),
    )
    .await
    .expect("should receive a push command")
    .expect("push command should not be None");

    assert_eq!(send_cmd_push.frames.len(), 1);
    let frame_push = &send_cmd_push.frames[0];
    if let Frame::Push { header, payload } = frame_push {
        assert_eq!(payload.as_ref(), b"hello");
        assert_eq!(header.connection_id, server_cid);
    } else {
        panic!("Expected a PUSH frame after connection was established");
    }
}

#[tokio::test]
async fn test_endpoint_receive_data_and_send_ack() {
    let mut harness = setup_endpoint();
    let server_cid = establish_connection(&mut harness).await;

    let create_push = |seq: u32, data: &'static [u8]| -> Frame {
        let push_header = ShortHeader {
            command: crate::packet::command::Command::Push,
            connection_id: 1, // The CID we chose for ourself (local_cid)
            sequence_number: seq,
            recv_window_size: 1024,
            timestamp: 0,
            recv_next_sequence: 0,
        };
        Frame::Push {
            header: push_header,
            payload: Bytes::from_static(data),
        }
    };

    harness
        .tx_to_endpoint_network
        .send(create_push(0, b"part1"))
        .await
        .unwrap();
    harness
        .tx_to_endpoint_network
        .send(create_push(1, b"part2"))
        .await
        .unwrap();

    let mut received_data = Vec::new();
    let expected_data = b"part1part2";
    while received_data.len() < expected_data.len() {
        let chunk = tokio::time::timeout(
            Duration::from_millis(100),
            harness.rx_from_endpoint_user.recv()
        ).await.expect("should receive data").unwrap();
        received_data.extend_from_slice(&chunk);
    }
    assert_eq!(received_data, expected_data);

    // The endpoint may send multiple ACKs. We loop until we find one that
    // acknowledges up to sequence number 1.
    loop {
        let ack_cmd = tokio::time::timeout(
            Duration::from_millis(100),
            harness.rx_from_endpoint_network.recv(),
        )
        .await
        .expect("Endpoint should have sent an ACK")
        .unwrap();

        assert!(!ack_cmd.frames.is_empty());
        if let Frame::Ack { header, payload } = &ack_cmd.frames[0] {
            assert_eq!(header.connection_id, server_cid);
            let sack_ranges = crate::packet::sack::decode_sack_ranges(payload.clone());
            if !sack_ranges.is_empty() && sack_ranges.iter().any(|r| r.end >= 1) {
                // Found an ACK that confirms receipt of the second packet.
                break;
            }
        }
        // If it's not the ACK we're looking for, loop again.
    }
}

#[tokio::test]
async fn test_endpoint_rto_retransmission() {
    let mut config = Config::default();
    config.initial_rto = Duration::from_millis(50);
    config.min_rto = Duration::from_millis(50);
    let mut harness = setup_endpoint_with_config(config);
    establish_connection(&mut harness).await;

    harness
        .tx_to_endpoint_user
        .send(StreamCommand::SendData(Bytes::from_static(b"rto_test")))
        .await
        .unwrap();

    let first_cmd = harness
        .rx_from_endpoint_network
        .recv()
        .await
        .expect("should receive first transmission");
    assert_eq!(first_cmd.frames.len(), 1);
    let first_seq = first_cmd.frames[0].sequence_number().unwrap();

    let retransmit_cmd = tokio::time::timeout(
        Duration::from_millis(100),
        harness.rx_from_endpoint_network.recv(),
    )
    .await
    .expect("Endpoint should have retransmitted after RTO")
    .expect("retransmit command should not be None");

    assert_eq!(retransmit_cmd.frames.len(), 1);
    let retransmit_seq = retransmit_cmd.frames[0].sequence_number().unwrap();
    assert_eq!(
        retransmit_seq, first_seq,
        "Retransmitted packet should have the same sequence number"
    );
}

#[tokio::test]
async fn test_endpoint_fast_retransmission() {
    let mut config = Config::default();
    config.fast_retx_threshold = 3;
    let mut harness = setup_endpoint_with_config(config);
    let _server_cid = establish_connection(&mut harness).await;

    let mut sent_seqs = Vec::new();
    for i in 0..5 {
        harness
            .tx_to_endpoint_user
            .send(StreamCommand::SendData(Bytes::from(format!(
                "packet-{}",
                i
            ))))
            .await
            .unwrap();
        let cmd = harness.rx_from_endpoint_network.recv().await.unwrap();
        sent_seqs.push(cmd.frames[0].sequence_number().unwrap());
    }
    assert_eq!(sent_seqs, vec![0, 1, 2, 3, 4]);

    let create_ack = |seq: u32| -> Frame {
        let ack_header = ShortHeader {
            command: crate::packet::command::Command::Ack,
            connection_id: 1,
            sequence_number: 999,
            recv_window_size: 1024,
            timestamp: 0,
            recv_next_sequence: seq, // Acknowledge up to `seq - 1`
        };
        let mut payload = bytes::BytesMut::new();
        crate::packet::sack::encode_sack_ranges(
            &[crate::packet::sack::SackRange {
                start: seq,
                end: seq,
            }],
            &mut payload,
        );
        Frame::Ack {
            header: ack_header,
            payload: payload.freeze(),
        }
    };

    // ACK packets 0, 2, 3, 4, which should trigger a fast retransmit for packet 1.
    harness
        .tx_to_endpoint_network
        .send(create_ack(sent_seqs[0]))
        .await
        .unwrap();
    harness
        .tx_to_endpoint_network
        .send(create_ack(sent_seqs[2]))
        .await
        .unwrap();
    harness
        .tx_to_endpoint_network
        .send(create_ack(sent_seqs[3]))
        .await
        .unwrap();
    harness
        .tx_to_endpoint_network
        .send(create_ack(sent_seqs[4]))
        .await
        .unwrap();

    let retransmit_cmd = tokio::time::timeout(
        Duration::from_millis(100),
        harness.rx_from_endpoint_network.recv(),
    )
    .await
    .expect("Endpoint should have fast-retransmitted")
    .unwrap();

    assert_eq!(retransmit_cmd.frames.len(), 1);
    let retransmit_seq = retransmit_cmd.frames[0].sequence_number().unwrap();
    assert_eq!(
        retransmit_seq, sent_seqs[1],
        "Should have retransmitted packet with seq 1"
    );
} 