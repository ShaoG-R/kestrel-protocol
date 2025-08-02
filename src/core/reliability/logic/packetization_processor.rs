//! 打包处理器 - 专门处理数据打包逻辑
//! Packetization Processor - Specialized data packetization logic
//!
//! 职责：
//! - 将缓冲区数据转换为网络帧
//! - 应用流量控制和拥塞控制限制
//! - 处理0-RTT场景的智能分包
//! - 帧大小和分片管理

use super::super::data::buffer_stores::SendBufferStore;
use crate::packet::frame::Frame;
use tracing::{debug, trace, warn};

/// 打包上下文
/// Packetization context
#[derive(Debug, Clone)]
pub struct PacketizationContext {
    /// 对端连接ID
    /// Peer connection ID
    pub peer_cid: u32,
    /// 时间戳
    /// Timestamp
    pub timestamp: u32,
    /// 拥塞窗口
    /// Congestion window
    pub congestion_window: u32,
    /// 在途数据包数量
    /// In-flight packet count
    pub in_flight_count: usize,
    /// 对端接收窗口
    /// Peer receive window
    pub peer_recv_window: u32,
    /// 最大负载大小
    /// Maximum payload size
    pub max_payload_size: usize,
    /// ACK信息 (recv_next_sequence, local_window_size)
    /// ACK information
    pub ack_info: (u32, u16),
}

/// 打包结果
/// Packetization result
#[derive(Debug)]
pub struct PacketizationResult {
    /// 生成的帧
    /// Generated frames
    pub frames: Vec<Frame>,
    /// 消耗的字节数
    /// Consumed bytes
    pub consumed_bytes: usize,
    /// 受限原因
    /// Limitation reason
    pub limitation: PacketizationLimitation,
}

/// 打包限制原因
/// Packetization limitation reason
#[derive(Debug, Clone, PartialEq)]
pub enum PacketizationLimitation {
    /// 无限制
    /// No limitation
    None,
    /// 拥塞窗口限制
    /// Congestion window limited
    CongestionWindow,
    /// 流量控制限制
    /// Flow control limited
    FlowControl,
    /// 缓冲区为空
    /// Buffer empty
    BufferEmpty,
    /// 最大帧数限制
    /// Maximum frame count limited
    MaxFrameCount,
}

/// 0-RTT打包结果
/// 0-RTT packetization result
#[derive(Debug)]
pub struct ZeroRttPacketizationResult {
    /// 打包的数据包列表（每个包含多个帧）
    /// Packetized packet list (each containing multiple frames)
    pub packets: Vec<Vec<Frame>>,
    /// 消耗的字节数
    /// Consumed bytes
    pub consumed_bytes: usize,
    /// 限制原因
    /// Limitation reason
    pub limitation: PacketizationLimitation,
}

/// 打包处理器
/// Packetization processor
#[derive(Debug)]
pub struct PacketizationProcessor {
    /// 最大帧数限制
    /// Maximum frame count limit
    max_frames_per_call: usize,
}

impl PacketizationProcessor {
    /// 创建新的打包处理器
    /// Create new packetization processor
    pub fn new(max_frames_per_call: usize) -> Self {
        Self {
            max_frames_per_call,
        }
    }
    
    /// 执行标准打包
    /// Perform standard packetization
    pub fn packetize(
        &self,
        context: &PacketizationContext,
        send_buffer: &mut SendBufferStore,
        sequence_counter: &mut u32,
        prepend_frame: Option<Frame>,
    ) -> PacketizationResult {
        let mut frames = if let Some(frame) = prepend_frame {
            vec![frame]
        } else {
            Vec::new()
        };
        
        let mut consumed_bytes = 0;
        
        // 计算发送许可
        let permit = self.calculate_send_permit(context);
        let effective_permit = std::cmp::min(permit, self.max_frames_per_call);
        
        let limitation = if permit == 0 {
            if send_buffer.is_empty() {
                PacketizationLimitation::BufferEmpty
            } else {
                self.determine_limitation_reason(context)
            }
        } else if effective_permit < permit {
            PacketizationLimitation::MaxFrameCount
        } else {
            PacketizationLimitation::None
        };
        
        // 生成数据帧
        for _ in 0..effective_permit {
            if send_buffer.is_empty() {
                break;
            }
            
            if let Some(chunk) = send_buffer.extract_chunk(context.max_payload_size) {
                let seq = *sequence_counter;
                *sequence_counter += 1;
                
                let frame = Frame::new_push(
                    context.peer_cid,
                    seq,
                    context.ack_info.0, // recv_next_sequence
                    context.ack_info.1, // recv_window_size
                    context.timestamp,
                    chunk.clone(),
                );
                
                consumed_bytes += chunk.len();
                frames.push(frame);
                
                trace!(
                    seq = seq,
                    payload_len = chunk.len(),
                    "Generated PUSH frame during packetization"
                );
            } else {
                break;
            }
        }
        
        debug!(
            frames_generated = frames.len(),
            consumed_bytes = consumed_bytes,
            limitation = ?limitation,
            "Standard packetization completed"
        );
        
        PacketizationResult {
            frames,
            consumed_bytes,
            limitation,
        }
    }
    
