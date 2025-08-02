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
pub mod packet_timer_manager;

use self::{
    packetizer::{packetize, PacketizerContext},
    recv_buffer::ReceiveBuffer,
    retransmission::{rtt::RttEstimator, RetransmissionManager},
    send_buffer::SendBuffer,
    packet_timer_manager::PacketTimerManager,
};
use crate::{
    config::Config,
    packet::{frame::Frame, sack::SackRange},
};
use bytes::Bytes;
use tokio::{sync::mpsc, time::Instant};
use tracing::{debug, trace};
use congestion::CongestionControl;
use crate::core::endpoint::unified_scheduler::{RetransmissionLayer, RetransmissionCheckResult};
use crate::timer::{
    event::{ConnectionId, TimerEventData},
    TimerActorHandle,
};
use crate::core::endpoint::timing::TimeoutEvent;
use crate::timer::task::types::SenderCallback;



/// ACK处理结果
/// ACK processing result
///
/// 该结构体包含ACK处理的完整结果，包括需要重传的帧和被确认的序列号。
/// This struct contains the complete result of ACK processing, including
/// frames that need retransmission and acknowledged sequence numbers.
#[derive(Debug)]
pub struct AckProcessingResult {
    /// 需要重传的帧
    /// Frames that need retransmission
    pub frames_to_retransmit: Vec<Frame>,
    /// 新确认的序列号列表
    /// List of newly acknowledged sequence numbers
    pub newly_acked_sequences: Vec<u32>,
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
    /// Packet timer manager for direct packet-timer binding
    /// 数据包定时器管理器，用于数据包与定时器的直接绑定
    packet_timer_manager: PacketTimerManager,
}

