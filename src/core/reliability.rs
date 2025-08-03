//! 统一可靠性层 - 新的统一架构实现
//! Unified Reliability Layer - New unified architecture implementation
//!
//! 职责：
//! - 提供统一的可靠性管理接口
//! - 替换原有的双重管理系统
//! - 集成分层架构的所有组件
//! - 保持向后兼容性
//! - 集成RTT估算和精确重传定时器管理

pub mod logic;
pub mod coordination;
pub mod data;


use coordination::{
        packet_coordinator::PacketCoordinator,
        buffer_coordinator::{BufferCoordinator, ReassemblyResult},
        flow_control_coordinator::FlowControlCoordinator,
    };
use logic::{packetization_processor::PacketizationContext, rtt::RttEstimator};
use crate::{
    config::Config,
    core::endpoint::timing::TimeoutEvent,
    packet::{frame::{Frame, RetransmissionContext}, sack::SackRange},
    timer::{
        actor::{ActorTimerId, SenderTimerActorHandle},
        event::{ConnectionId, TimerEventData},
    },
};
use bytes::Bytes;
use std::time::Duration;
use tokio::time::Instant;
use tokio::sync::mpsc;
use tracing::{debug, info, trace};

/// 重传触发类型
/// Retransmission trigger type
#[derive(Debug, Clone, Copy)]
pub enum RetransmissionTrigger {
    /// 快速重传 - 基于SACK的重传
    /// Fast retransmission - SACK-based retransmission
    FastRetransmit,
    /// RTO超时重传 - 基于定时器的重传
    /// RTO timeout retransmission - timer-based retransmission
    RtoTimeout,
}

/// ACK处理结果（保持与原有接口兼容）
/// ACK processing result (maintaining compatibility with existing interface)
#[derive(Debug)]
pub struct AckProcessingResult {
    /// 需要重传的帧
    /// Frames to retransmit
    pub frames_to_retransmit: Vec<Frame>,
    
    /// 新确认的序列号
    /// Newly acknowledged sequence numbers
    pub newly_acked_sequences: Vec<u32>,
    
    /// RTT样本
    /// RTT samples
    pub rtt_samples: Vec<Duration>,
}

/// 统一可靠性层
/// Unified reliability layer
pub struct UnifiedReliabilityLayer {
    /// 数据包协调器 - 处理重传和定时器
    /// Packet coordinator - handles retransmission and timers
    packet_coordinator: PacketCoordinator,
    
    /// 缓冲区协调器 - 处理发送和接收缓冲区
    /// Buffer coordinator - handles send and receive buffers
    buffer_coordinator: BufferCoordinator,
    
    /// 流量控制协调器 - 处理拥塞控制和流量控制
    /// Flow control coordinator - handles congestion and flow control
    flow_control_coordinator: FlowControlCoordinator,
    
    /// RTT估算器 - 计算准确的RTO用于重传定时器
    /// RTT estimator - calculates accurate RTO for retransmission timers
    rtt_estimator: RttEstimator,
    
    /// 序列号计数器
    /// Sequence number counter
    sequence_counter: u32,
    
    /// FIN帧在途标志
    /// FIN frame in-flight flag
    fin_in_flight: bool,
    
    /// 配置信息
    /// Configuration
    config: Config,
}

impl UnifiedReliabilityLayer {
    /// 创建新的统一可靠性层
    /// Create new unified reliability layer
    pub fn new(
        connection_id: ConnectionId,
        timer_actor: SenderTimerActorHandle,
        timeout_tx: mpsc::Sender<TimerEventData<TimeoutEvent>>,
        config: Config,
    ) -> Self {
        Self {
            packet_coordinator: PacketCoordinator::new(
                connection_id,
                timer_actor,
                timeout_tx,
                config.reliability.fast_retx_threshold,
                config.reliability.handshake_data_max_retries,
            ),
            buffer_coordinator: BufferCoordinator::new(
                &config,
            ),
            flow_control_coordinator: FlowControlCoordinator::new(config.clone()),
            rtt_estimator: RttEstimator::new(config.reliability.initial_rto),
            sequence_counter: 0,
            fin_in_flight: false,
            config,
        }
    }
    
