//! Test for the bidirectional Connection ID (CID) handshake.

use crate::{
    core::{endpoint::StreamCommand, test_utils::setup_server_harness},
    packet::{command::Command, frame::Frame, header::LongHeader},
    socket::SenderTaskCommand,
};
use bytes::Bytes;
use std::time::Duration;

#[tokio::test]
async fn test_cid_handshake() {
    // 1. Setup a server harness. It will be in `SynReceived` state,
    //    waiting for the application to "accept" the connection.
    let mut harness = setup_server_harness();
    let server_cid = harness.server_cid;
    let client_addr = harness.client_addr;
    let client_cid = 12345; // A random CID chosen by the "client".

    // 2. Simulate the client sending a SYN packet.
    // This is normally done by the SocketActor, but we do it manually here.
    let syn_frame = Frame::Syn {
        header: LongHeader {
            command: Command::Syn,
            protocol_version: 0,
            payload_length: 0,
            destination_cid: server_cid, // The client might know the server's CID. Let's use it.
            source_cid: client_cid,
        },
        payload: Bytes::new(), // No 0-RTT data for this test.
    };

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

    // 4. Verify the SYN-ACK contents.
    if let SenderTaskCommand::Send(cmd) = syn_ack_cmd {
        assert_eq!(cmd.remote_addr, client_addr);
        assert_eq!(cmd.frames.len(), 1);

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
            panic!("Expected a SYN-ACK frame, but got {:?}", cmd.frames[0]);
        }
    } else {
        panic!("Expected a Send command, but got {:?}", syn_ack_cmd);
    }
}
