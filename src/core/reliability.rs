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

pub mod packetizer;
pub mod recv_buffer;
pub mod retransmission;
pub mod send_buffer;

use self::{
    packetizer::{packetize, PacketizerContext},
    recv_buffer::ReceiveBuffer,
    retransmission::{RetransmissionManager, rtt::RttEstimator},
    send_buffer::SendBuffer,
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
    retransmission_manager: RetransmissionManager,
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
            retransmission_manager: RetransmissionManager::new(config.clone()),
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
        // Use the unified retransmission manager for ACK processing
        let result = self.retransmission_manager.process_ack(recv_next_seq, &sack_ranges, now);

        // Update RTT and congestion control with real samples from SACK-managed packets.
        // Simple retransmission packets don't contribute to RTT estimation or congestion control.
        for rtt_sample in result.rtt_samples {
            self.rto_estimator.update(rtt_sample, self.config.min_rto);
            let old_cwnd = self.congestion_control.congestion_window();
            self.congestion_control.on_ack(rtt_sample);
            let new_cwnd = self.congestion_control.congestion_window();
            if old_cwnd != new_cwnd {
                trace!(old_cwnd, new_cwnd, "Congestion window updated on ACK");
            }
        }

        if !result.frames_to_retransmit.is_empty() {
            let old_cwnd = self.congestion_control.congestion_window();
            self.congestion_control.on_packet_loss(now);
            let new_cwnd = self.congestion_control.congestion_window();
            debug!(
                old_cwnd,
                new_cwnd,
                count = result.frames_to_retransmit.len(),
                "Congestion window reduced due to retransmission"
            );
        }

        result.frames_to_retransmit
    }

    /// Checks for RTO and returns frames to be retransmitted.
    pub fn check_for_retransmissions(&mut self, now: Instant) -> Vec<Frame> {
        let rto = self.rto_estimator.rto();
        let frames_to_resend = self.retransmission_manager.check_for_retransmissions(rto, now);

        if !frames_to_resend.is_empty() {
            let old_cwnd = self.congestion_control.congestion_window();
            self.congestion_control.on_packet_loss(now);
            let new_cwnd = self.congestion_control.congestion_window();
            debug!(
                old_cwnd,
                new_cwnd,
                count = frames_to_resend.len(),
                "Congestion window reduced due to retransmissions"
            );
            self.rto_estimator.backoff();
        }

        frames_to_resend
    }

    /// Returns the deadline for the next RTO event.
    pub fn next_rto_deadline(&self) -> Option<Instant> {
        self.retransmission_manager.next_retransmission_deadline(self.rto_estimator.rto())
    }

    /// Takes data from the stream buffer and packetizes it.
    ///
    /// 从流缓冲区获取数据并打包。
    pub fn packetize_stream_data(
        &mut self,
        peer_cid: u32,
        peer_recv_window: u32,
        now: Instant,
        start_time: Instant,
        prepend_frame: Option<Frame>,
    ) -> Vec<Frame> {
        let (recv_next_sequence, local_window_size) = {
            let info = self.get_ack_info();
            (info.1, info.2)
        };
        let context = PacketizerContext {
            peer_cid,
            timestamp: now.duration_since(start_time).as_millis() as u32,
            congestion_window: self.congestion_control.congestion_window(),
            in_flight_count: self.retransmission_manager.sack_in_flight_count(),
            peer_recv_window,
            max_payload_size: self.config.max_payload_size,
            ack_info: (recv_next_sequence, local_window_size),
        };

        let frames = packetize(
            &context,
            &mut self.send_buffer,
            &mut self.sequence_number_counter,
            prepend_frame,
        );

        for frame in &frames {
            // Add all frames to the unified retransmission manager
            // 将所有帧添加到统一重传管理器
            self.retransmission_manager.add_in_flight_packet(frame.clone(), now);
        }

        frames
    }

    /// Determines if more packets can be sent.
    /// Only SACK-managed packets count towards congestion control.
    pub fn can_send_more(&self, peer_recv_window: u32) -> bool {
        let in_flight = self.retransmission_manager.sack_in_flight_count() as u32;
        let cwnd = self.congestion_control.congestion_window();
        in_flight < cwnd && in_flight < peer_recv_window
    }

    // --- Passthrough methods to recv_buffer ---

    pub fn receive_push(&mut self, sequence_number: u32, payload: Bytes) -> bool {
        let accepted = self.recv_buffer.receive_push(sequence_number, payload);
        if accepted {
            self.ack_pending = true;
            self.ack_eliciting_packets_since_last_ack += 1;
            self.retransmission_manager.on_ack_eliciting_packet_received();
        }
        accepted
    }

    pub fn receive_fin(&mut self, sequence_number: u32) -> bool {
        // A FIN packet is like a PUSH with no data, but is handled specially now.
        // It still occupies a sequence number and must be acknowledged.
        let accepted = self.recv_buffer.receive_fin(sequence_number);
        if accepted {
            self.ack_pending = true;
            self.ack_eliciting_packets_since_last_ack += 1;
            self.retransmission_manager.on_ack_eliciting_packet_received();
        }
        accepted
    }

    pub fn reassemble(&mut self) -> (Option<Vec<Bytes>>, bool) {
        self.recv_buffer.reassemble()
    }

    pub fn is_recv_buffer_empty(&self) -> bool {
        self.recv_buffer.is_empty()
    }

    pub fn get_ack_info(&self) -> (Vec<SackRange>, u32, u16) {
        (
            self.recv_buffer.get_sack_ranges(),
            self.recv_buffer.next_sequence(),
            self.recv_buffer.window_size(),
        )
    }

    pub fn should_send_standalone_ack(&self) -> bool {
        let sack_ranges = self.recv_buffer.get_sack_ranges();
        self.retransmission_manager.should_send_standalone_ack(&sack_ranges)
    }

    pub fn is_ack_pending(&self) -> bool {
        self.ack_pending
    }

    pub fn on_ack_sent(&mut self) {
        self.ack_pending = false;
        self.ack_eliciting_packets_since_last_ack = 0;
        self.retransmission_manager.on_ack_sent();
    }

    // --- Passthrough methods to send_buffer ---

    pub fn write_to_stream(&mut self, buf: Bytes) -> usize {
        self.send_buffer.write_to_stream(buf)
    }

    pub fn is_send_buffer_empty(&self) -> bool {
        self.send_buffer.is_stream_buffer_empty()
    }

    pub fn take_stream_buffer(&mut self) -> impl Iterator<Item = Bytes> {
        self.send_buffer.take_stream_buffer()
    }

    pub fn is_in_flight_empty(&self) -> bool {
        self.retransmission_manager.is_all_in_flight_empty()
    }

    pub fn has_fin_in_flight(&self) -> bool {
        self.retransmission_manager.has_fin_in_flight()
    }

    pub fn track_frame_in_flight(&mut self, frame: Frame, now: Instant) {
        self.retransmission_manager.add_in_flight_packet(frame, now);
    }

    /// Clears all in-flight packets. This is used when the connection is being
    /// torn down and we no longer need to track packets for retransmission.
    ///
    /// 清除所有在途数据包。这在连接被拆除且不再需要跟踪重传数据包时使用。
    pub fn clear_in_flight_packets(&mut self) {
        self.retransmission_manager.clear();
    }

    // --- Internal helpers ---

    pub fn next_sequence_number(&mut self) -> u32 {
        let seq = self.sequence_number_counter;
        self.sequence_number_counter += 1;
        seq
    }
} 