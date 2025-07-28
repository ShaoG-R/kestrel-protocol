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

    /// An error occurred during address parsing.
    /// 地址解析期间发生错误。
    #[error("Address parsing error: {0}")]
    AddressParse(#[from] std::net::AddrParseError),

    /// A received packet was invalid and could not be decoded.
    /// 接收到的包无效，无法解码。
    #[error("Invalid packet received")]
    InvalidPacket,

    /// The connection was closed by the peer.
    /// 连接被对端关闭。
    #[error("Connection closed by peer")]
    ConnectionClosed,

    /// The initial data provided for a 0-RTT connection attempt exceeds the
    /// maximum packet size and cannot be sent in a single datagram.
    ///
    /// 为0-RTT连接尝试提供的初始数据超出了最大包大小，无法在单个数据报中发送。
    #[error("Initial 0-RTT data is too large to fit into a single packet")]
    InitialDataTooLarge,

    /// The remote endpoint is not reachable.
    ///
    /// 远端端点不可达。
    #[error("Connection is closed or closing")]
    ConnectionAborted,

    /// The connection timed out due to inactivity.
    /// 连接因不活动而超时。
    #[error("Connection timed out")]
    ConnectionTimeout,

    /// The path validation process timed out.
    /// 路径验证过程超时。
    #[error("Path validation timed out")]
    PathValidationTimeout,

    /// The operation could not be completed because the connection is not in an established state.
    /// 由于连接未处于建立状态，操作无法完成。
    #[error("Connection not established")]
    NotConnected,

    /// An internal channel for communication between tasks was closed unexpectedly.
    /// 用于任务间通信的内部通道意外关闭。
    #[error("Internal channel is broken")]
    ChannelClosed,

    /// The provided message is larger than the configured `max_payload_size`.
    /// 提供的消息大于配置的 `max_payload_size`。
    #[error("the message is too large to be sent")]
    MessageTooLarge,
}

/// A specialized `Result` type for this library.
/// 本库专用的 `Result` 类型。
pub type Result<T> = std::result::Result<T, Error>;

impl From<Error> for std::io::Error {
    fn from(err: Error) -> Self {
        use std::io::ErrorKind;
        match err {
            Error::Io(e) => e,
            Error::AddressParse(e) => std::io::Error::new(ErrorKind::InvalidInput, e),
            Error::ConnectionClosed => ErrorKind::ConnectionReset.into(),
            Error::ConnectionAborted => ErrorKind::ConnectionAborted.into(),
            Error::ConnectionTimeout => ErrorKind::TimedOut.into(),
            Error::PathValidationTimeout => ErrorKind::TimedOut.into(),
            Error::InvalidPacket => ErrorKind::InvalidData.into(),
            Error::ChannelClosed => ErrorKind::BrokenPipe.into(),
            Error::MessageTooLarge => ErrorKind::InvalidInput.into(),
            Error::NotConnected => ErrorKind::NotConnected.into(),
            Error::InitialDataTooLarge => ErrorKind::InvalidInput.into(),
        }
    }
}
