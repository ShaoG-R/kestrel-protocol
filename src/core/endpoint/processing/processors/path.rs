//! 路径验证帧处理器 - 处理 PathChallenge 和 PathResponse 帧
//! Path Validation Frame Processor - Handles PathChallenge and PathResponse frames
//!
//! 该模块专门处理连接迁移和路径验证相关的帧，包括路径质询、
//! 路径响应、地址验证等逻辑。
//!
//! This module specifically handles connection migration and path validation
//! related frames, including path challenges, path responses,
//! address validation, etc.

use super::{FrameProcessingContext, UnifiedFrameProcessor, TypeSafeFrameProcessor, TypeSafeFrameValidator, frame_types::PathFrame};
use crate::{
    error::{Result, ProcessorErrorContext},
    packet::frame::Frame,
    socket::{SocketActorCommand, Transport},
};
use std::net::SocketAddr;
use tokio::time::Instant;
use tracing::{debug, info, trace, warn};
use async_trait::async_trait;
use crate::core::endpoint::types::state::ConnectionState;
use super::super::traits::ProcessorOperations;

/// 路径验证帧处理器
/// Path validation frame processor
pub struct PathProcessor;

// 最新的类型安全处理器接口实现
// Latest type-safe processor interface implementation
#[async_trait]
impl<T: Transport> TypeSafeFrameProcessor<T> for PathProcessor {
    type FrameTypeMarker = PathFrame;

    fn name() -> &'static str {
        "PathProcessor"
    }

    async fn process_frame(
        endpoint: &mut dyn ProcessorOperations,
        frame: Frame,
        src_addr: SocketAddr,
        now: Instant,
    ) -> Result<()> {
        // 类型安全验证：编译时保证只能处理路径验证帧
        // Type-safe validation: compile-time guarantee that only path validation frames can be processed
        <Self as TypeSafeFrameValidator>::validate_frame_type(&frame)?;

        Self::process_path_frame_internal(endpoint, frame, src_addr, now).await
    }
}

// 实现类型安全验证接口
// Implement type-safe validation interface
impl TypeSafeFrameValidator for PathProcessor {
    type FrameTypeMarker = PathFrame;
    
    /// 验证帧类型是否为路径验证帧
    /// Validate that the frame type is a path validation frame
    fn validate_frame_type(frame: &Frame) -> Result<()> {
        match frame {
            Frame::PathChallenge { .. } | Frame::PathResponse { .. } => Ok(()),
            _ => Err(crate::error::Error::InvalidFrame(
                format!(
                    "PathProcessor can only handle path validation frames (PathChallenge/PathResponse), got: {:?}", 
                    std::mem::discriminant(frame)
                )
            ))
        }
    }
}

impl PathProcessor {
    /// 内部路径验证帧处理方法，供所有接口实现调用
    /// Internal path validation frame processing method, called by all interface implementations
    async fn process_path_frame_internal(
        endpoint: &mut dyn ProcessorOperations,
        frame: Frame,
        src_addr: SocketAddr,
        now: Instant,
    ) -> Result<()> {
        let context = FrameProcessingContext::new(endpoint, src_addr, now);

        match frame {
            Frame::PathChallenge {
                header,
                challenge_data,
            } => {
                Self::handle_path_challenge_frame(endpoint, header, challenge_data, context)
                    .await
            }
            Frame::PathResponse {
                header,
                challenge_data,
            } => {
                Self::handle_path_response_frame(endpoint, header, challenge_data, context)
                    .await
            }
            _ => {
                let error_context = ProcessorErrorContext::new(
                    "PathProcessor",
                    endpoint.local_cid(),
                    src_addr,
                    format!("{:?}", endpoint.current_state()),
                    now,
                );
                Err(crate::error::Error::FrameTypeMismatch {
                    err: crate::error::FrameTypeMismatchError::new(
                        "path validation frame (PathChallenge or PathResponse)".to_string(),
                        format!("{:?}", std::mem::discriminant(&frame)),
                        error_context,
                    ).into(),
                })
            }
        }
    }
}

// 统一接口实现
// Unified interface implementation
#[async_trait]
impl<T: Transport> UnifiedFrameProcessor<T> for PathProcessor {
    type FrameType = PathFrame;

    fn can_handle(frame: &Frame) -> bool {
        matches!(frame, 
            Frame::PathChallenge { .. } |
            Frame::PathResponse { .. }
        )
    }

    fn name() -> &'static str {
        "PathProcessor"
    }

    async fn process_frame(
        endpoint: &mut dyn ProcessorOperations,
        frame: Frame,
        src_addr: SocketAddr,
        now: Instant,
    ) -> Result<()> {
        Self::process_path_frame_internal(endpoint, frame, src_addr, now).await
    }
}



impl PathProcessor {
    /// 处理 PathChallenge 帧
    /// Handle PathChallenge frame
    async fn handle_path_challenge_frame(
        endpoint: &mut dyn ProcessorOperations,
        header: crate::packet::header::ShortHeader,
        challenge_data: u64,
        context: FrameProcessingContext,
    ) -> Result<()> {
        debug!(
            cid = context.local_cid,
            seq = header.sequence_number,
            challenge_data = challenge_data,
            src_addr = %context.src_addr,
            state = ?context.connection_state,
            "Processing PathChallenge frame"
        );

        // PathChallenge 可以在大多数状态下处理，除了完全关闭的状态
        // PathChallenge can be handled in most states except completely closed states
        match context.connection_state {
            ConnectionState::Closed => {
                warn!(
                    cid = context.local_cid,
                    "Ignoring PathChallenge in Closed state"
                );
                Ok(())
            }
            _ => {
                // 在所有其他状态下处理路径质询
                // Handle path challenge in all other states
                Self::send_path_response(endpoint, header, challenge_data, context.src_addr).await
            }
        }
    }

