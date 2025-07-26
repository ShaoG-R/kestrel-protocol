//! Tests for 1-RTT and 0-RTT connection handshakes.

use crate::core::test_utils::init_tracing;
use crate::{
    config::Config,
    core::{endpoint::StreamCommand, test_utils::setup_server_harness},
    packet::frame::Frame,
    socket::SenderTaskCommand,
};
use bytes::Bytes;
use std::time::Duration;

#[tokio::test]
async fn test_1rtt_handshake_with_server_data() {
    init_tracing();
    // This test simulates a 1-RTT handshake where the server application
    // sends data immediately upon accepting a connection, which is coalesced
    // with the SYN-ACK frame.

    // 1. Setup a server harness. It will be in `SynReceived` state,
    //    waiting for the application to "accept" the connection.
    let mut harness = setup_server_harness();
    let server_cid = harness.server_cid;
    let client_addr = harness.client_addr;
    let client_cid = 12345; // A random CID chosen by the "client".

    // 2. Simulate the client sending a SYN packet.
    // In a real scenario, dest_cid would be 0. The harness endpoint doesn't check it,
    // as it assumes the SocketActor has already routed the frame correctly.
    let syn_frame = Frame::new_syn(Config::default().protocol_version, client_cid, 0);

    // Before the endpoint receives the SYN, we need to update its peer_cid,
    // which is normally done by the SocketActor upon receiving the first SYN.
    harness
        .tx_to_endpoint_user
        .send(StreamCommand::UpdatePeerCid(client_cid))
        .await
        .unwrap();

    harness
        .tx_to_endpoint_network
        .send((syn_frame, client_addr))
        .await
        .unwrap();

    // The endpoint is now in SynReceived. It won't send a SYN-ACK until
    // the application layer tries to send data (the "accept" trigger).
    harness
        .tx_to_endpoint_user
        .send(StreamCommand::SendData(Bytes::from_static(b"accept")))
        .await
        .unwrap();

    // 3. The server should now send a SYN-ACK back to the client.
    let syn_ack_cmd = tokio::time::timeout(
        Duration::from_millis(50),
        harness.rx_from_endpoint_network.recv(),
    )
    .await
    .expect("Server should send a SYN-ACK")
    .unwrap();

    // 4. Verify the SYN-ACK and PUSH frame contents.
    if let SenderTaskCommand::Send(cmd) = syn_ack_cmd {
        assert_eq!(cmd.remote_addr, client_addr);
        // We now expect two frames due to coalescing: SYN-ACK and PUSH.
        assert_eq!(cmd.frames.len(), 2);

        // Verify the first frame is the SYN-ACK.
        if let Frame::SynAck { header, .. } = &cmd.frames[0] {
            // The server's response must be directed to the client's CID.
            assert_eq!(
                header.destination_cid, client_cid,
                "SYN-ACK destination_cid should be the client's source_cid"
            );
            // The server must identify itself with its own CID.
            assert_eq!(
                header.source_cid, server_cid,
                "SYN-ACK source_cid should be the server's own CID"
            );
        } else {
            panic!(
                "Expected the first frame to be a SYN-ACK, but got {:?}",
                cmd.frames[0]
            );
        }

        // Verify the second frame is the PUSH.
        if !matches!(&cmd.frames[1], Frame::Push { .. }) {
            panic!(
                "Expected the second frame to be a PUSH, but got {:?}",
                cmd.frames[1]
            );
        }
    } else {
        panic!("Expected a Send command, but got {:?}", syn_ack_cmd);
    }
}

#[tokio::test]
async fn test_0rtt_handshake_with_client_data() {
    init_tracing();
    // This test simulates a 0-RTT handshake where the client sends data
    // immediately with its initial SYN packet.

    // 1. Setup a server harness.
    let mut harness = setup_server_harness();
    let client_addr = harness.client_addr;
    let client_cid = 12345;

    // 2. Simulate the client sending a SYN and a PUSH frame in one go.
    // The SocketActor would decode these from a single datagram.
    let syn_frame = Frame::new_syn(Config::default().protocol_version, client_cid, 0);
    let push_frame = Frame::new_push(
        0, // dest_cid is 0, actor would route by address.
        0, // sequence number
        0, // recv_next_sequence
        1024,
        0,
        Bytes::from_static(b"hello 0-rtt"),
    );

    // 3. The test acts as the SocketActor.
    // First, inform the new Endpoint of its peer's CID.
    harness
        .tx_to_endpoint_user
        .send(StreamCommand::UpdatePeerCid(client_cid))
        .await
        .unwrap();
    // Then, send the frames as if they were decoded from a datagram.
    harness
        .tx_to_endpoint_network
        .send((syn_frame, client_addr))
        .await
        .unwrap();
    harness
        .tx_to_endpoint_network
        .send((push_frame, client_addr))
        .await
        .unwrap();

    // 4. The 0-RTT data should be immediately available for the server app to read.
    let received_data = tokio::time::timeout(
        Duration::from_millis(50),
        harness.rx_from_endpoint_user.recv(),
    )
    .await
    .expect("Server application should have received 0-RTT data")
    .unwrap();
    assert_eq!(received_data.len(), 1);
    assert_eq!(received_data[0], "hello 0-rtt");

    // 5. The server app can now write a response, which triggers the SYN-ACK.
    harness
        .tx_to_endpoint_user
        .send(StreamCommand::SendData(Bytes::from_static(b"ack 0-rtt")))
        .await
        .unwrap();

    // 6. Verify the server sends a SYN-ACK and the response data.
    let response_cmd = tokio::time::timeout(
        Duration::from_millis(50),
        harness.rx_from_endpoint_network.recv(),
    )
    .await
    .expect("Server should send a response")
    .unwrap();

    if let SenderTaskCommand::Send(cmd) = response_cmd {
        assert_eq!(cmd.frames.len(), 2, "Expected SYN-ACK and PUSH");
        assert!(matches!(&cmd.frames[0], Frame::SynAck { .. }));
        if let Frame::Push { payload, .. } = &cmd.frames[1] {
            assert_eq!(payload, "ack 0-rtt");
        } else {
            panic!("Expected second frame to be PUSH");
        }
    } else {
        panic!("Expected a Send command");
    }
}
