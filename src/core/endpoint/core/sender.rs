//! Sending-related logic for the `Endpoint`.

use crate::core::endpoint::Endpoint;
use crate::{
    error::{Error, Result},
    packet::frame::Frame,
    socket::{AsyncUdpSocket, SendCommand, SenderTaskCommand},
};
use std::{future::Future, net::SocketAddr};
use tokio::time::Instant;
use crate::core::endpoint::core::frame::{create_ack_frame, create_fin_frame, create_syn_ack_frame, create_syn_frame};
use crate::core::endpoint::types::state::ConnectionState;

/// A helper to build packets that respect the configured MTU.
///
/// 一个辅助工具，用于构建遵守配置的MTU的包。
struct PacketBuilder<F> {
    sender: F,
    frames: Vec<Frame>,
    current_size: usize,
    max_size: usize,
}

impl<F, Fut> PacketBuilder<F>
where
    F: Fn(Vec<Frame>) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    fn new(max_size: usize, sender: F) -> Self {
        Self {
            sender,
            frames: Vec::new(),
            current_size: 0,
            max_size,
        }
    }

    /// Tries to add a frame to the current packet. If it doesn't fit,
    /// it sends the current packet and starts a new one with the given frame.
    ///
    /// 尝试将一个帧添加到当前包中。如果放不下，它会发送当前包，
    /// 并用给定的帧开始一个新包。
    async fn add_frame(&mut self, frame: Frame) -> Result<()> {
        let frame_size = frame.encoded_size();
        if !self.frames.is_empty() && self.current_size + frame_size > self.max_size {
            self.flush().await?;
        }

        self.frames.push(frame);
        self.current_size += frame_size;
        Ok(())
    }

    /// Adds a batch of frames, flushing as necessary.
    ///
    /// 添加一批帧，必要时进行刷新。
    async fn add_frames(&mut self, frames: Vec<Frame>) -> Result<()> {
        for frame in frames {
            self.add_frame(frame).await?;
        }
        Ok(())
    }

    /// Sends any buffered frames as a single packet.
    ///
    /// 将所有缓冲的帧作为一个单独的包发送。
    async fn flush(&mut self) -> Result<()> {
        if self.frames.is_empty() {
            return Ok(());
        }

        let frames_to_send = std::mem::take(&mut self.frames);
        (self.sender)(frames_to_send).await?;
        self.current_size = 0;
        Ok(())
    }
}

impl<S: AsyncUdpSocket> Endpoint<S> {
    pub(in crate::core::endpoint) async fn packetize_and_send(&mut self) -> Result<()> {
        let now = Instant::now();

        // 1. Collect all frames that need to be sent without requiring &mut self.
        // 1. 收集所有需要发送的帧，这些操作不需要 &mut self。
        let mut frames_to_send = self.reliability.packetize_stream_data(
            self.peer_cid,
            self.peer_recv_window,
            now,
            self.start_time,
            None,
        );

        // 2. Perform actions that require &mut self and collect their resulting frames.
        // 2. 执行需要 &mut self 的操作，并收集它们产生的帧。
        if *self.state() == ConnectionState::Closing
            && !self.reliability.has_fin_in_flight()
        {
            let fin_frame = create_fin_frame(
                self.peer_cid,
                self.reliability.next_sequence_number(),
                &self.reliability,
                self.start_time,
            );
            self.reliability
                .track_frame_in_flight(fin_frame.clone(), now);
            frames_to_send.push(fin_frame);
        }

        // 3. Now that all mutable operations are done, create the builder and send.
        // 3. 所有可变操作都完成后，创建构建器并发送。
        if !frames_to_send.is_empty() {
            let sender = |p: Vec<Frame>| self.send_raw_frames(p);
            let mut packet_builder =
                PacketBuilder::new(self.config.connection.max_packet_size, sender);
            packet_builder.add_frames(frames_to_send).await?;
            packet_builder.flush().await?;
        }

        Ok(())
    }

    pub(in crate::core::endpoint) async fn send_initial_syn(&mut self) -> Result<()> {
        // Phase 1: Collect all frames to be sent for the initial packet.
        let now = Instant::now();
        let mut frames_to_send = Vec::new();

        let syn_frame = create_syn_frame(&self.config, self.local_cid);
        frames_to_send.push(syn_frame);

        let push_frames = self.reliability.packetize_stream_data(
            self.peer_cid,
            self.peer_recv_window,
            now,
            self.start_time,
            None,
        );
        frames_to_send.extend(push_frames);

        // Phase 2: Assert the single-packet limit.
        // The check is now enforced at the API level by `InitialData::new`.
        // We assert here to catch any logic errors during development.
        let total_size: usize = frames_to_send.iter().map(|f| f.encoded_size()).sum();
        assert!(
            total_size <= self.config.connection.max_packet_size,
            "Initial packet size exceeded MTU, which should have been caught at the API level!"
        );

        // Phase 3: Send as a single raw datagram.
        self.send_raw_frames(frames_to_send).await
    }

