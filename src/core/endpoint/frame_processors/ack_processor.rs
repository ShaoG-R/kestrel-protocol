//! ACK 帧处理器 - 处理确认帧
//! ACK Frame Processor - Handles acknowledgment frames
//!
//! 该模块专门处理 ACK 帧，包括 SACK 信息解析、
//! RTT 计算、拥塞控制更新等逻辑。
//!
//! This module specifically handles ACK frames, including SACK information parsing,
//! RTT calculation, congestion control updates, etc.

use super::{FrameProcessor, FrameProcessorStatic, FrameProcessingContext};
use crate::{
    error::Result,
    packet::{frame::Frame, sack::decode_sack_ranges},
    socket::AsyncUdpSocket,
};
use std::net::SocketAddr;
use tokio::time::Instant;
use tracing::{debug, trace, warn};

use crate::core::endpoint::{Endpoint, state::ConnectionState};

/// ACK 帧处理器
/// ACK frame processor
pub struct AckProcessor;

impl<S: AsyncUdpSocket> FrameProcessor<S> for AckProcessor {
    fn process_frame(
        endpoint: &mut Endpoint<S>,
        frame: Frame,
        src_addr: SocketAddr,
        now: Instant,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        async move {
        let Frame::Ack { header, payload } = frame else {
            return Err(crate::error::Error::InvalidFrame("Expected ACK frame".into()));
        };

        let context = FrameProcessingContext::new(endpoint, src_addr, now);
        
        trace!(
            cid = context.local_cid,
            recv_next_seq = header.recv_next_sequence,
            recv_window = header.recv_window_size,
            payload_len = payload.len(),
            state = ?context.connection_state,
            "Processing ACK frame"
        );

        // 根据连接状态处理 ACK 帧
        // Process ACK frame based on connection state
        match context.connection_state {
            ConnectionState::SynReceived => {
                Self::handle_ack_in_syn_received(endpoint, header, payload, now).await
            }
            ConnectionState::Established => {
                Self::handle_ack_in_established(endpoint, header, payload, now).await
            }
            ConnectionState::ValidatingPath { .. } => {
                Self::handle_ack_in_validating_path(endpoint, header, payload, now).await
            }
            ConnectionState::FinWait => {
                Self::handle_ack_in_fin_wait(endpoint, header, payload, now).await
            }
            ConnectionState::Closing => {
                Self::handle_ack_in_closing(endpoint, header, payload, now).await
            }
            ConnectionState::ClosingWait => {
                Self::handle_ack_in_closing_wait(endpoint, header, payload, now).await
            }
            _ => {
                warn!(
                    cid = context.local_cid,
                    state = ?context.connection_state,
                    "Ignoring ACK frame in unexpected state"
                );
                Ok(())
            }
        }
        }
    }
}

impl FrameProcessorStatic for AckProcessor {
    fn can_handle(frame: &Frame) -> bool {
        matches!(frame, Frame::Ack { .. })
    }

    fn name() -> &'static str {
        "AckProcessor"
    }
}

impl AckProcessor {
    /// 处理 ACK 帧的通用逻辑
    /// Common logic for processing ACK frames
    async fn process_ack_common<S: AsyncUdpSocket>(
        endpoint: &mut Endpoint<S>,
        header: crate::packet::header::ShortHeader,
        payload: bytes::Bytes,
        now: Instant,
    ) -> Result<()> {
        // 更新对端接收窗口大小
        // Update peer receive window size
        endpoint.peer_recv_window = header.recv_window_size as u32;
        
        // 解析 SACK 范围
        // Parse SACK ranges
        let sack_ranges = decode_sack_ranges(payload);
        
        debug!(
            cid = endpoint.local_cid(),
            recv_next_seq = header.recv_next_sequence,
            sack_ranges_count = sack_ranges.len(),
            peer_recv_window = endpoint.peer_recv_window,
            "Processing ACK with SACK ranges"
        );

        // 处理 ACK 并获取需要重传的帧
        // Process ACK and get frames that need retransmission
        let frames_to_retx = endpoint.reliability_mut().handle_ack(
            header.recv_next_sequence,
            sack_ranges,
            now,
        );

        // 如果有需要重传的帧，立即发送
        // If there are frames to retransmit, send them immediately
        if !frames_to_retx.is_empty() {
            debug!(
                cid = endpoint.local_cid(),
                retx_count = frames_to_retx.len(),
                "Fast retransmitting frames based on SACK"
            );
            endpoint.send_frames(frames_to_retx).await?;
        }

        Ok(())
    }

