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
} 