    /// Sends a SYN-ACK frame. This is a high-priority control frame and is sent immediately.
    /// 发送一个SYN-ACK帧。这是一个高优先级的控制帧，会被立即发送。
    pub(in crate::core::endpoint) async fn send_syn_ack(&mut self) -> Result<()> {
        let frame = create_syn_ack_frame(&self.config, self.peer_cid, self.local_cid);
        self.send_raw_frames(vec![frame]).await
    }

    pub(in crate::core::endpoint) async fn send_standalone_ack(&mut self) -> Result<()> {
        if !self.reliability.is_ack_pending() {
            return Ok(());
        }
        let frame = create_ack_frame(self.peer_cid, &mut self.reliability, self.start_time);
        self.reliability.on_ack_sent(); // This requires &mut self

        // Since we've mutated self, we can now send the frame.
        self.send_raw_frames(vec![frame]).await
    }

    /// Sends a raw batch of frames as a single UDP packet.
    /// This should only be used for high-priority control frames that need to bypass the MTU logic,
    /// or by the PacketBuilder itself.
    ///
    /// 将一批原始帧作为单个UDP数据包发送。
    /// 这应该仅用于需要绕过MTU逻辑的高优先级控制帧，或由PacketBuilder本身使用。
    async fn send_raw_frames(&self, frames: Vec<Frame>) -> Result<()> {
        if frames.is_empty() {
            return Ok(());
        }
        
        // 添加调试信息
        if !frames.is_empty() {
            tracing::debug!(
                cid = self.local_cid,
                remote_addr = %self.remote_addr,
                frame_count = frames.len(),
                first_frame = ?frames[0],
                "Sending frames to remote address"
            );
        }
        
        let cmd = SendCommand {
            remote_addr: self.remote_addr,
            frames,
        };
        self.sender
            .send(SenderTaskCommand::Send(cmd))
            .await
            .map_err(|_| Error::ChannelClosed)
    }

    /// Sends a list of frames, respecting the MTU by chunking them into multiple packets if necessary.
    ///
    /// 发送一个帧列表，必要时通过将它们分块到多个包中来遵守MTU。
    pub(in crate::core::endpoint) async fn send_frames(&self, frames: Vec<Frame>) -> Result<()> {
        if frames.is_empty() {
            return Ok(());
        }
        let sender = |p: Vec<Frame>| self.send_raw_frames(p);
        let mut packet_builder =
            PacketBuilder::new(self.config.connection.max_packet_size, sender);
        packet_builder.add_frames(frames).await?;
        packet_builder.flush().await
    }

