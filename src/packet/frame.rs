//! 定义了协议中可以在网络上传输的完整数据帧。
//! Defines the complete data frames that can be transmitted over the network.

use super::command;
use super::header::{LongHeader, ShortHeader};
use crate::packet::sack::{self, SackRange};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use std::time::Duration;

/// Frame type enum for retransmission reconstruction
/// 用于重传重构的帧类型枚举
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameType {
    Push,
    Syn,
    SynAck,
    Fin,
    PathChallenge,
    PathResponse,
}

/// Context information needed for frame reconstruction during retransmission
/// 重传期间帧重构所需的上下文信息
#[derive(Debug, Clone)]
pub struct RetransmissionContext {
    /// Start time of the connection for timestamp calculation
    /// 连接的开始时间，用于时间戳计算
    pub start_time: tokio::time::Instant,
    /// Current peer connection ID
    /// 当前对端连接ID
    pub current_peer_cid: u32,
    /// Protocol version
    /// 协议版本
    pub protocol_version: u8,
    /// Local connection ID
    /// 本地连接ID
    pub local_cid: u32,
    /// Next expected receive sequence number
    /// 下一个期望接收的序列号
    pub recv_next_sequence: u32,
    /// Receive window size
    /// 接收窗口大小
    pub recv_window_size: u16,
}

impl RetransmissionContext {
    /// Creates a new retransmission context
    /// 创建新的重传上下文
    pub fn new(
        start_time: tokio::time::Instant,
        current_peer_cid: u32,
        protocol_version: u8,
        local_cid: u32,
        recv_next_sequence: u32,
        recv_window_size: u16,
    ) -> Self {
        Self {
            start_time,
            current_peer_cid,
            protocol_version,
            local_cid,
            recv_next_sequence,
            recv_window_size,
        }
    }

    /// Calculates the current timestamp based on the start time
    /// 基于开始时间计算当前时间戳
    pub fn current_timestamp(&self, now: tokio::time::Instant) -> u32 {
        now.duration_since(self.start_time).as_millis() as u32
    }
}

