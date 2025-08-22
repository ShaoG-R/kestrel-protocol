use crate::core::endpoint::lifecycle::ConnectionLifecycleManager;
use crate::core::endpoint::processing::dispatcher::EventDispatcher;
use crate::core::endpoint::processing::traits::ProcessorOperations;
use crate::core::endpoint::types::state::ConnectionState;
use crate::core::endpoint::{ConnectionCleaner, Endpoint};
use crate::{error::Result, socket::Transport};
use tokio::time::Instant;
use tracing::trace;

impl<T: Transport> Endpoint<T> {
    /// Runs the endpoint's main event loop.
    pub async fn run(&mut self) -> Result<()> {
        let _cleaner = ConnectionCleaner::<T> {
            cid: self.identity.local_cid(),
            command_tx: self.channels.command_tx().clone(),
            _marker: std::marker::PhantomData,
        };

        if *self.lifecycle_manager.current_state() == ConnectionState::Connecting {
            self.send_initial_syn().await?;
        }

        loop {
            trace!(cid = self.identity.local_cid(), state = ?self.lifecycle_manager.current_state(), "Event-driven loop waiting for events.");
            tokio::select! {
                biased; // Prioritize incoming packets and user commands

                // 1. Handle frames from the network
                Some((frame, src_addr)) = self.channels.receiver.recv() => {
                    // 使用统一时间管理更新接收时间
                    // Update receive time using unified time management
                    let now = Instant::now();
                    self.timing.on_packet_received(now);

                    EventDispatcher::dispatch_frame::<T>(self as &mut dyn ProcessorOperations, frame, src_addr).await?;
                    // After handling one frame, try to drain any other pending frames
                    // to process them in a batch.
                    while let Ok((frame, src_addr)) = self.channels.receiver.try_recv() {
                        // 为批量处理的帧也更新接收时间 - 共享时间戳以提高性能
                        // Update receive time for batched frames too - share timestamp for better performance
                        self.timing.on_packet_received(now);
                        EventDispatcher::dispatch_frame::<T>(self as &mut dyn ProcessorOperations, frame, src_addr).await?;
                    }
                }

                // 2. Handle commands from the user stream
                Some(cmd) = self.channels.rx_from_stream.recv() => {
                    EventDispatcher::dispatch_stream_command(self, cmd).await?;
                    // After handling one command, try to drain any other pending commands
                    // to process them in a batch. This is especially useful if the user
                    // calls `write()` multiple times in quick succession.
                    while let Ok(cmd) = self.channels.rx_from_stream_mut().try_recv() {
                        EventDispatcher::dispatch_stream_command(self, cmd).await?;
                    }
                }

                // 3. 处理定时器事件 - 事件驱动架构的核心
                // Handle timer events - core of event-driven architecture
                Some(timer_event_data) = self.channels.timer_event_rx.recv() => {
                    let timeout_event = timer_event_data.timeout_event;
                    trace!(cid = self.identity.local_cid(), timeout_event = ?timeout_event, "Received timer event");

                    // 处理定时器事件
                    self.handle_timeout_event(timeout_event).await?;
                }

                // 4. Stop if all channels are closed
                else => break,
            }

            // After handling all immediate events, perform follow-up actions.
            trace!(
                cid = self.identity.local_cid(),
                "Event handled, performing follow-up actions."
            );

            // 5. Reassemble data and send to the user stream
            let reassembly_result = self.transport.unified_reliability_mut().reassemble_data();
            let (data_to_send, fin_seen) = (reassembly_result.data, reassembly_result.fin_seen);
            if fin_seen {
                // The FIN has been reached in the byte stream. No more data will arrive.
                // We can now set the flag to schedule the EOF signal for the user stream.
                self.timing.set_fin_pending_eof(true).await;
            }
            if let Some(data_vec) = data_to_send {
                if !data_vec.is_empty() {
                    trace!(
                        cid = self.identity.local_cid(),
                        count = data_vec.len(),
                        "Reassembled data, sending to stream."
                    );
                    if let Some(tx) = self.channels.tx_to_stream().as_ref() {
                        if tx.send(data_vec).await.is_err() {
                            // User's stream handle has been dropped. We can no longer send.
                            // But we should not immediately close the connection - let it finish
                            // processing any remaining network events first.
                            // 用户的流句柄已被丢弃，我们无法再发送数据。
                            // 但我们不应该立即关闭连接 - 让它先完成处理任何剩余的网络事件。
                            trace!(
                                cid = self.identity.local_cid(),
                                "User stream handle dropped, marking for cleanup"
                            );
                            *self.channels.tx_to_stream_mut() = None;
                            // Note: We don't transition to Closing here to avoid race conditions
                            // The connection will be cleaned up when appropriate
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

            // 8. FIN pending EOF check is now handled by timer-driven architecture
            // The check for sending deferred EOF is now managed by FinProcessingTimeout
            // instead of polling in every event loop iteration

            // 9. Check if we need to close the connection
            if self.should_close().await {
                break;
            }
        }
        Ok(())
    }

    async fn should_close(&mut self) -> bool {
        // 仅当进入 ClosingWait（本端FIN已确认且已收到对端FIN）且在途数据为空时，才真正关闭连接。
        // Only transition to `Closed` when we're in ClosingWait (local FIN acked and peer FIN received)
        // and there are no in-flight packets remaining.
        let current_state = self.lifecycle_manager.current_state();
        if *current_state == ConnectionState::ClosingWait
            && self.transport.unified_reliability().is_in_flight_empty()
        {
            if let Ok(()) = self.transition_state(ConnectionState::Closed) {
                self.transport
                    .unified_reliability_mut()
                    .cleanup_all_retransmission_timers()
                    .await;
                // 连接关闭时，关闭用户流接收器（发送EOF）。
                // Close user stream receiver (send EOF) when connection is fully closed.
                *self.channels.tx_to_stream_mut() = None;
                return true;
            }
        }

        // The endpoint's run loop should terminate ONLY when the state is definitively `Closed`.
        // The previous logic for `FinWait` was causing premature termination. The endpoint
        // should wait in `FinWait` for the user to actively close the stream.
        self.lifecycle_manager.should_close()
    }
}
