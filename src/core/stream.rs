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
use bytes::{Buf, Bytes};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;

/// A reliable, ordered, stream-oriented connection.
///
/// 一个可靠、有序、面向流的连接。
#[derive(Debug)]
pub struct Stream {
    /// Sends commands to the `Endpoint` worker task.
    pub(crate) tx_to_endpoint: mpsc::Sender<StreamCommand>,
    /// Receives ordered, reliable data from the `Endpoint` worker task.
    pub(crate) rx_from_endpoint: mpsc::Receiver<Bytes>,
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
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
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
        // If we have leftover data from a previous read, use it first.
        if !self.read_buffer.is_empty() {
            let len = std::cmp::min(self.read_buffer.len(), buf.remaining());
            buf.put_slice(&self.read_buffer[..len]);
            // `advance` is a method on `impl Buf`, which `Bytes` implements.
            self.read_buffer.advance(len);
            return Poll::Ready(Ok(()));
        }

        // Otherwise, try to receive new data from the worker.
        match self.rx_from_endpoint.poll_recv(cx) {
            Poll::Ready(Some(mut data)) => {
                let len = std::cmp::min(data.len(), buf.remaining());
                buf.put_slice(&data[..len]);
                // If the received data is larger than the buffer, store the remainder.
                if data.len() > len {
                    self.read_buffer = data.split_off(len);
                }
                Poll::Ready(Ok(()))
            }
            Poll::Ready(None) => {
                // Channel closed, indicating the end of the stream.
                Poll::Ready(Ok(()))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}
