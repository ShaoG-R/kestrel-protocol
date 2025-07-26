//! The reliability layer.
//!
//! This layer is responsible for sequencing, acknowledgments, retransmissions (RTO),
//! SACK processing, and reordering. It provides an interface for sending and
//! receiving reliable data blocks.
//!
//! 可靠性层。
//!
//! 该层负责序列化、确认、重传（RTO）、SACK处理和重排序。
//! 它提供了一个发送和接收可靠数据块的接口。

pub mod recv_buffer;
pub mod rtt;
pub mod send_buffer;

use self::{
    recv_buffer::ReceiveBuffer, rtt::RttEstimator, send_buffer::SendBuffer,
};
use crate::{
    congestion::CongestionControl,
    config::Config,
    packet::{frame::Frame, sack::SackRange},
};
use bytes::Bytes;
use tokio::time::Instant;
use tracing::{debug, trace};

/// The reliability layer for a connection.
///
/// 连接的可靠性层。
pub struct ReliabilityLayer {
    send_buffer: SendBuffer,
    recv_buffer: ReceiveBuffer,
    rto_estimator: RttEstimator,
    congestion_control: Box<dyn CongestionControl>,
    config: Config,
    sequence_number_counter: u32,
    ack_pending: bool,
    ack_eliciting_packets_since_last_ack: u16,
}

impl ReliabilityLayer {
    pub fn new(config: Config, congestion_control: Box<dyn CongestionControl>) -> Self {
        Self {
            send_buffer: SendBuffer::new(config.send_buffer_capacity_bytes),
            recv_buffer: ReceiveBuffer::new(config.recv_buffer_capacity_packets),
            rto_estimator: RttEstimator::new(config.initial_rto),
            congestion_control,
            config,
            sequence_number_counter: 0,
            ack_pending: false,
            ack_eliciting_packets_since_last_ack: 0,
        }
    }

    /// Handles an incoming ACK, processing SACK ranges and updating RTT.
    pub fn handle_ack(
        &mut self,
        recv_next_seq: u32,
        sack_ranges: Vec<SackRange>,
        now: Instant,
    ) -> Vec<Frame> {
        let (frames_to_retx, rtt_samples) = self.send_buffer.handle_ack(
            recv_next_seq,
            &sack_ranges,
            self.config.fast_retx_threshold,
            now,
        );

        // Update RTT and congestion control with real samples.
        for rtt_sample in rtt_samples {
            self.rto_estimator.update(rtt_sample, self.config.min_rto);
            let old_cwnd = self.congestion_control.congestion_window();
            self.congestion_control.on_ack(rtt_sample);
            let new_cwnd = self.congestion_control.congestion_window();
            if old_cwnd != new_cwnd {
                trace!(old_cwnd, new_cwnd, "Congestion window updated on ACK");
            }
        }

        if !frames_to_retx.is_empty() {
            let old_cwnd = self.congestion_control.congestion_window();
            self.congestion_control.on_packet_loss(now);
            let new_cwnd = self.congestion_control.congestion_window();
            debug!(
                old_cwnd,
                new_cwnd,
                count = frames_to_retx.len(),
                "Congestion window reduced due to fast retransmission"
            );
        }

        frames_to_retx
    }

    /// Checks for RTO and returns frames to be retransmitted.
    pub fn check_for_retransmissions(&mut self, now: Instant) -> Vec<Frame> {
        let rto = self.rto_estimator.rto();
        let frames_to_resend = self.send_buffer.check_for_rto(rto, now);

        if !frames_to_resend.is_empty() {
            let old_cwnd = self.congestion_control.congestion_window();
            self.congestion_control.on_packet_loss(now);
            let new_cwnd = self.congestion_control.congestion_window();
            debug!(
                old_cwnd,
                new_cwnd,
                count = frames_to_resend.len(),
                "Congestion window reduced due to RTO"
            );
            self.rto_estimator.backoff();
        }

        frames_to_resend
    }

    /// Returns the deadline for the next RTO event.
    pub fn next_rto_deadline(&self) -> Option<Instant> {
        self.send_buffer.next_rto_deadline(self.rto_estimator.rto())
    }
    