    /// 执行0-RTT打包
    /// Perform 0-RTT packetization
    pub fn packetize_zero_rtt(
        &self,
        context: &PacketizationContext,
        send_buffer: &mut SendBufferStore,
        sequence_counter: &mut u32,
        syn_ack_frame: Frame,
        max_packet_size: usize,
    ) -> ZeroRttPacketizationResult {
        // 首先生成所有PUSH帧
        let packetization_result = self.packetize(
            context,
            send_buffer,
            sequence_counter,
            None,
        );
        
        if packetization_result.frames.is_empty() {
            // 只有SYN-ACK，没有数据
            return ZeroRttPacketizationResult {
                packets: vec![vec![syn_ack_frame]],
                consumed_bytes: 0,
                limitation: packetization_result.limitation,
            };
        }
        
        // 智能分包：将SYN-ACK和PUSH帧分布到多个包中
        let packets = self.distribute_frames_to_packets(
            syn_ack_frame,
            packetization_result.frames,
            max_packet_size,
        );
        
        debug!(
            packets_count = packets.len(),
            consumed_bytes = packetization_result.consumed_bytes,
            limitation = ?packetization_result.limitation,
            "0-RTT packetization completed"
        );
        
        ZeroRttPacketizationResult {
            packets,
            consumed_bytes: packetization_result.consumed_bytes,
            limitation: packetization_result.limitation,
        }
    }
    
    /// 计算发送许可
    /// Calculate send permit
    fn calculate_send_permit(&self, context: &PacketizationContext) -> usize {
        let cwnd_permit = context.congestion_window
            .saturating_sub(context.in_flight_count as u32) as usize;
        let flow_permit = context.peer_recv_window
            .saturating_sub(context.in_flight_count as u32) as usize;
        
        let permit = std::cmp::min(cwnd_permit, flow_permit);
        
        trace!(
            cwnd_permit = cwnd_permit,
            flow_permit = flow_permit,
            final_permit = permit,
            "Send permit calculated"
        );
        
        permit
    }
    
    /// 确定限制原因
    /// Determine limitation reason
    fn determine_limitation_reason(&self, context: &PacketizationContext) -> PacketizationLimitation {
        let cwnd_permit = context.congestion_window
            .saturating_sub(context.in_flight_count as u32);
        let flow_permit = context.peer_recv_window
            .saturating_sub(context.in_flight_count as u32);
        
        if cwnd_permit == 0 {
            PacketizationLimitation::CongestionWindow
        } else if flow_permit == 0 {
            PacketizationLimitation::FlowControl
        } else {
            PacketizationLimitation::None
        }
    }
    
    /// 将帧分布到数据包中
    /// Distribute frames to packets
    fn distribute_frames_to_packets(
        &self,
        syn_ack_frame: Frame,
        push_frames: Vec<Frame>,
        max_packet_size: usize,
    ) -> Vec<Vec<Frame>> {
        let syn_ack_size = syn_ack_frame.encoded_size();
        
        if syn_ack_size > max_packet_size {
            warn!(
                syn_ack_size = syn_ack_size,
                max_packet_size = max_packet_size,
                "SYN-ACK frame exceeds maximum packet size"
            );
            return vec![vec![syn_ack_frame]];
        }
        
        let mut packets = Vec::new();
        let mut current_packet = vec![syn_ack_frame];
        let mut current_size = syn_ack_size;
        
        // 尝试在第一个包中尽可能多地放入PUSH帧
        let mut remaining_frames = push_frames.into_iter();
        
        // 填充第一个包
        for push_frame in remaining_frames.by_ref() {
            let frame_size = push_frame.encoded_size();
            
            if current_size + frame_size <= max_packet_size {
                current_packet.push(push_frame);
                current_size += frame_size;
            } else {
                // 当前帧不适合，完成第一个包并开始新包
                packets.push(current_packet);
                current_packet = vec![push_frame];
                current_size = frame_size;
                break;
            }
        }
        
        // 处理剩余帧
        for push_frame in remaining_frames {
            let frame_size = push_frame.encoded_size();
            
            if current_size + frame_size <= max_packet_size {
                current_packet.push(push_frame);
                current_size += frame_size;
            } else {
                // 当前包满了，开始新包
                packets.push(current_packet);
                current_packet = vec![push_frame];
                current_size = frame_size;
            }
        }
        
        // 添加最后一个包
        if !current_packet.is_empty() {
            packets.push(current_packet);
        }
        
        trace!(
            total_packets = packets.len(),
            "Frames distributed to packets for 0-RTT"
        );
        
        packets
    }
    
    /// 估算帧大小
    /// Estimate frame size
    pub fn estimate_frame_size(payload_size: usize) -> usize {
        // 简化的帧大小估算：头部 + 负载
        // Simplified frame size estimation: header + payload
        const ESTIMATED_HEADER_SIZE: usize = 32; // 估算的头部大小
        ESTIMATED_HEADER_SIZE + payload_size
    }
    
    /// 获取处理器统计信息
    /// Get processor statistics
    pub fn get_statistics(&self) -> PacketizationStats {
        PacketizationStats {
            max_frames_per_call: self.max_frames_per_call,
        }
    }
}

/// 打包处理器统计信息
/// Packetization processor statistics
#[derive(Debug, Clone)]
pub struct PacketizationStats {
    pub max_frames_per_call: usize,
}

impl Default for PacketizationProcessor {
    fn default() -> Self {
        Self::new(64) // 默认最大帧数
    }
}