    /// 处理 PathResponse 帧
    /// Handle PathResponse frame
    async fn handle_path_response_frame(
        endpoint: &mut dyn ProcessorOperations,
        header: crate::packet::header::ShortHeader,
        challenge_data: u64,
        context: FrameProcessingContext,
    ) -> Result<()> {
        debug!(
            cid = context.local_cid,
            seq = header.sequence_number,
            challenge_data = challenge_data,
            src_addr = %context.src_addr,
            state = ?context.connection_state,
            "Processing PathResponse frame"
        );

        // PathResponse 只在 ValidatingPath 状态下有意义
        // PathResponse is only meaningful in ValidatingPath state
        match context.connection_state {
            ConnectionState::ValidatingPath {
                new_addr,
                challenge_data: expected_challenge,
                notifier,
            } => {
                Self::handle_path_validation_response(
                    endpoint,
                    context.src_addr,
                    new_addr,
                    challenge_data,
                    expected_challenge,
                    notifier,
                )
                .await
            }
            _ => {
                trace!(
                    cid = context.local_cid,
                    state = ?context.connection_state,
                    "Ignoring PathResponse in unexpected state"
                );
                Ok(())
            }
        }
    }

    /// 发送路径响应
    /// Send path response
    async fn send_path_response(
        endpoint: &mut dyn ProcessorOperations,
        header: crate::packet::header::ShortHeader,
        challenge_data: u64,
        src_addr: SocketAddr,
    ) -> Result<()> {
        trace!(
            cid = endpoint.local_cid(),
            challenge_data = challenge_data,
            response_to = %src_addr,
            "Sending PathResponse"
        );

        // 使用高级接口发送路径响应
        // Use high-level interface to send path response
        endpoint.send_path_response(header.sequence_number, challenge_data, src_addr).await?;

        Ok(())
    }

    /// 处理路径验证响应
    /// Handle path validation response
    async fn handle_path_validation_response(
        endpoint: &mut dyn ProcessorOperations,
        src_addr: SocketAddr,
        expected_addr: SocketAddr,
        received_challenge: u64,
        expected_challenge: u64,
        notifier: Option<tokio::sync::oneshot::Sender<Result<()>>>,
    ) -> Result<()> {
        if src_addr == expected_addr && received_challenge == expected_challenge {
            // 路径验证成功！
            // Path validation successful!
            info!(
                cid = endpoint.local_cid(),
                old_addr = %endpoint.remote_addr(),
                new_addr = %expected_addr,
                "Path validation successful, migrating connection"
            );

            endpoint.complete_path_validation(true)?;
            endpoint.set_remote_addr(expected_addr);

            // 通知 migrate() 的调用者（如果有的话）
            // Notify the caller of migrate() if there is one
            if let Some(notifier) = notifier {
                let _ = notifier.send(Ok(()));
            }

            // 通知 ReliableUdpSocket 更新 addr_to_cid 映射
            // Notify ReliableUdpSocket to update the addr_to_cid map
            let _ = endpoint
                .command_tx()
                .send(SocketActorCommand::UpdateAddr {
                    cid: endpoint.local_cid(),
                    new_addr: expected_addr,
                })
                .await;
        } else {
            // 无效的路径响应，忽略它
            // Invalid path response, ignore it
            warn!(
                cid = endpoint.local_cid(),
                expected_addr = %expected_addr,
                received_addr = %src_addr,
                expected_challenge = expected_challenge,
                received_challenge = received_challenge,
                "Received invalid PathResponse, ignoring"
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::command::Command;
    use crate::packet::frame::Frame;
    use crate::packet::header::ShortHeader;
    use crate::core::test_utils::MockTransport;

    #[test]
    fn test_path_processor_can_handle() {
        let path_challenge_frame = Frame::PathChallenge {
            header: ShortHeader {
                command: Command::PathChallenge,
                connection_id: 1,
                payload_length: 8,
                recv_window_size: 0,
                timestamp: 1000,
                sequence_number: 1,
                recv_next_sequence: 0,
            },
            challenge_data: 0x1234567890abcdef,
        };

        let path_response_frame = Frame::PathResponse {
            header: ShortHeader {
                command: Command::PathResponse,
                connection_id: 1,
                payload_length: 8,
                recv_window_size: 0,
                timestamp: 1000,
                sequence_number: 1,
                recv_next_sequence: 0,
            },
            challenge_data: 0x1234567890abcdef,
        };

        assert!(<PathProcessor as UnifiedFrameProcessor<MockTransport>>::can_handle(
            &path_challenge_frame
        ));
        assert!(<PathProcessor as UnifiedFrameProcessor<MockTransport>>::can_handle(
            &path_response_frame
        ));

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
            payload: bytes::Bytes::from("test data"),
        };

        assert!(!<PathProcessor as UnifiedFrameProcessor<MockTransport>>::can_handle(
            &push_frame
        ));
    }

    #[test]
    fn test_processor_name() {
        assert_eq!(
            <PathProcessor as UnifiedFrameProcessor<MockTransport>>::name(),
            "PathProcessor"
        );
    }
}
