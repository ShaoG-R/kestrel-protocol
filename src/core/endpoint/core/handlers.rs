//! The core event loop and logic for the `Endpoint`.

use crate::core::endpoint::Endpoint;
use crate::core::endpoint::core::frame::{
    create_path_challenge_frame, create_path_response_frame, create_syn_ack_frame,
};
use crate::core::endpoint::lifecycle::ConnectionLifecycleManager;
use crate::core::endpoint::types::command::StreamCommand;
use crate::core::endpoint::types::state::ConnectionState;
use crate::core::reliability::logic::congestion::traits::CongestionController;
use crate::{
    error::Result,
    packet::{frame::Frame, sack::decode_sack_ranges},
    socket::{SocketActorCommand, Transport},
};
use tokio::time::Instant;
use tracing::{info, trace};

impl<T: Transport, C: CongestionController> Endpoint<T, C> {
    // handle_frame 方法已被 EventDispatcher 替代
    // handle_frame method has been replaced by EventDispatcher

    pub async fn check_path_migration(&mut self, src_addr: std::net::SocketAddr) -> Result<()> {
        if src_addr != self.identity.remote_addr()
            && *self.lifecycle_manager.current_state() == ConnectionState::Established
        {
            // Address has changed, initiate path validation.
            let challenge_data = rand::random();
            let (tx, _rx) = tokio::sync::oneshot::channel();
            self.start_path_validation(src_addr, challenge_data, tx)?;

            // 注册路径验证超时定时器
            // Register path validation timeout timer
            let path_validation_timeout = self.config.connection.path_validation_timeout;
            if let Err(e) = self
                .timing
                .register_path_validation_timeout(path_validation_timeout)
                .await
            {
                tracing::warn!("Failed to register path validation timeout: {}", e);
            }

            // 使用新的路径验证管理器启动验证
            // Use new path validation manager to start validation
            // 注意：路径验证现在由 lifecycle manager 管理，而不是 timing manager
            // Note: Path validation is now managed by lifecycle manager, not timing manager

            let challenge_frame = create_path_challenge_frame(
                self.identity.peer_cid(),
                self.transport.unified_reliability_mut(),
                self.timing.start_time(),
                challenge_data,
            );
            self.send_frame_to(challenge_frame, src_addr).await?;
            // From now on, we will continue processing packets from the old address,
            // but will not send anything other than path validation packets to the new address.
        }
        Ok(())
    }

    pub async fn handle_frame_connecting(
        &mut self,
        frame: Frame,
        _src_addr: std::net::SocketAddr,
    ) -> Result<()> {
        match frame {
            Frame::SynAck { header } => {
                self.transition_state(ConnectionState::Established)?;
                self.identity.set_peer_cid(header.source_cid);

                // Acknowledge the SYN-ACK, potentially with piggybacked data if the
                // user has already called write().
                if !self.transport.unified_reliability().is_send_buffer_empty() {
                    self.packetize_and_send().await?;
                } else {
                    self.send_standalone_ack().await?;
                }
            }
            Frame::PathChallenge {
                header,
                challenge_data,
            } => {
                self.handle_path_challenge(header, challenge_data, _src_addr)
                    .await?;
            }
            _ => {
                trace!(
                    cid = self.identity.local_cid(),
                    ?frame,
                    "Ignoring unexpected frame in Connecting state"
                );
            }
        }
        Ok(())
    }

