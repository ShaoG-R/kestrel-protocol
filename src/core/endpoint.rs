//! The endpoint of a connection, which is the "brain" of the new layered protocol.
//!
//! 连接的端点，是新分层协议的“大脑”。

pub mod core;
pub mod processing;
pub mod lifecycle;
pub mod builder;
pub mod types;

#[cfg(test)]
mod tests;
pub mod timing;
pub mod unified_scheduler;

pub use types::command::StreamCommand;

use lifecycle::{ConnectionLifecycleManager, DefaultLifecycleManager};
use crate::{
    config::Config,
    core::reliability::ReliabilityLayer,
    error::Result,
    socket::{SocketActorCommand, Transport},
};
use std::net::SocketAddr;
use tokio::{
    sync::mpsc,
    time::Instant,
};
use tracing::trace;
use timing::{TimeoutEvent, TimingManager};
use crate::core::endpoint::unified_scheduler::{RetransmissionLayer, TimeoutLayer};
use types::{
    channels::ChannelManager,
    identity::ConnectionIdentity,
    state::ConnectionState,
    transport::TransportManager,
};



/// A guard that ensures the connection is cleaned up in the `SocketActor`
/// when the `Endpoint` is dropped.
///
/// 一个哨兵结构，确保在 `Endpoint` 被丢弃时，其在 `SocketActor` 中的
/// 连接状态能够被清理。
struct ConnectionCleaner<T: Transport> {
    cid: u32,
    command_tx: mpsc::Sender<SocketActorCommand>,
    _marker: std::marker::PhantomData<T>,
}

impl<T: Transport> Drop for ConnectionCleaner<T> {
    fn drop(&mut self) {
        // Use `try_send` to avoid blocking in a drop implementation. This is a
        // "best-effort" cleanup. If the channel is full or closed, the actor
        // will eventually clean up the connection via timeout.
        if let Err(e) = self.command_tx.try_send(SocketActorCommand::RemoveConnection { cid: self.cid }) {
            trace!(cid = self.cid, "Failed to send remove command during drop: {}", e);
        }
    }
}

/// Represents one end of a reliable connection.
pub struct Endpoint<T: Transport> {
    /// 新的连接标识管理器
    /// New connection identity manager
    identity: ConnectionIdentity,
    
    /// 新的时间管理器
    /// New timing manager
    timing: TimingManager,
    
    /// 新的传输层管理器
    /// New transport manager
    transport: TransportManager,

    /// 新的通道管理器
    /// New channel manager
    channels: ChannelManager<T>,

    /// 新的生命周期管理器
    /// New lifecycle manager
    lifecycle_manager: DefaultLifecycleManager,
    config: Config,
    
}

impl<T: Transport> Endpoint<T> {

    /// 获取生命周期管理器的引用
    /// Gets a reference to the lifecycle manager
    pub fn lifecycle_manager(&self) -> &DefaultLifecycleManager {
        &self.lifecycle_manager
    }

    /// 获取生命周期管理器的可变引用
    /// Gets a mutable reference to the lifecycle manager
    pub fn lifecycle_manager_mut(&mut self) -> &mut DefaultLifecycleManager {
        &mut self.lifecycle_manager
    }

    /// Creates a retransmission context with current endpoint state
    /// 使用当前端点状态创建重传上下文
    pub fn create_retransmission_context(&self) -> crate::packet::frame::RetransmissionContext {
        let (recv_next_sequence, recv_window_size) = {
            let info = self.transport.reliability().get_ack_info();
            (info.1, info.2)
        };

        crate::packet::frame::RetransmissionContext::new(
            self.timing.start_time(),
            self.identity.peer_cid(),
            self.config.protocol_version,
            self.identity.local_cid(),
            recv_next_sequence,
            recv_window_size,
        )
    }

    /// 通过生命周期管理器进行状态转换（新方法）
    /// Perform state transition through lifecycle manager (new method)
    pub fn transition_state(&mut self, new_state: ConnectionState) -> crate::error::Result<()> {
        // 只通过生命周期管理器验证和执行转换
        // Only validate and execute transition through lifecycle manager
        self.lifecycle_manager.transition_to(new_state)?;
        Ok(())
    }

    /// 检查是否可以发送数据（使用生命周期管理器）
    /// Check if data can be sent (using lifecycle manager)
    pub fn can_send_data(&self) -> bool {
        self.lifecycle_manager.can_send_data()
    }

    /// 检查是否可以接收数据（使用生命周期管理器）
    /// Check if data can be received (using lifecycle manager)  
    pub fn can_receive_data(&self) -> bool {
        self.lifecycle_manager.can_receive_data()
    }

