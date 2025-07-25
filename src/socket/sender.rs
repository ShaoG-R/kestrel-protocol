//! The dedicated task for sending UDP packets.

use super::{command::SenderTaskCommand, traits::AsyncUdpSocket};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

/// The dedicated task for sending UDP packets.
/// This centralizes all writes to the socket.
///
/// 用于发送UDP包的专用任务。
/// 这将所有对套接字的写入操作集中起来。
pub(crate) async fn sender_task<S: AsyncUdpSocket>(
    mut socket: Arc<S>,
    mut rx: mpsc::Receiver<SenderTaskCommand<S>>,
) {
    const MAX_BATCH_SIZE: usize = 64;
    let mut send_buf = Vec::with_capacity(2048);
    let mut commands = Vec::with_capacity(MAX_BATCH_SIZE);

    loop {
        // Wait for the first command to arrive.
        let first_cmd = match rx.recv().await {
            Some(cmd) => cmd,
            None => return, // Channel closed.
        };

        match first_cmd {
            SenderTaskCommand::Send(send_cmd) => {
                commands.push(send_cmd);

                // Try to drain the channel of any pending Send commands to process in a batch.
                while commands.len() < MAX_BATCH_SIZE {
                    if let Ok(SenderTaskCommand::Send(cmd)) = rx.try_recv() {
                        commands.push(cmd);
                    } else {
                        break;
                    }
                }

                for cmd in commands.drain(..) {
                    send_buf.clear();
                    // This is "packet coalescing" at the sender level.
                    for frame in cmd.frames {
                        frame.encode(&mut send_buf);
                    }

                    if send_buf.is_empty() {
                        continue;
                    }
                    debug!(
                        addr = %cmd.remote_addr,
                        bytes = send_buf.len(),
                        "sender_task sending datagram"
                    );

                    if let Err(e) = socket.send_to(&send_buf, cmd.remote_addr).await {
                        error!(addr = %cmd.remote_addr, "Failed to send packet: {}", e);
                    }
                }
            }
            SenderTaskCommand::SwapSocket(new_socket) => {
                info!("Sender task is swapping to a new socket.");
                socket = new_socket;
            }
        }
    }
} 