    pub async fn handle_frame_syn_received(
        &mut self,
        frame: Frame,
        src_addr: std::net::SocketAddr,
    ) -> Result<()> {
        match frame {
            Frame::Syn { .. } => {
                // This is now primarily handled by the Socket creating the Endpoint.
                // If we receive another SYN, it might be a retransmission from the client
                // because it hasn't received our SYN-ACK yet.
                info!(
                    cid = self.identity.local_cid(),
                    "Received duplicate SYN, ignoring."
                );
                // If we have already been triggered to send a SYN-ACK (i.e., data is in the
                // send buffer), we can resend it.
                if !self.transport.unified_reliability().is_send_buffer_empty() {
                    self.send_syn_ack().await?;
                }
            }
            Frame::Push { header, payload } => {
                // For 0-RTT PUSH frames received during the `SynReceived` state, the ACK
                // will be piggybacked onto the eventual SYN-ACK.
                self.transport
                    .unified_reliability_mut()
                    .receive_packet(header.sequence_number, payload);
            }
            Frame::Ack { header, payload } => {
                self.handle_ack_frame(header, payload).await?;
            }
            Frame::Fin { header, .. } => {
                // Handle FIN even in SynReceived state - this can happen with 0-RTT
                // where client sends data and immediately closes
                if self
                    .transport
                    .unified_reliability_mut()
                    .receive_fin(header.sequence_number)
                {
                    self.send_standalone_ack().await?;
                    // 在0-RTT场景中，从SynReceived状态转换到FinWait - 使用生命周期管理器
                    // In 0-RTT scenario, transition from SynReceived to FinWait - using lifecycle manager
                    let _ = self.transition_state(ConnectionState::FinWait);
                }
            }
            Frame::PathChallenge {
                header,
                challenge_data,
            } => {
                self.handle_path_challenge(header, challenge_data, src_addr)
                    .await?;
            }
            _ => {
                trace!(
                    cid = self.identity.local_cid(),
                    ?frame,
                    "Ignoring unexpected frame in SynReceived state"
                );
            }
        }
        Ok(())
    }

    pub async fn handle_frame_established(
        &mut self,
        frame: Frame,
        src_addr: std::net::SocketAddr,
    ) -> Result<()> {
        match frame {
            Frame::Push { header, payload } => {
                if self
                    .transport
                    .unified_reliability_mut()
                    .receive_packet(header.sequence_number, payload)
                {
                    self.send_standalone_ack().await?;
                }
            }
            Frame::Ack { header, payload } => {
                self.handle_ack_frame(header, payload).await?;
            }
            Frame::Fin { header, .. } => {
                // Just receive the FIN and ACK it. Do NOT change state here.
                // The state transition to FinWait is driven by the `reassemble`
                // method in the main loop, which is the single source of truth.
                if self
                    .transport
                    .unified_reliability_mut()
                    .receive_fin(header.sequence_number)
                {
                    self.send_standalone_ack().await?;
                }
            }
            Frame::PathChallenge {
                header,
                challenge_data,
            } => {
                self.handle_path_challenge(header, challenge_data, src_addr)
                    .await?;
            }
            _ => {
                trace!(
                    cid = self.identity.local_cid(),
                    ?frame,
                    "Ignoring unexpected frame in Established state"
                );
            }
        }
        Ok(())
    }

    pub async fn handle_frame_validating_path(
        &mut self,
        frame: Frame,
        src_addr: std::net::SocketAddr,
    ) -> Result<()> {
        match frame {
            Frame::PathResponse {
                header: _,
                challenge_data,
            } => {
                if let ConnectionState::ValidatingPath {
                    new_addr,
                    challenge_data: expected_challenge,
                    notifier,
                } = self.lifecycle_manager.current_state().clone()
                {
                    if src_addr == new_addr && challenge_data == expected_challenge {
                        // Path validation successful!
                        info!(
                            cid = self.identity.local_cid(),
                            old_addr = %self.identity.remote_addr(),
                            new_addr = %new_addr,
                            "Path validation successful, updating remote address"
                        );
                        self.complete_path_validation(true).await?;
                        self.identity.set_remote_addr(new_addr);

                        // Notify the caller of migrate() if there is one
                        if let Some(notifier) = notifier {
                            let _ = notifier.send(Ok(()));
                        }

                        // Notify ReliableUdpSocket to update the addr_to_cid map.
                        let _ = self
                            .channels
                            .command_tx
                            .send(SocketActorCommand::UpdateAddr {
                                cid: self.identity.local_cid(),
                                new_addr,
                            })
                            .await;
                    } else {
                        // Invalid path response, ignore it.
                        info!(
                            cid = self.identity.local_cid(),
                            "Received invalid PathResponse, ignoring."
                        );
                    }
                }
            }
            Frame::Push { header, payload } => {
                // Continue processing data frames during path validation
                if self
                    .transport
                    .unified_reliability_mut()
                    .receive_packet(header.sequence_number, payload)
                {
                    self.send_standalone_ack().await?;
                }
            }
            Frame::Ack { header, payload } => {
                self.handle_ack_frame(header, payload).await?;
            }
            Frame::Fin { header, .. } => {
                if self
                    .transport
                    .unified_reliability_mut()
                    .receive_fin(header.sequence_number)
                {
                    self.send_standalone_ack().await?;
                    // 路径验证期间收到FIN，转换到FinWait - 使用生命周期管理器
                    // Received FIN during path validation, transition to FinWait - using lifecycle manager
                    let _ = self.transition_state(ConnectionState::FinWait);
                }
            }
            Frame::PathChallenge {
                header,
                challenge_data,
            } => {
                self.handle_path_challenge(header, challenge_data, src_addr)
                    .await?;
            }
            _ => {
                trace!(
                    cid = self.identity.local_cid(),
                    ?frame,
                    "Ignoring unexpected frame in ValidatingPath state"
                );
            }
        }
        Ok(())
    }

