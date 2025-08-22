//! The endpoint of a connection, which is the "brain" of the new layered protocol.
//!
//! 连接的端点，是新分层协议的“大脑”。

pub mod builder;
pub mod core;
pub mod lifecycle;
pub mod processing;
pub mod types;

#[cfg(test)]
mod tests;
pub mod timing;
pub mod unified_scheduler;

pub use types::command::StreamCommand;

use crate::{
    config::Config,
    core::reliability::{
        UnifiedReliabilityLayer,
        logic::congestion::{traits::CongestionController, vegas_controller::VegasController},
    },
    error::Result,
    socket::{SocketActorCommand, Transport},
};
use lifecycle::{ConnectionLifecycleManager, DefaultLifecycleManager};
use std::net::SocketAddr;
use timing::{TimeoutEvent, TimingManager};
use tokio::{sync::mpsc, time::Instant};
use tracing::trace;
use types::{
    channels::ChannelManager, identity::ConnectionIdentity, state::ConnectionState,
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
        if let Err(e) = self
            .command_tx
            .try_send(SocketActorCommand::RemoveConnection { cid: self.cid })
        {
            trace!(
                cid = self.cid,
                "Failed to send remove command during drop: {}", e
            );
        }
    }
}

/// Represents one end of a reliable connection.
pub struct Endpoint<T: Transport, C: CongestionController = VegasController> {
    /// 新的连接标识管理器
    /// New connection identity manager
    identity: ConnectionIdentity,

    /// 新的时间管理器
    /// New timing manager
    timing: TimingManager,

    /// 新的传输层管理器
    /// New transport manager
    transport: TransportManager<C>,

    /// 新的通道管理器
    /// New channel manager
    channels: ChannelManager<T>,

    /// 新的生命周期管理器
    /// New lifecycle manager
    lifecycle_manager: DefaultLifecycleManager,
    config: Config,
}

impl<T: Transport, C: CongestionController> Endpoint<T, C> {
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

    /// 处理定时器事件 - 事件驱动架构的核心方法
    /// Handle timer event - core method of event-driven architecture
    ///
    /// 此方法处理从全局定时器系统接收到的超时事件，替代了原有的轮询式超时检查
    /// This method handles timeout events received from the global timer system,
    /// replacing the original polling-based timeout checks
    pub async fn handle_timeout_event(&mut self, timeout_event: TimeoutEvent) -> Result<()> {
        use crate::core::endpoint::timing::TimeoutEvent;

        trace!(cid = self.identity.local_cid(), timeout_event = ?timeout_event, "Processing timeout event");

        match timeout_event {
            TimeoutEvent::IdleTimeout => {
                // 处理空闲超时 - 连接长时间无活动
                // Handle idle timeout - connection has been inactive for too long
                trace!(cid = self.identity.local_cid(), "Processing idle timeout");

                // 转换到关闭状态
                // Transition to closed state
                let _ = self.transition_state(ConnectionState::Closed);

                // 可以在这里添加额外的清理逻辑
                // Additional cleanup logic can be added here
            }

            TimeoutEvent::ConnectionTimeout => {
                // 处理连接超时 - 连接建立超时
                // Handle connection timeout - connection establishment timed out
                trace!(
                    cid = self.identity.local_cid(),
                    "Processing connection timeout"
                );

                // 转换到关闭状态
                // Transition to closed state
                let _ = self.transition_state(ConnectionState::Closed);
            }

            TimeoutEvent::PacketRetransmissionTimeout {
                sequence_number,
                timer_id,
            } => {
                // 新的基于数据包的重传处理逻辑
                // New packet-based retransmission logic
                trace!(
                    cid = self.identity.local_cid(),
                    seq = sequence_number,
                    timer_id = timer_id,
                    "Processing packet-based retransmission timeout"
                );

                let context = self.create_retransmission_context();

                // 使用PacketTimerManager处理特定数据包的超时
                // Use PacketTimerManager to handle specific packet timeout
                if let Some(retx_frame) = self
                    .transport
                    .unified_reliability_mut()
                    .handle_packet_timer_timeout(timer_id, &context)
                    .await
                {
                    trace!(
                        cid = self.identity.local_cid(),
                        seq = sequence_number,
                        timer_id = timer_id,
                        "Retransmitting specific packet using PacketTimerManager"
                    );

                    // 发送重传的单个帧
                    // Send the retransmitted single frame
                    self.send_frames(vec![retx_frame]).await?;

                    trace!(
                        cid = self.identity.local_cid(),
                        seq = sequence_number,
                        "Packet retransmission completed using new logic"
                    );
                } else {
                    trace!(
                        cid = self.identity.local_cid(),
                        seq = sequence_number,
                        timer_id = timer_id,
                        "No retransmission needed for packet (may have been acknowledged or dropped)"
                    );
                }
            }

            TimeoutEvent::PathValidationTimeout => {
                // 处理路径验证超时
                // Handle path validation timeout
                trace!(
                    cid = self.identity.local_cid(),
                    "Processing path validation timeout"
                );

                // 委托给TimingManager处理路径验证超时
                // Delegate to TimingManager to handle path validation timeout
                if let Err(e) = self.timing.handle_timeout_event(timeout_event).await {
                    tracing::warn!(
                        cid = self.identity.local_cid(),
                        error = e,
                        "Failed to handle path validation timeout"
                    );
                }
            }

            TimeoutEvent::FinProcessingTimeout => {
                // 处理FIN处理超时 - 检查是否可以发送EOF
                // Handle FIN processing timeout - check if EOF can be sent
                trace!(
                    cid = self.identity.local_cid(),
                    "Processing FIN processing timeout"
                );

                // 委托给TimingManager处理FIN处理超时
                // Delegate to TimingManager to handle FIN processing timeout
                if let Err(e) = self.timing.handle_timeout_event(timeout_event).await {
                    tracing::warn!(
                        cid = self.identity.local_cid(),
                        error = e,
                        "Failed to handle FIN processing timeout"
                    );
                }

                // 检查是否可以发送EOF
                // Check if EOF can be sent
                if self
                    .timing
                    .should_send_eof(self.transport.is_recv_buffer_empty())
                {
                    if let Some(tx) = self.channels.tx_to_stream_mut().take() {
                        trace!(
                            cid = self.identity.local_cid(),
                            "All data drained after FIN, closing user stream (sending EOF)."
                        );
                        drop(tx); // This closes the channel, signaling EOF.
                        self.timing.clear_fin_pending_eof().await; // Reset the flag.
                    }
                } else {
                    // 如果还不能发送EOF，重新调度检查
                    // If EOF can't be sent yet, reschedule the check
                    if self.timing.is_fin_pending_eof() {
                        // 直接调度下一次检查，而不是重新设置标志
                        // Directly schedule next check instead of resetting the flag
                        self.timing.schedule_fin_processing_check().await;
                    }
                }
            }
        }

        Ok(())
    }

