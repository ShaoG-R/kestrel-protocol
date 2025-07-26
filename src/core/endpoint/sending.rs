//! Sending-related logic for the `Endpoint`.

use super::{
    frame_factory::{create_ack_frame, create_fin_frame, create_syn_ack_frame, create_syn_frame},
    Endpoint,
};
use crate::{
    error::{Error, Result},
    packet::frame::Frame,
    socket::{AsyncUdpSocket, SendCommand, SenderTaskCommand},
};
use std::net::SocketAddr;
use tokio::time::Instant;

impl<S: AsyncUdpSocket> Endpoint<S> {
    pub(super) async fn packetize_and_send(&mut self) -> Result<()> {
        let now = Instant::now();
        // Packetize any buffered data.
        let mut frames = self.reliability.packetize_stream_data(
            self.peer_cid,
            self.peer_recv_window,
            now,
            self.start_time,
        );

        if self.state == super::state::ConnectionState::Closing
            && !self.reliability.has_fin_in_flight()
        {
            let fin_frame = create_fin_frame(
                self.peer_cid,
                self.reliability.next_sequence_number(),
                self.start_time,
            );
            self.reliability
                .add_fin_to_in_flight(fin_frame.clone(), Instant::now());
            frames.push(fin_frame);
        }

        if !frames.is_empty() {
            self.send_frames(frames).await?;
        }
        Ok(())
    }

    pub(super) async fn send_initial_syn(&mut self) -> Result<()> {
        let initial_payload = self.reliability.take_stream_buffer();
        let frame = create_syn_frame(&self.config, self.local_cid, initial_payload);
        self.send_frames(vec![frame]).await
    }

    pub(super) async fn send_syn_ack(&mut self) -> Result<()> {
        let payload = self.reliability.take_stream_buffer();
        let frame = create_syn_ack_frame(&self.config, self.peer_cid, self.local_cid, payload);
        self.send_frames(vec![frame]).await
    }

    pub(super) async fn send_standalone_ack(&mut self) -> Result<()> {
        if !self.reliability.is_ack_pending() {
            return Ok(());
        }
        let frame = create_ack_frame(self.peer_cid, &mut self.reliability, self.start_time);
        self.send_frames(vec![frame]).await?;
        self.reliability.on_ack_sent();
        Ok(())
    }

    pub(super) async fn send_frames(&self, frames: Vec<Frame>) -> Result<()> {
        if frames.is_empty() {
            return Ok(());
        }
        let cmd = SendCommand {
            remote_addr: self.remote_addr,
            frames,
        };
        self.sender
            .send(SenderTaskCommand::Send(cmd))
            .await
            .map_err(|_| Error::ChannelClosed)
    }

    pub(super) async fn send_frame_to(&self, frame: Frame, remote_addr: SocketAddr) -> Result<()> {
        let cmd = SendCommand {
            remote_addr,
            frames: vec![frame],
        };
        self.sender
            .send(SenderTaskCommand::Send(cmd))
            .await
            .map_err(|_| Error::ChannelClosed)
    }
} 