/// Defines the retransmission strategy for different frame types.
/// 定义不同帧类型的重传策略。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReliabilityMode {
    /// Use full SACK-based reliable transmission.
    /// 使用基于SACK的完整可靠传输。
    Reliable,
    /// Use simple timeout-based retransmission.
    /// 使用基于超时的简单重传。
    SimpleRetransmit {
        max_retries: u8,
        retry_interval: Duration,
    },
    /// Best effort, no retransmission.
    /// 尽力而为，不重传。
    BestEffort,
}

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
    /// A SYN frame to initiate a connection. It no longer carries a payload.
    /// 用于发起连接的 SYN 帧。它不再携带载荷。
    Syn { header: LongHeader },
    /// A SYN-ACK frame to acknowledge a connection. It never carries a payload.
    /// 用于确认连接的 SYN-ACK 帧。它从不携带载荷。
    SynAck {
        header: LongHeader,
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
    // --- Smart Constructors ---
    // These constructors ensure that the payload_length in the header is always correct.
    // 这些构造函数确保头部中的 `payload_length` 始终是正确的。

    /// Creates a new PUSH frame.
    /// 创建一个新的 PUSH 帧。
    pub fn new_push(
        peer_cid: u32,
        sequence_number: u32,
        recv_next_sequence: u32,
        recv_window_size: u16,
        timestamp: u32,
        payload: Bytes,
    ) -> Self {
        let header = ShortHeader {
            command: command::Command::Push,
            connection_id: peer_cid,
            payload_length: payload.len() as u16,
            recv_window_size,
            timestamp,
            sequence_number,
            recv_next_sequence,
        };
        Frame::Push { header, payload }
    }

    /// Creates a new ACK frame.
    /// 创建一个新的 ACK 帧。
    pub fn new_ack(
        peer_cid: u32,
        recv_next_sequence: u32,
        recv_window_size: u16,
        sack_ranges: &[SackRange],
        timestamp: u32,
    ) -> Self {
        let mut payload = BytesMut::with_capacity(sack_ranges.len() * 8);
        sack::encode_sack_ranges(sack_ranges, &mut payload);
        let payload = payload.freeze();

        let header = ShortHeader {
            command: command::Command::Ack,
            connection_id: peer_cid,
            payload_length: payload.len() as u16,
            recv_window_size,
            timestamp,
            sequence_number: 0, // Per our design, ACKs don't have their own sequence number
            recv_next_sequence,
        };
        Frame::Ack { header, payload }
    }

    /// Creates a new PING frame.
    /// 创建一个新的 PING 帧。
    pub fn new_ping(
        peer_cid: u32,
        sequence_number: u32,
        recv_next_sequence: u32,
        recv_window_size: u16,
        timestamp: u32,
    ) -> Self {
        let header = ShortHeader {
            command: command::Command::Ping,
            connection_id: peer_cid,
            payload_length: 0,
            recv_window_size,
            timestamp,
            sequence_number,
            recv_next_sequence,
        };
        Frame::Ping { header }
    }

    /// Creates a new FIN frame.
    /// 创建一个新的 FIN 帧。
    pub fn new_fin(
        peer_cid: u32,
        sequence_number: u32,
        timestamp: u32,
        recv_next_sequence: u32,
        recv_window_size: u16,
    ) -> Self {
        let header = ShortHeader {
            command: command::Command::Fin,
            connection_id: peer_cid,
            payload_length: 0,
            recv_window_size,
            timestamp,
            sequence_number,
            recv_next_sequence,
        };
        Frame::Fin { header }
    }

    /// Creates a new SYN frame.
    /// 创建一个新的 SYN 帧。
    pub fn new_syn(protocol_version: u8, source_cid: u32, destination_cid: u32) -> Self {
        let header = LongHeader {
            command: command::Command::Syn,
            protocol_version,
            destination_cid,
            source_cid,
        };
        Frame::Syn { header }
    }

    /// Creates a new SYN-ACK frame.
    /// 创建一个新的 SYN-ACK 帧。
    pub fn new_syn_ack(
        protocol_version: u8,
        source_cid: u32,
        destination_cid: u32,
    ) -> Self {
        let header = LongHeader {
            command: command::Command::SynAck,
            protocol_version,
            destination_cid,
            source_cid,
        };
        Frame::SynAck { header }
    }

    /// Creates a new PathChallenge frame.
    /// 创建一个新的 PathChallenge 帧。
    pub fn new_path_challenge(
        peer_cid: u32,
        sequence_number: u32,
        timestamp: u32,
        challenge_data: u64,
    ) -> Self {
        let header = ShortHeader {
            command: command::Command::PathChallenge,
            connection_id: peer_cid,
            payload_length: 8,
            recv_window_size: 0, // Not relevant for this frame type
            timestamp,
            sequence_number,
            recv_next_sequence: 0, // Not relevant for this frame type
        };
        Frame::PathChallenge {
            header,
            challenge_data,
        }
    }

    /// Creates a new PathResponse frame.
    /// 创建一个新的 PathResponse 帧。
    pub fn new_path_response(
        peer_cid: u32,
        sequence_number: u32,
        timestamp: u32,
        challenge_data: u64,
    ) -> Self {
        let header = ShortHeader {
            command: command::Command::PathResponse,
            connection_id: peer_cid,
            payload_length: 8,
            recv_window_size: 0, // Not relevant for this frame type
            timestamp,
            sequence_number,
            recv_next_sequence: 0, // Not relevant for this frame type
        };
        Frame::PathResponse {
            header,
            challenge_data,
        }
    }

    // --- End of Smart Constructors ---

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
            return match header.command {
                command::Command::Syn => {
                    Some(Frame::Syn { header })
                }
                command::Command::SynAck => {
                    Some(Frame::SynAck { header })
                }
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
            Frame::Syn { header } => {
                header.encode(buf);
            }
            Frame::SynAck { header } => {
                header.encode(buf);
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
            Frame::Syn { .. } => None,
            Frame::SynAck { .. } => None,
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

    /// Returns the reliability mode for this frame type.
    /// This determines how the frame should be retransmitted.
    ///
    /// 返回此帧类型的可靠性模式。
    /// 这决定了帧应该如何重传。
    pub fn reliability_mode(&self) -> ReliabilityMode {
        match self {
            // Regular PUSH frames use full SACK-based reliability
            // 常规PUSH帧使用基于SACK的完整可靠性
            Frame::Push { .. } => ReliabilityMode::Reliable,
            
            // Control frames use simple retransmission
            // 控制帧使用简单重传
            Frame::Syn { .. } | Frame::SynAck { .. } | Frame::Fin { .. } => {
                ReliabilityMode::SimpleRetransmit {
                    max_retries: 5,
                    retry_interval: Duration::from_millis(500),
                }
            }
            
            // ACK and PING frames don't need retransmission
            // ACK和PING帧不需要重传
            Frame::Ack { .. } | Frame::Ping { .. } => ReliabilityMode::BestEffort,
            
            // Path validation frames use simple retransmission
            // 路径验证帧使用简单重传
            Frame::PathChallenge { .. } | Frame::PathResponse { .. } => {
                ReliabilityMode::SimpleRetransmit {
                    max_retries: 3,
                    retry_interval: Duration::from_millis(100),
                }
            }
        }
    }

    /// Updates the connection ID in frames that use ShortHeader.
    /// This is used for retransmission when the peer CID changes after handshake.
    /// 
    /// 更新使用ShortHeader的帧中的连接ID。
    /// 这用于握手后对端CID更改时的重传。
    pub fn update_connection_id(&mut self, new_connection_id: u32) {
        match self {
            // Long header frames (SYN, SYN-ACK) don't update connection_id
            // 长头部帧（SYN、SYN-ACK）不更新connection_id
            Frame::Syn { .. } | Frame::SynAck { .. } => {
                // Do nothing - long header frames use destination_cid which is different
                // 不做任何事 - 长头部帧使用destination_cid，这是不同的
            }
            // Short header frames update their connection_id
            // 短头部帧更新它们的connection_id
            Frame::Push { header, .. } => {
                header.connection_id = new_connection_id;
            }
            Frame::Ack { header, .. } => {
                header.connection_id = new_connection_id;
            }
            Frame::Fin { header, .. } => {
                header.connection_id = new_connection_id;
            }
            Frame::Ping { header, .. } => {
                header.connection_id = new_connection_id;
            }
            Frame::PathChallenge { header, .. } => {
                header.connection_id = new_connection_id;
            }
            Frame::PathResponse { header, .. } => {
                header.connection_id = new_connection_id;
            }
        }
    }

    /// Returns the reliability mode for this frame type with custom configuration.
    /// 使用自定义配置返回此帧类型的可靠性模式。
    pub fn reliability_mode_with_config(&self, config: &crate::config::Config) -> ReliabilityMode {
        if !config.reliability.enable_layered_retransmission {
            // If layered retransmission is disabled, all frames except ACK/PING use SACK
            // 如果禁用分层重传，除了ACK/PING外的所有帧都使用SACK
            return match self {
                Frame::Ack { .. } | Frame::Ping { .. } => ReliabilityMode::BestEffort,
                _ => ReliabilityMode::Reliable,
            };
        }

        match self {
            // For now, only use simple retransmission for control frames
            // Regular PUSH frames always use SACK-based reliability for better performance
            // 目前，只对控制帧使用简单重传
            // 常规PUSH帧始终使用基于SACK的可靠性以获得更好的性能
            Frame::Push { .. } => ReliabilityMode::Reliable,
            
            // Control frames use configured parameters
            // 控制帧使用配置的参数
            Frame::Syn { .. } | Frame::SynAck { .. } | Frame::Fin { .. } => {
                ReliabilityMode::SimpleRetransmit {
                    max_retries: config.reliability.control_frame_max_retries,
                    retry_interval: config.reliability.control_frame_retry_interval,
                }
            }
            
            Frame::Ack { .. } | Frame::Ping { .. } => ReliabilityMode::BestEffort,
            
            Frame::PathChallenge { .. } | Frame::PathResponse { .. } => {
                ReliabilityMode::SimpleRetransmit {
                    max_retries: 3,
                    retry_interval: Duration::from_millis(100),
                }
            }
        }
    }

    /// The wire format of a frame.
    ///
    /// 帧的有线格式。
    pub fn command(&self) -> command::Command {
        match self {
            Frame::Syn { .. } => command::Command::Syn,
            Frame::SynAck { .. } => command::Command::SynAck,
            Frame::Fin { .. } => command::Command::Fin,
            Frame::Push { .. } => command::Command::Push,
            Frame::Ack { .. } => command::Command::Ack,
            Frame::Ping { .. } => command::Command::Ping,
            Frame::PathChallenge { .. } => command::Command::PathChallenge,
            Frame::PathResponse { .. } => command::Command::PathResponse,
        }
    }

    /// Calculates the encoded size of the frame.
    ///
    /// 计算帧编码后的大小。
    pub fn encoded_size(&self) -> usize {
        let command_size = 1;
        let header_size = match self {
            Frame::Syn { .. } => LongHeader::ENCODED_SIZE,
            Frame::SynAck { .. } => LongHeader::ENCODED_SIZE,
            _ => ShortHeader::ENCODED_SIZE,
        };
        let payload_size = match self {
            Frame::Push { payload, .. } => payload.len(),
            Frame::Ack { payload, .. } => payload.len(),
            Frame::PathChallenge { .. } => 8, // 64-bit challenge data
            Frame::PathResponse { .. } => 8, // 64-bit challenge data
            Frame::Ping { .. } => 0,
            _ => 0,
        };
        command_size + header_size + payload_size
    }

    /// Returns the frame type for retransmission purposes
    /// 返回用于重传目的的帧类型
    pub fn frame_type(&self) -> Option<FrameType> {
        match self {
            Frame::Push { .. } => Some(FrameType::Push),
            Frame::Syn { .. } => Some(FrameType::Syn),
            Frame::SynAck { .. } => Some(FrameType::SynAck),
            Frame::Fin { .. } => Some(FrameType::Fin),
            Frame::PathChallenge { .. } => Some(FrameType::PathChallenge),
            Frame::PathResponse { .. } => Some(FrameType::PathResponse),
            // ACK and PING frames should not be stored for retransmission
            _ => None,
        }
    }

    /// Checks if this frame needs reliability tracking (retransmission timers)
    /// 检查此帧是否需要可靠性跟踪（重传定时器）
    pub fn needs_reliability_tracking(&self) -> bool {
        match self.reliability_mode() {
            ReliabilityMode::Reliable => true,
            ReliabilityMode::SimpleRetransmit { .. } => true,
            ReliabilityMode::BestEffort => false,
        }
    }
} 