    pub async fn handle_frame_closing(
        &mut self,
        frame: Frame,
        src_addr: std::net::SocketAddr,
    ) -> Result<()> {
        match frame {
            Frame::Push { header, payload } => {
                // It's possible to receive data after we've decided to close,
                // as the peer might have sent it before receiving our FIN.
                if self
                    .transport
                    .unified_reliability_mut()
                    .receive_packet(header.sequence_number, payload)
                {
                    self.send_standalone_ack().await?;
                }
            }
            Frame::Ack { header, payload } => {
                self.handle_ack_frame(header, payload).await?;
            }
            Frame::Fin { header, .. } => {
                if self
                    .transport
                    .unified_reliability_mut()
                    .receive_fin(header.sequence_number)
                {
                    self.send_standalone_ack().await?;
                    // 在Closing状态收到FIN，转换到ClosingWait - 使用生命周期管理器
                    // Received FIN in Closing state, transition to ClosingWait - using lifecycle manager
                    let _ = self.transition_state(ConnectionState::ClosingWait);
                }
            }
            Frame::PathChallenge {
                header,
                challenge_data,
            } => {
                self.handle_path_challenge(header, challenge_data, src_addr)
                    .await?;
            }
            _ => {
                trace!(
                    cid = self.identity.local_cid(),
                    ?frame,
                    "Ignoring unexpected frame in Closing state"
                );
            }
        }
        Ok(())
    }

    pub async fn handle_frame_closing_wait(
        &mut self,
        frame: Frame,
        src_addr: std::net::SocketAddr,
    ) -> Result<()> {
        match frame {
            Frame::Ack { header, payload } => {
                self.handle_ack_frame(header, payload).await?;
            }
            Frame::PathChallenge {
                header,
                challenge_data,
            } => {
                self.handle_path_challenge(header, challenge_data, src_addr)
                    .await?;
            }
            _ => {
                trace!(
                    cid = self.identity.local_cid(),
                    ?frame,
                    "Ignoring unexpected frame in ClosingWait state"
                );
            }
        }
        Ok(())
    }

    pub async fn handle_frame_fin_wait(
        &mut self,
        frame: Frame,
        src_addr: std::net::SocketAddr,
    ) -> Result<()> {
        match frame {
            Frame::Push { header, payload } => {
                if self
                    .transport
                    .unified_reliability_mut()
                    .receive_packet(header.sequence_number, payload)
                {
                    self.send_standalone_ack().await?;
                }
            }
            Frame::Ack { header, payload } => {
                self.handle_ack_frame(header, payload).await?;
            }
            Frame::Fin { header, .. } => {
                // This is a retransmitted FIN from the peer. Just acknowledge it again.
                // Do NOT change state here. The state should only transition to `Closing`
                // when the local application calls `shutdown()`.
                if self
                    .transport
                    .unified_reliability_mut()
                    .receive_fin(header.sequence_number)
                {
                    self.send_standalone_ack().await?;
                }
            }
            Frame::PathChallenge {
                header,
                challenge_data,
            } => {
                self.handle_path_challenge(header, challenge_data, src_addr)
                    .await?;
            }
            _ => {
                trace!(
                    cid = self.identity.local_cid(),
                    ?frame,
                    "Ignoring unexpected frame in FinWait state"
                );
            }
        }
        Ok(())
    }