impl ReliabilityLayer {
    /// Creates a new `ReliabilityLayer`.
    ///
    /// 创建一个新的 `ReliabilityLayer`。
    pub fn new(
        config: Config, 
        congestion_control: Box<dyn CongestionControl>,
        connection_id: ConnectionId,
        timer_actor: TimerActorHandle<SenderCallback<TimeoutEvent>>,
    ) -> Self {
        // 创建一个临时的timeout_tx通道，稍后会被实际的通道替换
        // Create a temporary timeout_tx channel, will be replaced with the actual channel later
        let (temp_timeout_tx, _temp_timeout_rx) = mpsc::channel(32);
        let packet_timer_manager = PacketTimerManager::new(connection_id, timer_actor.clone(), temp_timeout_tx);
        
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
            packet_timer_manager,
        }
    }

    /// 更新PacketTimerManager的timeout_tx通道
    /// Update PacketTimerManager's timeout_tx channel
    pub fn update_packet_timer_timeout_tx(&mut self, timeout_tx: mpsc::Sender<TimerEventData<TimeoutEvent>>) {
        self.packet_timer_manager.update_timeout_tx(timeout_tx);
    }

    /// Handles an incoming ACK frame and returns comprehensive result.
    ///
    /// This method processes the SACK ranges to identify acknowledged and lost packets,
    /// updates the RTT estimate, adjusts the congestion window, and returns both
    /// frames that need retransmission and newly acknowledged sequence numbers.
    ///
    /// 处理传入的 ACK 帧并返回综合结果。
    ///
    /// 此方法处理 SACK 范围以识别已确认和丢失的数据包，
    /// 更新 RTT 估计，调整拥塞窗口，并返回需要重传的帧和新确认的序列号。
    pub fn handle_ack_comprehensive(
        &mut self,
        recv_next_seq: u32,
        sack_ranges: Vec<SackRange>,
        now: Instant,
        context: &crate::packet::frame::RetransmissionContext,
    ) -> AckProcessingResult {
        // Use the unified retransmission manager for ACK processing.
        // 使用统一的重传管理器进行ACK处理。
        let result = self.retransmission_manager.process_ack(recv_next_seq, &sack_ranges, now, context);

        // Store newly acknowledged sequences for async processing
        // 存储新确认的序列号用于异步处理
        let newly_acked_sequences = result.newly_acked_sequences.clone();

        // 取消已确认数据包的重传定时器
        // Cancel retransmission timers for acknowledged packets
        if !result.newly_acked_sequences.is_empty() {
            trace!(acked_seqs = ?result.newly_acked_sequences, "Cancelling retransmission timers for ACKed packets");
            
            // 标记需要异步批量确认的序列号（新实现）
            // Mark sequences for async batch acknowledgment (new implementation)
            trace!(
                sequences_count = result.newly_acked_sequences.len(),
                sequences = ?result.newly_acked_sequences,
                "Packets marked for async batch acknowledgment using PacketTimerManager"
            );
            
            // 注意：实际的PacketTimerManager批量确认将在调用者的异步上下文中处理
            // Note: Actual PacketTimerManager batch acknowledgment will be handled in caller's async context
            
            // 在新的重传系统中，定时器取消由PacketTimerManager在ACK处理时自动完成
            // In the new retransmission system, timer cancellation is automatically done by PacketTimerManager during ACK processing
            trace!(
                sequences_count = result.newly_acked_sequences.len(),
                "Timer cancellation will be handled by PacketTimerManager during async ACK acknowledgment"
            );
        }

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

        AckProcessingResult {
            frames_to_retransmit: result.frames_to_retransmit,
            newly_acked_sequences,
        }
    }

    /// Handles an incoming ACK frame (legacy interface for backward compatibility).
    ///
    /// This method processes the SACK ranges to identify acknowledged and lost packets,
    /// updates the RTT estimate, adjusts the congestion window, and queues lost packets
    /// for fast retransmission.
    ///
    /// 处理传入的 ACK 帧（向后兼容的遗留接口）。
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
        // 使用新的综合方法获取完整结果，但只返回需要重传的帧以保持向后兼容性
        // Use new comprehensive method to get full result, but only return frames that need retransmission for backward compatibility
        let ack_result = self.handle_ack_comprehensive(recv_next_seq, sack_ranges, now, context);
        ack_result.frames_to_retransmit
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
            // 在新的系统中，只使用传统重传管理器进行ACK管理，不进行重传超时管理
            // In the new system, only use legacy retransmission manager for ACK management, not for retransmission timeout management
            self.retransmission_manager.add_in_flight_packet(frame.clone(), now);
            
            // 标记需要在异步上下文中添加到PacketTimerManager的帧
            // Mark frames that need to be added to PacketTimerManager in async context
            if frame.needs_reliability_tracking() {
                if let Some(seq) = frame.sequence_number() {
                    trace!(
                        seq = seq,
                        frame_type = ?frame.frame_type(),
                        "Frame marked for PacketTimerManager registration in async context"
                    );
                }
            }
        }

        frames
    }

    /// 为刚刚打包的帧注册重传定时器（使用PacketTimerManager）
    /// Register retransmission timers for just packetized frames (using PacketTimerManager)
    pub async fn register_timers_for_packetized_frames(&mut self, frames: &[crate::packet::frame::Frame]) -> usize {
        let mut registered_count = 0;
        let rto = self.rto_estimator.rto();
        
        for frame in frames {
            if frame.needs_reliability_tracking() {
                let now = Instant::now();
                if self.packet_timer_manager.add_packet(frame, now, rto).await {
                    registered_count += 1;
                    if let Some(seq) = frame.sequence_number() {
                        trace!(
                            seq = seq,
                            rto = ?rto,
                            "Registered packet with PacketTimerManager"
                        );
                    }
                }
            }
        }
        
        trace!(
            registered_count = registered_count,
            total_frames = frames.len(),
            "Completed PacketTimerManager registration for packetized frames"
        );
        
        registered_count
    }
    
    /// 异步添加单个数据包到PacketTimerManager
    /// Async add single packet to PacketTimerManager
    pub async fn add_packet_to_timer_manager(&mut self, frame: &crate::packet::frame::Frame) -> bool {
        if !frame.needs_reliability_tracking() {
            return false;
        }
        
        let now = Instant::now();
        let rto = self.rto_estimator.rto();
        
        let result = self.packet_timer_manager.add_packet(frame, now, rto).await;
        
        if result {
            if let Some(seq) = frame.sequence_number() {
                trace!(
                    seq = seq,
                    rto = ?rto,
                    "Added packet to PacketTimerManager async"
                );
            }
        }
        
        result
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
    
    /// 使用新的数据包定时器管理器添加数据包
    /// Add packet using new packet timer manager
    pub async fn add_packet_with_timer(&mut self, frame: &Frame) -> bool {
        let now = Instant::now();
        let rto = self.rto_estimator.rto();
        
        // 同时添加到传统管理器和新的数据包定时器管理器
        // Add to both traditional manager and new packet timer manager
        self.retransmission_manager.add_in_flight_packet(frame.clone(), now);
        self.packet_timer_manager.add_packet(frame, now, rto).await
    }
    
    /// 处理特定定时器的超时（新接口）
    /// Handle specific timer timeout (new interface)
    pub async fn handle_packet_timer_timeout(
        &mut self,
        timer_id: crate::timer::actor::ActorTimerId,
        context: &crate::packet::frame::RetransmissionContext,
    ) -> Option<Frame> {
        let rto = self.rto_estimator.rto();
        self.packet_timer_manager.handle_timer_timeout(timer_id, context, rto).await
    }
    
    /// 确认数据包（使用新接口）
    /// Acknowledge packet (using new interface)
    pub async fn acknowledge_packet_with_timer(&mut self, seq: u32) -> bool {
        self.packet_timer_manager.acknowledge_packet(seq).await
    }
    
    /// 批量确认数据包（使用新接口）
    /// Batch acknowledge packets (using new interface)
    pub async fn batch_acknowledge_packets_with_timer(&mut self, sequences: &[u32]) -> usize {
        self.packet_timer_manager.batch_acknowledge_packets(sequences).await
    }
    
    /// 获取数据包定时器管理器的在途数据包数量
    /// Get in-flight packet count from packet timer manager
    pub fn packet_timer_in_flight_count(&self) -> usize {
        self.packet_timer_manager.in_flight_count()
    }

    /// 批量取消重传定时器
    /// Batch cancel retransmission timers
    ///
    /// 当多个数据包被确认时，批量取消对应的RTO定时器
    /// When multiple packets are acknowledged, batch cancel corresponding RTO timers
    pub async fn batch_cancel_retransmission_timers(&mut self, seq_nums: &[u32]) -> usize {
        if seq_nums.is_empty() {
            return 0;
        }
        
        // 使用PacketTimerManager批量确认数据包（这会自动取消对应的定时器）
        // Use PacketTimerManager to batch acknowledge packets (this automatically cancels corresponding timers)
        let acked_count = self.packet_timer_manager.batch_acknowledge_packets(seq_nums).await;
        
        trace!(
            count = acked_count,
            "Batch cancelled retransmission timers using PacketTimerManager"
        );
        
        acked_count
    }
    
    /// 单个取消重传超时定时器（简化版本）
    /// Cancel single retransmission timeout timer (simplified version)
    pub async fn cancel_retransmission_timer(&mut self, seq_num: u32) -> bool {
        self.batch_cancel_retransmission_timers(&[seq_num]).await > 0
    }

    /// 清理所有重传定时器
    /// Clean up all retransmission timers
    ///
    /// 连接关闭时清理所有活跃的重传定时器
    /// Clean up all active retransmission timers when connection closes
    pub async fn cleanup_all_retransmission_timers(&mut self) {
        // 使用PacketTimerManager清理所有定时器
        // Use PacketTimerManager to clean up all timers
        self.packet_timer_manager.clear().await;
        
        trace!("Cleaned up all retransmission timers using PacketTimerManager");
    }

    /// 获取活跃重传定时器数量
    /// Get the number of active retransmission timers
    pub fn active_retransmission_timer_count(&self) -> usize {
        self.packet_timer_manager.in_flight_count()
    }
    
    /// 异步处理ACK确认后的PacketTimerManager更新
    /// Async processing of PacketTimerManager updates after ACK confirmation
    /// 
    /// 该方法应在处理ACK后被调用，用于异步更新PacketTimerManager状态
    /// This method should be called after processing ACK to asynchronously update PacketTimerManager state
    pub async fn process_ack_acknowledgment(&mut self, acked_sequences: &[u32]) -> usize {
        if acked_sequences.is_empty() {
            return 0;
        }
        
        trace!(
            sequences_count = acked_sequences.len(),
            sequences = ?acked_sequences,
            "Processing async PacketTimerManager acknowledgment"
        );
        
        // 使用PacketTimerManager进行批量确认
        // Use PacketTimerManager for batch acknowledgment
        let acked_count = self.packet_timer_manager.batch_acknowledge_packets(acked_sequences).await;
        
        trace!(
            acked_count = acked_count,
            "Completed async PacketTimerManager acknowledgment"
        );
        
        acked_count
    }
}


impl RetransmissionLayer for ReliabilityLayer {
    fn check_retransmissions(&mut self, _now: Instant, _context: &crate::packet::frame::RetransmissionContext) -> RetransmissionCheckResult {
        // 在新的重传系统中，重传由PacketTimerManager通过事件驱动方式处理
        // In the new retransmission system, retransmissions are handled by PacketTimerManager through event-driven approach
        trace!("check_retransmissions called on RetransmissionLayer, but retransmissions are now handled by PacketTimerManager");
        
        RetransmissionCheckResult {
            frames_to_retransmit: Vec::new(),
        }
    }
    
    fn next_retransmission_deadline(&self) -> Option<Instant> {
        // 在新的重传系统中，重传截止时间由PacketTimerManager管理
        // In the new retransmission system, retransmission deadlines are managed by PacketTimerManager
        trace!("next_retransmission_deadline called on RetransmissionLayer, but timing is now managed by PacketTimerManager");
        None
    }
}