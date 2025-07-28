//! Sending-related logic for the `Endpoint`.

use super::{
    frame_factory::{create_ack_frame, create_fin_frame, create_syn_ack_frame, create_syn_frame},
    Endpoint,
};
use crate::{
    error::{Error, Result},
    packet::frame::Frame,
    socket::{AsyncUdpSocket, SendCommand, SenderTaskCommand},
};
use std::net::SocketAddr;
use tokio::time::Instant;

/// A helper to build packets that respect the configured MTU.
///
/// 一个辅助工具，用于构建遵守配置的MTU的包。
struct PacketBuilder<'a, S: AsyncUdpSocket> {
    endpoint: &'a Endpoint<S>,
    frames: Vec<Frame>,
    current_size: usize,
    max_size: usize,
}

impl<'a, S: AsyncUdpSocket> PacketBuilder<'a, S> {
    fn new(endpoint: &'a Endpoint<S>) -> Self {
        Self {
            endpoint,
            frames: Vec::new(),
            current_size: 0,
            max_size: endpoint.config.max_packet_size,
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
        self.endpoint.send_raw_frames(frames_to_send).await?;
        self.current_size = 0;
        Ok(())
    }
}

impl<S: AsyncUdpSocket> Endpoint<S> {
    pub(super) async fn packetize_and_send(&mut self) -> Result<()> {
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
        if self.state == super::state::ConnectionState::Closing
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
            let mut packet_builder = PacketBuilder::new(self);
            packet_builder.add_frames(frames_to_send).await?;
            packet_builder.flush().await?;
        }

        Ok(())
    }

    pub(super) async fn send_initial_syn(&mut self) -> Result<()> {
        // Phase 1: Collect all frames to be sent for the initial packet.
        let now = Instant::now();
        let mut frames_to_send = Vec::new();
        let mut total_size = 0;

        let syn_frame = create_syn_frame(&self.config, self.local_cid);
        total_size += syn_frame.encoded_size();
        frames_to_send.push(syn_frame);

        let push_frames = self.reliability.packetize_stream_data(
            self.peer_cid,
            self.peer_recv_window,
            now,
            self.start_time,
            None,
        );
        for frame in &push_frames {
            total_size += frame.encoded_size();
        }
        frames_to_send.extend(push_frames);

        // Phase 2: Enforce the single-packet limit for 0-RTT.
        if total_size > self.config.max_packet_size {
            return Err(Error::InitialDataTooLarge);
        }

        // Phase 3: Send as a single raw datagram.
        self.send_raw_frames(frames_to_send).await
    }

    /// Sends a SYN-ACK frame. This is a high-priority control frame and is sent immediately.
    /// 发送一个SYN-ACK帧。这是一个高优先级的控制帧，会被立即发送。
    pub(super) async fn send_syn_ack(&mut self) -> Result<()> {
        let frame = create_syn_ack_frame(&self.config, self.peer_cid, self.local_cid);
        self.send_raw_frames(vec![frame]).await
    }

    pub(super) async fn send_standalone_ack(&mut self) -> Result<()> {
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
    pub(super) async fn send_frames(&self, frames: Vec<Frame>) -> Result<()> {
        if frames.is_empty() {
            return Ok(());
        }
        let mut packet_builder = PacketBuilder::new(self);
        packet_builder.add_frames(frames).await?;
        packet_builder.flush().await
    }

    pub(super) async fn send_frame_to(&self, frame: Frame, remote_addr: SocketAddr) -> Result<()> {
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