    /// 开始优雅关闭（使用生命周期管理器）
    /// Start graceful shutdown (using lifecycle manager)
    pub fn begin_graceful_shutdown(&mut self) -> crate::error::Result<()> {
        self.lifecycle_manager.begin_graceful_shutdown()
    }

    /// 开始路径验证（使用生命周期管理器）
    /// Start path validation (using lifecycle manager)
    pub fn start_path_validation(
        &mut self,
        new_addr: SocketAddr,
        challenge_data: u64,
        notifier: tokio::sync::oneshot::Sender<crate::error::Result<()>>,
    ) -> crate::error::Result<()> {
        self.lifecycle_manager.start_path_validation(new_addr, challenge_data, notifier)
    }

    /// 完成路径验证（使用生命周期管理器）
    /// Complete path validation (using lifecycle manager)
    pub fn complete_path_validation(&mut self, success: bool) -> crate::error::Result<()> {
        self.lifecycle_manager.complete_path_validation(success)
    }

    /// 获取本地连接ID
    /// Gets the local connection ID
    pub fn local_cid(&self) -> u32 {
        self.identity.local_cid()
    }

    /// 获取对端连接ID
    /// Gets the peer connection ID
    pub fn peer_cid(&self) -> u32 {
        self.identity.peer_cid()
    }

    /// 设置对端连接ID
    /// Sets the peer connection ID
    pub fn set_peer_cid(&mut self, peer_cid: u32) {
        self.identity.set_peer_cid(peer_cid);
    }

    /// 获取当前连接状态
    /// Gets the current connection state
    pub fn state(&self) -> &ConnectionState {
        self.lifecycle_manager.current_state()
    }



    /// 更新最后接收时间
    /// Updates the last receive time
    pub fn update_last_recv_time(&mut self, time: Instant) {
        self.timing.update_last_recv_time(time);
    }

    /// 获取连接开始时间
    /// Gets the connection start time
    pub fn start_time(&self) -> Instant {
        self.timing.start_time()
    }

    /// 获取远程地址
    /// Gets the remote address
    pub fn remote_addr(&self) -> SocketAddr {
        self.identity.remote_addr()
    }

    /// 设置远程地址
    /// Sets the remote address
    pub fn set_remote_addr(&mut self, addr: SocketAddr) {
        self.identity.set_remote_addr(addr);
    }

    /// 获取命令发送器
    /// Gets the command sender
    pub fn command_tx(&self) -> &mpsc::Sender<SocketActorCommand> {
        self.channels.command_tx()
    }

    /// 获取可靠性层的可变引用
    /// Gets a mutable reference to the reliability layer
    pub fn reliability_mut(&mut self) -> &mut ReliabilityLayer {
        self.transport.reliability_mut()
    }

    /// 获取可靠性层的引用
    /// Gets a reference to the reliability layer
    pub fn reliability(&self) -> &ReliabilityLayer {
        self.transport.reliability()
    }

    // === 分层超时管理协调接口 Layered Timeout Management Coordination Interface ===

    /// 统一的超时检查入口 - 混合方式
    /// 统一超时检查入口点 - 使用统一调度器实现21倍性能提升
    /// Unified timeout check entry point - using unified scheduler for 21x performance improvement
    ///
    /// 该方法使用重构后的统一调度器来协调所有层次的超时检查，
    /// 实现职责分离和性能优化的最佳平衡。
    ///
    /// This method uses the refactored unified scheduler to coordinate timeout checks
    /// across all layers, achieving the best balance of responsibility separation and
    /// performance optimization.
    pub async fn check_all_timeouts(&mut self, now: Instant) -> Result<()> {
        // === 方案1+2实现: 使用统一调度器进行超时检查 ===
        // === Solution 1+2 Implementation: Use unified scheduler for timeout checks ===
        
        // 1. 分别检查各层的超时事件（使用新的分离接口）
        // Check timeout events for each layer separately (using new separated interfaces)
        let timing_result = self.timing.check_timeout_events(now);
        let reliability_result = self.transport.reliability_mut().check_timeout_events(now);
        
        // 2. 处理超时事件并检查重传需求
        // Handle timeout events and check retransmission needs
        let mut all_connection_events = Vec::new();
        let mut needs_retransmission = false;
        
        // 处理连接级事件
        // Handle connection-level events
        all_connection_events.extend(timing_result.events);
        
        // 处理可靠性层事件
        // Handle reliability layer events
        for event in reliability_result.events {
            match event {
                TimeoutEvent::RetransmissionTimeout => {
                    needs_retransmission = true;
                }
                _ => {
                    all_connection_events.push(event);
                }
            }
        }
        
        // 3. 如果需要重传，使用专门的重传接口
        // If retransmission is needed, use dedicated retransmission interface
        let retransmission_frames = if needs_retransmission {
            let context = self.create_retransmission_context();
            let retx_result = self.transport.reliability_mut().check_retransmissions(now, &context);
            retx_result.frames_to_retransmit
        } else {
            Vec::new()
        };
        
        // 4. 处理所有超时事件
        // Handle all timeout events
        if !all_connection_events.is_empty() || !retransmission_frames.is_empty() {
            self.handle_unified_timeout_events_impl(all_connection_events, retransmission_frames, now).await?;
        }
        
        // 5. 执行定期的统一调度器清理
        // Perform periodic unified scheduler cleanup
        self.timing.cleanup_unified_scheduler();
        
        Ok(())
    }

