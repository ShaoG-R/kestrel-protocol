//! 定义了库中所有可能的错误类型。
//! Defines all possible error types in the library.

use thiserror::Error;

/// The primary error type for the reliable UDP protocol library.
/// 可靠UDP协议库的主要错误类型。
#[derive(Debug, Error)]
pub enum Error {
    /// An underlying I/O error occurred.
    /// 发生了底层的I/O错误。
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A received packet was invalid and could not be decoded.
    /// 接收到的包无效，无法解码。
    #[error("Invalid packet received")]
    InvalidPacket,

    /// The connection was closed by the peer.
    /// 连接被对端关闭。
    #[error("Connection closed by peer")]
    ConnectionClosed,

    /// An attempt was made to use a connection that was already closed or is in the process of closing.
    /// 尝试使用一个已经关闭或正在关闭的连接。
    #[error("Connection is closed or closing")]
    ConnectionAborted,

    /// The connection timed out.
    /// 连接超时。
    #[error("Connection timed out")]
    Timeout,

    /// An internal channel for communication between tasks was closed unexpectedly.
    /// 用于任务间通信的内部通道意外关闭。
    #[error("Internal channel is broken")]
    ChannelClosed,
}

/// A specialized `Result` type for this library.
/// 本库专用的 `Result` 类型。
pub type Result<T> = std::result::Result<T, Error>;

impl From<Error> for std::io::Error {
    fn from(err: Error) -> Self {
        use std::io::ErrorKind;
        match err {
            Error::Io(e) => e,
            Error::ConnectionClosed => ErrorKind::ConnectionReset.into(),
            Error::ConnectionAborted => ErrorKind::ConnectionAborted.into(),
            Error::Timeout => ErrorKind::TimedOut.into(),
            Error::InvalidPacket => ErrorKind::InvalidData.into(),
            Error::ChannelClosed => ErrorKind::BrokenPipe.into(),
        }
    }
}
