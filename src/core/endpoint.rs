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

pub use types::command::StreamCommand;

use lifecycle::{ConnectionLifecycleManager, DefaultLifecycleManager};
use crate::{
    config::Config,
    core::reliability::{ReliabilityLayer, TimeoutCheckResult},
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

    /// 统一的超时检查入口
    /// Unified timeout check entry point
    ///
    /// 该方法协调所有层次的超时检查，是分层超时管理架构的核心协调器。
    /// 它按顺序检查连接级和可靠性级的超时，并统一处理超时事件。
    ///
    /// This method coordinates timeout checks across all layers and serves as the
    /// core coordinator of the layered timeout management architecture. It checks
    /// connection-level and reliability-level timeouts in sequence and handles
    /// timeout events uniformly.
    pub async fn check_all_timeouts(&mut self, now: Instant) -> Result<()> {
        // 1. 检查全局定时器事件
        // Check global timer events
        let connection_timeout_events = self.timing.check_timer_events().await;
        
        // 2. 检查可靠性超时，使用帧重构
        // Check reliability timeouts with frame reconstruction
        let context = self.create_retransmission_context();
        let reliability_timeout_result = self.transport.reliability_mut().check_reliability_timeouts(now, &context);
        
        // 3. 处理超时事件
        // Handle timeout events
        self.handle_timeout_events(connection_timeout_events, reliability_timeout_result, now).await
    }

    /// 统一的下次唤醒时间计算
    /// Unified next wakeup time calculation
    ///
    /// 该方法协调所有层次的超时截止时间，计算事件循环的最优唤醒时间。
    /// 这确保了事件循环能够及时处理各种超时事件。
    ///
    /// This method coordinates timeout deadlines across all layers to calculate
    /// the optimal wakeup time for the event loop. This ensures the event loop
    /// can handle various timeout events in a timely manner.
    pub fn calculate_next_wakeup_time(&self) -> Instant {
        let rto_deadline = self.transport.reliability().next_reliability_timeout_deadline();
        
        // 使用全局定时器时，我们使用更频繁的检查间隔
        // When using global timer, we use more frequent check intervals
        let timer_check_interval = Instant::now() + std::time::Duration::from_millis(50);
        
        // 返回RTO截止时间和定时器检查间隔中的较早者
        // Return the earlier of RTO deadline and timer check interval
        match rto_deadline {
            Some(deadline) => deadline.min(timer_check_interval),
            None => timer_check_interval,
        }
    }

    /// 处理各种超时事件
    /// Handle various timeout events
    ///
    /// 该方法是超时事件处理的统一入口，根据不同的超时类型执行相应的处理逻辑。
    /// 它确保了超时处理的一致性和可维护性。
    ///
    /// This method is the unified entry point for timeout event handling,
    /// executing appropriate handling logic based on different timeout types.
    /// It ensures consistency and maintainability of timeout handling.
    async fn handle_timeout_events(
        &mut self,
        connection_events: Vec<TimeoutEvent>,
        reliability_result: TimeoutCheckResult,
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
        
        // 处理可靠性超时事件
        // Handle reliability timeout events
        for event in reliability_result.events {
            match event {
                TimeoutEvent::RetransmissionTimeout => {
                    // 重传超时，发送需要重传的帧
                    // Retransmission timeout, send frames that need retransmission
                    if !reliability_result.frames_to_retransmit.is_empty() {
                        self.send_frames(reliability_result.frames_to_retransmit.clone()).await?;
                    }
                }
                _ => {
                    // 其他可靠性超时事件的处理可以在这里添加
                    // Handling for other reliability timeout events can be added here
                }
            }
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
