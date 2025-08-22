//! 连接管理帧处理器 - 处理 SYN、SYN-ACK、FIN 帧
//! Connection Management Frame Processor - Handles SYN, SYN-ACK, FIN frames
//!
//! 该模块专门处理连接生命周期管理相关的帧，包括连接建立、
//! 连接确认、连接关闭等逻辑。
//!
//! This module specifically handles connection lifecycle management related frames,
//! including connection establishment, connection acknowledgment,
//! connection termination, etc.

use super::{
    FrameProcessingContext, TypeSafeFrameProcessor, TypeSafeFrameValidator, UnifiedFrameProcessor,
    frame_types::ConnectionFrame,
};
use crate::{
    error::{ProcessorErrorContext, Result},
    packet::frame::Frame,
    socket::Transport,
};
use async_trait::async_trait;
use std::net::SocketAddr;
use tokio::time::Instant;
use tracing::{debug, info, trace, warn};

use super::super::traits::ProcessorOperations;
use crate::core::endpoint::types::state::ConnectionState;

/// 连接管理帧处理器
/// Connection management frame processor
pub struct ConnectionProcessor;

// 最新的类型安全处理器接口实现
// Latest type-safe processor interface implementation
#[async_trait]
impl<T: Transport> TypeSafeFrameProcessor<T> for ConnectionProcessor {
    type FrameTypeMarker = ConnectionFrame;

    fn name() -> &'static str {
        "ConnectionProcessor"
    }

    async fn process_frame(
        endpoint: &mut dyn ProcessorOperations,
        frame: Frame,
        src_addr: SocketAddr,
        now: Instant,
    ) -> Result<()> {
        // 类型安全验证：编译时保证只能处理连接管理帧
        // Type-safe validation: compile-time guarantee that only connection management frames can be processed
        <Self as TypeSafeFrameValidator>::validate_frame_type(&frame)?;

        Self::process_connection_frame_internal(endpoint, frame, src_addr, now).await
    }
}

// 实现类型安全验证接口
// Implement type-safe validation interface
impl TypeSafeFrameValidator for ConnectionProcessor {
    type FrameTypeMarker = ConnectionFrame;

    /// 验证帧类型是否为连接管理帧
    /// Validate that the frame type is a connection management frame
    fn validate_frame_type(frame: &Frame) -> Result<()> {
        match frame {
            Frame::Syn { .. } | Frame::SynAck { .. } | Frame::Fin { .. } => Ok(()),
            _ => Err(crate::error::Error::InvalidFrame(format!(
                "ConnectionProcessor can only handle connection management frames (SYN/SYN-ACK/FIN), got: {:?}",
                std::mem::discriminant(frame)
            ))),
        }
    }
}

impl ConnectionProcessor {
    /// 内部连接管理帧处理方法，供所有接口实现调用
    /// Internal connection management frame processing method, called by all interface implementations
    async fn process_connection_frame_internal(
        endpoint: &mut dyn ProcessorOperations,
        frame: Frame,
        src_addr: SocketAddr,
        now: Instant,
    ) -> Result<()> {
        let context = FrameProcessingContext::new(endpoint, src_addr, now);

        match frame {
            Frame::Syn { header } => Self::handle_syn_frame(endpoint, header, context).await,
            Frame::SynAck { header } => Self::handle_syn_ack_frame(endpoint, header, context).await,
            Frame::Fin { header } => Self::handle_fin_frame(endpoint, header, context).await,
            _ => {
                let error_context = ProcessorErrorContext::new(
                    "ConnectionProcessor",
                    endpoint.local_cid(),
                    src_addr,
                    format!("{:?}", endpoint.current_state()),
                    now,
                );
                Err(crate::error::Error::FrameTypeMismatch {
                    err: crate::error::FrameTypeMismatchError::new(
                        "connection management frame (SYN, SYN-ACK, or FIN)".to_string(),
                        format!("{:?}", std::mem::discriminant(&frame)),
                        error_context,
                    )
                    .into(),
                })
            }
        }
    }
}

// 统一接口实现
// Unified interface implementation
#[async_trait]
impl<T: Transport> UnifiedFrameProcessor<T> for ConnectionProcessor {
    type FrameType = ConnectionFrame;

    fn can_handle(frame: &Frame) -> bool {
        matches!(
            frame,
            Frame::Syn { .. } | Frame::SynAck { .. } | Frame::Fin { .. }
        )
    }

    fn name() -> &'static str {
        "ConnectionProcessor"
    }

    async fn process_frame(
        endpoint: &mut dyn ProcessorOperations,
        frame: Frame,
        src_addr: SocketAddr,
        now: Instant,
    ) -> Result<()> {
        Self::process_connection_frame_internal(endpoint, frame, src_addr, now).await
    }
}

