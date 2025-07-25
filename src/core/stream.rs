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
use crate::error::Result;
use bytes::{Buf, Bytes};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{mpsc, oneshot};

/// A reliable, ordered, stream-oriented connection.
///
/// 一个可靠、有序、面向流的连接。
#[derive(Debug)]
pub struct Stream {
    /// Sends commands to the `Endpoint` worker task.
    pub(crate) tx_to_endpoint: mpsc::Sender<StreamCommand>,
    /// Receives ordered, reliable data from the `Endpoint` worker task.
    pub(crate) rx_from_endpoint: mpsc::Receiver<Vec<Bytes>>,
    /// A buffer of received data chunks, waiting to be read by the user.
    read_buffer: VecDeque<Bytes>,
}

impl Stream {
    /// Creates a new `Stream`.
    pub(crate) fn new(
        tx_to_endpoint: mpsc::Sender<StreamCommand>,
        rx_from_endpoint: mpsc::Receiver<Vec<Bytes>>,
    ) -> Self {
        Self {
            tx_to_endpoint,
            rx_from_endpoint,
            read_buffer: VecDeque::new(),
        }
    }

    /// Checks if the connection is closed. This happens if the underlying
    /// Endpoint worker task has terminated.
    pub fn is_closed(&self) -> bool {
        self.tx_to_endpoint.is_closed()
    }

    /// Actively migrates the connection to a new remote address.
    ///
    /// This function will initiate the path validation process and return once
    /// the migration is successfully completed or has failed.
    ///
    /// 主动将连接迁移到一个新的远程地址。
    ///
    /// 此函数将启动路径验证过程，并在迁移成功完成或失败后返回。
    pub async fn migrate(&self, new_remote_addr: SocketAddr) -> Result<()> {
        let (notifier_tx, notifier_rx) = oneshot::channel();
        let cmd = StreamCommand::Migrate {
            new_addr: new_remote_addr,
            notifier: notifier_tx,
        };

        if self.tx_to_endpoint.send(cmd).await.is_err() {
            return Err(crate::error::Error::ChannelClosed);
        }

        notifier_rx
            .await
            .map_err(|_| crate::error::Error::ChannelClosed)?
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
        while !self.read_buffer.is_empty() {
            let current_chunk = &mut self.read_buffer[0];
            let len = std::cmp::min(current_chunk.len(), buf.remaining());

            if len > 0 {
                buf.put_slice(&current_chunk[..len]);
                current_chunk.advance(len);
            }

            // If we've finished with the current chunk, drop it.
            if current_chunk.is_empty() {
                self.read_buffer.pop_front();
            }

            // If the user's buffer is full, we're done for now.
            if buf.remaining() == 0 {
                return Poll::Ready(Ok(()));
            }
        }

        // Buffer is empty, try to receive new data from the worker.
        match self.rx_from_endpoint.poll_recv(cx) {
            Poll::Ready(Some(data_vec)) => {
                // Extend the buffer with the new chunks.
                self.read_buffer.extend(data_vec);

                // Now that we have new data, try to fulfill the read request again
                // by calling poll_read recursively. The `while` loop at the start
                // will handle the actual copying.
                // It's safe to call poll_read again because we're in a Poll::Ready state.
                self.poll_read(cx, buf)
            }
            Poll::Ready(None) => {
                // Channel closed, indicating the end of the stream.
                Poll::Ready(Ok(()))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}
