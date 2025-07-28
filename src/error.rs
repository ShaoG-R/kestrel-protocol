//! 定义了库中所有可能的错误类型。
//! Defines all possible error types in the library.

use thiserror::Error;
use std::net::SocketAddr;
use tokio::time::Instant;

/// 帧处理器错误上下文，包含错误发生时的详细信息
/// Frame processor error context containing detailed information when error occurs
#[derive(Debug, Clone)]
pub struct ProcessorErrorContext {
    /// 处理器名称
    /// Processor name
    pub processor_name: &'static str,
    
    /// 连接ID
    /// Connection ID
    pub connection_id: u32,
    
    /// 源地址
    /// Source address
    pub src_addr: SocketAddr,
    
    /// 连接状态（发生错误时）
    /// Connection state (when error occurred)
    pub connection_state: String,
    
    /// 时间戳
    /// Timestamp
    pub timestamp: Instant,
    
    /// 额外的上下文信息
    /// Additional context information
    pub additional_info: Option<String>,
}

impl ProcessorErrorContext {
    /// 创建新的处理器错误上下文
    /// Create new processor error context
    pub fn new(
        processor_name: &'static str,
        connection_id: u32,
        src_addr: SocketAddr,
        connection_state: String,
        timestamp: Instant,
    ) -> Self {
        Self {
            processor_name,
            connection_id,
            src_addr,
            connection_state,
            timestamp,
            additional_info: None,
        }
    }
    
    /// 添加额外的上下文信息
    /// Add additional context information
    pub fn with_info<S: Into<String>>(mut self, info: S) -> Self {
        self.additional_info = Some(info.into());
        self
    }
}

impl std::fmt::Display for ProcessorErrorContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Processor: {}, CID: {}, Addr: {}, State: {}",
            self.processor_name, self.connection_id, self.src_addr, self.connection_state
        )?;
        
        if let Some(ref info) = self.additional_info {
            write!(f, ", Info: {}", info)?;
        }
        
        Ok(())
    }
}

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

    /// A received frame was invalid or unexpected.
    /// 接收到的帧无效或意外。
    #[error("Invalid frame: {0}")]
    InvalidFrame(String),


    /// 帧类型不匹配错误
    /// Frame type mismatch error
    #[error("Frame type mismatch: expected {expected}, got {actual}")]
    FrameTypeMismatch {
        expected: String,
        actual: String,
        context: ProcessorErrorContext,
    },


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
            Error::InvalidFrame(_) => ErrorKind::InvalidData.into(),
            Error::FrameTypeMismatch { .. } => ErrorKind::InvalidData.into(),
            Error::ChannelClosed => ErrorKind::BrokenPipe.into(),
            Error::MessageTooLarge => ErrorKind::InvalidInput.into(),
            Error::NotConnected => ErrorKind::NotConnected.into(),
            Error::InitialDataTooLarge => ErrorKind::InvalidInput.into(),
        }
    }
}
