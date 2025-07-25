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
    let header = ShortHeader {
        command: Command::Push,
        connection_id: 12345,
        recv_window_size: 1024,
        timestamp: 54321,
        sequence_number: 100,
        recv_next_sequence: 50,
    };
    let payload = Bytes::from_static(b"hello world");
    let frame = Frame::Push {
        header,
        payload,
    };
    frame_roundtrip_test(frame);
}

#[test]
fn test_ack_frame_roundtrip() {
    let header = ShortHeader {
        command: Command::Ack,
        connection_id: 12345,
        recv_window_size: 1024,
        timestamp: 54321,
        sequence_number: 100,
        recv_next_sequence: 50,
    };
    let sack_ranges = vec![
        SackRange { start: 10, end: 20 },
        SackRange { start: 25, end: 30 },
    ];
    let mut payload = BytesMut::new();
    sack::encode_sack_ranges(&sack_ranges, &mut payload);
    let frame = Frame::Ack {
        header,
        payload: payload.freeze(),
    };
    frame_roundtrip_test(frame);
}

#[test]
fn test_ping_frame_roundtrip() {
    let header = ShortHeader {
        command: Command::Ping,
        connection_id: 12345,
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
    let header = LongHeader {
        command: Command::Syn,
        protocol_version: 1,
        destination_cid: 9876,
        source_cid: 5432,
    };
    let payload = Bytes::from_static(b"optional 0-rtt data");
    let frame = Frame::Syn {
        header,
        payload,
    };
    frame_roundtrip_test(frame);
}

#[test]
fn test_syn_ack_frame_roundtrip() {
    let header = LongHeader {
        command: Command::SynAck,
        protocol_version: 1,
        destination_cid: 5432,
        source_cid: 9876,
    };
    let frame = Frame::SynAck {
        header,
        payload: Bytes::new(),
    };
    frame_roundtrip_test(frame);
}

#[test]
fn test_frame_decode_no_header_pollution() {
    // 1. Create a known header and payload.
    let header = header::ShortHeader {
        command: Command::Push,
        connection_id: 123,
        recv_window_size: 1024,
        timestamp: 456,
        sequence_number: 789,
        recv_next_sequence: 10,
    };
    let payload = Bytes::from_static(b"this is the pure payload");

    // 2. Create the original frame and encode it into a buffer.
    let original_frame = Frame::Push {
        header: header.clone(),
        payload: payload.clone(),
    };
    let mut encoded_buffer = bytes::BytesMut::new();
    original_frame.encode(&mut encoded_buffer);

    // 3. Decode the buffer back into a frame.
    let mut cursor = &encoded_buffer[..];
    let decoded_frame = Frame::decode(&mut cursor).expect("Decoding should succeed");
    assert!(cursor.is_empty(), "decode should consume the entire buffer for a single frame");

    // 4. Assert that the decoded frame is correct and the payload is not polluted.
    if let Frame::Push {
        header: decoded_header,
        payload: decoded_payload,
    } = decoded_frame
    {
        assert_eq!(decoded_header, header, "Decoded header should match original");
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