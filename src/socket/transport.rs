//! Transport layer abstraction for network communication.
//!
//! This module provides an abstraction over the underlying network transport,
//! handling frame serialization/deserialization, address management, and routing.
//!
//! 网络通信的传输层抽象。
//!
//! 此模块提供底层网络传输的抽象，处理帧的序列化/反序列化、地址管理和路由。

pub mod command;
pub mod sender;
pub mod udp;

use crate::{
    error::Result,
    packet::frame::Frame,
};
use async_trait::async_trait;
use std::{fmt::Debug, net::SocketAddr};

pub use command::TransportCommand;
pub use sender::transport_sender_task;
pub use udp::UdpTransport;

/// A batch of frames to be sent to a remote address.
///
/// 要发送到远程地址的帧批次。
#[derive(Debug, Clone)]
pub struct FrameBatch {
    /// The destination address.
    /// 目标地址。
    pub remote_addr: SocketAddr,
    /// The frames to send.
    /// 要发送的帧。
    pub frames: Vec<Frame>,
}

/// A received datagram containing decoded frames.
///
/// 包含已解码帧的接收数据报。
#[derive(Debug)]
pub struct ReceivedDatagram {
    /// The source address of the datagram.
    /// 数据报的源地址。
    pub remote_addr: SocketAddr,
    /// The decoded frames from the datagram.
    /// 从数据报解码的帧。
    pub frames: Vec<Frame>,
}

/// Transport layer interface for network communication.
///
/// This trait abstracts the underlying network transport, providing
/// high-level operations for sending and receiving frame batches.
///
/// 网络通信的传输层接口。
///
/// 此trait抽象底层网络传输，提供发送和接收帧批次的高级操作。
#[async_trait]
pub trait Transport: Send + Sync + Debug + 'static {
    /// Sends a batch of frames to the specified remote address.
    ///
    /// The transport is responsible for serializing the frames into
    /// a single UDP datagram and sending it.
    ///
    /// 向指定的远程地址发送一批帧。
    ///
    /// 传输层负责将帧序列化为单个UDP数据报并发送。
    async fn send_frames(&self, batch: FrameBatch) -> Result<()>;

    /// Receives the next datagram and decodes it into frames.
    ///
    /// This method blocks until a datagram is received, then decodes
    /// all frames from it and returns them as a batch.
    ///
    /// 接收下一个数据报并将其解码为帧。
    ///
    /// 此方法会阻塞直到接收到数据报，然后解码其中的所有帧并作为批次返回。
    async fn recv_frames(&self) -> Result<ReceivedDatagram>;

    /// Returns the local address this transport is bound to.
    ///
    /// 返回此传输绑定的本地地址。
    fn local_addr(&self) -> Result<SocketAddr>;
}

/// A transport that can be bound to a local address.
///
/// This extends the `Transport` trait with the ability to create a new
/// transport by binding to an address.
///
/// 可绑定到本地地址的传输。
///
/// 该trait扩展了`Transport`，增加了通过绑定地址创建新传输的能力。
#[async_trait]
pub trait BindableTransport: Transport + Sized {
    /// Creates a new transport bound to the specified address.
    ///
    /// 创建绑定到指定地址的新传输。
    async fn bind(addr: SocketAddr) -> Result<Self>;

    /// Rebinds the transport to a new local address.
    ///
    /// 将传输重新绑定到新的本地地址。
    async fn rebind(&self, new_addr: SocketAddr) -> Result<()>;
}