    /// 添加数据包到重传管理
    /// Add packet to retransmission management
    pub async fn add_in_flight_packet(&mut self, frame: &Frame, now: Instant) -> bool {
        // 使用RTT估算器计算的RTO，而不是传入的硬编码值
        // Use RTO calculated by RTT estimator instead of hardcoded input value
        let rto = self.rtt_estimator.rto();
        
        // 检查是否为FIN帧并更新状态
        // Check if this is a FIN frame and update status
        if matches!(frame, crate::packet::frame::Frame::Fin { .. }) {
            self.fin_in_flight = true;
            trace!(
                seq = frame.sequence_number(),
                "FIN frame added to in-flight tracking"
            );
        }
        
        trace!(
            seq = frame.sequence_number(),
            rto_ms = rto.as_millis(),
            estimated_rto = true,
            "Adding packet to unified reliability management with estimated RTO"
        );
        
        self.packet_coordinator.add_packet(frame, now, rto).await
    }
    
    /// 综合处理ACK
    /// Comprehensive ACK processing
    pub async fn handle_ack_comprehensive(
        &mut self,
        recv_next_seq: u32,
        sack_ranges: Vec<SackRange>,
        now: Instant,
        context: &RetransmissionContext,
    ) -> AckProcessingResult {
        debug!(
            recv_next_seq = recv_next_seq,
            sack_ranges_count = sack_ranges.len(),
            "Processing ACK with unified reliability layer"
        );
        
        // 步骤1：使用数据包协调器处理ACK
        let result = self.packet_coordinator.process_ack_comprehensive(
            recv_next_seq,
            sack_ranges,
            now,
            context,
        ).await;
        
        // 步骤2：更新RTT估算器（这是关键改进！）
        // Step 2: Update RTT estimator (this is the key improvement!)
        for rtt_sample in &result.rtt_samples {
            let old_rto = self.rtt_estimator.rto();
            self.rtt_estimator.update(*rtt_sample, self.config.reliability.min_rto);
            let new_rto = self.rtt_estimator.rto();
            
            trace!(
                rtt_sample_ms = rtt_sample.as_millis(),
                old_rto_ms = old_rto.as_millis(),
                new_rto_ms = new_rto.as_millis(),
                "RTT estimator updated with new sample"
            );
        }
        
        // 步骤3：更新流量控制状态
        // Step 3: Update flow control state
        if !result.rtt_samples.is_empty() {
            // 使用最新的RTT样本更新拥塞控制
            let latest_rtt = result.rtt_samples[result.rtt_samples.len() - 1];
            let flow_decision = self.flow_control_coordinator.handle_ack(latest_rtt, now);
            
            // 更新在途数据包数量
            self.flow_control_coordinator.update_in_flight_count(
                self.packet_coordinator.in_flight_count() as u32
            );
            
            trace!(
                rtt_ms = latest_rtt.as_millis(),
                new_cwnd = flow_decision.congestion_decision.new_congestion_window,
                send_permit = flow_decision.send_permit,
                "Flow control updated after ACK processing"
            );
        }
        
        // 步骤4：如果有数据包丢失（快速重传），通知拥塞控制
        // Step 4: If packets were lost (fast retransmission), notify congestion control
        if !result.frames_to_retransmit.is_empty() {
            let flow_decision = self.flow_control_coordinator.handle_packet_loss(now);
            debug!(
                trigger = ?RetransmissionTrigger::FastRetransmit,
                lost_packets = result.frames_to_retransmit.len(),
                new_cwnd = flow_decision.congestion_decision.new_congestion_window,
                "Congestion control updated due to fast retransmission packet loss"
            );
        }
        
        // 步骤5：检查FIN是否被确认
        // Step 5: Check if FIN was acknowledged
        if self.fin_in_flight && !result.newly_acked_sequences.is_empty() {
            // 简化实现：如果没有在途数据包，则清除FIN标志
            // Simplified implementation: if no in-flight packets, clear FIN flag
            if self.packet_coordinator.is_empty() {
                self.fin_in_flight = false;
                trace!("FIN frame acknowledged, clearing fin_in_flight flag");
            }
        }
        
        // 转换为兼容的结果格式
        AckProcessingResult {
            frames_to_retransmit: result.frames_to_retransmit,
            newly_acked_sequences: result.newly_acked_sequences,
            rtt_samples: result.rtt_samples,
        }
    }
    
