//! Traits for abstracting over UDP socket implementations.
use crate::error::Result;
use async_trait::async_trait;
use std::{fmt::Debug, net::SocketAddr};
use tokio::net::UdpSocket as TokioUdpSocket;

/// An asynchronous UDP socket interface.
///
/// This trait allows for abstracting over the underlying UDP socket implementation,
/// enabling custom socket implementations for testing or other purposes.
///
/// 异步UDP套接字接口。
///
/// 此trait允许对底层UDP套接字实现进行抽象，从而可以为测试或其他目的自定义套接字实现。
#[async_trait]
pub trait AsyncUdpSocket: Send + Sync + Debug + 'static {
    async fn send_to(&self, buf: &[u8], target: SocketAddr) -> Result<usize>;
    async fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)>;
    fn local_addr(&self) -> Result<SocketAddr>;
}

#[async_trait]
impl AsyncUdpSocket for TokioUdpSocket {
    async fn send_to(&self, buf: &[u8], target: SocketAddr) -> Result<usize> {
        TokioUdpSocket::send_to(self, buf, target)
            .await
            .map_err(Into::into)
    }

    async fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)> {
        TokioUdpSocket::recv_from(self, buf)
            .await
            .map_err(Into::into)
    }

    fn local_addr(&self) -> Result<SocketAddr> {
        TokioUdpSocket::local_addr(self).map_err(Into::into)
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
    async fn bind(addr: SocketAddr) -> Result<Self>;
}

#[async_trait]
impl BindableUdpSocket for TokioUdpSocket {
    async fn bind(addr: SocketAddr) -> Result<Self> {
        TokioUdpSocket::bind(addr).await.map_err(Into::into)
    }
} 