    /// 在 SynReceived 状态下处理 ACK 帧
    /// Handle ACK frame in SynReceived state
    async fn handle_ack_in_syn_received<S: AsyncUdpSocket>(
        endpoint: &mut Endpoint<S>,
        header: crate::packet::header::ShortHeader,
        payload: bytes::Bytes,
        now: Instant,
    ) -> Result<()> {
        debug!(
            cid = endpoint.local_cid(),
            "Handling ACK in SynReceived state"
        );

        Self::process_ack_common(endpoint, header, payload, now).await
    }

    /// 在 Established 状态下处理 ACK 帧
    /// Handle ACK frame in Established state
    async fn handle_ack_in_established<S: AsyncUdpSocket>(
        endpoint: &mut Endpoint<S>,
        header: crate::packet::header::ShortHeader,
        payload: bytes::Bytes,
        now: Instant,
    ) -> Result<()> {
        trace!(
            cid = endpoint.local_cid(),
            "Handling ACK in Established state"
        );

        Self::process_ack_common(endpoint, header, payload, now).await
    }

    /// 在 ValidatingPath 状态下处理 ACK 帧
    /// Handle ACK frame in ValidatingPath state
    async fn handle_ack_in_validating_path<S: AsyncUdpSocket>(
        endpoint: &mut Endpoint<S>,
        header: crate::packet::header::ShortHeader,
        payload: bytes::Bytes,
        now: Instant,
    ) -> Result<()> {
        debug!(
            cid = endpoint.local_cid(),
            "Handling ACK during path validation"
        );

        Self::process_ack_common(endpoint, header, payload, now).await
    }

    /// 在 FinWait 状态下处理 ACK 帧
    /// Handle ACK frame in FinWait state
    async fn handle_ack_in_fin_wait<S: AsyncUdpSocket>(
        endpoint: &mut Endpoint<S>,
        header: crate::packet::header::ShortHeader,
        payload: bytes::Bytes,
        now: Instant,
    ) -> Result<()> {
        debug!(
            cid = endpoint.local_cid(),
            "Handling ACK in FinWait state"
        );

        Self::process_ack_common(endpoint, header, payload, now).await
    }

    /// 在 Closing 状态下处理 ACK 帧
    /// Handle ACK frame in Closing state
    async fn handle_ack_in_closing<S: AsyncUdpSocket>(
        endpoint: &mut Endpoint<S>,
        header: crate::packet::header::ShortHeader,
        payload: bytes::Bytes,
        now: Instant,
    ) -> Result<()> {
        debug!(
            cid = endpoint.local_cid(),
            "Handling ACK in Closing state"
        );

        Self::process_ack_common(endpoint, header, payload, now).await
    }

    /// 在 ClosingWait 状态下处理 ACK 帧
    /// Handle ACK frame in ClosingWait state
    async fn handle_ack_in_closing_wait<S: AsyncUdpSocket>(
        endpoint: &mut Endpoint<S>,
        header: crate::packet::header::ShortHeader,
        payload: bytes::Bytes,
        now: Instant,
    ) -> Result<()> {
        debug!(
            cid = endpoint.local_cid(),
            "Handling ACK in ClosingWait state"
        );

        Self::process_ack_common(endpoint, header, payload, now).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::frame::Frame;
    use crate::packet::header::ShortHeader;
    use crate::packet::command::Command;
    use bytes::Bytes;

    #[test]
    fn test_ack_processor_can_handle() {
        let ack_frame = Frame::Ack {
            header: ShortHeader {
                command: Command::Ack,
                connection_id: 1,
                payload_length: 0,
                recv_window_size: 100,
                timestamp: 1000,
                sequence_number: 0,
                recv_next_sequence: 1,
            },
            payload: Bytes::new(),
        };

        assert!(<AckProcessor as FrameProcessorStatic>::can_handle(&ack_frame));

        let push_frame = Frame::Push {
            header: ShortHeader {
                command: Command::Push,
                connection_id: 1,
                payload_length: 10,
                recv_window_size: 100,
                timestamp: 1000,
                sequence_number: 1,
                recv_next_sequence: 0,
            },
            payload: Bytes::from("test data"),
        };

        assert!(!<AckProcessor as FrameProcessorStatic>::can_handle(&push_frame));
    }

    #[test]
    fn test_processor_name() {
        assert_eq!(<AckProcessor as FrameProcessorStatic>::name(), "AckProcessor");
    }
}