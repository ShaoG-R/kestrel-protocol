//! The packetizer is responsible for creating PUSH frames from the send buffer.
//!
//! Packetizer 负责从发送缓冲区创建 PUSH 帧。

use crate::{
    core::reliability::send_buffer::SendBuffer,
    packet::frame::Frame,
};

/// Contains the necessary context for the packetizer to create frames.
/// This context is read-only and is provided by the `ReliabilityLayer`.
///
/// 包含 Packetizer 创建帧所需的必要上下文。
/// 此上下文是只读的，由 `ReliabilityLayer` 提供。
#[derive(Debug, Clone, Copy)]
pub(crate) struct PacketizerContext {
    pub peer_cid: u32,
    pub timestamp: u32,
    pub congestion_window: u32,
    pub in_flight_count: usize,
    pub peer_recv_window: u32,
    pub max_payload_size: usize,
    pub ack_info: (u32, u16), // (recv_next_sequence, local_window_size)
}

/// Creates PUSH frames based on the provided context and send buffer state.
///
/// 根据提供的上下文和发送缓冲区的状态创建 PUSH 帧。
pub(crate) fn packetize(
    context: &PacketizerContext,
    send_buffer: &mut SendBuffer,
    sequence_number_counter: &mut u32,
    prepend_frame: Option<Frame>,
) -> Vec<Frame> {
    let mut frames = if let Some(frame) = prepend_frame {
        vec![frame]
    } else {
        Vec::new()
    };

    // Calculate the sending permit based on congestion and flow control windows.
    let cwnd_permit = context
        .congestion_window
        .saturating_sub(context.in_flight_count as u32);
    let flow_permit = context
        .peer_recv_window
        .saturating_sub(context.in_flight_count as u32);
    let permit = std::cmp::min(cwnd_permit, flow_permit);

    for _ in 0..permit {
        if send_buffer.is_stream_buffer_empty() {
            break;
        }

        let Some(chunk) = send_buffer.create_chunk(context.max_payload_size) else {
            break;
        };

        let seq = *sequence_number_counter;
        *sequence_number_counter += 1;

        let frame = Frame::new_push(
            context.peer_cid,
            seq,
            context.ack_info.0, // recv_next_sequence
            context.ack_info.1, // recv_window_size
            context.timestamp,
            chunk,
        );
        frames.push(frame);
    }
    frames
}

