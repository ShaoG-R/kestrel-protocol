//! Tests for reliability mechanisms like ACKs, RTO, and fast retransmission.

use crate::{
    config::Config,
    core::{
        endpoint::StreamCommand,
        test_utils::{setup_client_server_pair, setup_client_server_with_filter},
    },
    packet::frame::Frame,
};
use bytes::Bytes;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

#[tokio::test]
async fn test_data_flow_with_acks() {
    let (mut client, mut server) = setup_client_server_pair();

    // --- Establish Connection ---
    server
        .tx_to_endpoint_user
        .send(StreamCommand::SendData(Bytes::new()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Send a few packets from client to server
    let mut client_sent_data = Vec::new();
    for i in 0..5 {
        let data = Bytes::from(format!("c-packet-{}", i));
        client_sent_data.extend_from_slice(&data);
        client
            .tx_to_endpoint_user
            .send(StreamCommand::SendData(data))
            .await
            .unwrap();
    }

    // Receive them on the server to ensure they all arrived by checking total bytes.
    let mut server_recv_data = Vec::new();
    while server_recv_data.len() < client_sent_data.len() {
        let chunks = tokio::time::timeout(
            Duration::from_millis(200),
            server.rx_from_endpoint_user.recv(),
        )
        .await
        .expect("Server should receive data")
        .unwrap();
        for chunk in chunks {
            server_recv_data.extend_from_slice(&chunk);
        }
    }
    assert_eq!(server_recv_data, client_sent_data);

    // Now send from server to client
    let mut server_sent_data = Vec::new();
    for i in 0..3 {
        let data = Bytes::from(format!("s-packet-{}", i));
        server_sent_data.extend_from_slice(&data);
        server
            .tx_to_endpoint_user
            .send(StreamCommand::SendData(data))
            .await
            .unwrap();
    }

    // Receive them on the client by checking total bytes.
    let mut client_recv_data = Vec::new();
    while client_recv_data.len() < server_sent_data.len() {
        let chunks = tokio::time::timeout(
            Duration::from_millis(200),
            client.rx_from_endpoint_user.recv(),
        )
        .await
        .expect("Client should receive data")
        .unwrap();
        for chunk in chunks {
            client_recv_data.extend_from_slice(&chunk);
        }
    }
    assert_eq!(client_recv_data, server_sent_data);
}

#[tokio::test]
async fn test_endpoint_rto_retransmission() {
    let mut client_config = Config::default();
    client_config.reliability.initial_rto = Duration::from_millis(100);
    client_config.reliability.min_rto = Duration::from_millis(100);

    // Filter to drop all ACK packets sent from the server.
    let server_tx_filter = Arc::new(|frame: &Frame| -> bool { !matches!(frame, Frame::Ack { .. }) });
    // Client filter allows all packets.
    let client_tx_filter = Arc::new(|_: &Frame| -> bool { true });

    let (mut client, mut server, client_sent_count, _server_sent_count) =
        setup_client_server_with_filter(
            client_config,
            Config::default(),
            client_tx_filter,
            server_tx_filter,
            None,
        );

    // Establish connection.
    server
        .tx_to_endpoint_user
        .send(StreamCommand::SendData(Bytes::new()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    // Drain the empty data from the client side that was used to trigger the SYN-ACK.
    let _ = client.rx_from_endpoint_user.try_recv();

    // Client sends data.
    let data_to_send = Bytes::from_static(b"rto test data");
    client
        .tx_to_endpoint_user
        .send(StreamCommand::SendData(data_to_send.clone()))
        .await
        .unwrap();

    // Server should receive the packet the first time.
    let mut received_data_1 = Vec::new();
    let chunks = tokio::time::timeout(
        Duration::from_millis(50),
        server.rx_from_endpoint_user.recv(),
    )
    .await
    .expect("Server should receive the first packet transmission")
    .unwrap();
    for chunk in chunks {
        received_data_1.extend_from_slice(&chunk);
    }
    assert_eq!(received_data_1, data_to_send);

    // At this point, the client has sent some number of packets for the handshake and the data.
    // Let's capture this count. We need a small delay to ensure the PUSH is sent.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let packets_sent_before_rto = client_sent_count.load(Ordering::Relaxed);
    if packets_sent_before_rto == 0 {
        panic!("Expected at least the PUSH packet to be sent. Got 0");
    }

    // Now, wait for the RTO to expire. The client should retransmit the PUSH.
    tokio::time::sleep(Duration::from_millis(200)).await; // RTO is 100ms

    let packets_sent_after_rto = client_sent_count.load(Ordering::Relaxed);

    assert_eq!(
        packets_sent_after_rto,
        packets_sent_before_rto + 1,
        "Client should have retransmitted exactly one packet after RTO timeout"
    );

    // Verify the server does NOT receive the data again at the application layer.
    let retransmit_recv_result =
        tokio::time::timeout(Duration::from_millis(50), server.rx_from_endpoint_user.recv()).await;
    assert!(
        retransmit_recv_result.is_err(),
        "Server should not receive the retransmitted data at the application layer"
    );
}

#[tokio::test]
async fn test_server_0rtt_sends_data_correctly() {
    let (mut client, server) = setup_client_server_pair();

    // 1. Server immediately sends two batches of data.
    // The first one triggers the SYN-ACK + PUSH (0-RTT) path.
    // The second one should be a normal PUSH with the correct next sequence number.
    let server_data1 = Bytes::from_static(b"server-0rtt");
    let server_data2 = Bytes::from_static(b"server-next");
    server
        .tx_to_endpoint_user
        .send(StreamCommand::SendData(server_data1.clone()))
        .await
        .unwrap();
    server
        .tx_to_endpoint_user
        .send(StreamCommand::SendData(server_data2.clone()))
        .await
        .unwrap();

    // 2. Client receives the data.
    // We expect to receive both chunks, concatenated.
    let mut client_recv_data = Vec::new();
    let expected_len = server_data1.len() + server_data2.len();
    while client_recv_data.len() < expected_len {
        let chunks = tokio::time::timeout(
            Duration::from_millis(200),
            client.rx_from_endpoint_user.recv(),
        )
        .await
        .expect("Client should receive server's 0-RTT and subsequent data")
        .unwrap();
        for chunk in chunks {
            client_recv_data.extend_from_slice(&chunk);
        }
    }

    // 3. Verify the data is correct.
    let mut expected_data = Vec::new();
    expected_data.extend_from_slice(&server_data1);
    expected_data.extend_from_slice(&server_data2);
    assert_eq!(client_recv_data, expected_data);
}

#[tokio::test]
async fn test_endpoint_fast_retransmission() {
    // Filter to drop the PUSH packet with sequence number 1, just once.
    let packet_to_drop_seq = 1;
    let packet_has_been_dropped = Arc::new(AtomicBool::new(false));
    let client_tx_filter = Arc::new(move |frame: &Frame| -> bool {
        if let Frame::Push { header, .. } = frame {
            if header.sequence_number == packet_to_drop_seq
                && !packet_has_been_dropped.swap(true, Ordering::Relaxed)
            {
                return false; // Drop packet
            }
        }
        true // Keep all other packets
    });
    let server_tx_filter = Arc::new(|_: &Frame| -> bool { true });

    let (mut client, mut server, _, _) = setup_client_server_with_filter(
        Config::default(),
        Config::default(),
        client_tx_filter,
        server_tx_filter,
        None,
    );

    // Establish connection.
    server
        .tx_to_endpoint_user
        .send(StreamCommand::SendData(Bytes::new()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let _ = client.rx_from_endpoint_user.try_recv();

    // Client sends 5 packets. Packet with seq=1 will be dropped by the filter.
    let mut sent_data = Vec::new();
    for i in 0..5 {
        let data = Bytes::from(format!("packet-{}", i));
        sent_data.push(data.clone());
        client
            .tx_to_endpoint_user
            .send(StreamCommand::SendData(data))
            .await
            .unwrap();
    }

    // Server should receive packets 0, 2, 3, 4 first, then the retransmitted packet 1.
    // The reassemble buffer will wait for packet 1 before delivering anything to the user.
    let mut all_received_data = Vec::new();
    let total_len: usize = sent_data.iter().map(|d| d.len()).sum();

    while all_received_data.len() < total_len {
        let chunks =
            tokio::time::timeout(Duration::from_millis(200), server.rx_from_endpoint_user.recv())
                .await
                .expect("Server should eventually receive all data")
                .unwrap();
        for chunk in chunks {
            all_received_data.extend_from_slice(&chunk);
        }
    }

    // Verify that all data was received correctly and in order.
    let expected_data: Vec<u8> = sent_data.into_iter().flatten().collect();
    assert_eq!(all_received_data, expected_data);
} 