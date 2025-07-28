//! The core event loop and logic for the `Endpoint`.

use super::{
    ConnectionCleaner, Endpoint,
    command::StreamCommand,
    event_dispatcher::EventDispatcher,
    frame_factory::{
        create_path_challenge_frame, create_path_response_frame, create_syn_ack_frame,
    },
    lifecycle_manager::ConnectionLifecycleManager,
    state::ConnectionState,
};
use crate::{
    error::{Error, Result},
    packet::{frame::Frame, sack::decode_sack_ranges},
    socket::{AsyncUdpSocket, SocketActorCommand},
};
use tokio::time::{Instant, sleep_until};
use tracing::{info, trace};

impl<S: AsyncUdpSocket> Endpoint<S> {
    /// Runs the endpoint's main event loop.
    pub async fn run(&mut self) -> Result<()> {
        let _cleaner = ConnectionCleaner::<S> {
            cid: self.local_cid,
            command_tx: self.command_tx.clone(),
            _marker: std::marker::PhantomData,
        };

        if *self.lifecycle_manager.current_state() == ConnectionState::Connecting {
            self.send_initial_syn().await?;
        }

        loop {
            // In SynReceived, we don't set a timeout. We wait for the user to accept.
            let next_wakeup = if *self.lifecycle_manager.current_state() == ConnectionState::SynReceived
            {
                Instant::now() + self.config.connection.idle_timeout // Effectively, sleep forever until a message
            } else {
                self.reliability
                    .next_rto_deadline()
                    .unwrap_or_else(|| Instant::now() + self.config.connection.idle_timeout)
            };

            trace!(cid = self.local_cid, state = ?self.lifecycle_manager.current_state(), "Main loop waiting for event.");
            tokio::select! {
                biased; // Prioritize incoming packets and user commands

                // 1. Handle frames from the network
                Some((frame, src_addr)) = self.receiver.recv() => {
                    EventDispatcher::dispatch_frame(self, frame, src_addr).await?;
                    // After handling one frame, try to drain any other pending frames
                    // to process them in a batch.
                    while let Ok((frame, src_addr)) = self.receiver.try_recv() {
                        EventDispatcher::dispatch_frame(self, frame, src_addr).await?;
                    }
                }

                // 2. Handle commands from the user stream
                Some(cmd) = self.rx_from_stream.recv() => {
                    EventDispatcher::dispatch_stream_command(self, cmd).await?;
                    // After handling one command, try to drain any other pending commands
                    // to process them in a batch. This is especially useful if the user
                    // calls `write()` multiple times in quick succession.
                    while let Ok(cmd) = self.rx_from_stream.try_recv() {
                        EventDispatcher::dispatch_stream_command(self, cmd).await?;
                    }
                }

                // 3. Handle timeouts
                _ = sleep_until(next_wakeup) => {
                    EventDispatcher::dispatch_timeout(self, Instant::now()).await?;
                }

                // 4. Stop if all channels are closed
                else => break,
            }

            // After handling all immediate events, perform follow-up actions.
            trace!(
                cid = self.local_cid,
                "Event handled, performing follow-up actions."
            );

            // 5. Reassemble data and send to the user stream
            let (data_to_send, fin_seen) = self.reliability.reassemble();
            if fin_seen {
                // The FIN has been reached in the byte stream. No more data will arrive.
                // We can now set the flag to schedule the EOF signal for the user stream.
                self.fin_pending_eof = true;
            }
            if let Some(data_vec) = data_to_send {
                if !data_vec.is_empty() {
                    trace!(
                        cid = self.local_cid,
                        count = data_vec.len(),
                        "Reassembled data, sending to stream."
                    );
                    if let Some(tx) = self.tx_to_stream.as_ref() {
                        if tx.send(data_vec).await.is_err() {
                            // User's stream handle has been dropped. We can no longer send.
                            self.tx_to_stream = None;
                            let _ = self.transition_state(ConnectionState::Closing);
                        }
                    }
                }
            }

            // 6. After reassembly, check if a FIN was processed and transition state accordingly.
            // This is the single source of truth for moving to the FinWait state.
            if fin_seen {
                // 基于当前状态决定FIN后的状态转换 - 使用生命周期管理器
                // Determine FIN transition based on current state - using lifecycle manager
                match self.lifecycle_manager.current_state() {
                    ConnectionState::Established => {
                        let _ = self.transition_state(ConnectionState::FinWait);
                    }
                    ConnectionState::Closing => {
                        let _ = self.transition_state(ConnectionState::ClosingWait);
                    }
                    ConnectionState::SynReceived => {
                        // 在0-RTT场景中，从SynReceived状态转换到FinWait
                        // In 0-RTT scenario, transition from SynReceived to FinWait
                        let _ = self.transition_state(ConnectionState::FinWait);
                    }
                    _ => {
                        // 其他状态下忽略FIN
                        // Ignore FIN in other states
                    }
                }
            }

            // 7. Packetize and send any pending user data, but only if established.
            let current_state = self.lifecycle_manager.current_state();
            if *current_state == ConnectionState::Established
                || *current_state == ConnectionState::FinWait
                || *current_state == ConnectionState::Closing
            {
                self.packetize_and_send().await?;
            }

            // 8. Check if we need to send a deferred EOF.
            // This happens after a FIN has been received and all data that came before
            // the FIN has been passed to the user's stream.
            if self.fin_pending_eof && self.reliability.is_recv_buffer_empty() {
                if let Some(tx) = self.tx_to_stream.take() {
                    trace!(
                        cid = self.local_cid,
                        "All data drained after FIN, closing user stream (sending EOF)."
                    );
                    drop(tx); // This closes the channel, signaling EOF.
                    self.fin_pending_eof = false; // Reset the flag.
                }
            }

            // 9. Check if we need to close the connection
            if self.should_close() {
                break;
            }
        }
        Ok(())
    }

