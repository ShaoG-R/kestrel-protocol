//! ACK 帧处理器 - 处理确认帧
//! ACK Frame Processor - Handles acknowledgment frames
//!
//! 该模块专门处理 ACK 帧，包括 SACK 信息解析、
//! RTT 计算、拥塞控制更新等逻辑。
//!
//! This module specifically handles ACK frames, including SACK information parsing,
//! RTT calculation, congestion control updates, etc.

use super::{FrameProcessingContext, UnifiedFrameProcessor, TypeSafeFrameProcessor, TypeSafeFrameValidator, frame_types::AckFrame};
use crate::{
    error::{Result, ProcessorErrorContext},
    packet::{frame::Frame, sack::decode_sack_ranges},
    socket::{Transport},
};
use std::net::SocketAddr;
use tokio::time::Instant;
use tracing::{debug, trace, warn};
use async_trait::async_trait;

use crate::core::endpoint::types::state::ConnectionState;
use super::super::traits::{ProcessorOperations};

/// ACK 帧处理器
/// ACK frame processor
pub struct AckProcessor;

// 最新的类型安全处理器接口实现
// Latest type-safe processor interface implementation
#[async_trait]
impl<T: Transport> TypeSafeFrameProcessor<T> for AckProcessor {
    type FrameTypeMarker = AckFrame;

    fn name() -> &'static str {
        "AckProcessor"
    }

    async fn process_frame(
        endpoint: &mut dyn ProcessorOperations,
        frame: Frame,
        src_addr: SocketAddr,
        now: Instant,
    ) -> Result<()> {
        // 类型安全验证：编译时保证只能处理ACK帧
        // Type-safe validation: compile-time guarantee that only ACK frames can be processed
        <Self as TypeSafeFrameValidator>::validate_frame_type(&frame)?;

        Self::process_ack_frame_internal(endpoint, frame, src_addr, now).await
    }
}

// 实现类型安全验证接口
// Implement type-safe validation interface
impl TypeSafeFrameValidator for AckProcessor {
    type FrameTypeMarker = AckFrame;
    
    /// 验证帧类型是否为 ACK 帧
    /// Validate that the frame type is an ACK frame
    fn validate_frame_type(frame: &Frame) -> Result<()> {
        match frame {
            Frame::Ack { .. } => Ok(()),
            _ => Err(crate::error::Error::InvalidFrame(
                format!(
                    "AckProcessor can only handle ACK frames, got: {:?}", 
                    std::mem::discriminant(frame)
                )
            ))
        }
    }
}

impl AckProcessor {
    /// 内部ACK帧处理方法，供所有接口实现调用
    /// Internal ACK frame processing method, called by all interface implementations
    async fn process_ack_frame_internal(
        endpoint: &mut dyn ProcessorOperations,
        frame: Frame,
        src_addr: SocketAddr,
        now: Instant,
    ) -> Result<()> {
        let Frame::Ack { header, payload } = frame else {
            let error_context = Self::create_error_context(endpoint, src_addr, now);
            return Err(crate::error::Error::FrameTypeMismatch {
                expected: "ACK frame".to_string(),
                actual: format!("{:?}", std::mem::discriminant(&frame)),
                context: error_context,
            });
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



// 统一接口实现
// Unified interface implementation
#[async_trait]
impl<T: Transport> UnifiedFrameProcessor<T> for AckProcessor {
    type FrameType = AckFrame;

    fn can_handle(frame: &Frame) -> bool {
        matches!(frame, Frame::Ack { .. })
    }

    fn name() -> &'static str {
        "AckProcessor"
    }

    async fn process_frame(
        endpoint: &mut dyn ProcessorOperations,
        frame: Frame,
        src_addr: SocketAddr,
        now: Instant,
    ) -> Result<()> {
        Self::process_ack_frame_internal(endpoint, frame, src_addr, now).await
    }
}



impl AckProcessor {
    /// 创建处理器错误上下文
    /// Create processor error context
    fn create_error_context(
        endpoint: &dyn ProcessorOperations,
        src_addr: SocketAddr,
        now: Instant,
    ) -> ProcessorErrorContext {
        ProcessorErrorContext::new(
            "AckProcessor",
            endpoint.local_cid(),
            src_addr,
            format!("{:?}", endpoint.current_state()),
            now,
        )
    }

    /// 处理 ACK 帧的通用逻辑
    /// Common logic for processing ACK frames
    async fn process_ack_common(
        endpoint: &mut dyn ProcessorOperations,
        header: crate::packet::header::ShortHeader,
        payload: bytes::Bytes,
        now: Instant,
    ) -> Result<()> {
        // 更新对端接收窗口大小
        // Update peer receive window size
        endpoint.set_peer_recv_window(header.recv_window_size as u32);
        
        // 解析 SACK 范围
        // Parse SACK ranges
        let sack_ranges = decode_sack_ranges(payload);
        
        debug!(
            cid = endpoint.local_cid(),
            recv_next_seq = header.recv_next_sequence,
            sack_ranges_count = sack_ranges.len(),
            peer_recv_window = endpoint.peer_recv_window(),
            "Processing ACK with SACK ranges"
        );

        // 使用高级接口处理 ACK 并获取需要重传的帧
        // Use high-level interface to process ACK and get frames that need retransmission
        let sack_tuples: Vec<(u32, u32)> = sack_ranges.into_iter()
            .map(|range| (range.start, range.end))
            .collect();

        let frames_to_retx = endpoint.process_ack_and_get_retx_frames(
            header.recv_next_sequence,
            sack_tuples,
            now,
        ).await?;

        // 如果有需要重传的帧，立即发送
        // If there are frames to retransmit, send them immediately
        if !frames_to_retx.is_empty() {
            debug!(
                cid = endpoint.local_cid(),
                retx_count = frames_to_retx.len(),
                "Fast retransmitting frames based on SACK"
            );
            endpoint.send_frame_list(frames_to_retx).await?;
        }

        Ok(())
    }

    /// 在 SynReceived 状态下处理 ACK 帧
    /// Handle ACK frame in SynReceived state
    async fn handle_ack_in_syn_received(
        endpoint: &mut dyn ProcessorOperations,
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
    async fn handle_ack_in_established(
        endpoint: &mut dyn ProcessorOperations,
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
    async fn handle_ack_in_validating_path(
        endpoint: &mut dyn ProcessorOperations,
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
    async fn handle_ack_in_fin_wait(
        endpoint: &mut dyn ProcessorOperations,
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
    async fn handle_ack_in_closing(
        endpoint: &mut dyn ProcessorOperations,
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
    async fn handle_ack_in_closing_wait(
        endpoint: &mut dyn ProcessorOperations,
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
    use crate::core::test_utils::MockTransport;

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

        assert!(<AckProcessor as UnifiedFrameProcessor<MockTransport>>::can_handle(&ack_frame));

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

        assert!(!<AckProcessor as UnifiedFrameProcessor<MockTransport>>::can_handle(&push_frame));
    }

    #[test]
    fn test_processor_name() {
        assert_eq!(<AckProcessor as UnifiedFrameProcessor<MockTransport>>::name(), "AckProcessor");
    }
}