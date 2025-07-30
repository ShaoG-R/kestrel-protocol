//! The core event loop and logic for the `Endpoint`.

use crate::core::endpoint::Endpoint;
use crate::{
    error::Result,
    packet::{frame::Frame, sack::decode_sack_ranges},
    socket::{SocketActorCommand, Transport},
};
use tokio::time::Instant;
use tracing::{info, trace};
use crate::core::endpoint::core::frame::{
    create_path_challenge_frame, create_path_response_frame, create_syn_ack_frame,
};
use crate::core::endpoint::lifecycle::ConnectionLifecycleManager;
use crate::core::endpoint::types::command::StreamCommand;
use crate::core::endpoint::types::state::ConnectionState;

impl<T: Transport> Endpoint<T> {

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
            let challenge_frame = create_path_challenge_frame(
                self.identity.peer_cid(),
                self.transport.reliability_mut().next_sequence_number(),
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
                if !self.transport.reliability().is_send_buffer_empty() {
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
                info!(cid = self.identity.local_cid(), "Received duplicate SYN, ignoring.");
                // If we have already been triggered to send a SYN-ACK (i.e., data is in the
                // send buffer), we can resend it.
                if !self.transport.reliability().is_send_buffer_empty() {
                    self.send_syn_ack().await?;
                }
            }
            Frame::Push { header, payload } => {
                // For 0-RTT PUSH frames received during the `SynReceived` state, the ACK
                // will be piggybacked onto the eventual SYN-ACK.
                self.transport.reliability_mut()
                    .receive_push(header.sequence_number, payload);
            }
            Frame::Ack { header, payload } => {
                self.handle_ack_frame(header, payload).await?;
            }
            Frame::Fin { header, .. } => {
                // Handle FIN even in SynReceived state - this can happen with 0-RTT
                // where client sends data and immediately closes
                if self.transport.reliability_mut().receive_fin(header.sequence_number) {
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
                    .transport.reliability_mut()
                    .receive_push(header.sequence_number, payload)
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
                if self.transport.reliability_mut().receive_fin(header.sequence_number) {
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
                        self.complete_path_validation(true)?;
                        self.identity.set_remote_addr(new_addr);

                        // Notify the caller of migrate() if there is one
                        if let Some(notifier) = notifier {
                            let _ = notifier.send(Ok(()));
                        }

                        // Notify ReliableUdpSocket to update the addr_to_cid map.
                        let _ = self
                            .channels.command_tx
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
                    .transport.reliability_mut()
                    .receive_push(header.sequence_number, payload)
                {
                    self.send_standalone_ack().await?;
                }
            }
            Frame::Ack { header, payload } => {
                self.handle_ack_frame(header, payload).await?;
            }
            Frame::Fin { header, .. } => {
                if self.transport.reliability_mut().receive_fin(header.sequence_number) {
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
                    .transport.reliability_mut()
                    .receive_push(header.sequence_number, payload)
                {
                    self.send_standalone_ack().await?;
                }
            }
            Frame::Ack { header, payload } => {
                self.handle_ack_frame(header, payload).await?;
            }
            Frame::Fin { header, .. } => {
                if self.transport.reliability_mut().receive_fin(header.sequence_number) {
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
                    .transport.reliability_mut()
                    .receive_push(header.sequence_number, payload)
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
                if self.transport.reliability_mut().receive_fin(header.sequence_number) {
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
        self.transport.set_peer_recv_window(header.recv_window_size as u32);
        let sack_ranges = decode_sack_ranges(payload);
        let frames_to_retx =
            self.transport.reliability_mut()
                .handle_ack(header.recv_next_sequence, sack_ranges, Instant::now());
        if !frames_to_retx.is_empty() {
            self.send_frames(frames_to_retx).await?;
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
                    self.transport.reliability_mut().write_to_stream(data);

                    // 2. Create the payload-less SYN-ACK frame.
                    let syn_ack_frame =
                        create_syn_ack_frame(&self.config, self.identity.peer_cid(), self.identity.local_cid());

                    // 3. Packetize the stream data into PUSH frames. This will correctly
                    //    assign sequence numbers starting from 0.
                    let now = Instant::now();
                    let peer_recv_window = self.transport.peer_recv_window();
                    let frames_to_send = self.transport.reliability_mut().packetize_stream_data(
                        self.identity.peer_cid(),
                        peer_recv_window,
                        now,
                        self.timing.start_time(),
                        Some(syn_ack_frame),
                    );

                    // 4. Prepend the SYN-ACK frame to coalesce it with the PUSH frames.
                    // frames_to_send.insert(0, syn_ack_frame);

                    // 5. Send them all in one go.
                    self.send_frames(frames_to_send).await?;
                } else {
                    self.transport.reliability_mut().write_to_stream(data);
                }
            }
            StreamCommand::Close => {
                self.shutdown();
                // Only attempt to send a FIN packet if we're not already closed
                if !self.lifecycle_manager.should_close() {
                    self.packetize_and_send().await?;
                }
            }
            StreamCommand::Migrate { new_addr, notifier } => {
                info!(cid = self.identity.local_cid(), new_addr = %new_addr, "Actively migrating to new address.");
                let challenge_data = rand::random();

                if let Err(_e) = self.start_path_validation(
                    new_addr,
                    challenge_data,
                    notifier,
                ) {
                    // 如果状态管理器返回错误，通知调用者
                    return Ok(());
                }

                let challenge_frame = create_path_challenge_frame(
                    self.identity.peer_cid(),
                    self.transport.reliability_mut().next_sequence_number(),
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

    /// 处理超时事件（向后兼容方法）
    /// Handle timeout events (backward compatibility method)
    ///
    /// 该方法保持向后兼容性，内部委托给新的统一超时管理接口。
    /// 建议使用 `check_all_timeouts` 方法替代此方法。
    ///
    /// This method maintains backward compatibility by internally delegating
    /// to the new unified timeout management interface. It's recommended to
    /// use the `check_all_timeouts` method instead of this one.
    pub async fn handle_timeout(&mut self, now: Instant) -> Result<()> {
        // 委托给新的统一超时检查方法
        // Delegate to the new unified timeout check method
        self.check_all_timeouts(now).await
    }


    fn shutdown(&mut self) {
        // 用户主动关闭，根据状态决定处理方式
        // User-initiated close, handle based on current state

        match self.lifecycle_manager.current_state() {
            crate::core::endpoint::types::state::ConnectionState::SynReceived => {
                // 在SynReceived状态下，连接还没有真正建立，立即关闭用户流接收器
                // In SynReceived state, connection is not established yet, immediately close user stream receiver
                *self.channels.tx_to_stream_mut() = None;

                // 尝试优雅关闭
                if let Ok(()) = self.begin_graceful_shutdown() {
                    if self.lifecycle_manager.should_close() {
                        self.transport.reliability_mut().clear_in_flight_packets();
                    }
                }
            }
            crate::core::endpoint::types::state::ConnectionState::Connecting => {
                if self.transport.reliability().is_send_buffer_empty() {
                    // 没有待发送数据，直接关闭
                    if let Ok(()) = self.lifecycle_manager.transition_to(crate::core::endpoint::types::state::ConnectionState::Closed) {
                        self.transport.reliability_mut().clear_in_flight_packets();
                        // 连接直接关闭时才关闭用户流
                        *self.channels.tx_to_stream_mut() = None;
                    }
                } else {
                    // 有待发送数据，转换到Closing状态，继续尝试连接
                    if let Ok(()) = self.begin_graceful_shutdown() {
                        if self.lifecycle_manager.should_close() {
                            self.transport.reliability_mut().clear_in_flight_packets();
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
                        self.transport.reliability_mut().clear_in_flight_packets();
                        *self.channels.tx_to_stream_mut() = None;
                    }
                }
            }
        }
    }
}
