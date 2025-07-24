//! 定义了协议中可以在网络上传输的完整数据帧。
//! Defines the complete data frames that can be transmitted over the network.

use super::command;
use super::header::{LongHeader, ShortHeader};
use bytes::{BufMut, Bytes};

/// A complete protocol frame that can be sent or received.
/// 一个可以被发送或接收的完整协议帧。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// A PUSH frame carrying data.
    /// 携带数据的 PUSH 帧。
    Push {
        header: ShortHeader,
        payload: Bytes,
    },
    /// An ACK frame carrying SACK information.
    /// 携带SACK信息的 ACK 帧。
    Ack {
        header: ShortHeader,
        payload: Bytes,
    },
    /// A PING frame.
    /// PING 帧。
    Ping { header: ShortHeader },
    /// A SYN frame to initiate a connection.
    /// 用于发起连接的 SYN 帧。
    Syn { header: LongHeader },
    /// A SYN-ACK frame to acknowledge a connection.
    /// 用于确认连接的 SYN-ACK 帧。
    SynAck { header: LongHeader },
    /// A FIN frame to close a connection.
    /// 用于关闭连接的 FIN 帧。
    Fin { header: LongHeader },
}

impl Frame {
    /// 从缓冲区解码一个完整的帧。
    /// Decodes a complete frame from a buffer.
    ///
    /// The buffer is expected to contain exactly one UDP datagram's payload.
    /// 缓冲区应包含一个完整的UDP数据报的载荷。
    pub fn decode(mut buf: &[u8]) -> Option<Self> {
        if buf.is_empty() {
            return None;
        }

        // 偷窥第一个字节来决定是长头还是短头
        // Peek the first byte to decide between long and short header
        let command = command::Command::from_u8(buf[0])?;

        if command.is_long_header() {
            // TODO: 实现长头的解码逻辑
            // For now, we don't have a full long header spec, so we can't decode it yet.
            // 由于我们尚未完全定义长头格式，暂时无法解码。
            None
        } else {
            // 解码短头
            // Decode the short header
            let header = ShortHeader::decode(&mut buf)?;
            let payload = Bytes::copy_from_slice(buf);

            match header.command {
                command::Command::Push => Some(Frame::Push { header, payload }),
                command::Command::Ack => Some(Frame::Ack { header, payload }),
                command::Command::Ping => Some(Frame::Ping { header }),
                _ => None, // 不应该是长头指令
            }
        }
    }

    /// 将帧编码到缓冲区。
    /// Encodes the frame into a buffer.
    pub fn encode<B: BufMut>(&self, buf: &mut B) {
        match self {
            Frame::Push { header, payload } => {
                header.encode(buf);
                buf.put_slice(payload);
            }
            Frame::Ack { header, payload } => {
                header.encode(buf);
                buf.put_slice(payload);
            }
            Frame::Ping { header } => {
                header.encode(buf);
            }
            Frame::Syn { header } => {
                // TODO: 实现长头的编码逻辑
                // header.encode(buf);
            }
            Frame::SynAck { header } => {
                // TODO: 实现长头的编码逻辑
                // header.encode(buf);
            }
            Frame::Fin { header } => {
                // TODO: 实现长头的编码逻辑
                // header.encode(buf);
            }
        }
    }
} 