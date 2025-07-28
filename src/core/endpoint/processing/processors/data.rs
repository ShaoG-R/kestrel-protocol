//! 数据帧处理器 - 处理 PUSH 帧
//! Data Frame Processor - Handles PUSH frames
//!
//! 该模块专门处理数据传输相关的帧，包括 PUSH 帧的接收、
//! 序列号验证、数据重组等逻辑。
//!
//! This module specifically handles data transmission related frames,
//! including PUSH frame reception, sequence number validation,
//! data reassembly logic, etc.

use super::{FrameProcessingContext, UnifiedFrameProcessor, TypeSafeFrameProcessor, TypeSafeFrameValidator, frame_types::PushFrame};
use crate::{
    error::{Result, ProcessorErrorContext},
    packet::frame::Frame,
    socket::AsyncUdpSocket,
};
use std::net::SocketAddr;
use tokio::time::Instant;
use tracing::{debug, trace};
use async_trait::async_trait;

use crate::core::endpoint::Endpoint;
use crate::core::endpoint::types::state::ConnectionState;
use crate::core::endpoint::lifecycle::ConnectionLifecycleManager;

/// PUSH 帧处理器
/// PUSH frame processor
pub struct PushProcessor;

// 最新的类型安全处理器接口实现
// Latest type-safe processor interface implementation
#[async_trait]
impl<S: AsyncUdpSocket> TypeSafeFrameProcessor<S> for PushProcessor {
    type FrameTypeMarker = PushFrame;

    fn name() -> &'static str {
        "PushProcessor"
    }

    async fn process_frame(
        endpoint: &mut Endpoint<S>,
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
            processor = <Self as TypeSafeFrameProcessor<S>>::name(),
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
}

// 新的统一处理器接口实现
// New unified processor interface implementation
#[async_trait]
impl<S: AsyncUdpSocket> UnifiedFrameProcessor<S> for PushProcessor {
    type FrameType = crate::packet::frame::Frame;

    fn can_handle(frame: &Frame) -> bool {
        matches!(frame, Frame::Push { .. })
    }

