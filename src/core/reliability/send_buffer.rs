//! Manages the sending of data, including buffering, packetizing, and tracking
//! in-flight packets.
//!
//! 管理数据的发送，包括缓冲、打包和跟踪在途数据包。

use bytes::Bytes;
use std::collections::VecDeque;

/// Manages outgoing data stream buffering.
/// In-flight packet tracking has been moved to SackManager.
#[derive(Debug)]
pub struct SendBuffer {
    /// A queue of `Bytes` objects waiting to be packetized. This approach avoids
    /// copying data into a single large buffer. Each `Bytes` object is a separate
    /// block of data from a `write` call.
    stream_buffer: VecDeque<Bytes>,
    /// The total size of all `Bytes` objects in `stream_buffer`.
    stream_buffer_size: usize,
    /// Capacity of the stream buffer in bytes.
    stream_buffer_capacity: usize,
}

impl SendBuffer {
    /// Creates a new `SendBuffer`.
    pub fn new(capacity_bytes: usize) -> Self {
        Self {
            stream_buffer: VecDeque::new(),
            stream_buffer_size: 0,
            stream_buffer_capacity: capacity_bytes,
        }
    }

    /// Writes data to the stream buffer. Returns the number of bytes written.
    /// This implementation is zero-copy; it just adds the `Bytes` object to a queue.
    ///
    /// 将数据写入流缓冲区。返回写入的字节数。
    /// 这个实现是零拷贝的；它只是将 `Bytes` 对象添加到一个队列中。
    pub fn write_to_stream(&mut self, buf: Bytes) -> usize {
        let space_available = self
            .stream_buffer_capacity
            .saturating_sub(self.stream_buffer_size);
        if space_available == 0 {
            return 0;
        }

        let bytes_to_write = std::cmp::min(buf.len(), space_available);
        if bytes_to_write < buf.len() {
            let chunk = buf.slice(..bytes_to_write);
            self.stream_buffer.push_back(chunk);
        } else {
            self.stream_buffer.push_back(buf);
        }
        self.stream_buffer_size += bytes_to_write;
        bytes_to_write
    }

    /// Creates a data chunk for a new packet from the stream buffer.
    pub fn create_chunk(&mut self, max_size: usize) -> Option<Bytes> {
        let first_chunk = self.stream_buffer.front_mut()?;
        let chunk_size = std::cmp::min(first_chunk.len(), max_size);

        if chunk_size == 0 {
            // This can happen if the front chunk is empty for some reason.
            self.stream_buffer.pop_front();
            return self.create_chunk(max_size); // Try again with the next one.
        }

        let chunk = if chunk_size >= first_chunk.len() {
            // The whole chunk is being taken, so we can pop it. Since we know
            // the buffer is not empty from the `front_mut` check above, this
            // pop should always succeed. We handle the `None` case just to
            // satisfy the linter, but it should be unreachable.
            self.stream_buffer.pop_front()?
        } else {
            // Only a part of the chunk is taken.
            first_chunk.split_to(chunk_size)
        };

        self.stream_buffer_size -= chunk.len();
        Some(chunk)
    }

    /// Takes all data from the stream buffer.
    ///
    /// 取出流缓冲区中的所有数据。
    pub fn take_stream_buffer(&mut self) -> impl Iterator<Item = Bytes> {
        self.stream_buffer_size = 0;
        self.stream_buffer.drain(..)
    }

    /// Checks if the stream buffer is empty.
    ///
    /// 检查流缓冲区是否为空。
    pub fn is_stream_buffer_empty(&self) -> bool {
        self.stream_buffer.is_empty()
    }


}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_send_buffer() -> SendBuffer {
        SendBuffer::new(1024 * 1024)
    }

    #[test]
    fn test_stream_buffering_and_chunking() {
        let mut buffer = create_test_send_buffer();
        let data1 = Bytes::from_static(b"hello world");
        let data2 = Bytes::from_static(b", this is a test");

        assert_eq!(buffer.write_to_stream(data1), 11);
        assert_eq!(buffer.write_to_stream(data2), 16);
        assert_eq!(buffer.stream_buffer_size, 27);

        // First chunk should come from the first `Bytes` object.
        let chunk1 = buffer.create_chunk(5).unwrap();
        assert_eq!(chunk1, "hello");
        assert_eq!(buffer.stream_buffer_size, 22);
        assert_eq!(buffer.stream_buffer.front().unwrap(), " world");

        // Second chunk finishes off the first `Bytes` object.
        let chunk2 = buffer.create_chunk(10).unwrap();
        assert_eq!(chunk2, " world");
        assert_eq!(buffer.stream_buffer_size, 16);
        assert!(buffer
            .stream_buffer
            .front()
            .unwrap()
            .eq(", this is a test"));

        // Third chunk takes the entire second `Bytes` object.
        let chunk3 = buffer.create_chunk(100).unwrap();
        assert_eq!(chunk3, ", this is a test");
        assert_eq!(buffer.stream_buffer_size, 0);

        assert!(buffer.create_chunk(10).is_none());
        assert!(buffer.is_stream_buffer_empty());
    }

    #[test]
    fn test_capacity_limits() {
        let mut buffer = SendBuffer::new(10); // Small capacity for testing
        let data1 = Bytes::from_static(b"hello");
        let data2 = Bytes::from_static(b"world");
        let data3 = Bytes::from_static(b"!");

        assert_eq!(buffer.write_to_stream(data1), 5);
        assert_eq!(buffer.write_to_stream(data2), 5);
        assert_eq!(buffer.write_to_stream(data3), 0); // Should be rejected due to capacity

        assert_eq!(buffer.stream_buffer_size, 10);
    }
} 