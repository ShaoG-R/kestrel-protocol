//! Tests for retransmission logic (RTO and Fast Retransmission).

use crate::{
    config::Config,
    core::test_utils::{init_tracing, new_stream_pair_with_filter},
    packet::frame::Frame,
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::info;

/// This test specifically validates the RTO (Retransmission Timeout) mechanism.
///
/// It creates a scenario where the client's first data packet (`seq=0`) is
/// intentionally lost. Because no subsequent packets are sent by the client immediately
/// after, the server won't generate SACKs that could trigger a Fast Retransmission.
/// The connection's only way to recover is for the client's RTO timer to fire,
/// prompting it to re-send the lost packet.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_rto_recovery_on_single_packet_loss() {
    init_tracing();
    info!("--- RTO Recovery Test ---");

    // 1. Configure a very short RTO for the client to ensure the test runs quickly.
    let mut client_config = Config::default();
    client_config.initial_rto = Duration::from_millis(200);
    client_config.min_rto = Duration::from_millis(200);

    // 2. Create a filter that drops the first PUSH packet from the client AFTER handshake.
    let handshake_complete = Arc::new(AtomicBool::new(false));
    let packet_dropped = Arc::new(AtomicBool::new(false));
    let dropper = {
        let packet_dropped = packet_dropped.clone();
        let handshake_complete = handshake_complete.clone();
        move |frame: &Frame| -> bool {
            // The handshake is considered complete once the client sends an ACK for the SYN-ACK.
            if let Frame::Ack { .. } = frame {
                handshake_complete.store(true, Ordering::SeqCst);
            }

            if handshake_complete.load(Ordering::SeqCst) {
                if let Frame::Push { .. } = frame {
                    if !packet_dropped.load(Ordering::SeqCst) {
                        info!(seq = frame.sequence_number().unwrap(), "Intentionally dropping client data packet post-handshake");
                        packet_dropped.store(true, Ordering::SeqCst);
                        return false; // Drop the packet
                    }
                }
            }
            true // Keep all other packets
        }
    };

    // 3. Create a connection pair with the custom config and packet dropper.
    let (mut client_stream, mut server_stream) =
        new_stream_pair_with_filter(client_config, Config::default(), dropper).await;

    // 4. HANDSHAKE: Server writes first to send a SYN-ACK and establish the connection.
    let server_probe = b"probe";
    info!("[Server] Sending probe to establish connection...");
    server_stream.write_all(server_probe).await.unwrap();
    server_stream.flush().await.unwrap();

    let mut client_buf = vec![0; server_probe.len()];
    client_stream
        .read_exact(&mut client_buf)
        .await
        .expect("Client should receive server probe");
    assert_eq!(&client_buf, server_probe);
    info!("[Client] Handshake complete.");

    // 5. RTO TEST: Client sends a message. The underlying packet will be dropped.
    info!("[Client] Writing message...");
    let client_message = b"hello, this is a test message";
    client_stream.write_all(client_message).await.unwrap();
    client_stream.flush().await.unwrap();

    // 6. Server attempts to read the data.
    // This should initially block, waiting for the client's RTO to fire and
    // retransmit the lost packet.
    info!("[Server] Reading message...");
    let mut server_buf = vec![0; client_message.len()];
    server_stream
        .read_exact(&mut server_buf)
        .await
        .expect("Server should successfully read data after RTO recovery");

    assert_eq!(&server_buf, client_message);
    info!("[Server] Successfully received message after RTO.");

    // 7. Verify the connection still works in the other direction.
    info!("[Server] Sending response...");
    let server_response = b"ack";
    server_stream.write_all(server_response).await.unwrap();
    server_stream.flush().await.unwrap();

    let mut client_buf = vec![0; server_response.len()];
    client_stream.read_exact(&mut client_buf).await.unwrap();
    assert_eq!(&client_buf, server_response);
    info!("[Client] Successfully received server response.");

    info!("--- RTO Recovery Test Passed ---");
}