    /// Creates a retransmission context with current endpoint state
    /// 使用当前端点状态创建重传上下文
    pub fn create_retransmission_context(&self) -> crate::packet::frame::RetransmissionContext {
        let (recv_next_sequence, recv_window_size) = self
            .transport
            .unified_reliability()
            .get_receive_window_info();

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
        self.lifecycle_manager
            .start_path_validation(new_addr, challenge_data, notifier)
    }

    /// 完成路径验证（使用生命周期管理器）
    /// Complete path validation (using lifecycle manager)
    pub async fn complete_path_validation(&mut self, success: bool) -> crate::error::Result<()> {
        // 取消路径验证超时定时器
        // Cancel path validation timeout timer
        let cancelled = self
            .timing
            .cancel_timer(&crate::core::endpoint::timing::TimeoutEvent::PathValidationTimeout)
            .await;
        tracing::debug!(
            "Path validation timeout timer cancelled on completion: {}",
            cancelled
        );

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
    pub fn unified_reliability_mut(&mut self) -> &mut UnifiedReliabilityLayer<C> {
        self.transport.unified_reliability_mut()
    }

    /// 获取可靠性层的引用
    /// Gets a reference to the reliability layer
    pub fn unified_reliability(&self) -> &UnifiedReliabilityLayer<C> {
        self.transport.unified_reliability()
    }

    // === 分层超时管理协调接口 Layered Timeout Management Coordination Interface ===

    // 轮询式统一超时检查接口已移除，改用事件驱动
    // Polling-based unified timeout check interface removed; using event-driven

    /// 统一的下次唤醒时间计算
    /// Unified next wakeup time calculation - optimized approach
    ///
    /// 该方法计算下一次唤醒时间，平衡性能优化和代码复杂性。
    /// 在新的重传系统中，主要依赖全局定时器系统和连接级超时。
    ///
    /// This method calculates the next wakeup time, balancing performance optimization
    /// and code complexity. In the new retransmission system, it mainly relies on the
    /// global timer system and connection-level timeouts.
    pub fn calculate_next_wakeup_time(&self) -> Instant {
        // 在新系统中，RTO超时由PacketTimerManager通过全局定时器系统管理
        // In the new system, RTO timeouts are managed by PacketTimerManager through the global timer system

        // 获取连接级超时截止时间
        // Get connection-level timeout deadline
        let connection_deadline = self.timing.next_connection_timeout_deadline(&self.config);

        // 使用连接级截止时间，或回退到默认检查间隔
        // Use connection-level deadline, or fallback to default check interval
        let fallback_interval = Instant::now() + std::time::Duration::from_millis(50);

        connection_deadline
            .unwrap_or(fallback_interval)
            .min(fallback_interval)
    }

    /// 统一的超时事件处理方法（新版本 - 支持分离接口）
    /// Unified timeout event handling method (new version - supports separated interfaces)
    ///
    /// 该方法处理分离后的超时事件和重传帧，实现了职责分离和性能优化。
    ///
    /// This method handles separated timeout events and retransmission frames,
    /// achieving responsibility separation and performance optimization.
    #[allow(dead_code)]
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
                TimeoutEvent::ConnectionTimeout => {
                    // 连接超时处理
                    // Connection timeout handling
                    tracing::info!("Connection establishment timeout");
                    self.lifecycle_manager.force_close()?;
                    return Err(crate::error::Error::ConnectionTimeout);
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
    #[allow(dead_code)]
    async fn handle_path_validation_timeout(&mut self) -> Result<()> {
        if matches!(
            self.lifecycle_manager.current_state(),
            ConnectionState::ValidatingPath { .. }
        ) {
            if let ConnectionState::ValidatingPath { notifier, .. } =
                self.lifecycle_manager.current_state().clone()
            {
                // 取消路径验证超时定时器
                // Cancel path validation timeout timer
                let cancelled = self
                    .timing
                    .cancel_timer(
                        &crate::core::endpoint::timing::TimeoutEvent::PathValidationTimeout,
                    )
                    .await;
                tracing::debug!("Path validation timeout timer cancelled: {}", cancelled);

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
