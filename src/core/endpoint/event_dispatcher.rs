//! 事件分发器 - 负责将不同类型的事件路由到相应的处理器
//! Event Dispatcher - Routes different types of events to appropriate handlers

use super::{command::StreamCommand, state::ConnectionState, Endpoint};
use crate::{
    error::Result,
    packet::frame::Frame,
    socket::AsyncUdpSocket,
};
use std::net::SocketAddr;
use tokio::time::Instant;
use tracing::trace;

/// 事件分发器，负责将各种事件路由到正确的处理方法
/// Event dispatcher that routes various events to the correct handling methods
pub struct EventDispatcher;

impl EventDispatcher {
    /// 分发网络帧事件到对应的状态处理器
    /// Dispatches network frame events to the corresponding state handlers
    pub async fn dispatch_frame<S: AsyncUdpSocket>(
        endpoint: &mut Endpoint<S>,
        frame: Frame,
        src_addr: SocketAddr,
    ) -> Result<()> {
        trace!(local_cid = endpoint.local_cid(), ?frame, "Processing incoming frame");
        endpoint.update_last_recv_time(Instant::now());

        // 检查路径迁移
        // Check for path migration
        endpoint.check_path_migration(src_addr).await?;

        // 根据当前状态分发帧处理
        // Dispatch frame handling based on current state
        match endpoint.state() {
            ConnectionState::Connecting => {
                endpoint.handle_frame_connecting(frame, src_addr).await
            }
            ConnectionState::SynReceived => {
                endpoint.handle_frame_syn_received(frame, src_addr).await
            }
            ConnectionState::Established => {
                endpoint.handle_frame_established(frame, src_addr).await
            }
            ConnectionState::ValidatingPath { .. } => {
                endpoint.handle_frame_validating_path(frame, src_addr).await
            }
            ConnectionState::Closing => endpoint.handle_frame_closing(frame, src_addr).await,
            ConnectionState::FinWait => endpoint.handle_frame_fin_wait(frame, src_addr).await,
            ConnectionState::ClosingWait => {
                endpoint.handle_frame_closing_wait(frame, src_addr).await
            }
            ConnectionState::Closed => {
                // 已关闭的连接不处理任何帧
                // Closed connections don't handle any frames
                Ok(())
            }
        }
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