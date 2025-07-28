//! 帧处理器模块 - 将不同类型帧的处理逻辑模块化
//! Frame Processors Module - Modularizes the processing logic for different frame types
//!
//! 该模块将帧处理逻辑从主要的 endpoint logic 中分离出来，
//! 为每种帧类型提供专门的处理器，提高代码的可维护性和可测试性。
//!
//! This module separates frame processing logic from the main endpoint logic,
//! providing specialized processors for each frame type to improve code
//! maintainability and testability.

pub mod ack;
pub mod connection;
pub mod data;
pub mod path;

// 重新导出主要的处理器类型
// Re-export main processor types
pub use ack::AckProcessor;
pub use connection::ConnectionProcessor;
pub use data::PushProcessor;
pub use path::PathProcessor;

use crate::{
    error::Result,
    packet::frame::Frame,
    socket::AsyncUdpSocket,
};
use std::net::SocketAddr;
use tokio::time::Instant;
use crate::core::endpoint::lifecycle::manager::ConnectionLifecycleManager;
use crate::core::endpoint::types::state::ConnectionState;
use crate::core::endpoint::Endpoint;

/// 帧处理器特征，定义了所有帧处理器的通用接口
/// Frame processor trait that defines the common interface for all frame processors
pub trait FrameProcessor<S: AsyncUdpSocket> {
    /// 处理特定类型的帧
    /// Process a specific type of frame
    fn process_frame(
        endpoint: &mut Endpoint<S>,
        frame: Frame,
        src_addr: SocketAddr,
        now: Instant,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
}

/// 帧处理器静态方法特征，用于不依赖泛型参数的方法
/// Frame processor static methods trait for methods that don't depend on generic parameters
pub trait FrameProcessorStatic {
    /// 检查该处理器是否可以处理给定的帧类型
    /// Check if this processor can handle the given frame type
    fn can_handle(frame: &Frame) -> bool;

    /// 获取处理器的名称，用于日志记录
    /// Get the processor name for logging
    fn name() -> &'static str;
}

/// 帧处理器路由器，负责将帧分发给合适的处理器
/// Frame processor router that dispatches frames to appropriate processors
pub struct FrameProcessorRouter;

impl FrameProcessorRouter {
    /// 根据帧类型和连接状态路由帧到合适的处理器
    /// Route frames to appropriate processors based on frame type and connection state
    pub async fn route_frame<S: AsyncUdpSocket>(
        endpoint: &mut Endpoint<S>,
        frame: Frame,
        src_addr: SocketAddr,
        now: Instant,
    ) -> Result<()> {
        // 更新最后接收时间
        // Update last receive time
        endpoint.update_last_recv_time(now);

        // 检查路径迁移
        // Check for path migration
        endpoint.check_path_migration(src_addr).await?;

        // 根据帧类型分发到相应的处理器
        // Dispatch to appropriate processor based on frame type
        match &frame {
            // 数据传输相关帧
            // Data transmission related frames
            Frame::Push { .. } => {
                PushProcessor::process_frame(endpoint, frame, src_addr, now).await
            }
            
            // 确认帧
            // Acknowledgment frames
            Frame::Ack { .. } => {
                AckProcessor::process_frame(endpoint, frame, src_addr, now).await
            }
            
            // 连接管理帧
            // Connection management frames
            Frame::Syn { .. } | Frame::SynAck { .. } | Frame::Fin { .. } => {
                ConnectionProcessor::process_frame(endpoint, frame, src_addr, now).await
            }
            
            // 路径验证帧
            // Path validation frames
            Frame::PathChallenge { .. } | Frame::PathResponse { .. } => {
                PathProcessor::process_frame(endpoint, frame, src_addr, now).await
            }
            
            // 心跳帧
            // Heartbeat frames
            Frame::Ping { .. } => {
                // PING 帧处理逻辑相对简单，直接在这里处理
                // PING frame processing is relatively simple, handle it directly here
                tracing::trace!(
                    cid = endpoint.local_cid(),
                    "Received PING frame, no action needed"
                );
                Ok(())
            }
        }
    }
}

/// 帧处理上下文，包含处理帧时需要的通用信息
/// Frame processing context containing common information needed when processing frames
pub struct FrameProcessingContext {
    /// 当前时间
    /// Current time
    pub now: Instant,
    
    /// 源地址
    /// Source address
    pub src_addr: SocketAddr,
    
    /// 连接状态
    /// Connection state
    pub connection_state: ConnectionState,
    
    /// 本地连接ID
    /// Local connection ID
    pub local_cid: u32,
}

impl FrameProcessingContext {
    /// 创建新的帧处理上下文
    /// Create a new frame processing context
    pub fn new<S: AsyncUdpSocket>(
        endpoint: &Endpoint<S>,
        src_addr: SocketAddr,
        now: Instant,
    ) -> Self {
        Self {
            now,
            src_addr,
            connection_state: endpoint.lifecycle_manager().current_state().clone(),
            local_cid: endpoint.local_cid(),
        }
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
    fn test_frame_processor_routing() {
        // 测试不同帧类型的路由逻辑
        // Test routing logic for different frame types
        
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
        assert!(!<AckProcessor as FrameProcessorStatic>::can_handle(&push_frame));
        assert!(!<ConnectionProcessor as FrameProcessorStatic>::can_handle(&push_frame));
        assert!(!<PathProcessor as FrameProcessorStatic>::can_handle(&push_frame));
    }
}