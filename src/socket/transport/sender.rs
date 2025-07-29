//! Transport sender task for batched frame transmission.
//!
//! 用于批量帧传输的传输发送任务。

use super::{command::TransportCommand, Transport};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error};

/// The dedicated task for sending frames through the transport.
/// This centralizes all transport write operations and provides batching.
///
/// 通过传输发送帧的专用任务。
/// 这集中了所有传输写入操作并提供批处理。
pub async fn transport_sender_task<T: Transport>(
    mut transport: Arc<T>,
    mut rx: mpsc::Receiver<TransportCommand<T>>,
) {
    const MAX_BATCH_SIZE: usize = 64;
    let mut commands = Vec::with_capacity(MAX_BATCH_SIZE);

    loop {
        // Wait for the first command to arrive
        let first_cmd = match rx.recv().await {
            Some(cmd) => cmd,
            None => return, // Channel closed
        };

        match first_cmd {
            TransportCommand::Send(batch) => {
                commands.push(batch);

                // Try to drain the channel of any pending Send commands to process in a batch
                while commands.len() < MAX_BATCH_SIZE {
                    if let Ok(TransportCommand::Send(batch)) = rx.try_recv() {
                        commands.push(batch);
                    } else {
                        break;
                    }
                }

                // Process all batched send commands
                for batch in commands.drain(..) {
                    debug!(
                        addr = %batch.remote_addr,
                        frame_count = batch.frames.len(),
                        "transport_sender_task sending frame batch"
                    );

                    if let Err(e) = transport.send_frames(batch).await {
                        error!("Failed to send frame batch: {}", e);
                    }
                }
            }
            TransportCommand::SwapTransport(new_transport) => {
                debug!("Transport sender task is swapping to a new transport");
                transport = new_transport;
            }
        }
    }
}