    // Helper methods for common frame handling logic
    async fn handle_ack_frame(
        &mut self,
        header: crate::packet::header::ShortHeader,
        payload: bytes::Bytes,
    ) -> Result<()> {
        self.transport
            .set_peer_recv_window(header.recv_window_size as u32);
        let sack_ranges = decode_sack_ranges(payload);
        let now = Instant::now();
        let context = self.create_retransmission_context();
        let frames_to_retx = self
            .transport
            .unified_reliability_mut()
            .handle_ack_comprehensive(header.recv_next_sequence, sack_ranges, now, &context)
            .await;
        if !frames_to_retx.frames_to_retransmit.is_empty() {
            self.send_frames(frames_to_retx.frames_to_retransmit)
                .await?;
        }
        Ok(())
    }

    async fn handle_path_challenge(
        &mut self,
        header: crate::packet::header::ShortHeader,
        challenge_data: u64,
        src_addr: std::net::SocketAddr,
    ) -> Result<()> {
        let response_frame = create_path_response_frame(
            self.identity.peer_cid(),
            header.sequence_number, // Echo the sequence number
            self.timing.start_time(),
            challenge_data,
        );
        // The response MUST be sent back to the address the challenge came from.
        self.send_frame_to(response_frame, src_addr).await?;
        Ok(())
    }

    pub async fn handle_stream_command(&mut self, cmd: StreamCommand) -> Result<()> {
        match cmd {
            StreamCommand::SendData(data) => {
                // If this is the first data sent on a server-side connection,
                // it triggers the SYN-ACK and establishes the connection. This is our 0-RTT path.
                if *self.lifecycle_manager.current_state() == ConnectionState::SynReceived {
                    self.transition_state(ConnectionState::Established)?;

                    // 1. Queue the user's data into the reliability layer's stream buffer.
                    self.transport
                        .unified_reliability_mut()
                        .write_to_send_buffer(data);

                    // 2. Create the payload-less SYN-ACK frame.
                    let syn_ack_frame = create_syn_ack_frame(
                        &self.config,
                        self.identity.peer_cid(),
                        self.identity.local_cid(),
                    );

                    // 3. Use intelligent 0-RTT packet splitting to ensure SYN-ACK is in the first packet
                    let peer_recv_window = self.transport.peer_recv_window();
                    let packet_groups =
                        self.transport.unified_reliability_mut().packetize_zero_rtt(
                            self.identity.peer_cid(),
                            peer_recv_window,
                            syn_ack_frame,
                        );

                    // 4. Register timers for all frames in packet groups
                    // 4. 为数据包组中的所有帧注册定时器
                    let mut all_frames = Vec::new();
                    for packet_group in &packet_groups {
                        all_frames.extend(packet_group.iter().cloned());
                    }

                    if !all_frames.is_empty() {
                        let timer_count = self
                            .transport
                            .unified_reliability_mut()
                            .track_frames_for_retransmission(&all_frames)
                            .await;
                        trace!(
                            count = timer_count,
                            total_frames = all_frames.len(),
                            "Registered retransmission timers for 0-RTT frames"
                        );
                    }

                    // 5. Send each packet group separately to maintain the intelligent splitting
                    self.send_zero_rtt_packets(packet_groups).await?;
                } else {
                    self.transport
                        .unified_reliability_mut()
                        .write_to_send_buffer(data);
                }
            }
            StreamCommand::Close => {
                self.shutdown().await;
                // 不在这里立即打包发送。让事件循环在处理完所有挂起的流命令后，
                // 统一执行打包与发送，以保证应用层最终ACK数据先进入发送缓冲区，
                // 再与FIN正确排序并可能被合并发送。
            }
            StreamCommand::Migrate { new_addr, notifier } => {
                info!(cid = self.identity.local_cid(), new_addr = %new_addr, "Actively migrating to new address.");
                let challenge_data = rand::random();

                if let Err(_e) = self.start_path_validation(new_addr, challenge_data, notifier) {
                    // 如果状态管理器返回错误，通知调用者
                    return Ok(());
                }

                let challenge_frame = create_path_challenge_frame(
                    self.identity.peer_cid(),
                    self.transport.unified_reliability_mut(),
                    self.timing.start_time(),
                    challenge_data,
                );
                self.send_frame_to(challenge_frame, new_addr).await?;
            }
            #[cfg(test)]
            StreamCommand::UpdatePeerCid(peer_cid) => {
                // This is a test-only command to simulate the SocketActor's role.
                self.identity.set_peer_cid(peer_cid);
            }
        }
        Ok(())
    }

