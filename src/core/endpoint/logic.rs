//! The core event loop and logic for the `Endpoint`.

use super::{
    command::StreamCommand,
    frame_factory::{
        create_path_challenge_frame, create_path_response_frame, create_syn_ack_frame,
    },
    state::ConnectionState,
    ConnectionCleaner, Endpoint,
};
use crate::{
    error::{Error, Result},
    packet::{frame::Frame, sack::decode_sack_ranges},
    socket::{AsyncUdpSocket, SocketActorCommand},
};
use tokio::time::{sleep_until, Instant};
use tracing::{info, trace};

impl<S: AsyncUdpSocket> Endpoint<S> {
    /// Runs the endpoint's main event loop.
    pub async fn run(&mut self) -> Result<()> {
        let _cleaner = ConnectionCleaner::<S> {
            cid: self.local_cid,
            command_tx: self.command_tx.clone(),
            _marker: std::marker::PhantomData,
        };

        if self.state == ConnectionState::Connecting {
            self.send_initial_syn().await?;
        }

        loop {
            // In SynReceived, we don't set a timeout. We wait for the user to accept.
            let next_wakeup = if self.state == ConnectionState::SynReceived {
                Instant::now() + self.config.idle_timeout // Effectively, sleep forever until a message
            } else {
                self.reliability
                    .next_rto_deadline()
                    .unwrap_or_else(|| Instant::now() + self.config.idle_timeout)
            };

            trace!(cid = self.local_cid, state = ?self.state, "Main loop waiting for event.");
            tokio::select! {
                biased; // Prioritize incoming packets and user commands

                // 1. Handle frames from the network
                Some((frame, src_addr)) = self.receiver.recv() => {
                    self.handle_frame(frame, src_addr).await?;
                    // After handling one frame, try to drain any other pending frames
                    // to process them in a batch.
                    while let Ok((frame, src_addr)) = self.receiver.try_recv() {
                        self.handle_frame(frame, src_addr).await?;
                    }
                }

                // 2. Handle commands from the user stream
                Some(cmd) = self.rx_from_stream.recv() => {
                    self.handle_stream_command(cmd).await?;
                    // After handling one command, try to drain any other pending commands
                    // to process them in a batch. This is especially useful if the user
                    // calls `write()` multiple times in quick succession.
                    while let Ok(cmd) = self.rx_from_stream.try_recv() {
                        self.handle_stream_command(cmd).await?;
                    }
                }

                // 3. Handle timeouts
                _ = sleep_until(next_wakeup) => {
                    self.handle_timeout(Instant::now()).await?;
                }

                // 4. Stop if all channels are closed
                else => break,
            }

            // After handling all immediate events, perform follow-up actions.
            trace!(cid = self.local_cid, "Event handled, performing follow-up actions.");

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
                            self.state = ConnectionState::Closing;
                        }
                    }
                }
            }

            // 6. Packetize and send any pending user data, but only if established.
            if self.state == ConnectionState::Established
                || self.state == ConnectionState::FinWait
            {
                self.packetize_and_send().await?;
            }

            // 7. Check if we need to send a deferred EOF.
            // This happens after a FIN has been received and all data that came before
            // the FIN has been passed to the user's stream.
            if self.fin_pending_eof && self.reliability.is_recv_buffer_empty() {
                if let Some(tx) = self.tx_to_stream.take() {
                    trace!(cid = self.local_cid, "All data drained after FIN, closing user stream (sending EOF).");
                    drop(tx); // This closes the channel, signaling EOF.
                    self.fin_pending_eof = false; // Reset the flag.
                }
            }

            // 8. Check if we need to close the connection
            if self.should_close() {
                break;
            }
        }
        Ok(())
    }

    async fn handle_frame(&mut self, frame: Frame, src_addr: std::net::SocketAddr) -> Result<()> {
        trace!(local_cid = self.local_cid, ?frame, "Processing incoming frame");
        self.last_recv_time = Instant::now();

        // Check for path migration before dispatching to state handlers
        self.check_path_migration(src_addr).await?;

        // Dispatch frame handling based on current state
        match &self.state {
            ConnectionState::Connecting => self.handle_frame_connecting(frame, src_addr).await,
            ConnectionState::SynReceived => self.handle_frame_syn_received(frame, src_addr).await,
            ConnectionState::Established => self.handle_frame_established(frame, src_addr).await,
            ConnectionState::ValidatingPath { .. } => self.handle_frame_validating_path(frame, src_addr).await,
            ConnectionState::Closing => self.handle_frame_closing(frame, src_addr).await,
            ConnectionState::ClosingWait => self.handle_frame_closing_wait(frame, src_addr).await,
            ConnectionState::FinWait => self.handle_frame_fin_wait(frame, src_addr).await,
            ConnectionState::Closed => {
                // Ignore all frames in closed state
                trace!(cid = self.local_cid, "Ignoring frame in Closed state");
                Ok(())
            }
        }
    }

    async fn check_path_migration(&mut self, src_addr: std::net::SocketAddr) -> Result<()> {
        if src_addr != self.remote_addr && self.state == ConnectionState::Established {
            // Address has changed, initiate path validation.
            let challenge_data = rand::random();
            self.state = ConnectionState::ValidatingPath {
                new_addr: src_addr,
                challenge_data,
                notifier: None, // No notifier for this case, as it's a passive migration
            };
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

    async fn handle_frame_connecting(&mut self, frame: Frame, _src_addr: std::net::SocketAddr) -> Result<()> {
        match frame {
            Frame::SynAck { header } => {
                self.state = ConnectionState::Established;
                self.peer_cid = header.source_cid;
                info!(cid = self.local_cid, "Connection established (client-side)");

                // Acknowledge the SYN-ACK, potentially with piggybacked data if the
                // user has already called write().
                if !self.reliability.is_send_buffer_empty() {
                    self.packetize_and_send().await?;
                } else {
                    self.send_standalone_ack().await?;
                }
            }
            Frame::PathChallenge { header, challenge_data } => {
                self.handle_path_challenge(header, challenge_data, _src_addr).await?;
            }
            _ => {
                trace!(cid = self.local_cid, ?frame, "Ignoring unexpected frame in Connecting state");
            }
        }
        Ok(())
    }

    async fn handle_frame_syn_received(&mut self, frame: Frame, src_addr: std::net::SocketAddr) -> Result<()> {
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
                self.reliability.receive_push(header.sequence_number, payload);
                // For 0-RTT PUSH frames received during the `SynReceived` state, the ACK
                // will be piggybacked onto the eventual SYN-ACK.
            }
            Frame::Ack { header, payload } => {
                self.handle_ack_frame(header, payload).await?;
            }
            Frame::Fin { header, .. } => {
                // Handle FIN even in SynReceived state - this can happen with 0-RTT
                // where client sends data and immediately closes
                self.handle_fin_frame(header.sequence_number).await?;
                self.state = ConnectionState::FinWait;
            }
            Frame::PathChallenge { header, challenge_data } => {
                self.handle_path_challenge(header, challenge_data, src_addr).await?;
            }
            _ => {
                trace!(cid = self.local_cid, ?frame, "Ignoring unexpected frame in SynReceived state");
            }
        }
        Ok(())
    }

    async fn handle_frame_established(&mut self, frame: Frame, src_addr: std::net::SocketAddr) -> Result<()> {
        match frame {
            Frame::Push { header, payload } => {
                self.reliability.receive_push(header.sequence_number, payload);
                self.send_standalone_ack().await?;
            }
            Frame::Ack { header, payload } => {
                self.handle_ack_frame(header, payload).await?;
            }
            Frame::Fin { header, .. } => {
                self.handle_fin_frame(header.sequence_number).await?;
                self.state = ConnectionState::FinWait;
            }
            Frame::PathChallenge { header, challenge_data } => {
                self.handle_path_challenge(header, challenge_data, src_addr).await?;
            }
            _ => {
                trace!(cid = self.local_cid, ?frame, "Ignoring unexpected frame in Established state");
            }
        }
        Ok(())
    }

    async fn handle_frame_validating_path(&mut self, frame: Frame, src_addr: std::net::SocketAddr) -> Result<()> {
        match frame {
            Frame::PathResponse { header: _, challenge_data } => {
                if let ConnectionState::ValidatingPath {
                    new_addr,
                    challenge_data: expected_challenge,
                    notifier,
                } = self.state.clone()
                {
                    if src_addr == new_addr && challenge_data == expected_challenge {
                        // Path validation successful!
                        info!(cid = self.local_cid, old_addr = %self.remote_addr, new_addr = %new_addr, "Path validation successful, migrating connection.");
                        let _old_addr = self.remote_addr;
                        self.remote_addr = new_addr;
                        self.state = ConnectionState::Established;

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
                        info!(cid = self.local_cid, "Received invalid PathResponse, ignoring.");
                    }
                }
            }
            Frame::Push { header, payload } => {
                // Continue processing data frames during path validation
                self.reliability.receive_push(header.sequence_number, payload);
                self.send_standalone_ack().await?;
            }
            Frame::Ack { header, payload } => {
                self.handle_ack_frame(header, payload).await?;
            }
            Frame::Fin { header, .. } => {
                self.handle_fin_frame(header.sequence_number).await?;
                self.state = ConnectionState::FinWait;
            }
            Frame::PathChallenge { header, challenge_data } => {
                self.handle_path_challenge(header, challenge_data, src_addr).await?;
            }
            _ => {
                trace!(cid = self.local_cid, ?frame, "Ignoring unexpected frame in ValidatingPath state");
            }
        }
        Ok(())
    }

    async fn handle_frame_closing(&mut self, frame: Frame, src_addr: std::net::SocketAddr) -> Result<()> {
        match frame {
            Frame::Push { header, payload } => {
                // It's possible to receive data after we've decided to close,
                // as the peer might have sent it before receiving our FIN.
                self.reliability.receive_push(header.sequence_number, payload);
                self.send_standalone_ack().await?;
            }
            Frame::Ack { header, payload } => {
                self.handle_ack_frame(header, payload).await?;
            }
            Frame::Fin { header, .. } => {
                self.handle_fin_frame(header.sequence_number).await?;
                self.state = ConnectionState::ClosingWait;
            }
            Frame::PathChallenge { header, challenge_data } => {
                self.handle_path_challenge(header, challenge_data, src_addr).await?;
            }
            _ => {
                trace!(cid = self.local_cid, ?frame, "Ignoring unexpected frame in Closing state");
            }
        }
        Ok(())
    }

    async fn handle_frame_closing_wait(&mut self, frame: Frame, src_addr: std::net::SocketAddr) -> Result<()> {
        match frame {
            Frame::Ack { header, payload } => {
                self.handle_ack_frame(header, payload).await?;
            }
            Frame::PathChallenge { header, challenge_data } => {
                self.handle_path_challenge(header, challenge_data, src_addr).await?;
            }
            _ => {
                trace!(cid = self.local_cid, ?frame, "Ignoring unexpected frame in ClosingWait state");
            }
        }
        Ok(())
    }

    async fn handle_frame_fin_wait(&mut self, frame: Frame, src_addr: std::net::SocketAddr) -> Result<()> {
        match frame {
            Frame::Push { header, payload } => {
                self.reliability.receive_push(header.sequence_number, payload);
                self.send_standalone_ack().await?;
            }
            Frame::Ack { header, payload } => {
                self.handle_ack_frame(header, payload).await?;
            }
            Frame::Fin { header, .. } => {
                // This is a retransmitted FIN from the peer. Just acknowledge it again.
                // Do NOT change state here. The state should only transition to `Closing`
                // when the local application calls `shutdown()`.
                self.handle_fin_frame(header.sequence_number).await?;
            }
            Frame::PathChallenge { header, challenge_data } => {
                self.handle_path_challenge(header, challenge_data, src_addr).await?;
            }
            _ => {
                trace!(cid = self.local_cid, ?frame, "Ignoring unexpected frame in FinWait state");
            }
        }
        Ok(())
    }

    // Helper methods for common frame handling logic
    async fn handle_ack_frame(&mut self, header: crate::packet::header::ShortHeader, payload: bytes::Bytes) -> Result<()> {
        self.peer_recv_window = header.recv_window_size as u32;
        let sack_ranges = decode_sack_ranges(payload);
        let frames_to_retx = self.reliability.handle_ack(
            header.recv_next_sequence,
            sack_ranges,
            Instant::now(),
        );
        if !frames_to_retx.is_empty() {
            self.send_frames(frames_to_retx).await?;
        }
        Ok(())
    }

    async fn handle_fin_frame(&mut self, sequence_number: u32) -> Result<()> {
        // The reliability layer will now handle the FIN and its sequence number.
        // The reassembly process will later tell us when this FIN is "seen".
        self.reliability.receive_fin(sequence_number);
        self.send_standalone_ack().await?;
        Ok(())
    }

    async fn handle_path_challenge(&mut self, header: crate::packet::header::ShortHeader, challenge_data: u64, src_addr: std::net::SocketAddr) -> Result<()> {
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

    async fn handle_stream_command(&mut self, cmd: StreamCommand) -> Result<()> {
        match cmd {
            StreamCommand::SendData(data) => {
                // If this is the first data sent on a server-side connection,
                // it triggers the SYN-ACK and establishes the connection. This is our 0-RTT path.
                if self.state == ConnectionState::SynReceived {
                    info!(cid = self.local_cid, "Connection accepted by user, preparing 0-RTT SYN-ACK with data.");
                    self.state = ConnectionState::Established;

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
                // Immediately attempt to send a FIN packet.
                self.packetize_and_send().await?;
            }
            StreamCommand::Migrate { new_addr, notifier } => {
                if self.state != ConnectionState::Established {
                    let _ = notifier.send(Err(Error::NotConnected));
                    return Ok(());
                }

                info!(cid = self.local_cid, new_addr = %new_addr, "Actively migrating to new address.");
                let challenge_data = rand::random();
                self.state = ConnectionState::ValidatingPath {
                    new_addr,
                    challenge_data,
                    notifier: Some(notifier),
                };

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

    async fn handle_timeout(&mut self, now: Instant) -> Result<()> {
        let frames_to_resend = self.reliability.check_for_retransmissions(now);
        if !frames_to_resend.is_empty() {
            self.send_frames(frames_to_resend).await?;
        }

        // Check for path validation timeout
        if let ConnectionState::ValidatingPath { notifier, .. } = &mut self.state {
            if now.saturating_duration_since(self.last_recv_time) > self.config.idle_timeout {
                info!(cid = self.local_cid, "Path validation timed out.");
                if let Some(notifier) = notifier.take() {
                    let _ = notifier.send(Err(Error::PathValidationTimeout));
                }
                self.state = ConnectionState::Established; // Revert to established
            }
        }

        if now.saturating_duration_since(self.last_recv_time) > self.config.idle_timeout {
            info!(cid = self.local_cid, "Connection timed out");
            self.state = ConnectionState::Closed;
            return Err(Error::ConnectionTimeout);
        }
        Ok(())
    }

    fn should_close(&mut self) -> bool {
        // Condition 1: We are in a closing state AND all in-flight data is ACKed.
        // This is the primary condition to transition into the `Closed` state.
        if (self.state == ConnectionState::Closing
            || self.state == ConnectionState::ClosingWait)
            && self.reliability.is_in_flight_empty()
        {
            info!(cid = self.local_cid, "All data ACKed, entering Closed state.");
            self.state = ConnectionState::Closed;
            self.reliability.clear_in_flight_packets(); // Clean up here
        }

        // The endpoint's run loop should terminate ONLY when the state is definitively `Closed`.
        // The previous logic for `FinWait` was causing premature termination. The endpoint
        // should wait in `FinWait` for the user to actively close the stream.
        self.state == ConnectionState::Closed
    }

    fn shutdown(&mut self) {
        match self.state {
            // Standard active close from Established.
            ConnectionState::Established | ConnectionState::FinWait => {
                self.state = ConnectionState::Closing;
            }
            // If the user closes a connecting stream that already has data buffered,
            // we must proceed to a full graceful close to ensure the data is sent.
            ConnectionState::Connecting if !self.reliability.is_send_buffer_empty() => {
                info!(cid = self.local_cid, "Close requested on connecting stream with pending data; proceeding to graceful shutdown.");
                self.state = ConnectionState::Closing;
            }
            // If already closing, do nothing.
            _ if self.state == ConnectionState::Closing
                || self.state == ConnectionState::ClosingWait
                || self.state == ConnectionState::Closed => {}
            // For any other non-established state, abort immediately.
            _ => {
                info!(
                    cid = self.local_cid,
                    state = ?self.state,
                    "Connection closed by user during non-established state, aborting."
                );
                self.state = ConnectionState::Closed;
                self.reliability.clear_in_flight_packets(); // Also clean up here for aborts
            }
        }
    }
} 