    /// 处理特定定时器的超时
    /// Handle specific timer timeout
    pub async fn handle_packet_timer_timeout(
        &mut self,
        timer_id: ActorTimerId,
        context: &RetransmissionContext,
    ) -> Option<Frame> {
        // 使用RTT估算器的当前RTO值
        // Use current RTO value from RTT estimator
        let rto = self.rtt_estimator.rto();
        let now = Instant::now();
        
        trace!(
            timer_id = timer_id,
            rto_ms = rto.as_millis(),
            "Handling packet timer timeout with unified layer using estimated RTO"
        );
        
        let result = self.packet_coordinator.handle_timer_timeout(timer_id, context, rto, now).await;
        
        // 只在实际产生重传帧时进行RTO退避和拥塞控制更新
        // Only perform RTO backoff and congestion control update when actually producing retransmission frame
        if let Some(ref frame) = result {
            // RTO退避 - 仅在实际重传时执行一次
            // RTO backoff - execute only once when actually retransmitting
            let old_rto = self.rtt_estimator.rto();
            self.rtt_estimator.backoff();
            let new_rto = self.rtt_estimator.rto();
            
            debug!(
                timer_id = timer_id,
                seq = frame.sequence_number(),
                old_rto_ms = old_rto.as_millis(),
                new_rto_ms = new_rto.as_millis(),
                "RTO backoff applied after individual packet timeout"
            );
            
            // 通知流量控制发生丢包 - 使用context中的时间戳以保持一致性
            // Notify flow control of packet loss - use timestamp from context for consistency
            let now = tokio::time::Instant::now(); // TODO: 应从context获取统一时间戳
            let flow_decision = self.flow_control_coordinator.handle_packet_loss(now);
            debug!(
                trigger = ?RetransmissionTrigger::RtoTimeout,
                seq = frame.sequence_number(),
                new_cwnd = flow_decision.congestion_decision.new_congestion_window,
                "Flow control updated after individual RTO timeout packet loss"
            );
        }
        
        result
    }
    
    /// 检查RTO重传（备用轮询机制）
    /// Check RTO retransmission (backup polling mechanism)
    /// 
    /// 注意：在新的事件驱动架构中，这主要作为备用机制使用。
    /// 正常情况下，RTO重传由handle_packet_timer_timeout事件驱动处理。
    /// Note: In the new event-driven architecture, this is mainly used as a backup mechanism.
    /// Normally, RTO retransmissions are handled by handle_packet_timer_timeout event-driven approach.
    pub async fn check_rto_retransmission(
        &mut self,
        context: &RetransmissionContext,
        now: Instant,
    ) -> Vec<Frame> {
        // 使用RTT估算器的当前RTO值
        // Use current RTO value from RTT estimator
        let rto = self.rtt_estimator.rto();
        
        debug!(
            rto_ms = rto.as_millis(),
            "Checking RTO retransmission (backup mechanism) with unified layer"
        );
        
        let frames_to_retransmit = self.packet_coordinator.check_rto_retransmission(context, rto, now).await;
        
        // 在备用轮询机制中，避免重复的RTO退避
        // 因为单个包的RTO退避已经由handle_packet_timer_timeout处理
        // In backup polling mechanism, avoid duplicate RTO backoff
        // because individual packet RTO backoff is already handled by handle_packet_timer_timeout
        if !frames_to_retransmit.is_empty() {
            debug!(
                frames_to_retx = frames_to_retransmit.len(),
                current_rto_ms = rto.as_millis(),
                "RTO retransmission check found frames (backup mechanism) - RTO backoff handled per-packet"
            );
            
            // 通知流量控制发生批量丢包（使用统一时间戳）
            // Notify flow control of batch packet loss (using unified timestamp)
            let flow_decision = self.flow_control_coordinator.handle_packet_loss(now);
            debug!(
                trigger = ?RetransmissionTrigger::RtoTimeout,
                batch_size = frames_to_retransmit.len(),
                new_cwnd = flow_decision.congestion_decision.new_congestion_window,
                "Flow control updated after batch RTO timeout retransmission check"
            );
        }
        
        frames_to_retransmit
    }
    
