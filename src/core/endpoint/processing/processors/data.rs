//! 数据帧处理器 - 处理 PUSH 帧
//! Data Frame Processor - Handles PUSH frames
//!
//! 该模块专门处理数据传输相关的帧，包括 PUSH 帧的接收、
//! 序列号验证、数据重组等逻辑。
//!
//! This module specifically handles data transmission related frames,
//! including PUSH frame reception, sequence number validation,
//! data reassembly logic, etc.

use super::{
    FrameProcessingContext, TypeSafeFrameProcessor, TypeSafeFrameValidator, UnifiedFrameProcessor,
    frame_types::PushFrame,
};
use crate::{
    error::{ProcessorErrorContext, Result},
    packet::frame::Frame,
    socket::Transport,
};
use async_trait::async_trait;
use std::net::SocketAddr;
use tokio::time::Instant;
use tracing::{debug, trace};

use super::super::traits::ProcessorOperations;
use crate::core::endpoint::types::state::ConnectionState;

/// PUSH 帧处理器
/// PUSH frame processor
pub struct PushProcessor;

// 最新的类型安全处理器接口实现
// Latest type-safe processor interface implementation
#[async_trait]
impl<T: Transport> TypeSafeFrameProcessor<T> for PushProcessor {
    type FrameTypeMarker = PushFrame;

    fn name() -> &'static str {
        "PushProcessor"
    }

    async fn process_frame(
        endpoint: &mut dyn ProcessorOperations,
        frame: Frame,
        src_addr: SocketAddr,
        now: Instant,
    ) -> Result<()> {
        // 类型安全验证：编译时保证只能处理PUSH帧
        // Type-safe validation: compile-time guarantee that only PUSH frames can be processed
        <Self as TypeSafeFrameValidator>::validate_frame_type(&frame)?;

        let Frame::Push { header, payload } = frame else {
            // 这个分支在类型安全验证后应该永远不会到达
            // This branch should never be reached after type-safe validation
            unreachable!("Frame type validation should prevent non-PUSH frames")
        };

        let context = FrameProcessingContext::new(endpoint, src_addr, now);

        trace!(
            cid = context.local_cid,
            seq = header.sequence_number,
            payload_len = payload.len(),
            state = ?context.connection_state,
            processor = <Self as TypeSafeFrameProcessor<T>>::name(),
            "Processing PUSH frame with type-safe processor"
        );

        // 根据连接状态处理 PUSH 帧
        // Handle PUSH frame based on connection state
        match context.connection_state {
            ConnectionState::SynReceived => {
                Self::handle_push_in_syn_received(endpoint, header, payload).await
            }
            ConnectionState::Established => {
                Self::handle_push_in_established(endpoint, header, payload).await
            }
            ConnectionState::ValidatingPath { .. } => {
                Self::handle_push_in_validating_path(endpoint, header, payload).await
            }
            ConnectionState::FinWait => {
                Self::handle_push_in_fin_wait(endpoint, header, payload).await
            }
            ConnectionState::Closing => {
                Self::handle_push_in_closing(endpoint, header, payload).await
            }
            ConnectionState::ClosingWait => {
                Self::handle_push_in_closing(endpoint, header, payload).await
            }
            _ => {
                debug!(
                    cid = context.local_cid,
                    state = ?context.connection_state,
                    "Ignoring PUSH frame in state"
                );
                Ok(())
            }
        }
    }
}

// 类型安全的帧验证器实现
// Type-safe frame validator implementation
impl TypeSafeFrameValidator for PushProcessor {
    type FrameTypeMarker = PushFrame;

    /// 验证帧类型是否为 PUSH 帧
    /// Validate that the frame type is a PUSH frame
    fn validate_frame_type(frame: &Frame) -> Result<()> {
        match frame {
            Frame::Push { .. } => Ok(()),
            _ => Err(crate::error::Error::InvalidFrame(format!(
                "PushProcessor can only handle PUSH frames, got: {:?}",
                std::mem::discriminant(frame)
            ))),
        }
    }
}

// 新的统一处理器接口实现
// New unified processor interface implementation
#[async_trait]
impl<T: Transport> UnifiedFrameProcessor<T> for PushProcessor {
    type FrameType = crate::packet::frame::Frame;

    fn can_handle(frame: &Frame) -> bool {
        matches!(frame, Frame::Push { .. })
    }