    // handle_frame 方法已被 EventDispatcher 替代
    // handle_frame method has been replaced by EventDispatcher

    pub async fn check_path_migration(&mut self, src_addr: std::net::SocketAddr) -> Result<()> {
        if src_addr != self.remote_addr
            && *self.lifecycle_manager.current_state() == ConnectionState::Established
        {
            // Address has changed, initiate path validation.
            let challenge_data = rand::random();
            let (tx, _rx) = tokio::sync::oneshot::channel();
            self.start_path_validation(src_addr, challenge_data, tx)?;
            let challenge_frame = create_path_challenge_frame(
                self.peer_cid,
                self.reliability.next_sequence_number(),
                self.start_time,
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
                self.peer_cid = header.source_cid;

                // Acknowledge the SYN-ACK, potentially with piggybacked data if the
                // user has already called write().
                if !self.reliability.is_send_buffer_empty() {
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
                    cid = self.local_cid,
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
                info!(cid = self.local_cid, "Received duplicate SYN, ignoring.");
                // If we have already been triggered to send a SYN-ACK (i.e., data is in the
                // send buffer), we can resend it.
                if !self.reliability.is_send_buffer_empty() {
                    self.send_syn_ack().await?;
                }
            }
            Frame::Push { header, payload } => {
                // For 0-RTT PUSH frames received during the `SynReceived` state, the ACK
                // will be piggybacked onto the eventual SYN-ACK.
                self.reliability
                    .receive_push(header.sequence_number, payload);
            }
            Frame::Ack { header, payload } => {
                self.handle_ack_frame(header, payload).await?;
            }
            Frame::Fin { header, .. } => {
                // Handle FIN even in SynReceived state - this can happen with 0-RTT
                // where client sends data and immediately closes
                if self.reliability.receive_fin(header.sequence_number) {
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
                    cid = self.local_cid,
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
                    .reliability
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
                if self.reliability.receive_fin(header.sequence_number) {
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
                    cid = self.local_cid,
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
                            cid = self.local_cid,
                            old_addr = %self.remote_addr,
                            new_addr = %new_addr,
                            "Path validation successful, updating remote address"
                        );
                        self.complete_path_validation(true)?;
                        self.remote_addr = new_addr;

                        // Notify the caller of migrate() if there is one
                        if let Some(notifier) = notifier {
                            let _ = notifier.send(Ok(()));
                        }

                        // Notify ReliableUdpSocket to update the addr_to_cid map.
                        let _ = self
                            .command_tx
                            .send(SocketActorCommand::UpdateAddr {
                                cid: self.local_cid,
                                new_addr,
                            })
                            .await;
                    } else {
                        // Invalid path response, ignore it.
                        info!(
                            cid = self.local_cid,
                            "Received invalid PathResponse, ignoring."
                        );
                    }
                }
            }
            Frame::Push { header, payload } => {
                // Continue processing data frames during path validation
                if self
                    .reliability
                    .receive_push(header.sequence_number, payload)
                {
                    self.send_standalone_ack().await?;
                }
            }
            Frame::Ack { header, payload } => {
                self.handle_ack_frame(header, payload).await?;
            }
            Frame::Fin { header, .. } => {
                if self.reliability.receive_fin(header.sequence_number) {
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
                    cid = self.local_cid,
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
                    .reliability
                    .receive_push(header.sequence_number, payload)
                {
                    self.send_standalone_ack().await?;
                }
            }
            Frame::Ack { header, payload } => {
                self.handle_ack_frame(header, payload).await?;
            }
            Frame::Fin { header, .. } => {
                if self.reliability.receive_fin(header.sequence_number) {
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
                    cid = self.local_cid,
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
                    cid = self.local_cid,
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
                    .reliability
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
                if self.reliability.receive_fin(header.sequence_number) {
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
                    cid = self.local_cid,
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
        self.peer_recv_window = header.recv_window_size as u32;
        let sack_ranges = decode_sack_ranges(payload);
        let frames_to_retx =
            self.reliability
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
            self.peer_cid,
            header.sequence_number, // Echo the sequence number
            self.start_time,
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
                    self.reliability.write_to_stream(data);

                    // 2. Create the payload-less SYN-ACK frame.
                    let syn_ack_frame =
                        create_syn_ack_frame(&self.config, self.peer_cid, self.local_cid);

                    // 3. Packetize the stream data into PUSH frames. This will correctly
                    //    assign sequence numbers starting from 0.
                    let now = Instant::now();
                    let frames_to_send = self.reliability.packetize_stream_data(
                        self.peer_cid,
                        self.peer_recv_window,
                        now,
                        self.start_time,
                        Some(syn_ack_frame),
                    );

                    // 4. Prepend the SYN-ACK frame to coalesce it with the PUSH frames.
                    // frames_to_send.insert(0, syn_ack_frame);

                    // 5. Send them all in one go.
                    self.send_frames(frames_to_send).await?;
                } else {
                    self.reliability.write_to_stream(data);
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
                info!(cid = self.local_cid, new_addr = %new_addr, "Actively migrating to new address.");
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
                    self.peer_cid,
                    self.reliability.next_sequence_number(),
                    self.start_time,
                    challenge_data,
                );
                self.send_frame_to(challenge_frame, new_addr).await?;
            }
            #[cfg(test)]
            StreamCommand::UpdatePeerCid(peer_cid) => {
                // This is a test-only command to simulate the SocketActor's role.
                self.peer_cid = peer_cid;
            }
        }
        Ok(())
    }

    pub async fn handle_timeout(&mut self, now: Instant) -> Result<()> {
        let frames_to_resend = self.reliability.check_for_retransmissions(now);
        if !frames_to_resend.is_empty() {
            self.send_frames(frames_to_resend).await?;
        }

        // Check for path validation timeout - only if still in ValidatingPath state
        // and not already closing
        if matches!(
            self.lifecycle_manager.current_state(),
            ConnectionState::ValidatingPath { .. }
        ) {
            if now.saturating_duration_since(self.last_recv_time)
                > self.config.connection.idle_timeout
            {
                if let ConnectionState::ValidatingPath { notifier, .. } =
                    self.lifecycle_manager.current_state().clone()
                {
                    if let Some(notifier) = notifier {
                        let _ = notifier.send(Err(Error::PathValidationTimeout));
                    }
                    // 路径验证超时，回到Established状态 - 使用生命周期管理器
                    // Path validation timeout, return to Established state - using lifecycle manager
                    self.transition_state(ConnectionState::Established)?;
                }
            }
        }

        if now.saturating_duration_since(self.last_recv_time) > self.config.connection.idle_timeout
        {
            // 连接超时，强制关闭 - 使用生命周期管理器
            // Connection timeout, force close - using lifecycle manager
            self.lifecycle_manager.force_close()?;
            return Err(Error::ConnectionTimeout);
        }
        Ok(())
    }

    fn should_close(&mut self) -> bool {
        // Condition 1: We are in a closing state AND all in-flight data is ACKed.
        // This is the primary condition to transition into the `Closed` state.
        let current_state = self.lifecycle_manager.current_state();
        if (*current_state == ConnectionState::Closing
            || *current_state == ConnectionState::ClosingWait)
            && self.reliability.is_in_flight_empty()
        {
            // 所有数据已确认，转换到Closed状态 - 使用生命周期管理器
            // All data acknowledged, transition to Closed state - using lifecycle manager
            if let Ok(()) = self.transition_state(ConnectionState::Closed) {
                self.reliability.clear_in_flight_packets(); // Clean up here
                // 连接关闭时，关闭用户流接收器
                // Close user stream receiver when connection closes
                self.tx_to_stream = None;
                return true;
            }
        }

        // The endpoint's run loop should terminate ONLY when the state is definitively `Closed`.
        // The previous logic for `FinWait` was causing premature termination. The endpoint
        // should wait in `FinWait` for the user to actively close the stream.
        self.lifecycle_manager.should_close()
    }

    fn shutdown(&mut self) {
        // 用户主动关闭，根据状态决定处理方式
        // User-initiated close, handle based on current state
        
        match self.lifecycle_manager.current_state() {
            crate::core::endpoint::state::ConnectionState::SynReceived => {
                // 在SynReceived状态下，连接还没有真正建立，立即关闭用户流接收器
                // In SynReceived state, connection is not established yet, immediately close user stream receiver
                self.tx_to_stream = None;
                
                // 尝试优雅关闭
                if let Ok(()) = self.begin_graceful_shutdown() {
                    if self.lifecycle_manager.should_close() {
                        self.reliability.clear_in_flight_packets();
                    }
                }
            }
            crate::core::endpoint::state::ConnectionState::Connecting => {
                if self.reliability.is_send_buffer_empty() {
                    // 没有待发送数据，直接关闭
                    if let Ok(()) = self.lifecycle_manager.transition_to(crate::core::endpoint::state::ConnectionState::Closed) {
                        self.reliability.clear_in_flight_packets();
                        // 连接直接关闭时才关闭用户流
                        self.tx_to_stream = None;
                    }
                } else {
                    // 有待发送数据，转换到Closing状态，继续尝试连接
                    if let Ok(()) = self.begin_graceful_shutdown() {
                        if self.lifecycle_manager.should_close() {
                            self.reliability.clear_in_flight_packets();
                            self.tx_to_stream = None;
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
                        self.reliability.clear_in_flight_packets();
                        self.tx_to_stream = None;
                    }
                }
            }
        }
    }
}
