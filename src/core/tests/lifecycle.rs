//! Tests for connection lifecycle, including establishment, data transfer, and migration.

use crate::{
    core::{
        endpoint::StreamCommand,
        test_utils::{setup_client_server_pair, setup_server_harness},
    },
    packet::frame::Frame,
    socket::SenderTaskCommand,
};
use bytes::Bytes;
use std::time::Duration;

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
    let payload = Bytes::from_static(b"data from new address");
    let push_from_new_addr = Frame::new_push(2, 10, 0, 1024, 0, payload);
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
    let response_frame =
        Frame::new_path_response(2, 999, 0, challenge_data); // Timestamps aren't critical here
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