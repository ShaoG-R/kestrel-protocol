//! Packet serialization and deserialization tests.
use super::command::Command;
use super::frame::Frame;
use super::header::{LongHeader, ShortHeader};
use super::sack::{self, SackRange};
use bytes::{Bytes, BytesMut};

fn frame_roundtrip_test(frame: Frame) {
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let decoded_frame = Frame::decode(&buf).expect("decode should succeed");
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