    fn name() -> &'static str {
        "PushProcessor"
    }

    async fn process_frame(
        endpoint: &mut Endpoint<S>,
        frame: Frame,
        src_addr: SocketAddr,
        now: Instant,
    ) -> Result<()> {
        let Frame::Push { header, payload } = frame else {
            let error_context = Self::create_error_context(endpoint, src_addr, now);
            return Err(crate::error::Error::FrameTypeMismatch {
                expected: "PUSH frame".to_string(),
                actual: format!("{:?}", std::mem::discriminant(&frame)),
                context: error_context,
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
    fn create_error_context<S: AsyncUdpSocket>(
        endpoint: &Endpoint<S>,
        src_addr: SocketAddr,
        now: Instant,
    ) -> ProcessorErrorContext {
        ProcessorErrorContext::new(
            "PushProcessor",
            endpoint.local_cid(),
            src_addr,
            format!("{:?}", endpoint.lifecycle_manager().current_state()),
            now,
        )
    }

    /// 在 SynReceived 状态下处理 PUSH 帧
    /// Handle PUSH frame in SynReceived state
    async fn handle_push_in_syn_received<S: AsyncUdpSocket>(
        endpoint: &mut Endpoint<S>,
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
        endpoint.reliability_mut().receive_push(header.sequence_number, payload);
        
        Ok(())
    }

    /// 在 Established 状态下处理 PUSH 帧
    /// Handle PUSH frame in Established state
    async fn handle_push_in_established<S: AsyncUdpSocket>(
        endpoint: &mut Endpoint<S>,
        header: crate::packet::header::ShortHeader,
        payload: bytes::Bytes,
    ) -> Result<()> {
        trace!(
            cid = endpoint.local_cid(),
            seq = header.sequence_number,
            payload_len = payload.len(),
            "Handling PUSH in Established state"
        );

        // 接收数据并检查是否需要立即发送 ACK
        // Receive data and check if immediate ACK is needed
        if endpoint.reliability_mut().receive_push(header.sequence_number, payload) {
            endpoint.send_standalone_ack().await?;
        }
        
        Ok(())
    }

    /// 在 ValidatingPath 状态下处理 PUSH 帧
    /// Handle PUSH frame in ValidatingPath state
    async fn handle_push_in_validating_path<S: AsyncUdpSocket>(
        endpoint: &mut Endpoint<S>,
        header: crate::packet::header::ShortHeader,
        payload: bytes::Bytes,
    ) -> Result<()> {
        debug!(
            cid = endpoint.local_cid(),
            seq = header.sequence_number,
            "Handling PUSH during path validation"
        );

        // 在路径验证期间继续处理数据帧
        // Continue processing data frames during path validation
        if endpoint.reliability_mut().receive_push(header.sequence_number, payload) {
            endpoint.send_standalone_ack().await?;
        }
        
        Ok(())
    }

    /// 在 FinWait 状态下处理 PUSH 帧
    /// Handle PUSH frame in FinWait state
    async fn handle_push_in_fin_wait<S: AsyncUdpSocket>(
        endpoint: &mut Endpoint<S>,
        header: crate::packet::header::ShortHeader,
        payload: bytes::Bytes,
    ) -> Result<()> {
        debug!(
            cid = endpoint.local_cid(),
            seq = header.sequence_number,
            "Handling PUSH in FinWait state"
        );

        // 即使在 FinWait 状态下也要处理数据，因为对端可能还在发送数据
        // Process data even in FinWait state as peer might still be sending data
        if endpoint.reliability_mut().receive_push(header.sequence_number, payload) {
            endpoint.send_standalone_ack().await?;
        }
        
        Ok(())
    }

    /// 在 Closing 状态下处理 PUSH 帧
    /// Handle PUSH frame in Closing state
    async fn handle_push_in_closing<S: AsyncUdpSocket>(
        endpoint: &mut Endpoint<S>,
        header: crate::packet::header::ShortHeader,
        payload: bytes::Bytes,
    ) -> Result<()> {
        debug!(
            cid = endpoint.local_cid(),
            seq = header.sequence_number,
            "Handling PUSH in Closing state"
        );

        // 在关闭过程中仍可能收到数据，因为对端可能在收到我们的 FIN 之前发送了数据
        // It's possible to receive data during closing as the peer might have
        // sent it before receiving our FIN
        if endpoint.reliability_mut().receive_push(header.sequence_number, payload) {
            endpoint.send_standalone_ack().await?;
        }
        
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::test_utils::MockUdpSocket,
        packet::{frame::Frame, header::ShortHeader, command::Command},
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
        assert!(<PushProcessor as UnifiedFrameProcessor<MockUdpSocket>>::can_handle(&push_frame));

        // 测试不能处理非PUSH帧
        // Test cannot handle non-PUSH frame
        let ack_frame = create_test_ack_frame();
        assert!(!<PushProcessor as UnifiedFrameProcessor<MockUdpSocket>>::can_handle(&ack_frame));
    }

    #[test]
    fn test_processor_name() {
        assert_eq!(<PushProcessor as UnifiedFrameProcessor<MockUdpSocket>>::name(), "PushProcessor");
        assert_eq!(<PushProcessor as TypeSafeFrameProcessor<MockUdpSocket>>::name(), "PushProcessor");
    }

    #[test]
    fn test_frame_type_validation() {
        // 测试类型安全验证
        // Test type-safe validation
        let push_frame = create_test_push_frame(1, "test");
        assert!(<PushProcessor as TypeSafeFrameValidator>::validate_frame_type(&push_frame).is_ok());

        let ack_frame = create_test_ack_frame();
        assert!(<PushProcessor as TypeSafeFrameValidator>::validate_frame_type(&ack_frame).is_err());
    }

    #[test]
    fn test_frame_types_marker_matching() {
        use super::super::frame_types::*;
        
        let push_frame = create_test_push_frame(1, "test");
        let ack_frame = create_test_ack_frame();

        // 测试帧类型标记匹配
        // Test frame type marker matching
        assert!(PushFrame::matches(&push_frame));
        assert!(!PushFrame::matches(&ack_frame));
        
        assert!(!AckFrame::matches(&push_frame));
        assert!(AckFrame::matches(&ack_frame));

        // 测试类型名称
        // Test type names
        assert_eq!(PushFrame::type_name(), "PushFrame");
        assert_eq!(AckFrame::type_name(), "AckFrame");
    }

    #[test]
    fn test_all_processor_interfaces_consistency() {
        let push_frame = create_test_push_frame(1, "test");
        
        // 确保所有接口实现的行为一致
        // Ensure all interface implementations behave consistently
        let unified_can_handle = <PushProcessor as UnifiedFrameProcessor<MockUdpSocket>>::can_handle(&push_frame);
        assert!(unified_can_handle);
        
        // 确保名称一致
        // Ensure names are consistent
        let unified_name = <PushProcessor as UnifiedFrameProcessor<MockUdpSocket>>::name();
        let type_safe_name = <PushProcessor as TypeSafeFrameProcessor<MockUdpSocket>>::name();
        
        assert_eq!(unified_name, type_safe_name);
        assert_eq!(unified_name, "PushProcessor");
    }
}