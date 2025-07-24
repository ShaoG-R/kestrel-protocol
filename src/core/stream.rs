//! The user-facing stream API.
//!
//! This module provides the `Stream` struct, which implements `AsyncRead` and
//! `AsyncWrite` to provide a familiar stream-based interface for the reliable
//! connection.
//!
//! 面向用户的流 API。
//!
//! 此模块提供 `Stream` 结构体，该结构体实现 `AsyncRead` 和 `AsyncWrite`，
//! 为可靠连接提供熟悉的基于流的接口。

use crate::core::endpoint::StreamCommand;
use bytes::Bytes;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::AsyncWrite;
use tokio::sync::mpsc;
use tokio::io::{AsyncRead, ReadBuf};

/// A reliable, ordered, stream-oriented connection.
///
/// 一个可靠、有序、面向流的连接。
#[derive(Debug)]
pub struct Stream {
    /// Sends commands to the `Endpoint` worker task.
    tx_to_endpoint: mpsc::Sender<StreamCommand>,
    /// Receives ordered, reliable data from the `Endpoint` worker task.
    rx_from_endpoint: mpsc::Receiver<Bytes>,
    /// A small buffer to handle cases where the user's read buffer is smaller
    /// than the received data chunk.
    read_buffer: Bytes,
}

impl Stream {
    /// Creates a new `Stream`.
    pub(crate) fn new(
        tx_to_endpoint: mpsc::Sender<StreamCommand>,
        rx_from_endpoint: mpsc::Receiver<Bytes>,
    ) -> Self {
        Self {
            tx_to_endpoint,
            rx_from_endpoint,
            read_buffer: Bytes::new(),
        }
    }

    /// Checks if the connection is closed. This happens if the underlying
    /// Endpoint worker task has terminated.
    pub fn is_closed(&self) -> bool {
        self.tx_to_endpoint.is_closed()
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self
            .tx_to_endpoint
            .try_send(StreamCommand::SendData(Bytes::copy_from_slice(buf)))
        {
            Ok(_) => Poll::Ready(Ok(buf.len())),
            Err(mpsc::error::TrySendError::Full(_)) => Poll::Pending,
            Err(mpsc::error::TrySendError::Closed(_)) => {
                Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()))
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        // Data is flushed automatically by the Endpoint worker task.
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        // Send the close command. Ignore if the channel is already closed.
        let _ = self.tx_to_endpoint.try_send(StreamCommand::Close);
        Poll::Ready(Ok(()))
    }
}

impl AsyncRead for Stream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        // If we have data in our local buffer, use it first.
        if !self.read_buffer.is_empty() {
            let len = std::cmp::min(self.read_buffer.len(), buf.remaining());
            buf.put_slice(&self.read_buffer.split_to(len));
            return Poll::Ready(Ok(()));
        }

        // Otherwise, try to receive from the worker.
        match self.rx_from_endpoint.poll_recv(cx) {
            Poll::Ready(Some(data)) => {
                let len = std::cmp::min(data.len(), buf.remaining());
                buf.put_slice(&data[..len]);
                // If there's leftover data, buffer it.
                if data.len() > len {
                    self.read_buffer = data.slice(len..);
                }
                Poll::Ready(Ok(()))
            }
            Poll::Ready(None) => {
                // Channel closed, connection is dead.
                Poll::Ready(Ok(()))
            }
            Poll::Pending => Poll::Pending,
        }
    }
} 