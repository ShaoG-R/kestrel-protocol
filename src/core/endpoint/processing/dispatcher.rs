//! 事件分发器 - 负责将不同类型的事件路由到相应的处理器
//! Event Dispatcher - Routes different types of events to appropriate handlers

use super::traits::ProcessorOperations;
use crate::core::endpoint::Endpoint;
use crate::core::endpoint::processing::processors::StaticFrameProcessorRegistry;
use crate::core::endpoint::types::command::StreamCommand;
use crate::{error::Result, packet::frame::Frame, socket::Transport};
use std::net::SocketAddr;
use tokio::time::Instant;
use tracing::trace;

/// 事件分发器，负责将各种事件路由到正确的处理方法
/// Event dispatcher that routes various events to the correct handling methods
pub struct EventDispatcher;

impl EventDispatcher {
    /// 分发网络帧事件到对应的帧处理器
    /// Dispatches network frame events to the corresponding frame processors
    pub async fn dispatch_frame<T: Transport>(
        endpoint: &mut dyn ProcessorOperations,
        frame: Frame,
        src_addr: SocketAddr,
    ) -> Result<()> {
        trace!(
            local_cid = endpoint.local_cid(),
            ?frame,
            "Processing incoming frame"
        );

        // 🚀 使用高性能静态分发帧处理器 - 零开销抽象
        // 无需创建对象实例，直接静态分发，编译器可以内联所有调用
        // Use high-performance static dispatch frame processor - zero-cost abstraction
        // No object instantiation needed, direct static dispatch, compiler can inline all calls
        StaticFrameProcessorRegistry::route_frame::<T>(
            endpoint as &mut dyn ProcessorOperations,
            frame,
            src_addr,
            Instant::now(),
        )
        .await
    }

    /// 分发流命令事件
    /// Dispatches stream command events
    pub async fn dispatch_stream_command<T: Transport>(
        endpoint: &mut Endpoint<T>,
        cmd: StreamCommand,
    ) -> Result<()> {
        endpoint.handle_stream_command(cmd).await
    }

    // 轮询式分发超时事件接口已移除
    // Polling-based timeout dispatch has been removed
}
