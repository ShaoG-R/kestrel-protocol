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
pub mod congestion;

use self::{
    packetizer::{packetize, PacketizerContext},
    recv_buffer::ReceiveBuffer,
    retransmission::{rtt::RttEstimator, RetransmissionManager},
    send_buffer::SendBuffer,
};
use crate::{
    config::Config,
    packet::{frame::Frame, sack::SackRange},
};
use bytes::Bytes;
use tokio::time::Instant;
use tracing::{debug, trace};
use congestion::CongestionControl;
use crate::core::endpoint::timing::TimeoutEvent;
use crate::core::endpoint::unified_scheduler::TimeoutLayer;

/// 超时检查结果
/// Timeout check result
///
/// 该结构体包含可靠性层超时检查的结果，包括发生的超时事件和需要重传的帧。
///
/// This struct contains the result of reliability layer timeout checks,
/// including timeout events that occurred and frames that need retransmission.
#[derive(Debug)]
pub struct TimeoutCheckResult {
    /// 发生的超时事件列表
    /// List of timeout events that occurred
    pub events: Vec<TimeoutEvent>,
    /// 需要重传的帧（仅在RetransmissionTimeout时有效）
    /// Frames that need retransmission (only valid for RetransmissionTimeout)
    pub frames_to_retransmit: Vec<Frame>,
}

/// The reliability layer for a connection.
///
/// It orchestrates all logic related to making the UDP connection reliable. This includes
/// managing send/receive buffers, handling acknowledgements (ACKs) and Selective ACKs (SACKs),
/// managing retransmission timers (RTO), and interacting with the pluggable congestion
/// control algorithm.
///
/// 连接的可靠性层。
///
/// 该层协调所有与实现可靠UDP连接相关的逻辑。这包括管理发送/接收缓冲区、
/// 处理确认（ACK）和选择性确认（SACK）、管理重传计时器（RTO），
/// 以及与可插拔的拥塞控制算法进行交互。
pub struct ReliabilityLayer {
    /// Manages the buffer for outgoing application data that has not yet been packetized.
    ///
    /// 管理尚未打包的待发出应用数据的缓冲区。
    send_buffer: SendBuffer,
    /// Manages the buffer for incoming data packets, handling reordering and reassembly.
    ///
    /// 管理传入数据包的缓冲区，处理重排序和重组。
    recv_buffer: ReceiveBuffer,
    /// Estimates the Round-Trip Time (RTT) and calculates the Retransmission Timeout (RTO).
    ///
    /// 估计往返时间（RTT）并计算重传超时（RTO）。
    rto_estimator: RttEstimator,
    /// The pluggable congestion control algorithm implementation.
    ///
    /// 可插拔的拥塞控制算法实现。
    congestion_control: Box<dyn CongestionControl>,
    /// Manages in-flight packets, detects losses, and determines when to retransmit.
    /// This includes both SACK-based loss detection and RTO-based timeouts.
    ///
    /// 管理在途数据包，检测丢失，并决定何时重传。
    /// 这包括基于SACK的丢包检测和基于RTO的超时。
    retransmission_manager: RetransmissionManager,
    /// Shared configuration for the connection.
    ///
    /// 连接的共享配置。
    config: Config,
    /// The next sequence number to be assigned to an outgoing `PUSH` or `FIN` frame.
    ///
    /// 将要分配给传出 `PUSH` 或 `FIN` 帧的下一个序列号。
    sequence_number_counter: u32,
    /// A flag indicating that an acknowledgment should be sent for a received packet.
    ///
    /// 一个标志，指示应为收到的数据包发送确认。
    ack_pending: bool,
    /// Counts the number of ACK-eliciting packets received since the last ACK was sent.
    /// Used to decide when to send a standalone ACK.
    ///
    /// 统计自上次发送ACK以来收到的引发ACK的数据包数量。
    /// 用于决定何时发送独立的ACK。
    ack_eliciting_packets_since_last_ack: u16,
}

impl ReliabilityLayer {
    /// Creates a new `ReliabilityLayer`.
    ///
    /// 创建一个新的 `ReliabilityLayer`。
    pub fn new(config: Config, congestion_control: Box<dyn CongestionControl>) -> Self {
        Self {
            send_buffer: SendBuffer::new(config.connection.send_buffer_capacity_bytes),
            recv_buffer: ReceiveBuffer::new(config.connection.recv_buffer_capacity_packets),
            rto_estimator: RttEstimator::new(config.reliability.initial_rto),
            congestion_control,
            retransmission_manager: RetransmissionManager::new(config.clone()),
            config,
            sequence_number_counter: 0,
            ack_pending: false,
            ack_eliciting_packets_since_last_ack: 0,
        }
    }

