//! Packet serialization and deserialization tests.
use super::command::Command;
use super::frame::Frame;
use super::header::{LongHeader, ShortHeader};
use super::sack::{self, SackRange};
use bytes::{Bytes, BytesMut};
use crate::packet::header;

fn frame_roundtrip_test(frame: Frame) {
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let mut cursor = &buf[..];
    let decoded_frame = Frame::decode(&mut cursor).expect("decode should succeed");
    assert!(cursor.is_empty(), "decode should consume the entire buffer");
    assert_eq!(frame, decoded_frame);
}

#[test]
fn test_push_frame_roundtrip() {
    let payload = Bytes::from_static(b"hello world");
    let header = ShortHeader {
        command: Command::Push,
        connection_id: 12345,
        payload_length: payload.len() as u16,
        recv_window_size: 1024,
        timestamp: 54321,
        sequence_number: 100,
        recv_next_sequence: 50,
    };
    let frame = Frame::Push {
        header,
        payload,
    };
    frame_roundtrip_test(frame);
}

#[test]
fn test_ack_frame_roundtrip() {
    let sack_ranges = vec![
        SackRange { start: 10, end: 20 },
        SackRange { start: 25, end: 30 },
    ];
    let mut payload_buf = BytesMut::new();
    sack::encode_sack_ranges(&sack_ranges, &mut payload_buf);
    let payload = payload_buf.freeze();

    let header = ShortHeader {
        command: Command::Ack,
        connection_id: 12345,
        payload_length: payload.len() as u16,
        recv_window_size: 1024,
        timestamp: 54321,
        sequence_number: 100,
        recv_next_sequence: 50,
    };
    let frame = Frame::Ack {
        header,
        payload,
    };
    frame_roundtrip_test(frame);
}

#[test]
fn test_ping_frame_roundtrip() {
    let header = ShortHeader {
        command: Command::Ping,
        connection_id: 12345,
        payload_length: 0,
        recv_window_size: 1024,
        timestamp: 54321,
        sequence_number: 100,
        recv_next_sequence: 50,
    };
    let frame = Frame::Ping { header };
    frame_roundtrip_test(frame);
}

#[test]
fn test_fin_frame_roundtrip() {
    let header = ShortHeader {
        command: Command::Fin,
        connection_id: 12345,
        payload_length: 0,
        recv_window_size: 1024,
        timestamp: 54321,
        sequence_number: 100,
        recv_next_sequence: 50,
    };
    let frame = Frame::Fin { header };
    frame_roundtrip_test(frame);
}

#[test]
fn test_syn_frame_roundtrip() {
    let payload = Bytes::from_static(b"optional 0-rtt data");
    let header = LongHeader {
        command: Command::Syn,
        protocol_version: 1,
        payload_length: payload.len() as u16,
        destination_cid: 9876,
        source_cid: 5432,
    };
    let frame = Frame::Syn {
        header,
        payload,
    };
    frame_roundtrip_test(frame);
}

#[test]
fn test_syn_ack_frame_roundtrip() {
    let payload = Bytes::new();
    let header = LongHeader {
        command: Command::SynAck,
        protocol_version: 1,
        payload_length: payload.len() as u16,
        destination_cid: 5432,
        source_cid: 9876,
    };
    let frame = Frame::SynAck {
        header,
        payload,
    };
    frame_roundtrip_test(frame);
}

#[test]
fn test_frame_decode_no_header_pollution() {
    // 1. Create a known payload first.
    let payload = Bytes::from_static(b"this is the pure payload");

    // 2. Create the header, using the payload's length.
    let header = header::ShortHeader {
        command: Command::Push,
        connection_id: 123,
        payload_length: payload.len() as u16,
        recv_window_size: 1024,
        timestamp: 456,
        sequence_number: 789,
        recv_next_sequence: 10,
    };

    // 3. Create the original frame and encode it into a buffer.
    let original_frame = Frame::Push {
        header: header.clone(),
        payload: payload.clone(),
    };
    let mut encoded_buffer = bytes::BytesMut::new();
    original_frame.encode(&mut encoded_buffer);

    // 4. Decode the buffer back into a frame.
    let mut cursor = &encoded_buffer[..];
    let decoded_frame = Frame::decode(&mut cursor).expect("Decoding should succeed");
    assert!(
        cursor.is_empty(),
        "decode should consume the entire buffer for a single frame"
    );

    // 5. Assert that the decoded frame is correct and the payload is not polluted.
    if let Frame::Push {
        header: decoded_header,
        payload: decoded_payload,
    } = decoded_frame
    {
        assert_eq!(
            decoded_header, header,
            "Decoded header should match original"
        );
        assert_eq!(
            decoded_payload, payload,
            "Decoded payload should match original"
        );
        assert_eq!(
            decoded_payload.len(),
            payload.len(),
            "Decoded payload length should be correct and not include the header"
        );
    } else {
        panic!("Decoded frame is not a Push frame");
    }
}

#[test]
fn test_coalesced_frames_decode() {
    // 1. Create a PUSH frame.
    let push_payload = Bytes::from_static(b"push data");
    let push_header = ShortHeader {
        command: Command::Push,
        connection_id: 123,
        payload_length: push_payload.len() as u16,
        recv_window_size: 1024,
        timestamp: 1,
        sequence_number: 1,
        recv_next_sequence: 0,
    };
    let push_frame = Frame::Push {
        header: push_header.clone(),
        payload: push_payload,
    };

    // 2. Create a FIN frame.
    let fin_header = ShortHeader {
        command: Command::Fin,
        connection_id: 123,
        payload_length: 0,
        recv_window_size: 1024,
        timestamp: 2,
        sequence_number: 2,
        recv_next_sequence: 0,
    };
    let fin_frame = Frame::Fin {
        header: fin_header.clone(),
    };

    // 3. Encode both frames into a single buffer.
    let mut buf = BytesMut::new();
    push_frame.encode(&mut buf);
    fin_frame.encode(&mut buf);

    // 4. Decode the frames from the buffer.
    let mut cursor = &buf[..];
    let decoded_push = Frame::decode(&mut cursor).expect("Should decode PUSH frame");
    let decoded_fin = Frame::decode(&mut cursor).expect("Should decode FIN frame");

    // 5. Assert that the buffer is fully consumed and frames are correct.
    assert!(cursor.is_empty(), "Buffer should be fully consumed");
    assert_eq!(
        push_frame, decoded_push,
        "Decoded PUSH frame should match original"
    );
    assert_eq!(
        fin_frame, decoded_fin,
        "Decoded FIN frame should match original"
    );
} 