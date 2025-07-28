//! 事件分发器 - 负责将不同类型的事件路由到相应的处理器
//! Event Dispatcher - Routes different types of events to appropriate handlers

use crate::core::endpoint::Endpoint;
use crate::{
    error::Result,
    packet::frame::Frame,
    socket::AsyncUdpSocket,
};
use super::traits::ProcessorOperations;
use std::net::SocketAddr;
use tokio::time::Instant;
use tracing::trace;
use crate::core::endpoint::processing::processors::FrameProcessorRegistry;
use crate::core::endpoint::types::command::StreamCommand;

/// 事件分发器，负责将各种事件路由到正确的处理方法
/// Event dispatcher that routes various events to the correct handling methods
pub struct EventDispatcher;

impl EventDispatcher {
    /// 分发网络帧事件到对应的帧处理器
    /// Dispatches network frame events to the corresponding frame processors
    pub async fn dispatch_frame<S: AsyncUdpSocket>(
        endpoint: &mut Endpoint<S>,
        frame: Frame,
        src_addr: SocketAddr,
    ) -> Result<()> {
        trace!(local_cid = endpoint.local_cid(), ?frame, "Processing incoming frame");
        
        // 使用默认的帧处理器注册表来处理帧
        // 通过 trait 对象实现解耦
        // Use default frame processor registry to handle the frame
        // Achieve decoupling through trait objects
        let registry = FrameProcessorRegistry::<S>::default();
        
        // 将具体的 Endpoint 转换为 trait 对象以实现解耦
        // Convert concrete Endpoint to trait object to achieve decoupling
        registry.route_frame(endpoint as &mut dyn ProcessorOperations, frame, src_addr, Instant::now()).await
    }

    /// 分发流命令事件
    /// Dispatches stream command events
    pub async fn dispatch_stream_command<S: AsyncUdpSocket>(
        endpoint: &mut Endpoint<S>,
        cmd: StreamCommand,
    ) -> Result<()> {
        endpoint.handle_stream_command(cmd).await
    }

    /// 分发超时事件
    /// Dispatches timeout events
    pub async fn dispatch_timeout<S: AsyncUdpSocket>(
        endpoint: &mut Endpoint<S>,
        now: Instant,
    ) -> Result<()> {
        endpoint.handle_timeout(now).await
    }
}