    /// Handles an incoming ACK frame.
    ///
    /// This method processes the SACK ranges to identify acknowledged and lost packets,
    /// updates the RTT estimate, adjusts the congestion window, and queues lost packets
    /// for fast retransmission.
    ///
    /// 处理传入的 ACK 帧。
    ///
    /// 此方法处理 SACK 范围以识别已确认和丢失的数据包，
    /// 更新 RTT 估计，调整拥塞窗口，并将丢失的数据包排队以便快速重传。
    pub fn handle_ack(
        &mut self,
        recv_next_seq: u32,
        sack_ranges: Vec<SackRange>,
        now: Instant,
        context: &crate::packet::frame::RetransmissionContext,
    ) -> Vec<Frame> {
        // Use the unified retransmission manager for ACK processing.
        // 使用统一的重传管理器进行ACK处理。
        let result = self.retransmission_manager.process_ack(recv_next_seq, &sack_ranges, now, context);

        // Update RTT and congestion control with real samples from SACK-managed packets.
        // Simple retransmission packets don't contribute to RTT estimation or congestion control.
        // 使用从SACK管理的数据包中获得的真实样本来更新RTT和拥塞控制。
        // 简单的重传数据包不参与RTT估算或拥塞控制。
        for rtt_sample in result.rtt_samples {
            self.rto_estimator.update(rtt_sample, self.config.reliability.min_rto);
            let old_cwnd = self.congestion_control.congestion_window();
            self.congestion_control.on_ack(rtt_sample);
            let new_cwnd = self.congestion_control.congestion_window();
            if old_cwnd != new_cwnd {
                trace!(old_cwnd, new_cwnd, "Congestion window updated on ACK");
            }
        }

        // If packets were considered lost (triggering fast retransmission), notify the congestion controller.
        // 如果有数据包被视为丢失（触发快速重传），则通知拥塞控制器。
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

    /// Checks for packets that have timed out based on the current RTO.
    ///
    /// If timeouts are detected, it notifies the congestion controller, backs off the RTO timer,
    /// and returns the frames that need to be retransmitted with fresh header information.
    ///
    /// 根据当前RTO检查已超时的数据包。
    ///
    /// 如果检测到超时，它会通知拥塞控制器，退避RTO计时器，
    /// 并返回需要重传的帧，这些帧使用新鲜的header信息重构。
    pub fn check_for_retransmissions(
        &mut self, 
        now: Instant, 
        context: &crate::packet::frame::RetransmissionContext,
    ) -> Vec<Frame> {
        let rto = self.rto_estimator.rto();
        let frames_to_resend = self.retransmission_manager.check_for_retransmissions(rto, now, context);

        // If packets timed out, notify congestion control and back off the RTO timer.
        // 如果数据包超时，通知拥塞控制并退避RTO计时器。
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
    ///
    /// This allows the connection's event loop to sleep efficiently until the next potential timeout.
    ///
    /// 返回下一个RTO事件的截止时间。
    ///
    /// 这允许连接的事件循环有效地休眠，直到下一个潜在的超时。
    pub fn next_rto_deadline(&self) -> Option<Instant> {
        self.retransmission_manager.next_retransmission_deadline(self.rto_estimator.rto())
    }

    /// Takes data from the stream buffer and packetizes it into frames.
    ///
    /// This function considers the congestion window, receiver window, and MTU to
    /// create as many `PUSH` frames as currently possible.
    ///
    /// 从流缓冲区获取数据并将其打包成帧。
    ///
    /// 此函数会考虑拥塞窗口、接收方窗口和MTU，以创建当前可能的最大数量的`PUSH`帧。
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
            max_payload_size: self.config.connection.max_payload_size,
            ack_info: (recv_next_sequence, local_window_size),
        };

        let frames = packetize(
            &context,
            &mut self.send_buffer,
            &mut self.sequence_number_counter,
            prepend_frame,
        );

        // After creating the frames, add them to the retransmission manager to be tracked.
        // 创建帧后，将它们添加到重传管理器中进行跟踪。
        for frame in &frames {
            // Add all frames to the unified retransmission manager.
            // 将所有帧添加到统一重传管理器。
            self.retransmission_manager.add_in_flight_packet(frame.clone(), now);
        }

        frames
    }