    // 轮询式超时处理入口已移除，改为事件驱动的 timer_event_rx → handle_timeout_event 流程
    // Polling-style timeout handling entry removed; using event-driven timer_event_rx → handle_timeout_event

    async fn shutdown(&mut self) {
        // 用户主动关闭，根据状态决定处理方式
        // User-initiated close, handle based on current state

        match self.lifecycle_manager.current_state() {
            crate::core::endpoint::types::state::ConnectionState::SynReceived => {
                // 在SynReceived状态下，连接还没有真正建立
                // 如果没有待发送的数据，可以立即关闭用户流
                // In SynReceived state, connection is not established yet
                // If there's no pending data, we can immediately close the user stream
                if self.transport.unified_reliability().is_send_buffer_empty() {
                    // 没有待发送数据，立即关闭用户流接收器
                    *self.channels.tx_to_stream_mut() = None;
                }

                // 尝试优雅关闭
                if let Ok(()) = self.begin_graceful_shutdown() {
                    if self.lifecycle_manager.should_close() {
                        self.transport
                            .unified_reliability_mut()
                            .cleanup_all_retransmission_timers()
                            .await;
                        // 确保用户流已关闭
                        *self.channels.tx_to_stream_mut() = None;
                    }
                }
            }
            crate::core::endpoint::types::state::ConnectionState::Connecting => {
                if self.transport.unified_reliability().is_send_buffer_empty() {
                    // 没有待发送数据，直接关闭
                    if let Ok(()) = self
                        .lifecycle_manager
                        .transition_to(crate::core::endpoint::types::state::ConnectionState::Closed)
                    {
                        self.transport
                            .unified_reliability_mut()
                            .cleanup_all_retransmission_timers()
                            .await;
                        // 连接直接关闭时才关闭用户流
                        *self.channels.tx_to_stream_mut() = None;
                    }
                } else {
                    // 有待发送数据，转换到Closing状态，继续尝试连接
                    if let Ok(()) = self.begin_graceful_shutdown() {
                        // 不要在这里关闭用户流，等待连接完全关闭
                        if self.lifecycle_manager.should_close() {
                            self.transport
                                .unified_reliability_mut()
                                .cleanup_all_retransmission_timers()
                                .await;
                            *self.channels.tx_to_stream_mut() = None;
                        }
                    }
                }
            }
            _ => {
                // 其他状态保持读端开放，直到收到FIN或连接完全关闭
                // For other states, keep read side open until FIN received or connection fully closed
                if let Ok(()) = self.begin_graceful_shutdown() {
                    // 只有连接完全关闭时才关闭用户流
                    if self.lifecycle_manager.should_close() {
                        self.transport
                            .unified_reliability_mut()
                            .cleanup_all_retransmission_timers()
                            .await;
                        *self.channels.tx_to_stream_mut() = None;
                    }
                }
            }
        }
    }
}