    /// 写入数据到发送缓冲区
    /// Write data to send buffer
    pub fn write_to_send_buffer(&mut self, data: Bytes) -> usize {
        self.buffer_coordinator.write_to_send_buffer(data)
    }
    
    /// 执行打包操作
    /// Perform packetization
    pub fn packetize(
        &mut self,
        peer_cid: u32,
        timestamp: u32,
        prepend_frame: Option<Frame>,
    ) -> Vec<Frame> {
        let flow_state = self.flow_control_coordinator.get_flow_control_state();
        let (recv_next_seq, recv_window_size) = self.buffer_coordinator.get_receive_window_info();
        
        let context = PacketizationContext {
            peer_cid,
            timestamp,
            congestion_window: flow_state.congestion_window,
            in_flight_count: flow_state.in_flight_count as usize,
            peer_recv_window: flow_state.peer_receive_window,
            max_payload_size: self.config.connection.max_payload_size, // 从配置获取正确的值
            ack_info: (recv_next_seq, recv_window_size),
            max_packet_size: self.config.connection.max_packet_size,
        };
        
        let result = self.buffer_coordinator.packetize(
            &context,
            &mut self.sequence_counter,
            prepend_frame,
        );
        
        debug!(
            frames_generated = result.frames.len(),
            bytes_consumed = result.consumed_bytes,
            limitation = ?result.limitation,
            "Packetization completed"
        );
        
        result.frames
    }
    
    /// 执行0-RTT打包操作
    /// Perform 0-RTT packetization
    pub fn packetize_zero_rtt(
        &mut self,
        peer_cid: u32,
        timestamp: u32,
        syn_ack_frame: Frame,
    ) -> Vec<Vec<Frame>> {
        let flow_state = self.flow_control_coordinator.get_flow_control_state();
        let (recv_next_seq, recv_window_size) = self.buffer_coordinator.get_receive_window_info();
        
        let context = PacketizationContext {
            peer_cid,
            timestamp,
            congestion_window: flow_state.congestion_window,
            in_flight_count: flow_state.in_flight_count as usize,
            peer_recv_window: flow_state.peer_receive_window,
            max_payload_size: self.config.connection.max_payload_size, // 使用内部配置而不是传入的config
            ack_info: (recv_next_seq, recv_window_size),
            max_packet_size: self.config.connection.max_packet_size,
        };
        
        let result = self.buffer_coordinator.packetize_zero_rtt(
            &context,
            &mut self.sequence_counter,
            syn_ack_frame,
        );
        
        debug!(
            packets_generated = result.packets.len(),
            bytes_consumed = result.consumed_bytes,
            limitation = ?result.limitation,
            "0-RTT packetization completed"
        );
        
        result.packets
    }
    
    /// 接收数据包
    /// Receive packet
    pub fn receive_packet(&mut self, sequence_number: u32, payload: Bytes) -> bool {
        self.buffer_coordinator.receive_packet(sequence_number, payload)
    }
    
    /// 接收FIN
    /// Receive FIN
    pub fn receive_fin(&mut self, sequence_number: u32) -> bool {
        self.buffer_coordinator.receive_fin(sequence_number)
    }
    
    /// 重组数据
    /// Reassemble data
    pub fn reassemble_data(&mut self) -> ReassemblyResult {
        self.buffer_coordinator.reassemble_data()
    }
    
    /// 生成SACK范围
    /// Generate SACK ranges
    pub fn generate_sack_ranges(&self) -> Vec<SackRange> {
        self.buffer_coordinator.generate_sack_ranges()
    }
    
    /// 更新对端接收窗口
    /// Update peer receive window
    pub fn update_peer_receive_window(&mut self, window_size: u32) {
        self.flow_control_coordinator.update_peer_receive_window(window_size);
    }
    
    /// 获取接收窗口信息（公开版本）
    /// Get receive window info (public version)
    pub fn get_receive_window_info(&self) -> (u32, u16) {
        self.buffer_coordinator.get_receive_window_info()
    }
    
