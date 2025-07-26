//! Contains factory functions for creating different types of frames.
//!
//! 包含用于创建不同类型帧的工厂函数。

use bytes::{Bytes, BytesMut};
use tokio::time::Instant;

use crate::{
    config::Config,
    core::reliability::ReliabilityLayer,
    packet::{
        command::Command,
        frame::Frame,
        header::{LongHeader, ShortHeader},
        sack::encode_sack_ranges,
    },
};

/// Creates a SYN frame.
pub(super) fn create_syn_frame(config: &Config, local_cid: u32, initial_payload: Bytes) -> Frame {
    let syn_header = LongHeader {
        command: Command::Syn,
        protocol_version: config.protocol_version,
        payload_length: initial_payload.len() as u16,
        destination_cid: 0, // Server's CID is unknown
        source_cid: local_cid,
    };
    Frame::Syn {
        header: syn_header,
        payload: initial_payload,
    }
}

/// Creates a SYN-ACK frame.
pub(super) fn create_syn_ack_frame(
    config: &Config,
    peer_cid: u32,
    local_cid: u32,
    payload: Bytes,
) -> Frame {
    let syn_ack_header = LongHeader {
        command: Command::SynAck,
        protocol_version: config.protocol_version,
        payload_length: payload.len() as u16,
        destination_cid: peer_cid,
        source_cid: local_cid,
    };
    Frame::SynAck {
        header: syn_ack_header,
        payload,
    }
}

/// Creates a standalone ACK frame.
pub(super) fn create_ack_frame(
    peer_cid: u32,
    reliability: &mut ReliabilityLayer,
    start_time: Instant,
) -> Frame {
    let (sack_ranges, recv_next, window_size) = reliability.get_ack_info();

    let mut ack_payload = BytesMut::with_capacity(sack_ranges.len() * 8);
    encode_sack_ranges(&sack_ranges, &mut ack_payload);

    let ack_header = ShortHeader {
        command: Command::Ack,
        connection_id: peer_cid,
        payload_length: ack_payload.len() as u16,
        recv_window_size: window_size,
        timestamp: Instant::now().duration_since(start_time).as_millis() as u32,
        sequence_number: 0, // ACK frames do not have a sequence number
        recv_next_sequence: recv_next,
    };

    Frame::Ack {
        header: ack_header,
        payload: ack_payload.freeze(),
    }
}

/// Creates a FIN frame.
pub(super) fn create_fin_frame(
    peer_cid: u32,
    sequence_number: u32,
    start_time: Instant,
) -> Frame {
    let fin_header = ShortHeader {
        command: Command::Fin,
        connection_id: peer_cid,
        payload_length: 0,
        recv_window_size: 0, // No more data will be received
        timestamp: Instant::now().duration_since(start_time).as_millis() as u32,
        sequence_number,
        recv_next_sequence: 0, // Not relevant for FIN
    };
    Frame::Fin { header: fin_header }
}

/// Creates a PATH_CHALLENGE frame.
pub(crate) fn create_path_challenge_frame(
    peer_cid: u32,
    sequence_number: u32,
    start_time: Instant,
    challenge_data: u64,
) -> Frame {
    let header = ShortHeader {
        command: Command::PathChallenge,
        connection_id: peer_cid,
        payload_length: 8, // The payload is the 8-byte challenge data
        recv_window_size: 0, // Not relevant
        timestamp: Instant::now().duration_since(start_time).as_millis() as u32,
        sequence_number,
        recv_next_sequence: 0, // Not relevant
    };
    Frame::PathChallenge {
        header,
        challenge_data,
    }
}

/// Creates a PATH_RESPONSE frame.
pub(crate) fn create_path_response_frame(
    peer_cid: u32,
    sequence_number: u32,
    start_time: Instant,
    challenge_data: u64,
) -> Frame {
    let header = ShortHeader {
        command: Command::PathResponse,
        connection_id: peer_cid,
        payload_length: 8, // The payload is the 8-byte challenge data
        recv_window_size: 0, // Not relevant
        timestamp: Instant::now().duration_since(start_time).as_millis() as u32,
        sequence_number,
        recv_next_sequence: 0, // Not relevant
    };
    Frame::PathResponse {
        header,
        challenge_data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::congestion::vegas::Vegas;

    fn create_test_reliability_layer() -> ReliabilityLayer {
        let config = Config::default();
        let congestion_control = Box::new(Vegas::new(config.clone()));
        ReliabilityLayer::new(config, congestion_control)
    }

    #[test]
    fn test_create_syn_frame() {
        let config = Config::default();
        let frame = create_syn_frame(&config, 123, Bytes::from_static(b"hello"));
        match frame {
            Frame::Syn { header, payload } => {
                assert_eq!(header.payload_length, 5);
                assert_eq!(header.source_cid, 123);
                assert_eq!(payload, "hello");
            }
            _ => panic!("Incorrect frame type"),
        }
    }

    #[test]
    fn test_create_fin_frame() {
        let frame = create_fin_frame(456, 10, Instant::now());
        match frame {
            Frame::Fin { header } => {
                assert_eq!(header.payload_length, 0);
                assert_eq!(header.connection_id, 456);
                assert_eq!(header.sequence_number, 10);
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
                assert_eq!(header.payload_length as usize, payload.len());
                assert_eq!(header.connection_id, 789);
                assert_eq!(header.recv_next_sequence, 1);
            }
            _ => panic!("Incorrect frame type"),
        }
    }
} 