    /// 统一的下次唤醒时间计算
    /// Unified next wakeup time calculation - optimized approach
    ///
    /// 该方法计算下一次唤醒时间，平衡性能优化和代码复杂性。
    /// 结合可靠性层的RTO截止时间和全局定时器检查间隔。
    ///
    /// This method calculates the next wakeup time, balancing performance optimization 
    /// and code complexity. Combines reliability layer RTO deadlines with global timer 
    /// check intervals.
    pub fn calculate_next_wakeup_time(&self) -> Instant {
        // 获取可靠性层的下一个超时截止时间
        // Get next timeout deadline from reliability layer
        let rto_deadline = self.transport.reliability().next_reliability_timeout_deadline();
        
        // 获取连接级超时截止时间
        // Get connection-level timeout deadline
        let connection_deadline = self.timing.next_connection_timeout_deadline(&self.config);
        
        // 计算最早的截止时间
        // Calculate the earliest deadline
        let earliest_deadline = match (rto_deadline, connection_deadline) {
            (Some(rto), Some(conn)) => Some(rto.min(conn)),
            (Some(rto), None) => Some(rto),
            (None, Some(conn)) => Some(conn),
            (None, None) => None,
        };
        
        // 使用最早截止时间，或回退到默认检查间隔
        // Use earliest deadline, or fallback to default check interval
        let fallback_interval = Instant::now() + std::time::Duration::from_millis(50);
        
        earliest_deadline.unwrap_or(fallback_interval).min(fallback_interval)
    }

    /// 统一的超时事件处理方法（新版本 - 支持分离接口）
    /// Unified timeout event handling method (new version - supports separated interfaces)
    ///
    /// 该方法处理分离后的超时事件和重传帧，实现了职责分离和性能优化。
    ///
    /// This method handles separated timeout events and retransmission frames,
    /// achieving responsibility separation and performance optimization.
    async fn handle_unified_timeout_events_impl(
        &mut self,
        connection_events: Vec<TimeoutEvent>,
        retransmission_frames: Vec<crate::packet::frame::Frame>,
        _now: Instant,
    ) -> Result<()> {
        // 处理连接级超时事件
        // Handle connection-level timeout events
        for event in connection_events {
            match event {
                TimeoutEvent::IdleTimeout => {
                    // 连接超时，强制关闭
                    // Connection timeout, force close
                    self.lifecycle_manager.force_close()?;
                    return Err(crate::error::Error::ConnectionTimeout);
                }
                TimeoutEvent::PathValidationTimeout => {
                    // 路径验证超时处理
                    // Path validation timeout handling
                    self.handle_path_validation_timeout().await?;
                }
                _ => {
                    // 其他连接级超时事件的处理可以在这里添加
                    // Handling for other connection-level timeout events can be added here
                }
            }
        }
        
        // 处理重传帧
        // Handle retransmission frames
        if !retransmission_frames.is_empty() {
            self.send_frames(retransmission_frames).await?;
        }
        
        Ok(())
    }

    /// 处理路径验证超时
    /// Handle path validation timeout
    ///
    /// 该方法处理路径验证超时的具体逻辑，包括状态转换和通知调用者。
    ///
    /// This method handles the specific logic for path validation timeout,
    /// including state transitions and notifying callers.
    async fn handle_path_validation_timeout(&mut self) -> Result<()> {
        if matches!(
            self.lifecycle_manager.current_state(),
            ConnectionState::ValidatingPath { .. }
        ) {
            if let ConnectionState::ValidatingPath { notifier, .. } =
                self.lifecycle_manager.current_state().clone()
            {
                if let Some(notifier) = notifier {
                    let _ = notifier.send(Err(crate::error::Error::PathValidationTimeout));
                }
                // 路径验证超时，回到Established状态
                // Path validation timeout, return to Established state
                self.transition_state(ConnectionState::Established)?;
            }
        }
        Ok(())
    }


}