    /// 获取完整的ACK信息（新方法，替代get_ack_info）
    /// Get complete ACK information (new method, replaces get_ack_info)
    pub fn get_acknowledgment_info(&self) -> (Vec<SackRange>, u32, u16) {
        let (recv_next_seq, recv_window_size) = self.buffer_coordinator.get_receive_window_info();
        let sack_ranges = self.buffer_coordinator.generate_sack_ranges();
        (sack_ranges, recv_next_seq, recv_window_size)
    }
    
    /// 为多个帧批量注册重传定时器（新方法，替代register_timers_for_packetized_frames）
    /// Batch register retransmission timers for multiple frames (new method, replaces register_timers_for_packetized_frames)
    pub async fn track_frames_for_retransmission(&mut self, frames: &[Frame]) -> usize {
        let mut count = 0;
        let now = tokio::time::Instant::now();

        for frame in frames {
            // 使用统一的add_in_flight_packet方法，它会使用RTT估算器的RTO
            // Use unified add_in_flight_packet method, which uses RTT estimator's RTO
            if self.add_in_flight_packet(frame, now).await {
                count += 1;
            }
        }

        count
    }
    
    /// 获取在途数据包数量
    /// Get in-flight packet count
    pub fn in_flight_count(&self) -> usize {
        self.packet_coordinator.in_flight_count()
    }
    
    /// 检查在途数据包是否为空
    /// Check if in-flight packets are empty
    pub fn is_in_flight_empty(&self) -> bool {
        self.packet_coordinator.is_empty()
    }
    
    /// 检查发送缓冲区是否为空
    /// Check if send buffer is empty
    pub fn is_send_buffer_empty(&self) -> bool {
        self.buffer_coordinator.is_send_buffer_empty()
    }
    
    /// 获取发送缓冲区可用空间
    /// Get send buffer available space
    pub fn send_buffer_available_space(&self) -> usize {
        self.buffer_coordinator.send_buffer_available_space()
    }
    
    /// 清理所有在途数据包和定时器
    /// Clean up all in-flight packets and timers
    pub async fn cleanup_all_retransmission_timers(&mut self) {
        info!("Cleaning up all retransmission timers with unified layer");
        self.packet_coordinator.cleanup_all().await;
    }
    
    /// 清理所有缓冲区
    /// Clear all buffers
    pub fn clear_all_buffers(&mut self) {
        self.buffer_coordinator.clear_all_buffers();
    }
    
    /// 重置流量控制
    /// Reset flow control
    pub fn reset_flow_control(&mut self) {
        self.flow_control_coordinator.reset();
    }
    
    /// 获取活跃重传定时器数量
    /// Get active retransmission timer count
    pub fn active_retransmission_timer_count(&self) -> usize {
        self.packet_coordinator.get_statistics().active_timers
    }
    
    /// 获取统计信息
    /// Get statistics
    pub fn get_statistics(&self) -> UnifiedReliabilityStats {
        let packet_stats = self.packet_coordinator.get_statistics();
        let buffer_stats = self.buffer_coordinator.get_statistics();
        let flow_stats = self.flow_control_coordinator.get_statistics();
        
        UnifiedReliabilityStats {
            total_in_flight: packet_stats.total_in_flight,
            needs_retx_count: packet_stats.needs_retx_count,
            fast_retx_candidates: packet_stats.fast_retx_candidates,
            active_timers: packet_stats.active_timers,
            send_buffer_utilization: buffer_stats.send_buffer_utilization,
            receive_buffer_utilization: buffer_stats.receive_buffer_utilization,
            congestion_window: flow_stats.vegas_stats.congestion_window,
            send_permit: flow_stats.send_permit,
            sequence_counter: self.sequence_counter,
        }
    }
    
    /// 验证内部状态一致性（用于调试）
    /// Validate internal state consistency (for debugging)
    pub fn validate_consistency(&self) -> bool {
        let stats = self.get_statistics();
        
        // 基本一致性检查
        let is_consistent = stats.total_in_flight >= stats.needs_retx_count
            && stats.total_in_flight >= stats.fast_retx_candidates;
        
        if !is_consistent {
            tracing::error!(
                total_in_flight = stats.total_in_flight,
                needs_retx = stats.needs_retx_count,
                fast_retx_candidates = stats.fast_retx_candidates,
                active_timers = stats.active_timers,
                "Unified reliability layer state inconsistency detected"
            );
        }
        
        is_consistent
    }

