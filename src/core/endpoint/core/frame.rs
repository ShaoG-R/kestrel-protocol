//! Contains factory functions for creating different types of frames.
//!
//! 包含用于创建不同类型帧的工厂函数。

use tokio::time::Instant;

use crate::{
    config::Config,
    core::reliability::UnifiedReliabilityLayer,
    packet::frame::Frame
};

/// Creates a SYN frame.
pub(in crate::core::endpoint) fn create_syn_frame(config: &Config, local_cid: u32) -> Frame {
    Frame::new_syn(config.protocol_version, local_cid, 0)
}

/// Creates a SYN-ACK frame.
pub(in crate::core::endpoint) fn create_syn_ack_frame(
    config: &Config,
    peer_cid: u32,
    local_cid: u32,
) -> Frame {
    Frame::new_syn_ack(config.protocol_version, local_cid, peer_cid)
}

/// Creates a standalone ACK frame.
pub(in crate::core::endpoint) fn create_ack_frame(
    peer_cid: u32,
    reliability: &mut UnifiedReliabilityLayer,
    start_time: Instant,
) -> Frame {
    let (sack_ranges, recv_next, window_size) = reliability.get_ack_info();
    let timestamp = Instant::now().duration_since(start_time).as_millis() as u32;
    Frame::new_ack(
        peer_cid,
        recv_next,
        window_size,
        &sack_ranges,
        timestamp,
    )
}

/// Creates a FIN frame.
pub(in crate::core::endpoint) fn create_fin_frame(
    peer_cid: u32,
    sequence_number: u32,
    reliability: &UnifiedReliabilityLayer,
    start_time: Instant,
) -> Frame {
    let timestamp = Instant::now().duration_since(start_time).as_millis() as u32;
    // A FIN frame should also carry the latest acknowledgment information, just like a PUSH frame.
    // This is the last chance to acknowledge any packets received before closing.
    let (_, recv_next_sequence, recv_window_size) = reliability.get_ack_info();
    Frame::new_fin(
        peer_cid,
        sequence_number,
        timestamp,
        recv_next_sequence,
        recv_window_size,
    )
}

/// Creates a PATH_CHALLENGE frame.
pub(crate) fn create_path_challenge_frame(
    peer_cid: u32,
    sequence_number: u32,
    start_time: Instant,
    challenge_data: u64,
) -> Frame {
    let timestamp = Instant::now().duration_since(start_time).as_millis() as u32;
    Frame::new_path_challenge(peer_cid, sequence_number, timestamp, challenge_data)
}

/// Creates a PATH_RESPONSE frame.
pub(crate) fn create_path_response_frame(
    peer_cid: u32,
    sequence_number: u32,
    start_time: Instant,
    challenge_data: u64,
) -> Frame {
    let timestamp = Instant::now().duration_since(start_time).as_millis() as u32;
    Frame::new_path_response(peer_cid, sequence_number, timestamp, challenge_data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    async fn create_test_reliability_layer() -> UnifiedReliabilityLayer {
        let config = Config::default();
        let connection_id = 1; // Test connection ID
        let timer_handle = crate::timer::start_hybrid_timer_task::<crate::core::endpoint::timing::TimeoutEvent, crate::timer::task::types::SenderCallback<crate::core::endpoint::timing::TimeoutEvent>>();
        let timer_actor = crate::timer::start_sender_timer_actor(timer_handle, None);
        let (tx_to_endpoint, _rx_from_stream) = tokio::sync::mpsc::channel(128);
        UnifiedReliabilityLayer::new(connection_id, timer_actor, tx_to_endpoint, config)
    }   

    #[test]
    fn test_create_syn_frame() {
        let config = Config::default();
        let frame = create_syn_frame(&config, 123);
        match frame {
            Frame::Syn { header } => {
                assert_eq!(header.source_cid, 123);
            }
            _ => panic!("Incorrect frame type"),
        }
    }

    #[tokio::test]
    async fn test_create_fin_frame() {
        let mut reliability = create_test_reliability_layer().await;
        reliability.receive_push(0, Bytes::new());
        let _ = reliability.reassemble_data();

        let frame = create_fin_frame(456, 10, &reliability, Instant::now());
        match frame {
            Frame::Fin { header } => {
                assert_eq!(header.connection_id, 456);
                assert_eq!(header.sequence_number, 10);
                assert_eq!(header.payload_length, 0);
                assert_eq!(header.recv_next_sequence, 1); // Should carry ACK info
            }
            _ => panic!("Incorrect frame type"),
        }
    }

    #[tokio::test]
    async fn test_create_ack_frame() {
        let mut reliability = create_test_reliability_layer().await;
        // Simulate receiving a packet to generate ACK info
        reliability.receive_push(0, Bytes::new());
        // Call reassemble to update the internal state of the receive buffer,
        // which moves the `next_sequence` forward.
        let _ = reliability.reassemble_data();

        let frame = create_ack_frame(789, &mut reliability, Instant::now());
        match frame {
            Frame::Ack { header, payload } => {
                assert_eq!(header.connection_id, 789);
                assert_eq!(header.recv_next_sequence, 1);
                assert_eq!(header.payload_length as usize, payload.len());
            }
            _ => panic!("Incorrect frame type"),
        }
    }
} 