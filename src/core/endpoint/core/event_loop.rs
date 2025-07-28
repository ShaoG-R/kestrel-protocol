

use crate::core::endpoint::{
    ConnectionCleaner,
    Endpoint,
};
use crate::{
    error::{Error, Result},
    packet::{frame::Frame, sack::decode_sack_ranges},
    socket::{AsyncUdpSocket, SocketActorCommand},
};
use tokio::time::{sleep_until, Instant};
use tracing::{info, trace};
use crate::core::endpoint::core::frame::{
    create_path_challenge_frame, create_path_response_frame, create_syn_ack_frame,
};
use crate::core::endpoint::lifecycle::manager::ConnectionLifecycleManager;
use crate::core::endpoint::processing::dispatcher::EventDispatcher;
use crate::core::endpoint::types::command::StreamCommand;
use crate::core::endpoint::types::state::ConnectionState;

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
}