    /// Takes data from the stream buffer and packetizes it.
    pub fn packetize_stream_data(
        &mut self,
        peer_cid: u32,
        now: Instant,
        start_time: Instant,
    ) -> Vec<Frame> {
        let mut frames = Vec::new();
        // Calculate the sending permit before the loop to ensure atomicity for this call.
        let permit = self
            .congestion_control
            .congestion_window()
            .saturating_sub(self.send_buffer.in_flight_count() as u32);

        for _ in 0..permit {
            if self.is_send_buffer_empty() {
                break;
            }
            // We re-check can_send_more inside the loop to respect the peer_recv_window,
            // which is less likely to change mid-function but is still good practice.
            if !self.can_send_more(self.recv_buffer.window_size() as u32) {
                break;
            }

            let Some(chunk) = self.send_buffer.create_chunk(self.config.max_payload_size) else {
                break;
            };

            let (_, next_ack, window) = self.get_ack_info();
            let frame = Frame::new_push(
                peer_cid,
                self.next_sequence_number(),
                next_ack,
                window,
                now.duration_since(start_time).as_millis() as u32,
                chunk,
            );
            frames.push(frame.clone());
            self.send_buffer.add_in_flight(frame, now);
        }
        frames
    }
    
    /// Determines if more packets can be sent.
    pub fn can_send_more(&self, peer_recv_window: u32) -> bool {
        let in_flight = self.send_buffer.in_flight_count() as u32;
        let cwnd = self.congestion_control.congestion_window();
        in_flight < cwnd && in_flight < peer_recv_window
    }
    
    // --- Passthrough methods to recv_buffer ---
    
    pub fn receive_push(&mut self, sequence_number: u32, payload: Bytes) {
        self.recv_buffer.receive(sequence_number, payload);
        self.ack_pending = true;
        self.ack_eliciting_packets_since_last_ack += 1;
    }

    pub fn receive_fin(&mut self, sequence_number: u32) {
        // A FIN packet is like a PUSH with no data. It still occupies a sequence number
        // and must be acknowledged.
        self.recv_buffer.receive(sequence_number, Bytes::new());
        self.ack_pending = true;
        self.ack_eliciting_packets_since_last_ack += 1;
    }

    pub fn reassemble(&mut self) -> Option<Vec<Bytes>> {
        self.recv_buffer.reassemble()
    }
    
    pub fn get_ack_info(&self) -> (Vec<SackRange>, u32, u16) {
        (
            self.recv_buffer.get_sack_ranges(),
            self.recv_buffer.next_sequence(),
            self.recv_buffer.window_size(),
        )
    }

    pub fn should_send_standalone_ack(&self) -> bool {
        self.ack_eliciting_packets_since_last_ack >= self.config.ack_threshold
            && !self.recv_buffer.get_sack_ranges().is_empty()
    }

    pub fn is_ack_pending(&self) -> bool {
        self.ack_pending
    }

    pub fn on_ack_sent(&mut self) {
        self.ack_pending = false;
        self.ack_eliciting_packets_since_last_ack = 0;
    }

    // --- Passthrough methods to send_buffer ---

    pub fn write_to_stream(&mut self, buf: &[u8]) -> usize {
        self.send_buffer.write_to_stream(buf)
    }

    pub fn is_send_buffer_empty(&self) -> bool {
        self.send_buffer.is_stream_buffer_empty()
    }

    pub fn take_stream_buffer(&mut self) -> Bytes {
        self.send_buffer.take_stream_buffer()
    }

    pub fn is_in_flight_empty(&self) -> bool {
        self.send_buffer.is_in_flight_empty()
    }
    
    pub fn has_fin_in_flight(&self) -> bool {
        self.send_buffer.has_fin_in_flight()
    }

    pub fn add_fin_to_in_flight(&mut self, fin_frame: Frame, now: Instant) {
        self.send_buffer.add_in_flight(fin_frame, now);
    }
    
    // --- Internal helpers ---
    
    pub fn next_sequence_number(&mut self) -> u32 {
        let seq = self.sequence_number_counter;
        self.sequence_number_counter += 1;
        seq
    }
} 