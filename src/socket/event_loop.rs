//! The implementation of the transport-based `SocketActor`.
//!
//! 基于传输的 `SocketActor` 实现。

use super::{command::SocketActorCommand, transport::BindableTransport};
use crate::{error::Result, packet::frame::Frame};
use session_coordinator::SocketSessionCoordinator;
use std::net::SocketAddr;
use tokio::sync::mpsc;
use tracing::debug;

pub mod draining;
pub mod routing;
pub mod session_coordinator;

/// Metadata associated with each connection managed by the `ReliableUdpSocket`.
///
/// 与每个由 `ReliableUdpSocket` 管理的连接相关联的元数据。
#[derive(Debug)]
pub(crate) struct ConnectionMeta {
    /// The channel sender to the connection's `Endpoint` task.
    /// 到连接 `Endpoint` 任务的通道发送端。
    pub(crate) sender: mpsc::Sender<(Frame, SocketAddr)>,
}

/// The actor that owns and manages the transport and all connection state.
///
/// This actor runs in a dedicated task and processes commands from the public
/// `ReliableUdpSocket` handle and incoming frames from the transport.
///
/// 拥有并管理传输和所有连接状态的actor。
///
/// 此actor在专用任务中运行，并处理来自公共 `ReliableUdpSocket` 句柄的命令和来自传输的传入帧。
pub(crate) struct SocketEventLoop<T: BindableTransport> {
    /// Socket会话协调器 - 统一协调各层交互的中央控制器
    /// Socket session coordinator - central controller that coordinates inter-layer interactions
    pub(crate) session_coordinator: SocketSessionCoordinator<T>,
    pub(crate) command_rx: mpsc::Receiver<SocketActorCommand>,
}

impl<T: BindableTransport> SocketEventLoop<T> {
    /// Runs the actor's main event loop.
    ///
    /// 运行 actor 的主事件循环。
    pub(crate) async fn run(&mut self) {
        let mut cleanup_interval = tokio::time::interval(std::time::Duration::from_secs(2));

        loop {
            // 获取传输引用用于接收操作
            // Get transport reference for receive operations
            let transport = self.session_coordinator.transport_manager().transport();

            tokio::select! {
                // 1. Handle incoming actor commands.
                // 1. 处理传入的 actor 命令。
                Some(command) = self.command_rx.recv() => {
                    if self.handle_actor_command(command).await.is_err() {
                        // Error during command handling, possibly fatal.
                        // 命令处理期间出错，可能是致命的。
                        break;
                    }
                }
                // 2. Handle incoming frames from transport.
                // 2. 处理来自传输的传入帧。
                Ok(datagram) = transport.recv_frames() => {
                    debug!(
                        addr = %datagram.remote_addr,
                        frame_count = datagram.frames.len(),
                        "Received frame batch from transport"
                    );

                    if !datagram.frames.is_empty() {
                        // Check if the first frame indicates a new connection attempt.
                        // 检查第一帧是否表示新的连接尝试。
                        if let Frame::Syn { .. } = &datagram.frames[0] {
                            if let Err(e) = self.session_coordinator.handle_new_connection(datagram.frames, datagram.remote_addr).await {
                                // Log error but continue running the event loop
                                // 记录错误但继续运行事件循环
                                tracing::warn!(
                                    addr = %datagram.remote_addr,
                                    error = %e,
                                    "Failed to handle new connection"
                                );
                            }
                        } else {
                            // Otherwise, dispatch frames individually to existing connections.
                            // 否则，将帧单独分派到现有连接。
                            for frame in datagram.frames {
                                self.session_coordinator.dispatch_frame(frame, datagram.remote_addr).await;
                            }
                        }
                    }
                }
                // 3. Handle periodic cleanup of draining CIDs and expired early frames.
                // 3. 处理 draining CIDs 和超时早到帧的定期清理。
                _ = cleanup_interval.tick() => {
                    self.session_coordinator.frame_router_mut().cleanup_draining_pool();
                    // Clean up early arrival frames older than 5 seconds
                    // 清理超过5秒的早到帧
                    self.session_coordinator.frame_router_mut().cleanup_expired_frames(
                        std::time::Duration::from_secs(5)
                    );
                }
                else => break,
            }
        }
    }

    /// Handles a command sent to the actor.
    ///
    /// 处理发送给 actor 的命令。
    async fn handle_actor_command(&mut self, command: SocketActorCommand) -> Result<()> {
        match command {
            SocketActorCommand::Connect {
                remote_addr,
                config,
                initial_data,
                response_tx,
            } => {
                let result = self
                    .session_coordinator
                    .create_client_connection(remote_addr, *config, initial_data)
                    .await;
                let _ = response_tx.send(result);
            }
            SocketActorCommand::Rebind {
                new_local_addr,
                response_tx,
            } => {
                let result = self
                    .session_coordinator
                    .transport_manager_mut()
                    .rebind(new_local_addr)
                    .await
                    .map(|_| ());
                let _ = response_tx.send(result);
            }
            SocketActorCommand::UpdateAddr { cid, new_addr } => {
                self.session_coordinator
                    .frame_router_mut()
                    .update_connection_address(cid, new_addr);
            }
            SocketActorCommand::RemoveConnection { cid } => {
                self.session_coordinator
                    .frame_router_mut()
                    .remove_connection_by_cid(cid);
            }
            SocketActorCommand::GetLocalAddr { response_tx } => {
                let result = self.session_coordinator.transport_manager().local_addr();
                let _ = response_tx.send(result);
            }
        }
        Ok(())
    }
}