/// Creates frames for 0-RTT scenario with intelligent packet splitting.
/// This ensures SYN-ACK is always in the first packet, with PUSH frames
/// distributed across packets respecting MTU limits.
///
/// 为0-RTT场景创建帧，采用智能分包策略。
/// 确保SYN-ACK始终在第一个包中，PUSH帧按MTU限制智能分布到各个包中。
pub(crate) fn packetize_zero_rtt(
    context: &PacketizerContext,
    send_buffer: &mut SendBuffer,
    sequence_number_counter: &mut u32,
    syn_ack_frame: Frame,
    max_packet_size: usize,
) -> Vec<Vec<Frame>> {
    // First, generate all PUSH frames using the standard packetizer
    let push_frames = packetize(context, send_buffer, sequence_number_counter, None);
    
    if push_frames.is_empty() {
        // Only SYN-ACK, no PUSH frames
        return vec![vec![syn_ack_frame]];
    }

    // Calculate SYN-ACK frame size
    let syn_ack_size = syn_ack_frame.encoded_size();
    
    // Ensure SYN-ACK can fit in a packet (should always be true)
    assert!(syn_ack_size <= max_packet_size, "SYN-ACK frame exceeds maximum packet size");
    
    let mut result_packets = Vec::new();
    let mut current_packet = vec![syn_ack_frame];
    let mut current_size = syn_ack_size;
    
    // Try to fit as many PUSH frames as possible in the first packet
    let mut frames_iter = push_frames.into_iter();
    
    // Fill the first packet (with SYN-ACK)
    while let Some(push_frame) = frames_iter.next() {
        let frame_size = push_frame.encoded_size();
        
        if current_size + frame_size <= max_packet_size {
            current_packet.push(push_frame);
            current_size += frame_size;
        } else {
            // Current frame doesn't fit, finish first packet and start second
            result_packets.push(current_packet);
            
            // Start a new packet with the current frame
            current_packet = vec![push_frame];
            current_size = frame_size;
            break;
        }
    }
    
    // Handle remaining frames
    for push_frame in frames_iter {
        let frame_size = push_frame.encoded_size();
        
        if current_size + frame_size <= max_packet_size {
            current_packet.push(push_frame);
            current_size += frame_size;
        } else {
            // Current packet is full, start a new one
            result_packets.push(current_packet);
            current_packet = vec![push_frame];
            current_size = frame_size;
        }
    }
    
    // Don't forget the last packet
    if !current_packet.is_empty() {
        result_packets.push(current_packet);
    }
    
    result_packets
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{core::reliability::send_buffer::SendBuffer, packet::frame::Frame};
    use bytes::Bytes;

    #[test]
    fn test_packetize_simple() {
        let mut send_buffer = SendBuffer::new(1024);
        send_buffer.write_to_stream(Bytes::from_static(b"hello world")); // 11 bytes

        let context = PacketizerContext {
            peer_cid: 1,
            timestamp: 123,
            congestion_window: 10,
            in_flight_count: 0,
            peer_recv_window: 10,
            max_payload_size: 5,
            ack_info: (0, 1024),
        };

        let mut seq_counter = 0;
        let frames = packetize(&context, &mut send_buffer, &mut seq_counter, None);

        // With a permit of 10 and max_payload_size of 5, the 11-byte buffer
        // should be packetized into three frames.
        assert_eq!(frames.len(), 3);
        assert_eq!(seq_counter, 3);

        if let Frame::Push { payload, header } = &frames[0] {
            assert_eq!(payload.as_ref(), b"hello");
            assert_eq!(header.sequence_number, 0);
        } else {
            panic!("Expected a PUSH frame");
        }
        if let Frame::Push { payload, header } = &frames[1] {
            assert_eq!(payload.as_ref(), b" worl");
            assert_eq!(header.sequence_number, 1);
        } else {
            panic!("Expected a PUSH frame");
        }
        if let Frame::Push { payload, header } = &frames[2] {
            assert_eq!(payload.as_ref(), b"d");
            assert_eq!(header.sequence_number, 2);
        } else {
            panic!("Expected a PUSH frame");
        }

        // The send buffer should be fully consumed.
        assert!(send_buffer.is_stream_buffer_empty());
    }

    #[test]
    fn test_packetize_respects_cwnd() {
        let mut send_buffer = SendBuffer::new(1024);
        send_buffer.write_to_stream(Bytes::from_static(b"one two three four five"));

        let context = PacketizerContext {
            peer_cid: 1,
            timestamp: 123,
            congestion_window: 2, // CWND is the bottleneck
            in_flight_count: 0,
            peer_recv_window: 10,
            max_payload_size: 5,
            ack_info: (0, 1024),
        };

        let mut seq_counter = 0;
        let frames = packetize(&context, &mut send_buffer, &mut seq_counter, None);

        // Permit is 2, so only two frames should be created
        assert_eq!(frames.len(), 2);
        assert_eq!(seq_counter, 2);
        assert!(!send_buffer.is_stream_buffer_empty());
    }

    #[test]
    fn test_packetize_respects_flow_control() {
        let mut send_buffer = SendBuffer::new(1024);
        send_buffer.write_to_stream(Bytes::from_static(b"one two three four five"));

        let context = PacketizerContext {
            peer_cid: 1,
            timestamp: 123,
            congestion_window: 10,
            in_flight_count: 0,
            peer_recv_window: 3, // Flow control is the bottleneck
            max_payload_size: 5,
            ack_info: (0, 1024),
        };

        let mut seq_counter = 0;
        let frames = packetize(&context, &mut send_buffer, &mut seq_counter, None);

        // Permit is 3, so only three frames should be created
        assert_eq!(frames.len(), 3);
        assert_eq!(seq_counter, 3);
        assert!(!send_buffer.is_stream_buffer_empty());
    }

    #[test]
    fn test_packetize_empty_buffer() {
        let mut send_buffer = SendBuffer::new(1024); // Empty buffer
        let context = PacketizerContext {
            peer_cid: 1,
            timestamp: 123,
            congestion_window: 10,
            in_flight_count: 0,
            peer_recv_window: 10,
            max_payload_size: 5,
            ack_info: (0, 1024),
        };
        let mut seq_counter = 0;
        let frames = packetize(&context, &mut send_buffer, &mut seq_counter, None);
        assert!(frames.is_empty());
        assert_eq!(seq_counter, 0);
    }

    #[test]
    fn test_packetize_prepend_frame() {
        let mut send_buffer = SendBuffer::new(1024);
        send_buffer.write_to_stream(Bytes::from_static(b"hello world")); // 11 bytes

        let context = PacketizerContext {
            peer_cid: 1,
            timestamp: 123,
            congestion_window: 10,
            in_flight_count: 0,
            peer_recv_window: 10,
            max_payload_size: 5,
            ack_info: (0, 1024),
        };

        let mut seq_counter = 0;
        let frames = packetize(
            &context,
            &mut send_buffer,
            &mut seq_counter,
            Some(Frame::new_fin(1, 99, 0, 1024, 123)),
        );
        assert_eq!(frames.len(), 4); // FIN + 3 PUSH
        assert_eq!(seq_counter, 3);
        if let Frame::Fin { .. } = &frames[0] {
            // Correct
        } else {
            panic!("Expected a FIN frame at the start");
        }
        if let Frame::Push { header, .. } = &frames[1] {
            assert_eq!(header.sequence_number, 0);
        } else {
            panic!("Expected a PUSH frame");
        }
    }

    #[test]
    fn test_packetize_zero_rtt_syn_ack_only() {
        let mut send_buffer = SendBuffer::new(1024); // Empty buffer
        let context = PacketizerContext {
            peer_cid: 1,
            timestamp: 123,
            congestion_window: 10,
            in_flight_count: 0,
            peer_recv_window: 10,
            max_payload_size: 100,
            ack_info: (0, 1024),
        };
        let mut seq_counter = 0;
        let syn_ack_frame = Frame::new_syn_ack(1, 2, 1);
        
        let packets = packetize_zero_rtt(&context, &mut send_buffer, &mut seq_counter, syn_ack_frame.clone(), 1500);
        
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].len(), 1);
        assert_eq!(packets[0][0], syn_ack_frame);
        assert_eq!(seq_counter, 0);
    }

    #[test]
    fn test_packetize_zero_rtt_syn_ack_with_data_single_packet() {
        let mut send_buffer = SendBuffer::new(1024);
        send_buffer.write_to_stream(Bytes::from_static(b"hello")); // 5 bytes, fits in one PUSH frame

        let context = PacketizerContext {
            peer_cid: 1,
            timestamp: 123,
            congestion_window: 10,
            in_flight_count: 0,
            peer_recv_window: 10,
            max_payload_size: 100,
            ack_info: (0, 1024),
        };

        let mut seq_counter = 0;
        let syn_ack_frame = Frame::new_syn_ack(1, 2, 1);
        
        let packets = packetize_zero_rtt(&context, &mut send_buffer, &mut seq_counter, syn_ack_frame.clone(), 1500);
        
        // Should fit in one packet: SYN-ACK + PUSH
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].len(), 2);
        assert_eq!(packets[0][0], syn_ack_frame);
        if let Frame::Push { header, payload } = &packets[0][1] {
            assert_eq!(header.sequence_number, 0);
            assert_eq!(payload.as_ref(), b"hello");
        } else {
            panic!("Expected a PUSH frame");
        }
        assert_eq!(seq_counter, 1);
    }

    #[test]
    fn test_packetize_zero_rtt_syn_ack_with_data_multiple_packets() {
        let mut send_buffer = SendBuffer::new(1024);
        // Add enough data to require multiple packets
        send_buffer.write_to_stream(Bytes::from_static(b"this is a long message that will be split across multiple frames and packets"));

        let context = PacketizerContext {
            peer_cid: 1,
            timestamp: 123,
            congestion_window: 10,
            in_flight_count: 0,
            peer_recv_window: 10,
            max_payload_size: 10, // Small payload to force multiple frames
            ack_info: (0, 1024),
        };

        let mut seq_counter = 0;
        let syn_ack_frame = Frame::new_syn_ack(1, 2, 1);
        let syn_ack_size = syn_ack_frame.encoded_size();
        
        // Set packet size to allow SYN-ACK + only a few PUSH frames per packet
        let max_packet_size = syn_ack_size + 2 * (21 + 10); // SYN-ACK + ~2 PUSH frames max
        
        let packets = packetize_zero_rtt(&context, &mut send_buffer, &mut seq_counter, syn_ack_frame.clone(), max_packet_size);
        
        // Should have multiple packets
        assert!(packets.len() > 1);
        
        // First packet should start with SYN-ACK
        assert!(!packets[0].is_empty());
        assert_eq!(packets[0][0], syn_ack_frame);
        
        // First packet should have SYN-ACK + some PUSH frames
        assert!(packets[0].len() > 1);
        
        // Verify all PUSH frames have correct sequence numbers
        let mut expected_seq = 0;
        for packet in &packets {
            for frame in packet {
                if let Frame::Push { header, .. } = frame {
                    assert_eq!(header.sequence_number, expected_seq);
                    expected_seq += 1;
                }
            }
        }
        
        // Verify all data was consumed
        assert!(send_buffer.is_stream_buffer_empty());
    }

    #[test]
    fn test_packetize_zero_rtt_respects_congestion_window() {
        let mut send_buffer = SendBuffer::new(1024);
        send_buffer.write_to_stream(Bytes::from_static(b"hello world this is a test"));

        let context = PacketizerContext {
            peer_cid: 1,
            timestamp: 123,
            congestion_window: 2, // Very limited congestion window
            in_flight_count: 0,
            peer_recv_window: 10,
            max_payload_size: 5,
            ack_info: (0, 1024),
        };

        let mut seq_counter = 0;
        let syn_ack_frame = Frame::new_syn_ack(1, 2, 1);
        
        let packets = packetize_zero_rtt(&context, &mut send_buffer, &mut seq_counter, syn_ack_frame.clone(), 1500);
        
        // Should only create packets limited by congestion window (2 PUSH frames max)
        let total_push_frames: usize = packets.iter().map(|p| p.len() - if p[0] == syn_ack_frame { 1 } else { 0 }).sum();
        assert_eq!(total_push_frames, 2);
        assert_eq!(seq_counter, 2);
        
        // Some data should remain in the buffer
        assert!(!send_buffer.is_stream_buffer_empty());
    }
} 