impl ConnectionProcessor {
    /// 处理 SYN 帧
    /// Handle SYN frame
    async fn handle_syn_frame(
        endpoint: &mut dyn ProcessorOperations,
        header: crate::packet::header::LongHeader,
        context: FrameProcessingContext,
    ) -> Result<()> {
        trace!(
            cid = context.local_cid,
            peer_cid = header.source_cid,
            state = ?context.connection_state,
            "Processing SYN frame"
        );

        match context.connection_state {
            ConnectionState::SynReceived => {
                // 这主要由 Socket 创建 Endpoint 时处理。
                // 如果我们收到另一个 SYN，可能是客户端的重传，
                // 因为它还没有收到我们的 SYN-ACK。
                // This is now primarily handled by the Socket creating the Endpoint.
                // If we receive another SYN, it might be a retransmission from the client
                // because it hasn't received our SYN-ACK yet.
                info!(cid = context.local_cid, "Received duplicate SYN, ignoring.");

                // 如果我们已经被触发发送 SYN-ACK（即发送缓冲区中有数据），
                // 我们可以重新发送它。
                // If we have already been triggered to send a SYN-ACK (i.e., data is in the
                // send buffer), we can resend it.
                if !endpoint.unified_reliability().is_send_buffer_empty() {
                    endpoint.send_syn_ack_frame().await?;
                }
                Ok(())
            }
            _ => {
                warn!(
                    cid = context.local_cid,
                    state = ?context.connection_state,
                    "Ignoring SYN frame in unexpected state"
                );
                Ok(())
            }
        }
    }

    /// 处理 SYN-ACK 帧
    /// Handle SYN-ACK frame
    async fn handle_syn_ack_frame(
        endpoint: &mut dyn ProcessorOperations,
        header: crate::packet::header::LongHeader,
        context: FrameProcessingContext,
    ) -> Result<()> {
        debug!(
            cid = context.local_cid,
            peer_cid = header.source_cid,
            state = ?context.connection_state,
            "Processing SYN-ACK frame"
        );

        match context.connection_state {
            ConnectionState::Connecting => {
                // 取消连接超时定时器
                // Cancel connection timeout timer
                endpoint.cancel_connection_timeout().await;

                // 处理连接建立 - 使用新的生命周期管理器API
                // Handle connection establishment - using new lifecycle manager API
                endpoint.transition_state(ConnectionState::Established)?;
                endpoint.set_peer_cid(header.source_cid);

                // 确认 SYN-ACK，如果用户已经调用了 write()，
                // 可能会捎带数据
                // Acknowledge the SYN-ACK, potentially with piggybacked data if the
                // user has already called write()
                if !endpoint.unified_reliability().is_send_buffer_empty() {
                    endpoint.packetize_and_send_data().await?;
                } else {
                    endpoint.send_standalone_ack_frame().await?;
                }

                info!(
                    cid = context.local_cid,
                    peer_cid = header.source_cid,
                    "Connection established"
                );
                Ok(())
            }
            _ => {
                warn!(
                    cid = context.local_cid,
                    state = ?context.connection_state,
                    "Ignoring SYN-ACK frame in unexpected state"
                );
                Ok(())
            }
        }
    }

    /// 处理 FIN 帧
    /// Handle FIN frame
    async fn handle_fin_frame(
        endpoint: &mut dyn ProcessorOperations,
        header: crate::packet::header::ShortHeader,
        context: FrameProcessingContext,
    ) -> Result<()> {
        debug!(
            cid = context.local_cid,
            seq = header.sequence_number,
            state = ?context.connection_state,
            "Processing FIN frame"
        );

        match context.connection_state {
            ConnectionState::SynReceived => {
                Self::handle_fin_in_syn_received(endpoint, header).await
            }
            ConnectionState::Established => Self::handle_fin_in_established(endpoint, header).await,
            ConnectionState::ValidatingPath { .. } => {
                Self::handle_fin_in_validating_path(endpoint, header).await
            }
            ConnectionState::FinWait => Self::handle_fin_in_fin_wait(endpoint, header).await,
            ConnectionState::Closing => Self::handle_fin_in_closing(endpoint, header).await,
            _ => {
                warn!(
                    cid = context.local_cid,
                    state = ?context.connection_state,
                    "Ignoring FIN frame in unexpected state"
                );
                Ok(())
            }
        }
    }

    /// 在 SynReceived 状态下处理 FIN 帧
    /// Handle FIN frame in SynReceived state
    async fn handle_fin_in_syn_received(
        endpoint: &mut dyn ProcessorOperations,
        header: crate::packet::header::ShortHeader,
    ) -> Result<()> {
        debug!(
            cid = endpoint.local_cid(),
            seq = header.sequence_number,
            "Handling FIN in SynReceived state (0-RTT close)"
        );

        // 即使在 SynReceived 状态下也处理 FIN - 这可能发生在 0-RTT 中，使用高级接口
        // Handle FIN even in SynReceived state using high-level interface
        let fin_received = endpoint.receive_fin_and_ack(header.sequence_number).await?;
        if fin_received {
            // 在0-RTT场景中，从SynReceived状态转换到FinWait - 使用生命周期管理器
            // In 0-RTT scenario, transition from SynReceived to FinWait - using lifecycle manager
            let _ = endpoint.transition_state(ConnectionState::FinWait);
        }
        Ok(())
    }

