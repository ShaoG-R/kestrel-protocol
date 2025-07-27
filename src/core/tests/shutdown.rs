//! Tests for the connection shutdown logic.

use crate::core::test_utils::{setup_client_server_pair, setup_server_harness, MockUdpSocket};
use crate::{
    core::endpoint::StreamCommand,
    packet::frame::Frame,
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
    let syn_frame = Frame::new_syn(
        0,
        1, // client's CID
        harness.server_cid,
    );
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

#[tokio::test]
async fn test_simultaneous_close() {
    // Both client and server initiate shutdown at the same time.
    // They should both go through Closing -> ClosingWait -> Closed.
    let (mut client, mut server) = setup_client_server_pair();

    // 1. Establish connection
    client
        .tx_to_endpoint_user
        .send(StreamCommand::SendData(Bytes::from("ping")))
        .await
        .unwrap();
    server
        .tx_to_endpoint_user
        .send(StreamCommand::SendData(Bytes::from("pong")))
        .await
        .unwrap();

    let client_recv = timeout(
        Duration::from_secs(1),
        client.rx_from_endpoint_user.recv(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(client_recv[0], "pong");
    let server_recv = timeout(
        Duration::from_secs(1),
        server.rx_from_endpoint_user.recv(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(server_recv[0], "ping");

    // 2. Both initiate shutdown
    let (client_close_res, server_close_res) = tokio::join!(
        client.tx_to_endpoint_user.send(StreamCommand::Close),
        server.tx_to_endpoint_user.send(StreamCommand::Close)
    );
    client_close_res.unwrap();
    server_close_res.unwrap();

    // 3. Verify both streams are closed cleanly (EOF)
    let client_read_result =
        timeout(Duration::from_secs(1), client.rx_from_endpoint_user.recv()).await;
    let server_read_result =
        timeout(Duration::from_secs(1), server.rx_from_endpoint_user.recv()).await;

    assert!(client_read_result.is_ok(), "Client should not time out");
    assert!(
        client_read_result.unwrap().is_none(),
        "Client stream should receive EOF"
    );

    assert!(server_read_result.is_ok(), "Server should not time out");
    assert!(
        server_read_result.unwrap().is_none(),
        "Server stream should receive EOF"
    );
}

#[tokio::test]
async fn test_shutdown_from_fin_wait() {
    crate::core::test_utils::init_tracing();
    // Client closes, server enters FinWait, then server closes.
    let (mut client, mut server) = setup_client_server_pair();

    // 1. Establish connection
    client
        .tx_to_endpoint_user
        .send(StreamCommand::SendData(Bytes::from("ping")))
        .await
        .unwrap();
    server
        .tx_to_endpoint_user
        .send(StreamCommand::SendData(Bytes::from("pong")))
        .await
        .unwrap();
    let client_recv = timeout(
        Duration::from_secs(1),
        client.rx_from_endpoint_user.recv(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(client_recv[0], "pong");
    let server_recv = timeout(
        Duration::from_secs(1),
        server.rx_from_endpoint_user.recv(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(server_recv[0], "ping");

    // 2. Client initiates shutdown
    client
        .tx_to_endpoint_user
        .send(StreamCommand::Close)
        .await
        .unwrap();

    // 3. Server should receive EOF, indicating it has entered FinWait.
    let server_read_result =
        timeout(Duration::from_secs(1), server.rx_from_endpoint_user.recv()).await;
    assert!(
        server_read_result.is_ok(),
        "Server should receive close notification"
    );
    assert!(
        server_read_result.unwrap().is_none(),
        "Server stream should receive EOF"
    );

    // 4. Server is now in FinWait. Now it decides to close too.
    server
        .tx_to_endpoint_user
        .send(StreamCommand::Close)
        .await
        .unwrap();

    // 5. Client, which was in Closing, should now receive the server's FIN,
    // transition to ClosingWait, and then fully close. Its receiver will yield None.
    let client_read_result =
        timeout(Duration::from_secs(1), client.rx_from_endpoint_user.recv()).await;
    assert!(client_read_result.is_ok(), "Client should not time out");
    assert!(
        client_read_result.unwrap().is_none(),
        "Client stream was already closing, and should now be fully terminated."
    );

    // Give a moment for tasks to fully terminate.
    tokio::time::sleep(Duration::from_millis(50)).await;
}

#[tokio::test]
async fn test_data_is_fully_read_before_shutdown_eof() {
    crate::core::test_utils::init_tracing();
    // This test targets the race condition where a PUSH and FIN arrive in the
    // same batch. The receiver must process the PUSH data fully before the
    // user stream receives the EOF signal from the FIN.
    let (client, mut server) = setup_client_server_pair();

    // 1. Establish connection (can be done implicitly by sending data)
    let test_data = Bytes::from("important data that must not be lost");
    
    // 2. Client sends data and immediately requests to close the stream.
    // This makes it highly likely that the resulting PUSH and FIN frames
    // will be processed by the server endpoint in the same event loop tick.
    client
        .tx_to_endpoint_user
        .send(StreamCommand::SendData(test_data.clone()))
        .await
        .unwrap();
    client
        .tx_to_endpoint_user
        .send(StreamCommand::Close)
        .await
        .unwrap();

    // 3. Server must receive the data first.
    let server_recv_data =
        timeout(Duration::from_secs(1), server.rx_from_endpoint_user.recv())
            .await
            .expect("Server should receive the data packet before timeout")
            .expect("Server's stream should not be closed yet")
            .into_iter()
            .flatten()
            .collect::<Bytes>();

    assert_eq!(server_recv_data, test_data, "The received data did not match what was sent.");

    // 4. After the data is read, the *next* read should signal EOF.
    // Our deferred EOF logic ensures the user channel is closed only after
    // the receive buffer is drained.
    let server_recv_eof =
        timeout(Duration::from_secs(1), server.rx_from_endpoint_user.recv())
            .await
            .expect("Server should not time out waiting for EOF")
            .is_none();
    
    assert!(server_recv_eof, "Server stream should now be closed (EOF)");
}

#[tokio::test]
async fn test_retransmission_after_fin_is_ignored() {
    crate::core::test_utils::init_tracing();
    // This test verifies that once a FIN has been processed by the receiver,
    // any subsequent (spurious) retransmissions of packets that came before
    // that FIN are ignored.
    use crate::packet::{command::Command, header::ShortHeader};

    // 1. Setup a server harness, which allows us to manually inject frames.
    let mut harness = setup_server_harness();

    // 2. The server starts in `SynReceived`. Trigger a transition to `Established`
    // by having the "user" send data, which causes a SYN-ACK to be sent.
    harness
        .tx_to_endpoint_user
        .send(StreamCommand::SendData(Bytes::new()))
        .await
        .unwrap();
    // Consume the SYN-ACK from the network out-queue to confirm state transition.
    assert!(
        timeout(
            Duration::from_millis(100),
            harness.rx_from_endpoint_network.recv()
        )
        .await
        .is_ok(),
        "Server should have sent a SYN-ACK"
    );

    // 3. Manually craft and send a PUSH frame followed by a FIN frame.
    let test_data = Bytes::from("final data");
    let push_frame = Frame::Push {
        header: ShortHeader {
            command: Command::Push,
            connection_id: harness.server_cid,
            payload_length: test_data.len() as u16,
            recv_window_size: 1024,
            timestamp: 100,
            sequence_number: 0,
            recv_next_sequence: 0,
        },
        payload: test_data.clone(),
    };

    let fin_frame = Frame::Fin {
        header: ShortHeader {
            command: Command::Fin,
            connection_id: harness.server_cid,
            payload_length: 0,
            recv_window_size: 1024,
            timestamp: 101,
            sequence_number: 1,
            recv_next_sequence: 0,
        },
    };

    harness
        .tx_to_endpoint_network
        .send((push_frame.clone(), harness.client_addr))
        .await
        .unwrap();
    harness
        .tx_to_endpoint_network
        .send((fin_frame, harness.client_addr))
        .await
        .unwrap();

    // 4. The server should process the data and then receive an EOF from the FIN.
    let received_data = timeout(
        Duration::from_millis(200),
        harness.rx_from_endpoint_user.recv(),
    )
    .await
    .expect("Should receive data")
    .expect("Stream should not be closed yet")
    .into_iter()
    .flatten()
    .collect::<Bytes>();
    assert_eq!(received_data, test_data);

    let eof = timeout(
        Duration::from_millis(200),
        harness.rx_from_endpoint_user.recv(),
    )
    .await
    .expect("Should receive EOF signal")
    .is_none();
    assert!(eof, "Stream should be closed with EOF");

    // At this point, the server's recv_buffer has `fin_reached = true`.

    // IMPORTANT: Consume the two legitimate ACKs that were sent for the
    // initial PUSH and FIN frames.
    assert!(harness.rx_from_endpoint_network.recv().await.is_some(), "Should have received ACK for PUSH");
    assert!(harness.rx_from_endpoint_network.recv().await.is_some(), "Should have received ACK for FIN");

    // 5. Send a spurious retransmission of the first PUSH packet.
    harness
        .tx_to_endpoint_network
        .send((push_frame, harness.client_addr))
        .await
        .unwrap();

    // 6. Verify that the endpoint ignores this packet completely.
    //    - No more data should be sent to the user stream (checked by timeout).
    //    - No more packets (e.g., ACKs) should be sent to the network.
    let spurious_ack = timeout(
        Duration::from_millis(100),
        harness.rx_from_endpoint_network.recv(),
    )
    .await;
    assert!(
        spurious_ack.is_err(),
        "Server should not send any packets in response to a retransmission after FIN"
    );
} 