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
    /// A SYN frame to initiate a connection. May carry 0-RTT data.
    /// 用于发起连接的 SYN 帧。可携带0-RTT数据。
    Syn {
        header: LongHeader,
        payload: Bytes,
    },
    /// A SYN-ACK frame to acknowledge a connection. May carry data.
    /// 用于确认连接的 SYN-ACK 帧。可携带数据。
    SynAck {
        header: LongHeader,
        payload: Bytes,
    },
    /// A FIN frame to close a connection.
    /// 用于关闭连接的 FIN 帧。
    Fin { header: ShortHeader },
    /// A path challenge packet for connection migration.
    /// 用于连接迁移的路径质询包。
    PathChallenge {
        header: ShortHeader,
        challenge_data: u64,
    },
    /// A path response packet for connection migration.
    /// 用于连接迁移的路径响应包。
    PathResponse {
        header: ShortHeader,
        challenge_data: u64,
    },
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
            let header = LongHeader::decode(&mut buf)?;
            let payload = Bytes::copy_from_slice(buf);
            match header.command {
                command::Command::Syn => Some(Frame::Syn { header, payload }),
                command::Command::SynAck => Some(Frame::SynAck { header, payload }),
                _ => None, // Unreachable, as is_long_header is checked
            }
        } else {
            // 解码短头
            // Decode the short header
            let header = ShortHeader::decode(&mut buf)?;

            match header.command {
                command::Command::Push => {
                    let payload = Bytes::copy_from_slice(buf);
                    Some(Frame::Push { header, payload })
                }
                command::Command::Ack => {
                    let payload = Bytes::copy_from_slice(buf);
                    Some(Frame::Ack { header, payload })
                }
                command::Command::Ping => Some(Frame::Ping { header }),
                command::Command::Fin => Some(Frame::Fin { header }),
                command::Command::PathChallenge => {
                    if buf.len() < 8 {
                        return None;
                    }
                    let challenge_data = u64::from_be_bytes(buf[..8].try_into().ok()?);
                    Some(Frame::PathChallenge {
                        header,
                        challenge_data,
                    })
                }
                command::Command::PathResponse => {
                    if buf.len() < 8 {
                        return None;
                    }
                    let challenge_data = u64::from_be_bytes(buf[..8].try_into().ok()?);
                    Some(Frame::PathResponse {
                        header,
                        challenge_data,
                    })
                }
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
            Frame::Syn { header, payload } => {
                header.encode(buf);
                buf.put_slice(payload);
            }
            Frame::SynAck { header, payload } => {
                header.encode(buf);
                buf.put_slice(payload);
            }
            Frame::Fin { header } => {
                header.encode(buf);
            }
            Frame::PathChallenge {
                header,
                challenge_data,
            } => {
                header.encode(buf);
                buf.put_slice(&challenge_data.to_be_bytes());
            }
            Frame::PathResponse {
                header,
                challenge_data,
            } => {
                header.encode(buf);
                buf.put_slice(&challenge_data.to_be_bytes());
            }
        }
    }

    /// 获取帧的序列号（如果存在）。
    /// Gets the sequence number of the frame, if it has one.
    pub fn sequence_number(&self) -> Option<u32> {
        match self {
            Frame::Push { header, .. } => Some(header.sequence_number),
            Frame::Ack { header, .. } => Some(header.sequence_number),
            Frame::Ping { header } => Some(header.sequence_number),
            Frame::Fin { header } => Some(header.sequence_number),
            Frame::PathChallenge { header, .. } => Some(header.sequence_number),
            Frame::PathResponse { header, .. } => Some(header.sequence_number),
            Frame::Syn { .. } | Frame::SynAck { .. } => None,
        }
    }

    /// 获取长头帧的源连接ID（如果存在）。
    /// Gets the source connection ID of a long-header frame, if it has one.
    pub fn source_cid(&self) -> Option<u32> {
        match self {
            Frame::Syn { header, .. } => Some(header.source_cid),
            Frame::SynAck { header, .. } => Some(header.source_cid),
            _ => None,
        }
    }
} 