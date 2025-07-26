//! Contains factory functions for creating different types of frames.
//!
//! 包含用于创建不同类型帧的工厂函数。

use tokio::time::Instant;

use crate::{
    config::Config,
    core::reliability::ReliabilityLayer,
    packet::frame::Frame,
};

/// Creates a SYN frame.
pub(super) fn create_syn_frame(config: &Config, local_cid: u32) -> Frame {
    Frame::new_syn(config.protocol_version, local_cid, 0)
}

/// Creates a SYN-ACK frame.
pub(super) fn create_syn_ack_frame(
    config: &Config,
    peer_cid: u32,
    local_cid: u32,
) -> Frame {
    Frame::new_syn_ack(config.protocol_version, local_cid, peer_cid)
}

/// Creates a standalone ACK frame.
pub(super) fn create_ack_frame(
    peer_cid: u32,
    reliability: &mut ReliabilityLayer,
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
pub(super) fn create_fin_frame(
    peer_cid: u32,
    sequence_number: u32,
    start_time: Instant,
) -> Frame {
    let timestamp = Instant::now().duration_since(start_time).as_millis() as u32;
    // For FIN frames, recv_next_sequence and recv_window_size are not strictly necessary,
    // as the connection is being torn down and no new data is expected.
    // However, including them can be a good practice for consistency. We'll use 0 for now.
    Frame::new_fin(peer_cid, sequence_number, timestamp, 0, 0)
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
    use crate::congestion::vegas::Vegas;
    use bytes::Bytes;

    fn create_test_reliability_layer() -> ReliabilityLayer {
        let config = Config::default();
        let congestion_control = Box::new(Vegas::new(config.clone()));
        ReliabilityLayer::new(config, congestion_control)
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

    #[test]
    fn test_create_fin_frame() {
        let frame = create_fin_frame(456, 10, Instant::now());
        match frame {
            Frame::Fin { header } => {
                assert_eq!(header.connection_id, 456);
                assert_eq!(header.sequence_number, 10);
                assert_eq!(header.payload_length, 0);
            }
            _ => panic!("Incorrect frame type"),
        }
    }

    #[test]
    fn test_create_ack_frame() {
        let mut reliability = create_test_reliability_layer();
        // Simulate receiving a packet to generate ACK info
        reliability.receive_push(0, Bytes::new());
        // Call reassemble to update the internal state of the receive buffer,
        // which moves the `next_sequence` forward.
        let _ = reliability.reassemble();

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