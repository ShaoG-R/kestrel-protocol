//! Tests for the connection shutdown logic.

use crate::core::test_utils::{setup_client_server_pair, setup_server_harness, MockUdpSocket};
use crate::{
    core::endpoint::StreamCommand,
    packet::{frame::Frame, header::LongHeader, command::Command},
};
use bytes::Bytes;
use std::{net::SocketAddr, time::Duration};
use tokio::{sync::oneshot, time::timeout};

#[tokio::test]
async fn test_shutdown_when_established() {
    // Standard case: closing an established connection should send a FIN.
    let (client, mut server) = setup_client_server_pair();

    // 1. Establish connection by having server send a SYN-ACK trigger
    client
        .tx_to_endpoint_user
        .send(StreamCommand::SendData(Bytes::from("hello")))
        .await
        .unwrap();
    // Trigger SYN-ACK from server to establish connection
    server
        .tx_to_endpoint_user
        .send(StreamCommand::SendData(Bytes::new()))
        .await
        .unwrap();
    let received = timeout(Duration::from_secs(1), server.rx_from_endpoint_user.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received[0], "hello");

    // 2. Client initiates shutdown
    client
        .tx_to_endpoint_user
        .send(StreamCommand::Close)
        .await
        .unwrap();

    // 3. Verify server receives a FIN frame.
    // The test setup's relay task will forward the frame from the mock socket to the server endpoint.
    // We can't directly inspect the frame here without a more complex harness,
    // but we can verify the outcome: the server stream should be closed for reading.
    let server_read_result =
        timeout(Duration::from_secs(1), server.rx_from_endpoint_user.recv()).await;
    assert!(
        server_read_result.is_ok(),
        "Server should receive notification that the stream has closed"
    );
    assert!(
        server_read_result.unwrap().is_none(),
        "Reading from a stream after peer closes write-end should yield None (EOF)"
    );
}

#[tokio::test]
async fn test_shutdown_when_connecting() {
    // We create a client endpoint MANUALLY.
    // To prevent `send_initial_syn` from erroring on a closed channel, we create a
    // dummy receiver task that just drains messages.
    let (sender_task_tx, mut sender_task_rx) = tokio::sync::mpsc::channel(32);
    tokio::spawn(async move {
        while sender_task_rx.recv().await.is_some() {}
    });

    let (mut client_endpoint, tx_to_stream, _) =
        crate::core::endpoint::Endpoint::<MockUdpSocket>::new_client(
            Default::default(),
            "127.0.0.1:1234".parse().unwrap(),
            1,
            tokio::sync::mpsc::channel(32).1, // dummy rx
            sender_task_tx,                   // real tx
            tokio::sync::mpsc::channel(32).0, // dummy tx
            None,
        );

    // Send a close command to the connecting client.
    tx_to_stream.send(StreamCommand::Close).await.unwrap();

    // Run the endpoint. With the new logic, it should immediately move to `Closed` and exit.
    // If it hangs, the test will time out, indicating the logic is flawed.
    let run_result = timeout(Duration::from_millis(100), client_endpoint.run()).await;

    assert!(
        run_result.is_ok(),
        "Endpoint should not time out when closing from `Connecting` state"
    );
    assert!(
        run_result.unwrap().is_ok(),
        "Endpoint run loop should exit cleanly"
    );
}

#[tokio::test]
async fn test_shutdown_when_syn_received() {
    // Test closing a server connection before the app `accepts` it (i.e., sends data).
    let mut harness = setup_server_harness();

    // 1. Manually send a SYN to the server to put it in `SynReceived` state.
    let syn_frame = Frame::Syn {
        header: LongHeader {
            protocol_version: 0,
            command: Command::Syn,
            source_cid: harness.server_cid, // client's idea of our CID
            destination_cid: 1,             // client's CID
        },
        payload: Bytes::new(),
    };
    harness
        .tx_to_endpoint_network
        .send((syn_frame, harness.client_addr))
        .await
        .unwrap();

    // Give the endpoint a moment to process the SYN.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // 2. Before the app sends any data (which would trigger SYN-ACK), close the stream.
    harness
        .tx_to_endpoint_user
        .send(StreamCommand::Close)
        .await
        .unwrap();

    // 3. Verify the endpoint task terminates quickly.
    // The user stream receiver will be dropped. We wait for it to return None.
    let user_stream_result =
        timeout(Duration::from_millis(100), harness.rx_from_endpoint_user.recv()).await;

    assert!(
        user_stream_result.is_ok(),
        "Endpoint task should close the user stream receiver promptly"
    );
    assert!(
        user_stream_result.unwrap().is_none(),
        "User stream should be closed without receiving any data"
    );
}

#[tokio::test]
async fn test_shutdown_when_validating_path() {
    let (client, mut server) = setup_client_server_pair();

    // 1. Establish connection
    client
        .tx_to_endpoint_user
        .send(StreamCommand::SendData(Bytes::from("hello")))
        .await
        .unwrap();
    // Trigger SYN-ACK from server to establish connection
    server
        .tx_to_endpoint_user
        .send(StreamCommand::SendData(Bytes::new()))
        .await
        .unwrap();
    timeout(Duration::from_secs(1), server.rx_from_endpoint_user.recv())
        .await
        .unwrap();

    // 2. Initiate path migration to put the client in `ValidatingPath` state.
    let new_addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
    let (tx, rx) = oneshot::channel();
    client
        .tx_to_endpoint_user
        .send(StreamCommand::Migrate {
            new_addr,
            notifier: tx,
        })
        .await
        .unwrap();

    // Give it a moment to enter the state.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // 3. Immediately close the stream.
    client
        .tx_to_endpoint_user
        .send(StreamCommand::Close)
        .await
        .unwrap();

    // 4. The endpoint should abort immediately, not wait for the migration timeout.
    // The migration notifier should receive an error indicating the connection was closed.
    let migration_result = timeout(Duration::from_millis(100), rx).await;

    // We expect the endpoint to close promptly, cancelling the migration.
    // This means the timeout should NOT be hit, and the inner result from the
    // oneshot channel should be an error because the sender was dropped.
    assert!(
        migration_result.is_ok(),
        "Timeout should not be hit; the endpoint should close immediately."
    );
    assert!(
        migration_result.unwrap().is_err(),
        "Migration should be cancelled by the close, causing a channel receive error."
    );
} 