    // === RTT和RTO相关方法 RTT and RTO Related Methods ===
    
    /// 获取当前RTO值
    /// Get current RTO value
    pub fn current_rto(&self) -> Duration {
        self.rtt_estimator.rto()
    }
    
    /// 获取平滑RTT估计
    /// Get smoothed RTT estimate
    pub fn smoothed_rtt(&self) -> Option<Duration> {
        self.rtt_estimator.smoothed_rtt()
    }
    
    /// 获取RTT变化值
    /// Get RTT variation
    pub fn rtt_var(&self) -> Option<Duration> {
        self.rtt_estimator.rtt_var()
    }
    
    /// 手动更新RTT样本（用于测试或特殊情况）
    /// Manually update RTT sample (for testing or special cases)
    pub fn update_rtt_sample(&mut self, rtt_sample: Duration) {
        self.rtt_estimator.update(rtt_sample, self.config.reliability.min_rto);
    }
    
    /// 手动触发RTO退避（用于测试或特殊情况）
    /// Manually trigger RTO backoff (for testing or special cases)
    pub fn trigger_rto_backoff(&mut self) {
        self.rtt_estimator.backoff();
    }
    /// 获取下一个序列号
    /// Get next sequence number
    pub fn next_sequence_number(&mut self) -> u32 {
        self.sequence_counter += 1;
        self.sequence_counter
    }
    
    /// 获取当前序列号（不递增）
    /// Get current sequence number (without incrementing)
    pub fn current_sequence_number(&self) -> u32 {
        self.sequence_counter
    }

    /// 检查是否有FIN在途
    /// Check if FIN is in flight
    pub fn has_fin_in_flight(&self) -> bool {
        self.fin_in_flight
    }
    
    /// 设置FIN在途状态
    /// Set FIN in-flight status
    pub fn set_fin_in_flight(&mut self, in_flight: bool) {
        self.fin_in_flight = in_flight;
    }

    /// 检查是否有ACK待发送
    /// Check if ACK is pending
    pub fn is_ack_pending(&self) -> bool {
        !self.buffer_coordinator.generate_sack_ranges().is_empty()
    }

    /// 检查接收缓冲区是否为空
    /// Check if receive buffer is empty
    pub fn is_recv_buffer_empty(&self) -> bool {
        self.buffer_coordinator.is_receive_buffer_empty()
    }

}

/// 统一可靠性层统计信息
/// Unified reliability layer statistics
#[derive(Debug, Clone)]
pub struct UnifiedReliabilityStats {
    /// 总在途数据包数
    /// Total in-flight packets
    pub total_in_flight: usize,
    
    /// 需要重传的数据包数
    /// Packets needing retransmission
    pub needs_retx_count: usize,
    
    /// 快速重传候选数
    /// Fast retransmission candidates
    pub fast_retx_candidates: usize,
    
    /// 活跃定时器数
    /// Active timers
    pub active_timers: usize,
    
    /// 发送缓冲区利用率
    /// Send buffer utilization
    pub send_buffer_utilization: f64,
    
    /// 接收缓冲区利用率
    /// Receive buffer utilization
    pub receive_buffer_utilization: f64,
    
    /// 拥塞窗口大小
    /// Congestion window size
    pub congestion_window: u32,
    
    /// 发送许可
    /// Send permit
    pub send_permit: u32,
    
    /// 当前序列号计数器
    /// Current sequence counter
    pub sequence_counter: u32,
}

impl std::fmt::Display for UnifiedReliabilityStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "UnifiedReliability[in_flight:{}, needs_retx:{}, fast_retx:{}, timers:{}, send_buf:{:.1}%, recv_buf:{:.1}%, cwnd:{}, permit:{}, seq:{}]",
            self.total_in_flight,
            self.needs_retx_count,
            self.fast_retx_candidates,
            self.active_timers,
            self.send_buffer_utilization,
            self.receive_buffer_utilization,
            self.congestion_window,
            self.send_permit,
            self.sequence_counter
        )
    }

}