    fn name() -> &'static str {
        "PushProcessor"
    }

    async fn process_frame(
        endpoint: &mut dyn ProcessorOperations,
        frame: Frame,
        src_addr: SocketAddr,
        now: Instant,
    ) -> Result<()> {
        let Frame::Push { header, payload } = frame else {
            let error_context = Self::create_error_context(endpoint, src_addr, now);
            return Err(crate::error::Error::FrameTypeMismatch {
                err: crate::error::FrameTypeMismatchError::new(
                    "PUSH frame".to_string(),
                    format!("{:?}", std::mem::discriminant(&frame)),
                    error_context,
                )
                .into(),
            });
        };

        let context = FrameProcessingContext::new(endpoint, src_addr, now);

        trace!(
            cid = context.local_cid,
            seq = header.sequence_number,
            payload_len = payload.len(),
            state = ?context.connection_state,
            "Processing PUSH frame"
        );

        // 根据连接状态处理 PUSH 帧
        // Handle PUSH frame based on connection state
        match context.connection_state {
            ConnectionState::SynReceived => {
                Self::handle_push_in_syn_received(endpoint, header, payload).await
            }
            ConnectionState::Established => {
                Self::handle_push_in_established(endpoint, header, payload).await
            }
            ConnectionState::ValidatingPath { .. } => {
                Self::handle_push_in_validating_path(endpoint, header, payload).await
            }
            ConnectionState::FinWait => {
                Self::handle_push_in_fin_wait(endpoint, header, payload).await
            }
            ConnectionState::Closing => {
                Self::handle_push_in_closing(endpoint, header, payload).await
            }
            ConnectionState::ClosingWait => {
                Self::handle_push_in_closing(endpoint, header, payload).await
            }
            _ => {
                debug!(
                    cid = context.local_cid,
                    state = ?context.connection_state,
                    "Ignoring PUSH frame in state"
                );
                Ok(())
            }
        }
    }
}

impl PushProcessor {
    /// 创建处理器错误上下文
    /// Create processor error context
    fn create_error_context(
        endpoint: &dyn ProcessorOperations,
        src_addr: SocketAddr,
        now: Instant,
    ) -> ProcessorErrorContext {
        ProcessorErrorContext::new(
            "PushProcessor",
            endpoint.local_cid(),
            src_addr,
            format!("{:?}", endpoint.current_state()),
            now,
        )
    }

    /// 在 SynReceived 状态下处理 PUSH 帧
    /// Handle PUSH frame in SynReceived state
    async fn handle_push_in_syn_received(
        endpoint: &mut dyn ProcessorOperations,
        header: crate::packet::header::ShortHeader,
        payload: bytes::Bytes,
    ) -> Result<()> {
        debug!(
            cid = endpoint.local_cid(),
            seq = header.sequence_number,
            "Handling PUSH in SynReceived state (0-RTT data)"
        );

        // 对于在 SynReceived 状态下收到的 0-RTT PUSH 帧，
        // ACK 将被捎带到最终的 SYN-ACK 上
        // For 0-RTT PUSH frames received during the SynReceived state,
        // the ACK will be piggybacked onto the eventual SYN-ACK
        endpoint
            .unified_reliability_mut()
            .receive_packet(header.sequence_number, payload);

        Ok(())
    }

    /// 在 Established 状态下处理 PUSH 帧
    /// Handle PUSH frame in Established state
    async fn handle_push_in_established(
        endpoint: &mut dyn ProcessorOperations,
        header: crate::packet::header::ShortHeader,
        payload: bytes::Bytes,
    ) -> Result<()> {
        trace!(
            cid = endpoint.local_cid(),
            seq = header.sequence_number,
            payload_len = payload.len(),
            "Handling PUSH in Established state"
        );

        // 使用高级接口接收数据并自动发送 ACK（如果需要）
        // Use high-level interface to receive data and automatically send ACK if needed
        endpoint
            .receive_data_and_maybe_ack(header.sequence_number, payload)
            .await?;

        Ok(())
    }

    /// 在 ValidatingPath 状态下处理 PUSH 帧
    /// Handle PUSH frame in ValidatingPath state
    async fn handle_push_in_validating_path(
        endpoint: &mut dyn ProcessorOperations,
        header: crate::packet::header::ShortHeader,
        payload: bytes::Bytes,
    ) -> Result<()> {
        debug!(
            cid = endpoint.local_cid(),
            seq = header.sequence_number,
            "Handling PUSH during path validation"
        );

        // 在路径验证期间继续处理数据帧，使用高级接口
        // Continue processing data frames during path validation using high-level interface
        endpoint
            .receive_data_and_maybe_ack(header.sequence_number, payload)
            .await?;

        Ok(())
    }

    /// 在 FinWait 状态下处理 PUSH 帧
    /// Handle PUSH frame in FinWait state
    async fn handle_push_in_fin_wait(
        endpoint: &mut dyn ProcessorOperations,
        header: crate::packet::header::ShortHeader,
        payload: bytes::Bytes,
    ) -> Result<()> {
        debug!(
            cid = endpoint.local_cid(),
            seq = header.sequence_number,
            "Handling PUSH in FinWait state"
        );

        // 即使在 FinWait 状态下也要处理数据，使用高级接口
        // Process data even in FinWait state using high-level interface
        endpoint
            .receive_data_and_maybe_ack(header.sequence_number, payload)
            .await?;

        Ok(())
    }