    /// 在 Established 状态下处理 FIN 帧
    /// Handle FIN frame in Established state
    async fn handle_fin_in_established(
        endpoint: &mut dyn ProcessorOperations,
        header: crate::packet::header::ShortHeader,
    ) -> Result<()> {
        trace!(
            cid = endpoint.local_cid(),
            seq = header.sequence_number,
            "Handling FIN in Established state"
        );

        // 只是接收 FIN 并确认它，使用高级接口。不要在这里改变状态。
        // 到 FinWait 的状态转换由主循环中的 `reassemble` 方法驱动，这是唯一的真实来源。
        // Just receive the FIN and ACK it using high-level interface. Do NOT change state here.
        // The state transition to FinWait is driven by the `reassemble` method in the main loop.
        endpoint.receive_fin_and_ack(header.sequence_number).await?;
        Ok(())
    }

    /// 在 ValidatingPath 状态下处理 FIN 帧
    /// Handle FIN frame in ValidatingPath state
    async fn handle_fin_in_validating_path(
        endpoint: &mut dyn ProcessorOperations,
        header: crate::packet::header::ShortHeader,
    ) -> Result<()> {
        debug!(
            cid = endpoint.local_cid(),
            seq = header.sequence_number,
            "Handling FIN during path validation"
        );

        let fin_received = endpoint.receive_fin_and_ack(header.sequence_number).await?;
        if fin_received {
            // 路径验证期间收到FIN，转换到FinWait - 使用生命周期管理器
            // Received FIN during path validation, transition to FinWait - using lifecycle manager
            let _ = endpoint.transition_state(ConnectionState::FinWait);
        }
        Ok(())
    }

    /// 在 FinWait 状态下处理 FIN 帧
    /// Handle FIN frame in FinWait state
    async fn handle_fin_in_fin_wait(
        endpoint: &mut dyn ProcessorOperations,
        header: crate::packet::header::ShortHeader,
    ) -> Result<()> {
        debug!(
            cid = endpoint.local_cid(),
            seq = header.sequence_number,
            "Handling FIN in FinWait state (retransmitted FIN)"
        );

        // 这是来自对端的重传 FIN，使用高级接口处理。不要在这里改变状态。
        // 状态应该只在本地应用程序调用 `shutdown()` 时转换到 `Closing`。
        // This is a retransmitted FIN from the peer, handled with high-level interface.
        // Do NOT change state here. State should only transition to `Closing` when local app calls `shutdown()`.
        endpoint.receive_fin_and_ack(header.sequence_number).await?;
        Ok(())
    }

    /// 在 Closing 状态下处理 FIN 帧
    /// Handle FIN frame in Closing state
    async fn handle_fin_in_closing(
        endpoint: &mut dyn ProcessorOperations,
        header: crate::packet::header::ShortHeader,
    ) -> Result<()> {
        debug!(
            cid = endpoint.local_cid(),
            seq = header.sequence_number,
            "Handling FIN in Closing state"
        );

        let fin_received = endpoint.receive_fin_and_ack(header.sequence_number).await?;
        if fin_received {
            // 基于当前状态决定FIN后的状态转换 - 使用生命周期管理器
            // Determine FIN transition based on current state - using lifecycle manager
            match endpoint.current_state() {
                ConnectionState::Closing => {
                    let _ = endpoint.transition_state(ConnectionState::ClosingWait);
                }
                _ => {
                    // 如果不在Closing状态，可能是重传的FIN，忽略状态转换
                    // If not in Closing state, might be retransmitted FIN, ignore state transition
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::test_utils::MockTransport;
    use crate::packet::command::Command;
    use crate::packet::frame::Frame;
    use crate::packet::header::{LongHeader, ShortHeader};

    #[test]
    fn test_connection_processor_can_handle() {
        let syn_frame = Frame::Syn {
            header: LongHeader {
                command: Command::Syn,
                protocol_version: 1,
                destination_cid: 0,
                source_cid: 123,
            },
        };

        let syn_ack_frame = Frame::SynAck {
            header: LongHeader {
                command: Command::SynAck,
                protocol_version: 1,
                destination_cid: 123,
                source_cid: 456,
            },
        };

        let fin_frame = Frame::Fin {
            header: ShortHeader {
                command: Command::Fin,
                connection_id: 456,
                payload_length: 0,
                recv_window_size: 100,
                timestamp: 1000,
                sequence_number: 10,
                recv_next_sequence: 5,
            },
        };

        assert!(<ConnectionProcessor as UnifiedFrameProcessor<
            MockTransport,
        >>::can_handle(&syn_frame));
        assert!(<ConnectionProcessor as UnifiedFrameProcessor<
            MockTransport,
        >>::can_handle(&syn_ack_frame));
        assert!(<ConnectionProcessor as UnifiedFrameProcessor<
            MockTransport,
        >>::can_handle(&fin_frame));

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

        assert!(!<ConnectionProcessor as UnifiedFrameProcessor<
            MockTransport,
        >>::can_handle(&push_frame));
    }

    #[test]
    fn test_processor_name() {
        assert_eq!(
            <ConnectionProcessor as UnifiedFrameProcessor<MockTransport>>::name(),
            "ConnectionProcessor"
        );
    }
}
