//! 数据帧处理器 - 处理 PUSH 帧
//! Data Frame Processor - Handles PUSH frames
//!
//! 该模块专门处理数据传输相关的帧，包括 PUSH 帧的接收、
//! 序列号验证、数据重组等逻辑。
//!
//! This module specifically handles data transmission related frames,
//! including PUSH frame reception, sequence number validation,
//! data reassembly logic, etc.

use super::{FrameProcessingContext, FrameProcessor, FrameProcessorStatic};
use crate::{
    error::Result,
    packet::frame::Frame,
    socket::AsyncUdpSocket,
};
use std::net::SocketAddr;
use tokio::time::Instant;
use tracing::{debug, trace, warn};

use crate::core::endpoint::Endpoint;
use crate::core::endpoint::types::state::ConnectionState;

/// PUSH 帧处理器
/// PUSH frame processor
pub struct PushProcessor;

impl<S: AsyncUdpSocket> FrameProcessor<S> for PushProcessor {
    fn process_frame(
        endpoint: &mut Endpoint<S>,
        frame: Frame,
        src_addr: SocketAddr,
        now: Instant,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        async move {
        let Frame::Push { header, payload } = frame else {
            return Err(crate::error::Error::InvalidFrame("Expected PUSH frame".into()));
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
        // Process PUSH frame based on connection state
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
            _ => {
                warn!(
                    cid = context.local_cid,
                    state = ?context.connection_state,
                    "Ignoring PUSH frame in unexpected state"
                );
                Ok(())
            }
        }
        }
    }
}

impl FrameProcessorStatic for PushProcessor {
    fn can_handle(frame: &Frame) -> bool {
        matches!(frame, Frame::Push { .. })
    }

    fn name() -> &'static str {
        "PushProcessor"
    }
}

impl PushProcessor {
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
    use crate::packet::frame::Frame;
    use crate::packet::header::ShortHeader;
    use crate::packet::command::Command;
    use bytes::Bytes;

    #[test]
    fn test_push_processor_can_handle() {
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

        assert!(<PushProcessor as FrameProcessorStatic>::can_handle(&push_frame));

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

        assert!(!<PushProcessor as FrameProcessorStatic>::can_handle(&ack_frame));
    }

    #[test]
    fn test_processor_name() {
        assert_eq!(<PushProcessor as FrameProcessorStatic>::name(), "PushProcessor");
    }
}