    /// 在 Closing 状态下处理 PUSH 帧
    /// Handle PUSH frame in Closing state
    async fn handle_push_in_closing(
        endpoint: &mut dyn ProcessorOperations,
        header: crate::packet::header::ShortHeader,
        payload: bytes::Bytes,
    ) -> Result<()> {
        debug!(
            cid = endpoint.local_cid(),
            seq = header.sequence_number,
            "Handling PUSH in Closing state"
        );

        // 在关闭过程中仍可能收到数据，使用高级接口处理
        // Process data during closing using high-level interface
        endpoint
            .receive_data_and_maybe_ack(header.sequence_number, payload)
            .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::test_utils::MockTransport,
        packet::{command::Command, frame::Frame, header::ShortHeader},
    };
    use bytes::Bytes;

    // 测试工具函数 - 创建测试用的PUSH帧
    // Test utility function - Create test PUSH frame
    fn create_test_push_frame(seq: u32, payload: &str) -> Frame {
        let payload_bytes = Bytes::from(payload.to_string());
        Frame::Push {
            header: ShortHeader {
                command: Command::Push,
                connection_id: 1,
                payload_length: payload_bytes.len() as u16,
                recv_window_size: 100,
                timestamp: 1000,
                sequence_number: seq,
                recv_next_sequence: 0,
            },
            payload: payload_bytes,
        }
    }

    // 测试工具函数 - 创建测试用的ACK帧
    // Test utility function - Create test ACK frame
    fn create_test_ack_frame() -> Frame {
        Frame::Ack {
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
        }
    }

    #[test]
    fn test_push_processor_can_handle() {
        // 测试能够处理PUSH帧
        // Test can handle PUSH frame
        let push_frame = create_test_push_frame(1, "test data");
        assert!(<PushProcessor as UnifiedFrameProcessor<MockTransport>>::can_handle(&push_frame));

        // 测试不能处理非PUSH帧
        // Test cannot handle non-PUSH frame
        let ack_frame = create_test_ack_frame();
        assert!(!<PushProcessor as UnifiedFrameProcessor<MockTransport>>::can_handle(&ack_frame));
    }

    #[test]
    fn test_processor_name() {
        assert_eq!(
            <PushProcessor as UnifiedFrameProcessor<MockTransport>>::name(),
            "PushProcessor"
        );
        assert_eq!(
            <PushProcessor as TypeSafeFrameProcessor<MockTransport>>::name(),
            "PushProcessor"
        );
    }

    #[test]
    fn test_frame_type_validation() {
        // 测试类型安全验证
        // Test type-safe validation
        let push_frame = create_test_push_frame(1, "test");
        assert!(
            <PushProcessor as TypeSafeFrameValidator>::validate_frame_type(&push_frame).is_ok()
        );

        let ack_frame = create_test_ack_frame();
        assert!(
            <PushProcessor as TypeSafeFrameValidator>::validate_frame_type(&ack_frame).is_err()
        );
    }

    #[test]
    fn test_processor_type_identification() {
        use super::super::ProcessorType;

        let push_frame = create_test_push_frame(1, "test");
        let ack_frame = create_test_ack_frame();

        // 测试统一的帧类型识别 - 使用 ProcessorType
        // Test unified frame type identification using ProcessorType
        assert_eq!(
            ProcessorType::from_frame(&push_frame),
            Some(ProcessorType::Push)
        );
        assert_eq!(
            ProcessorType::from_frame(&ack_frame),
            Some(ProcessorType::Ack)
        );

        // 测试不匹配的情况
        // Test non-matching cases
        assert_ne!(
            ProcessorType::from_frame(&push_frame),
            Some(ProcessorType::Ack)
        );
        assert_ne!(
            ProcessorType::from_frame(&ack_frame),
            Some(ProcessorType::Push)
        );

        // 测试处理器名称
        // Test processor names
        assert_eq!(ProcessorType::Push.name(), "PushProcessor");
        assert_eq!(ProcessorType::Ack.name(), "AckProcessor");
    }

    #[test]
    fn test_all_processor_interfaces_consistency() {
        let push_frame = create_test_push_frame(1, "test");

        // 确保所有接口实现的行为一致
        // Ensure all interface implementations behave consistently
        let unified_can_handle =
            <PushProcessor as UnifiedFrameProcessor<MockTransport>>::can_handle(&push_frame);
        assert!(unified_can_handle);

        // 确保名称一致
        // Ensure names are consistent
        let unified_name = <PushProcessor as UnifiedFrameProcessor<MockTransport>>::name();
        let type_safe_name = <PushProcessor as TypeSafeFrameProcessor<MockTransport>>::name();

        assert_eq!(unified_name, type_safe_name);
        assert_eq!(unified_name, "PushProcessor");
    }
}