    /// Packetize stream data for 0-RTT scenario with intelligent packet splitting.
    /// This ensures SYN-ACK is always in the first packet, with PUSH frames
    /// distributed across packets respecting MTU limits.
    ///
    /// 为0-RTT场景打包流数据，采用智能分包策略。
    /// 确保SYN-ACK始终在第一个包中，PUSH帧按MTU限制智能分布到各个包中。
    pub fn packetize_zero_rtt_stream_data(
        &mut self,
        peer_cid: u32,
        peer_recv_window: u32,
        now: Instant,
        start_time: Instant,
        syn_ack_frame: Frame,
        max_packet_size: usize,
    ) -> Vec<Vec<Frame>> {
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
            max_payload_size: self.config.connection.max_payload_size,
            ack_info: (recv_next_sequence, local_window_size),
        };

        let packet_groups = packetizer::packetize_zero_rtt(
            &context,
            &mut self.send_buffer,
            &mut self.sequence_number_counter,
            syn_ack_frame,
            max_packet_size,
        );

        // Track all frames in the retransmission manager
        // 在重传管理器中跟踪所有帧
        for packet in &packet_groups {
            for frame in packet {
                self.retransmission_manager.add_in_flight_packet(frame.clone(), now);
            }
        }

        packet_groups
    }

    /// Determines if more packets can be sent based on the congestion and flow control windows.
    ///
    /// Only SACK-managed packets count towards congestion control.
    ///
    /// 根据拥塞和流控制窗口确定是否可以发送更多数据包。
    ///
    /// 只有受SACK管理的数据包才计入拥塞控制。
    pub fn can_send_more(&self, peer_recv_window: u32) -> bool {
        let in_flight = self.retransmission_manager.sack_in_flight_count() as u32;
        let cwnd = self.congestion_control.congestion_window();
        in_flight < cwnd && in_flight < peer_recv_window
    }

    // --- Passthrough methods to recv_buffer ---

    /// Receives a `PUSH` frame's payload.
    ///
    /// Returns `true` if the packet was accepted (i.e., not a duplicate), `false` otherwise.
    ///
    /// 接收 `PUSH` 帧的有效负载。
    ///
    /// 如果数据包被接受（即不是重复的），则返回`true`，否则返回`false`。
    pub fn receive_push(&mut self, sequence_number: u32, payload: Bytes) -> bool {
        let accepted = self.recv_buffer.receive_push(sequence_number, payload);
        if accepted {
            self.ack_pending = true;
            self.ack_eliciting_packets_since_last_ack += 1;
            self.retransmission_manager.on_ack_eliciting_packet_received();
        }
        accepted
    }

    /// Receives a `FIN` frame.
    ///
    /// A FIN packet is like a PUSH with no data, but is handled specially.
    /// It still occupies a sequence number and must be acknowledged.
    /// Returns `true` if the FIN was accepted, `false` if it was a duplicate.
    ///
    /// 接收 `FIN` 帧。
    ///
    /// FIN包类似于一个没有数据的PUSH包，但会进行特殊处理。
    /// 它仍然占用一个序列号并且必须被确认。
    /// 如果FIN被接受则返回 `true`，如果是重复的则返回 `false`。
    pub fn receive_fin(&mut self, sequence_number: u32) -> bool {
        // A FIN packet is like a PUSH with no data, but is handled specially now.
        // It still occupies a sequence number and must be acknowledged.
        // FIN包类似于一个没有数据的PUSH包，但现在进行特殊处理。
        // 它仍然占用一个序列号并且必须被确认。
        let accepted = self.recv_buffer.receive_fin(sequence_number);
        if accepted {
            self.ack_pending = true;
            self.ack_eliciting_packets_since_last_ack += 1;
            self.retransmission_manager.on_ack_eliciting_packet_received();
        }
        accepted
    }

    /// Attempts to reassemble ordered data from the receive buffer.
    ///
    /// Returns a tuple containing an optional vector of `Bytes` (the reassembled data)
    /// and a boolean indicating if a `FIN` has been received and processed.
    ///
    /// 尝试从接收缓冲区重组有序数据。
    ///
    /// 返回一个元组，其中包含一个可选的`Bytes`向量（重组后的数据）
    /// 和一个布尔值，指示是否已接收并处理了`FIN`。
    pub fn reassemble(&mut self) -> (Option<Vec<Bytes>>, bool) {
        self.recv_buffer.reassemble()
    }

    /// Checks if the receive buffer is completely empty (no pending or out-of-order packets).
    ///
    /// 检查接收缓冲区是否完全为空（没有待处理或乱序的数据包）。
    pub fn is_recv_buffer_empty(&self) -> bool {
        self.recv_buffer.is_empty()
    }

    /// Gathers information required to build an ACK frame.
    ///
    /// Returns a tuple: (SACK ranges, next expected sequence number, available window size).
    ///
    /// 收集构建 ACK 帧所需的信息。
    ///
    /// 返回一个元组：（SACK范围，下一个期望的序列号，可用的窗口大小）。
    pub fn get_ack_info(&self) -> (Vec<SackRange>, u32, u16) {
        (
            self.recv_buffer.get_sack_ranges(),
            self.recv_buffer.next_sequence(),
            self.recv_buffer.window_size(),
        )
    }

    /// Determines if a standalone ACK should be sent immediately, based on the number
    /// of ACK-eliciting packets received.
    ///
    /// 根据收到的引发ACK的数据包数量，决定是否应立即发送一个独立的ACK。
    pub fn should_send_standalone_ack(&self) -> bool {
        let sack_ranges = self.recv_buffer.get_sack_ranges();
        self.retransmission_manager.should_send_standalone_ack(&sack_ranges)
    }

    /// Checks if there is an ACK pending to be sent.
    ///
    /// 检查是否有待发送的ACK。
    pub fn is_ack_pending(&self) -> bool {
        self.ack_pending
    }

    /// Called after an ACK frame has been sent. Resets ACK-pending state.
    ///
    /// 在发送ACK帧后调用。重置待处理ACK的状态。
    pub fn on_ack_sent(&mut self) {
        self.ack_pending = false;
        self.ack_eliciting_packets_since_last_ack = 0;
        self.retransmission_manager.on_ack_sent();
    }

    // --- Passthrough methods to send_buffer ---

    /// Writes data from the application into the stream send buffer.
    ///
    /// 将来自应用程序的数据写入流发送缓冲区。
    pub fn write_to_stream(&mut self, buf: Bytes) -> usize {
        self.send_buffer.write_to_stream(buf)
    }

    /// Checks if the application's stream buffer is empty.
    ///
    /// 检查应用程序的流缓冲区是否为空。
    pub fn is_send_buffer_empty(&self) -> bool {
        self.send_buffer.is_stream_buffer_empty()
    }

    /// Takes all buffered data from the stream buffer, leaving it empty.
    ///
    /// This is used during connection closing to flush any remaining data.
    ///
    /// 从流缓冲区中取出所有缓冲数据，使其变空。
    ///
    /// 这在连接关闭期间用于刷新任何剩余数据。
    pub fn take_stream_buffer(&mut self) -> impl Iterator<Item = Bytes> {
        self.send_buffer.take_stream_buffer()
    }

    /// Checks if there are no more packets in flight.
    ///
    /// 检查是否已没有在途数据包。
    pub fn is_in_flight_empty(&self) -> bool {
        self.retransmission_manager.is_all_in_flight_empty()
    }

    /// Checks if a `FIN` frame is currently in flight.
    ///
    /// 检查当前是否有`FIN`帧在途。
    pub fn has_fin_in_flight(&self) -> bool {
        self.retransmission_manager.has_fin_in_flight()
    }

    /// Explicitly tracks a frame (e.g., FIN, SYN-ACK) in the retransmission manager.
    ///
    /// 在重传管理器中显式跟踪一个帧（例如，FIN, SYN-ACK）。
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

    /// Gets the next sequence number for a new packet and increments the counter.
    ///
    /// 获取新数据包的下一个序列号并增加计数器。
    pub fn next_sequence_number(&mut self) -> u32 {
        let seq = self.sequence_number_counter;
        self.sequence_number_counter += 1;
        seq
    }

    // --- Congestion control and RTT passthrough methods ---

    /// Gets the current congestion window size.
    ///
    /// 获取当前拥塞窗口大小。
    pub fn congestion_window(&self) -> u32 {
        self.congestion_control.congestion_window()
    }

    /// Gets the current smoothed RTT estimate.
    ///
    /// 获取当前平滑的RTT估计值。
    pub fn smoothed_rtt(&self) -> Option<std::time::Duration> {
        self.rto_estimator.smoothed_rtt()
    }

    /// Gets the current RTT variation.
    ///
    /// 获取当前RTT变化值。
    pub fn rtt_var(&self) -> Option<std::time::Duration> {
        self.rto_estimator.rtt_var()
    }

    // === 分层超时管理接口 Layered Timeout Management Interface ===

    /// 检查可靠性相关超时事件
    /// Check reliability-related timeout events
    ///
    /// 该方法检查所有可靠性相关的超时情况，返回超时检查结果。
    /// 这是分层超时管理架构中可靠性层的统一入口。
    ///
    /// This method checks all reliability-related timeout conditions and returns
    /// timeout check results. This is the unified entry point for the reliability
    /// layer in the layered timeout management architecture.
    pub fn check_reliability_timeouts(
        &mut self, 
        now: Instant, 
        context: &crate::packet::frame::RetransmissionContext,
    ) -> TimeoutCheckResult {
        let mut all_events = Vec::new();
        let mut all_frames_to_retransmit = Vec::new();

        // 使用分层超时管理接口检查重传超时，使用帧重构
        // Use layered timeout management interface to check retransmission timeout with frame reconstruction
        let rto = self.rto_estimator.rto();
        let (retx_events, frames_to_resend) = self.retransmission_manager.check_retransmission_timeouts(rto, now, context);
        
        all_events.extend(retx_events);
        all_frames_to_retransmit.extend(frames_to_resend);

        // 如果有重传超时，处理拥塞控制和RTO退避
        // If there are retransmission timeouts, handle congestion control and RTO backoff
        if !all_frames_to_retransmit.is_empty() {
            let old_cwnd = self.congestion_control.congestion_window();
            self.congestion_control.on_packet_loss(now);
            let new_cwnd = self.congestion_control.congestion_window();
            debug!(
                old_cwnd,
                new_cwnd,
                count = all_frames_to_retransmit.len(),
                "Congestion window reduced due to retransmissions"
            );
            self.rto_estimator.backoff();
        }

        TimeoutCheckResult {
            events: all_events,
            frames_to_retransmit: all_frames_to_retransmit,
        }
    }

    /// 获取下一个可靠性超时的截止时间
    /// Get the deadline for the next reliability timeout
    ///
    /// 该方法计算所有可靠性相关超时中最早的截止时间，用于事件循环的等待时间优化。
    ///
    /// This method calculates the earliest deadline among all reliability-related
    /// timeouts, used for optimizing event loop wait times.
    pub fn next_reliability_timeout_deadline(&self) -> Option<Instant> {
        // 使用分层超时管理接口获取重传超时截止时间
        // Use layered timeout management interface to get retransmission timeout deadline
        let rto = self.rto_estimator.rto();
        self.retransmission_manager.next_retransmission_timeout_deadline(rto)
    }
}

// === TimeoutLayer trait 实现 TimeoutLayer trait implementation ===

impl TimeoutLayer for ReliabilityLayer {
    fn next_deadline(&self) -> Option<Instant> {
        self.next_reliability_timeout_deadline()
    }
    
    fn check_timeouts(&mut self, now: Instant) -> TimeoutCheckResult {
        // 创建重传上下文 - 使用默认值，实际使用时应传入正确的参数
        // Create retransmission context - using default values, should pass correct params in actual use
        let context = crate::packet::frame::RetransmissionContext::new(
            now,
            0, // peer_cid - 默认值
            1, // local_cid - 默认值  
            1, // timestamp - 默认值
            0, // recv_window - 默认值
            1024, // mtu - 默认值
        );
        
        self.check_reliability_timeouts(now, &context)
    }
    
    fn layer_name(&self) -> &'static str {
        "ReliabilityLayer"
    }
    
    fn stats(&self) -> Option<String> {
        Some(format!(
            "cwnd: {}, in_flight: {}, rto: {:?}",
            self.congestion_window(),
            self.retransmission_manager.total_in_flight_count(),
            self.rto_estimator.rto()
        ))
    }
} 