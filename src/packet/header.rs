//! 定义长、短两种协议头。
//! Defines the Long and Short protocol headers.

use super::command::Command;
use bytes::{Buf, BufMut};

pub const SHORT_HEADER_SIZE: usize = 21;

/// The short header, used for most data transmission packets after connection is established.
/// 短头，用于连接建立后的大部分数据传输包。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortHeader {
    /// The command of the packet.
    /// 包的指令。
    pub command: Command,
    /// The connection ID.
    /// 连接ID。
    pub connection_id: u32,
    /// The length of the payload following this header.
    /// 紧随此头部的载荷长度。
    pub payload_length: u16,
    /// The available receiving window size of the sender of this packet.
    /// 此包发送方的可用接收窗口大小。
    pub recv_window_size: u16,
    /// The timestamp when this packet is sent.
    /// 此包的发送时间戳。
    pub timestamp: u32,
    /// The sequence number of this packet.
    /// 包序号。
    pub sequence_number: u32,
    /// The next sequence number the sender is expecting to receive.
    /// 发送方期望接收的下一个包序号。
    pub recv_next_sequence: u32,
}

impl ShortHeader {
    /// 将短头编码到缓冲区。
    /// Encodes the short header into a buffer.
    pub fn encode<B: BufMut>(&self, buf: &mut B) {
        buf.put_u8(self.command as u8);
        buf.put_u32(self.connection_id);
        buf.put_u16(self.payload_length);
        buf.put_u16(self.recv_window_size);
        buf.put_u32(self.timestamp);
        buf.put_u32(self.sequence_number);
        buf.put_u32(self.recv_next_sequence);
    }

    /// 从缓冲区解码短头。
    /// Decodes a short header from a buffer.
    pub fn decode<B: Buf>(buf: &mut B) -> Option<Self> {
        if buf.remaining() < SHORT_HEADER_SIZE {
            return None;
        }
        let command = Command::from_u8(buf.get_u8())?;
        if command.is_long_header() {
            return None; // Should be a short header command
        }
        Some(ShortHeader {
            command,
            connection_id: buf.get_u32(),
            payload_length: buf.get_u16(),
            recv_window_size: buf.get_u16(),
            timestamp: buf.get_u32(),
            sequence_number: buf.get_u32(),
            recv_next_sequence: buf.get_u32(),
        })
    }
}

impl ShortHeader {
    pub const ENCODED_SIZE: usize = 4 + 2 + 4 + 4 + 4; // connection_id + recv_window_size + timestamp + sequence_number + recv_next_sequence
}

pub const LONG_HEADER_SIZE: usize = 10; // command(1) + version(1) + dcid(4) + scid(4)

/// The long header, used for connection management packets.
/// 长头，用于连接管理包。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LongHeader {
    /// The command of the packet.
    /// 包的指令。
    pub command: Command,
    /// The protocol version.
    /// 协议版本。
    pub protocol_version: u8,
    /// The destination connection ID.
    /// 目标连接ID。
    pub destination_cid: u32,
    /// The source connection ID.
    /// 源连接ID。
    pub source_cid: u32,
}

impl LongHeader {
    /// 将长头编码到缓冲区。
    /// Encodes the long header into a buffer.
    pub fn encode<B: BufMut>(&self, buf: &mut B) {
        buf.put_u8(self.command as u8);
        buf.put_u8(self.protocol_version);
        buf.put_u32(self.destination_cid);
        buf.put_u32(self.source_cid);
    }

    /// 从缓冲区解码长头。
    /// Decodes a long header from a buffer.
    pub fn decode<B: Buf>(buf: &mut B) -> Option<Self> {
        if buf.remaining() < LONG_HEADER_SIZE {
            return None;
        }
        let command = Command::from_u8(buf.get_u8())?;
        if !command.is_long_header() {
            return None; // Should be a long header command
        }
        Some(LongHeader {
            command,
            protocol_version: buf.get_u8(),
            destination_cid: buf.get_u32(),
            source_cid: buf.get_u32(),
        })
    }
}

impl LongHeader {
    pub const ENCODED_SIZE: usize = 1 + 1 + 4 + 4; // command + version + dest_cid + source_cid
}
