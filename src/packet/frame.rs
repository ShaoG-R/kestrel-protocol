//! 定义了协议中可以在网络上传输的完整数据帧。
//! Defines the complete data frames that can be transmitted over the network.

use super::command;
use super::header::{LongHeader, ShortHeader};
use bytes::{Buf, BufMut, Bytes};

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
    /// Decodes a single frame from the front of a buffer cursor.
    /// The cursor is advanced past the decoded frame.
    ///
    /// 从缓冲区光标的前端解码单个帧。
    /// 光标会前进到已解码帧之后。
    pub fn decode(cursor: &mut &[u8]) -> Option<Self> {
        if cursor.is_empty() {
            return None;
        }

        // Peek the first byte to decide between long and short header
        let command = command::Command::from_u8(cursor[0])?;

        if command.is_long_header() {
            let header = LongHeader::decode(cursor)?;
            let payload_len = header.payload_length as usize;
            if cursor.len() < payload_len {
                return None; // Not enough data for the payload
            }
            let payload = Bytes::copy_from_slice(&cursor[..payload_len]);
            cursor.advance(payload_len);
            return match header.command {
                command::Command::Syn => Some(Frame::Syn { header, payload }),
                command::Command::SynAck => Some(Frame::SynAck { header, payload }),
                _ => None,
            };
        }

        // Short header frames can be coalesced.
        let header = ShortHeader::decode(cursor)?;
        let payload_len = header.payload_length as usize;

        if payload_len > 0 && cursor.len() < payload_len {
            return None; // Not enough data for payload
        }

        match header.command {
            // Frames with variable-length payloads
            command::Command::Push => {
                let payload = Bytes::copy_from_slice(&cursor[..payload_len]);
                cursor.advance(payload_len);
                Some(Frame::Push { header, payload })
            }
            command::Command::Ack => {
                let payload = Bytes::copy_from_slice(&cursor[..payload_len]);
                cursor.advance(payload_len);
                Some(Frame::Ack { header, payload })
            }
            // Frames with fixed-length or no payload
            command::Command::Ping => Some(Frame::Ping { header }),
            command::Command::Fin => Some(Frame::Fin { header }),
            command::Command::PathChallenge => {
                if cursor.len() < 8 {
                    return None;
                }
                let challenge_data = u64::from_be_bytes(cursor[..8].try_into().ok()?);
                cursor.advance(8);
                Some(Frame::PathChallenge {
                    header,
                    challenge_data,
                })
            }
            command::Command::PathResponse => {
                if cursor.len() < 8 {
                    return None;
                }
                let challenge_data = u64::from_be_bytes(cursor[..8].try_into().ok()?);
                cursor.advance(8);
                Some(Frame::PathResponse {
                    header,
                    challenge_data,
                })
            }
            _ => None, // Not a short header command, or is a long header one.
        }
    }

    /// 将帧编码到缓冲区。
    /// Encodes the frame into a buffer.
    pub fn encode<B: BufMut>(&self, buf: &mut B) {
        match self {
            Frame::Push { header, payload } => {
                debug_assert_eq!(header.payload_length as usize, payload.len());
                header.encode(buf);
                buf.put_slice(payload);
            }
            Frame::Ack { header, payload } => {
                debug_assert_eq!(header.payload_length as usize, payload.len());
                header.encode(buf);
                buf.put_slice(payload);
            }
            Frame::Ping { header } => {
                debug_assert_eq!(header.payload_length, 0);
                header.encode(buf);
            }
            Frame::Syn { header, payload } => {
                debug_assert_eq!(header.payload_length as usize, payload.len());
                header.encode(buf);
                buf.put_slice(payload);
            }
            Frame::SynAck { header, payload } => {
                debug_assert_eq!(header.payload_length as usize, payload.len());
                header.encode(buf);
                buf.put_slice(payload);
            }
            Frame::Fin { header } => {
                debug_assert_eq!(header.payload_length, 0);
                header.encode(buf);
            }
            Frame::PathChallenge {
                header,
                challenge_data,
            } => {
                debug_assert_eq!(header.payload_length, 8);
                header.encode(buf);
                buf.put_slice(&challenge_data.to_be_bytes());
            }
            Frame::PathResponse {
                header,
                challenge_data,
            } => {
                debug_assert_eq!(header.payload_length, 8);
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

    /// Returns the destination connection ID of the frame.
    ///
    /// For `SYN` packets, this can be 0 if it's an initial connection attempt.
    /// For all other packets, this identifies the recipient's endpoint.
    ///
    /// 返回帧的目标连接ID。
    ///
    /// 对于 `SYN` 包，如果它是初始连接尝试，则可以为0。
    /// 对于所有其他包，这标识了接收者的端点。
    pub fn destination_cid(&self) -> u32 {
        match self {
            Frame::Syn { header, .. } => header.destination_cid,
            Frame::SynAck { header, .. } => header.destination_cid,
            Frame::Push { header, .. } => header.connection_id,
            Frame::Ack { header, .. } => header.connection_id,
            Frame::Fin { header, .. } => header.connection_id,
            Frame::Ping { header, .. } => header.connection_id,
            Frame::PathChallenge { header, .. } => header.connection_id,
            Frame::PathResponse { header, .. } => header.connection_id,
        }
    }
} 