    pub(in crate::core::endpoint) async fn send_frame_to(&self, frame: Frame, remote_addr: SocketAddr) -> Result<()> {
        let cmd = SendCommand {
            remote_addr,
            frames: vec![frame],
        };
        self.sender
            .send(SenderTaskCommand::Send(cmd))
            .await
            .map_err(|_| Error::ChannelClosed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::{command::Command, header::ShortHeader};
    use bytes::Bytes;
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;

    fn dummy_push_frame(payload_size: usize) -> Frame {
        Frame::Push {
            header: ShortHeader {
                command: Command::Push,
                connection_id: 123,
                recv_window_size: 65535,
                timestamp: 0,
                sequence_number: 0,
                recv_next_sequence: 0,
                payload_length: payload_size as u16,
            },
            payload: Bytes::from(vec![0u8; payload_size]),
        }
    }

    #[tokio::test]
    async fn test_packet_builder_flush_empty() {
        let sent_packets = Arc::new(Mutex::new(Vec::new()));
        let sent_packets_clone = sent_packets.clone();

        let sender = move |frames: Vec<Frame>| {
            let sent_packets_clone = sent_packets_clone.clone();
            async move {
                sent_packets_clone.lock().unwrap().push(frames);
                Ok(())
            }
        };

        let mut builder = PacketBuilder::new(1500, sender);
        builder.flush().await.unwrap();

        assert!(sent_packets.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_packet_builder_single_frame() {
        let (tx, mut rx) = mpsc::channel(10);
        let sender = |frames: Vec<Frame>| {
            let tx = tx.clone();
            async move {
                tx.send(frames).await.unwrap();
                Ok(())
            }
        };

        let mut builder = PacketBuilder::new(1500, sender);
        let frame = dummy_push_frame(100);
        builder.add_frame(frame.clone()).await.unwrap();
        builder.flush().await.unwrap();

        let packet = rx.recv().await.unwrap();
        assert_eq!(packet, vec![frame]);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_packet_builder_multiple_frames_fit() {
        let (tx, mut rx) = mpsc::channel(10);
        let sender = |frames: Vec<Frame>| {
            let tx = tx.clone();
            async move {
                tx.send(frames).await.unwrap();
                Ok(())
            }
        };

        let mut builder = PacketBuilder::new(1500, sender);
        let frame1 = dummy_push_frame(100);
        let frame2 = dummy_push_frame(200);

        builder.add_frame(frame1.clone()).await.unwrap();
        builder.add_frame(frame2.clone()).await.unwrap();
        builder.flush().await.unwrap();

        let packet = rx.recv().await.unwrap();
        assert_eq!(packet, vec![frame1, frame2]);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_packet_builder_exceeds_mtu() {
        let (tx, mut rx) = mpsc::channel(10);
        let sender = |frames: Vec<Frame>| {
            let tx = tx.clone();
            async move {
                tx.send(frames).await.unwrap();
                Ok(())
            }
        };

        let max_size = 200;
        let mut builder = PacketBuilder::new(max_size, sender);

        // frame1 size: 21 + 100 = 121. current_size = 121.
        let frame1 = dummy_push_frame(100);
        // frame2 size: 21 + 100 = 121.  121 (current) + 121 (new) = 242 > 200.
        // This will trigger a flush of frame1.
        let frame2 = dummy_push_frame(100);

        // Add frame1, it should be buffered
        builder.add_frame(frame1.clone()).await.unwrap();
        assert_eq!(builder.current_size, frame1.encoded_size());

        // Add frame2, this should trigger a flush of frame1
        builder.add_frame(frame2.clone()).await.unwrap();
        // After flush, builder should only contain frame2
        assert_eq!(builder.current_size, frame2.encoded_size());

        // The first packet should have only frame1
        let packet1 = rx.recv().await.unwrap();
        assert_eq!(packet1, vec![frame1]);

        // Flush the remaining frame
        builder.flush().await.unwrap();
        let packet2 = rx.recv().await.unwrap();
        assert_eq!(packet2, vec![frame2]);

        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_packet_builder_add_frames_batch() {
        let (tx, mut rx) = mpsc::channel(10);
        let sender = |frames: Vec<Frame>| {
            let tx = tx.clone();
            async move {
                tx.send(frames).await.unwrap();
                Ok(())
            }
        };

        let max_size = 400;
        let mut builder = PacketBuilder::new(max_size, sender);

        let frames = vec![
            dummy_push_frame(100), // 119
            dummy_push_frame(100), // 119 -> total 238
            dummy_push_frame(100), // 119 -> total 357
            dummy_push_frame(100), // 119 -> total 476 > 400. This will trigger flush.
            dummy_push_frame(100), // 119
        ];

        let frame_copies = frames.clone();
        builder.add_frames(frames).await.unwrap();
        builder.flush().await.unwrap();

        // First packet
        let packet1 = rx.recv().await.unwrap();
        assert_eq!(packet1, &frame_copies[0..3]);

        // Second packet
        let packet2 = rx.recv().await.unwrap();
        assert_eq!(packet2, &frame_copies[3..5]);

        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_packet_builder_frame_equals_max_size() {
        let (tx, mut rx) = mpsc::channel(10);
        let sender = |frames: Vec<Frame>| {
            let tx = tx.clone();
            async move {
                tx.send(frames).await.unwrap();
                Ok(())
            }
        };

        let max_size = 150;
        let mut builder = PacketBuilder::new(max_size, sender);
        let frame1 = dummy_push_frame(max_size - ShortHeader::ENCODED_SIZE - 1); // size is exactly max_size
        let frame2 = dummy_push_frame(10);

        builder.add_frame(frame1.clone()).await.unwrap();
        // The builder should not flush here, it should flush on the *next* add.
        assert_eq!(builder.current_size, max_size);
        assert!(rx.try_recv().is_err());

        builder.add_frame(frame2.clone()).await.unwrap();

        // Now frame1 should have been flushed.
        let packet1 = rx.recv().await.unwrap();
        assert_eq!(packet1, vec![frame1]);

        builder.flush().await.unwrap();
        let packet2 = rx.recv().await.unwrap();
        assert_eq!(packet2, vec![frame2]);

        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_packet_builder_frame_larger_than_max_size() {
        let (tx, mut rx) = mpsc::channel(10);
        let sender = |frames: Vec<Frame>| {
            let tx = tx.clone();
            async move {
                tx.send(frames).await.unwrap();
                Ok(())
            }
        };

        let max_size = 100;
        let mut builder = PacketBuilder::new(max_size, sender);
        let large_frame = dummy_push_frame(200); // 219 > 100

        builder.add_frame(large_frame.clone()).await.unwrap();
        // The builder should contain the large frame, but not flush until the next one is added or flush() is called.
        assert_eq!(builder.current_size, large_frame.encoded_size());
        assert!(rx.try_recv().is_err());

        builder.flush().await.unwrap();
        let packet = rx.recv().await.unwrap();
        assert_eq!(packet, vec![large_frame]);

        assert!(rx.try_recv().is_err());
    }
} 