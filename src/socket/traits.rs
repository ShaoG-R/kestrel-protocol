//! Traits for abstracting over UDP socket implementations.
use crate::error::Result;
use async_trait::async_trait;
use std::net::SocketAddr;
use tokio::net::UdpSocket;

/// An asynchronous UDP socket interface.
///
/// This trait allows for abstracting over the underlying UDP socket implementation,
/// enabling custom socket implementations for testing or other purposes.
///
/// 异步UDP套接字接口。
///
/// 此trait允许对底层UDP套接字实现进行抽象，从而可以为测试或其他目的自定义套接字实现。
#[async_trait]
pub trait AsyncUdpSocket: Send + Sync + 'static {
    /// Sends data on the socket to the given address.
    async fn send_to(&self, buf: &[u8], target: SocketAddr) -> Result<usize>;

    /// Receives a single datagram on the socket.
    async fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)>;

    /// Returns the local address that this socket is bound to.
    fn local_addr(&self) -> Result<SocketAddr>;
}

#[async_trait]
impl AsyncUdpSocket for UdpSocket {
    async fn send_to(&self, buf: &[u8], target: SocketAddr) -> Result<usize> {
        UdpSocket::send_to(self, buf, target).await.map_err(Into::into)
    }

    async fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)> {
        UdpSocket::recv_from(self, buf).await.map_err(Into::into)
    }

    fn local_addr(&self) -> Result<SocketAddr> {
        UdpSocket::local_addr(self).map_err(Into::into)
    }
}

/// A trait for UDP sockets that can be bound to a local address.
///
/// This extends the `AsyncUdpSocket` trait with the ability to create a new
/// socket by binding to an address.
///
/// 可绑定到本地地址的UDP套接字 trait。
///
/// 该 trait 扩展了 `AsyncUdpSocket`，增加了通过绑定地址创建新套接字的能力。
#[async_trait]
pub trait BindableUdpSocket: AsyncUdpSocket + Sized {
    /// Binds a new socket to the given address.
    /// 将新套接字绑定到给定地址。
    async fn bind(addr: SocketAddr) -> Result<Self>;
}

#[async_trait]
impl BindableUdpSocket for UdpSocket {
    async fn bind(addr: SocketAddr) -> Result<Self> {
        UdpSocket::bind(addr).await.map_err(Into::into)
    }
} 