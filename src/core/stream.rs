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
        loop {
            // First, try to fulfill the read from the internal buffer.
            if !self.read_buffer.is_empty() {
                let current_chunk = &mut self.read_buffer[0];
                let len = std::cmp::min(current_chunk.len(), buf.remaining());

                if len > 0 {
                    buf.put_slice(&current_chunk[..len]);
                    current_chunk.advance(len);
                }

                if current_chunk.is_empty() {
                    self.read_buffer.pop_front();
                }

                // If we copied ANY data, we MUST return Ready. This is the fix.
                // It prevents us from incorrectly returning Pending later.
                if len > 0 {
                    return Poll::Ready(Ok(()));
                }
            }

            // If we reached here, the internal buffer was empty and we copied no data.
            // Now, we must try to poll the channel for new data.
            match self.rx_from_endpoint.poll_recv(cx) {
                Poll::Ready(Some(data_vec)) => {
                    // We got new data. It's crucial to continue the loop to
                    // process this new data immediately.
                    self.read_buffer.extend(data_vec);
                    continue;
                }
                Poll::Ready(None) => {
                    // Channel closed, and internal buffer is empty. This is EOF.
                    return Poll::Ready(Ok(()));
                }
                Poll::Pending => {
                    // No data in our buffer, and no data from the channel. We must wait.
                    return Poll::Pending;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::endpoint::StreamCommand;
    use bytes::Bytes;
    use tokio::io::AsyncReadExt;

    /// Creates a Stream and the sender-side of its internal command channel
    /// for testing purposes.
    fn setup_stream() -> (
        Stream,
        mpsc::Sender<StreamCommand>,
        mpsc::Sender<Vec<Bytes>>,
    ) {
        let (tx_to_endpoint, _) = mpsc::channel::<StreamCommand>(10);
        let (tx_from_endpoint, rx_from_endpoint) = mpsc::channel::<Vec<Bytes>>(10);

        let stream = Stream::new(tx_to_endpoint.clone(), rx_from_endpoint);
        (stream, tx_to_endpoint, tx_from_endpoint)
    }

    #[tokio::test]
    async fn test_stream_read_simple() {
        let (mut stream, _tx_cmd, tx_data) = setup_stream();

        let data = Bytes::from("hello");
        tx_data.send(vec![data.clone()]).await.unwrap();

        let mut buf = vec![0; 5];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, data);
    }

    #[tokio::test]
    async fn test_stream_read_pre_buffered() {
        let (mut stream, _tx_cmd, tx_data) = setup_stream();

        // Send data before the stream is ever read from.
        tx_data.send(vec![Bytes::from("world")]).await.unwrap();

        let mut buf = vec![0; 5];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"world");
    }

    #[tokio::test]
    async fn test_stream_read_multiple_chunks() {
        let (mut stream, _tx_cmd, tx_data) = setup_stream();

        // Send data as multiple chunks in a single Vec
        let chunks = vec![Bytes::from("he"), Bytes::from("llo"), Bytes::from(" world")];
        tx_data.send(chunks).await.unwrap();

        let mut buf = vec![0; 11];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello world");
    }

    #[tokio::test]
    async fn test_stream_read_multiple_sends() {
        let (mut stream, _tx_cmd, tx_data) = setup_stream();

        // Send data in multiple distinct sends
        tokio::spawn({
            let tx_data = tx_data.clone();
            async move {
                tx_data.send(vec![Bytes::from("hello ")]).await.unwrap();
                // small delay to ensure reads happen in parts
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                tx_data.send(vec![Bytes::from("world")]).await.unwrap();
            }
        });

        let mut buf = vec![0; 11];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello world");
    }

    #[tokio::test]
    async fn test_stream_read_into_small_buffers() {
        let (mut stream, _tx_cmd, tx_data) = setup_stream();
        tx_data
            .send(vec![Bytes::from("first_second_third")])
            .await
            .unwrap();

        let mut buf1 = vec![0; 6];
        stream.read_exact(&mut buf1).await.unwrap();
        assert_eq!(&buf1, b"first_");

        let mut buf2 = vec![0; 6];
        stream.read_exact(&mut buf2).await.unwrap();
        assert_eq!(&buf2, b"second");

        let mut buf3 = vec![0; 6];
        stream.read_exact(&mut buf3).await.unwrap();
        assert_eq!(&buf3, b"_third");
    }

    #[tokio::test]
    async fn test_stream_eof() {
        let (mut stream, _tx_cmd, tx_data) = setup_stream();
        tx_data.send(vec![Bytes::from("final")]).await.unwrap();
        drop(tx_data); // Close the channel to signal EOF

        let mut buf = vec![0; 5];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"final");

        // The next read should return 0 bytes, indicating EOF.
        let n = stream.read(&mut buf).await.unwrap();
        assert_eq!(n, 0);
    }
}
