//! 定义协议的所有命令/包类型。
//! Defines all commands/packet types for the protocol.

use std::fmt;

/// The type of a packet. The first byte on the wire.
/// 包类型，网络传输的第一个字节。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Command {
    /// (Long Header) Connection request.
    /// (长头) 连接请求。
    Syn = 0x01,
    /// (Long Header) Connection acknowledgment.
    /// (长头) 连接确认。
    SynAck = 0x02,
    /// (Short Header) Unilateral connection closing.
    /// (短头) 单向关闭连接。
    Fin = 0x03,
    /// (Short Header) Data packet.
    /// (短头) 数据包。
    Push = 0x10,
    /// (Short Header) Acknowledgment packet with SACK information.
    /// (短头) 带有SACK信息的确认包。
    Ack = 0x11,
    /// (Short Header) Keep-alive and network probing packet.
    /// (短头) 用于保活和网络探测的心跳包。
    Ping = 0x12,
}

impl Command {
    /// 从一个字节尝试转换成 `Command`。
    /// Tries to convert a byte into a `Command`.
    pub fn from_u8(byte: u8) -> Option<Self> {
        match byte {
            0x01 => Some(Command::Syn),
            0x02 => Some(Command::SynAck),
            0x03 => Some(Command::Fin),
            0x10 => Some(Command::Push),
            0x11 => Some(Command::Ack),
            0x12 => Some(Command::Ping),
            _ => None,
        }
    }

    /// 检查命令是否使用长头。
    /// Checks if the command uses a Long Header.
    pub fn is_long_header(&self) -> bool {
        matches!(self, Command::Syn | Command::SynAck)
    }
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Command::Syn => "SYN",
            Command::SynAck => "SYN-ACK",
            Command::Fin => "FIN",
            Command::Push => "PUSH",
            Command::Ack => "ACK",
            Command::Ping => "PING",
        };
        write!(f, "{}", s)
    }
}
