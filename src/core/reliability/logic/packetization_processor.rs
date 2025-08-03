//! 打包处理器 - 专门处理数据打包逻辑
//! Packetization Processor - Specialized data packetization logic
//!
//! 职责：
//! - 将缓冲区数据转换为网络帧
//! - 应用流量控制和拥塞控制限制
//! - 处理0-RTT场景的智能分包
//! - 帧大小和分片管理

use super::super::data::buffer_stores::SendBufferStore;
use crate::packet::{frame::Frame, header::ShortHeader};
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
    /// 最大数据包大小（用于MTU分包）
    /// Maximum packet size (for MTU-based packetization)
    pub max_packet_size: usize,
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
    // 移除硬编码的估算值，改用精确计算
}

impl PacketizationProcessor {
    /// 创建新的打包处理器
    /// Create new packetization processor
    pub fn new() -> Self {
        Self {}
    }
    
    /// 计算PUSH帧的头部大小（精确计算）
    /// Calculate PUSH frame header size (exact calculation)
    fn calculate_push_frame_header_size() -> usize {
        // PUSH帧使用ShortHeader + command字节
        1 + ShortHeader::ENCODED_SIZE
    }
    
    /// 计算给定载荷大小的完整PUSH帧大小
    /// Calculate complete PUSH frame size for given payload size
    fn calculate_push_frame_size(payload_size: usize) -> usize {
        Self::calculate_push_frame_header_size() + payload_size
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
        
        let current_packet_size = frames.iter().map(|f| f.encoded_size()).sum::<usize>();
        
        // 生成PUSH帧，考虑MTU限制
        let (mut push_frames, consumed_bytes, mut limitation) = 
            self.generate_push_frames_with_mtu_limit(context, send_buffer, sequence_counter, current_packet_size);
        
        // 如果没有生成任何帧且缓冲区为空，更新限制原因
        if push_frames.is_empty() && limitation == PacketizationLimitation::None && send_buffer.is_empty() {
            limitation = PacketizationLimitation::BufferEmpty;
        }
        
        frames.append(&mut push_frames);
        
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
    
    /// 生成PUSH帧（考虑MTU限制）
    /// Generate PUSH frames (with MTU consideration)
    fn generate_push_frames_with_mtu_limit(
        &self,
        context: &PacketizationContext,
        send_buffer: &mut SendBufferStore,
        sequence_counter: &mut u32,
        initial_packet_size: usize,
    ) -> (Vec<Frame>, usize, PacketizationLimitation) {
        let mut frames = Vec::new();
        let mut consumed_bytes = 0;
        let mut current_packet_size = initial_packet_size;
        
        // 计算发送许可（基于流量控制）
        let permit = self.calculate_send_permit(context);
        
        let limitation = if permit == 0 {
            if send_buffer.is_empty() {
                PacketizationLimitation::BufferEmpty
            } else {
                self.determine_limitation_reason(context)
            }
        } else {
            PacketizationLimitation::None
        };
        
        // 生成数据帧，受流量控制和MTU限制
        let mut frames_generated = 0;
        while frames_generated < permit && !send_buffer.is_empty() {
            // 检查是否还有足够的包空间来放置新帧
            let push_frame_header_size = Self::calculate_push_frame_header_size();
            let remaining_packet_size = context.max_packet_size.saturating_sub(current_packet_size);
            
            if remaining_packet_size <= push_frame_header_size {
                // 包空间不足，无法添加更多帧
                break;
            }
            
            // 计算这个帧可以使用的最大载荷大小（考虑MTU限制）
            let max_payload_for_mtu = remaining_packet_size.saturating_sub(push_frame_header_size);
            let effective_max_payload = std::cmp::min(context.max_payload_size, max_payload_for_mtu);
            
            if effective_max_payload == 0 {
                break;
            }
            
            if let Some(chunk) = send_buffer.extract_chunk(effective_max_payload) {
                let seq = *sequence_counter;
                *sequence_counter += 1;
                let payload_size = chunk.len();
                
                let frame = Frame::new_push(
                    context.peer_cid,
                    seq,
                    context.ack_info.0,
                    context.ack_info.1,
                    context.timestamp,
                    chunk,
                );
                
                let frame_size = frame.encoded_size();
                consumed_bytes += payload_size;
                current_packet_size += frame_size;
                frames.push(frame);
                frames_generated += 1;
                
                trace!(
                    seq = seq,
                    payload_size = payload_size,
                    frame_size = frame_size,
                    current_packet_size = current_packet_size,
                    remaining_space = context.max_packet_size.saturating_sub(current_packet_size),
                    "Generated PUSH frame with MTU consideration"
                );
            } else {
                break;
            }
        }
        
        (frames, consumed_bytes, limitation)
    }
    
    /// 生成PUSH帧（不考虑MTU限制）
    /// Generate PUSH frames (without MTU consideration)
    fn generate_push_frames(
        &self,
        context: &PacketizationContext,
        send_buffer: &mut SendBufferStore,
        sequence_counter: &mut u32,
    ) -> (Vec<Frame>, usize, PacketizationLimitation) {
        let mut frames = Vec::new();
        let mut consumed_bytes = 0;
        
        // 计算发送许可（基于流量控制）
        let permit = self.calculate_send_permit(context);
        
        let limitation = if permit == 0 {
            if send_buffer.is_empty() {
                PacketizationLimitation::BufferEmpty
            } else {
                self.determine_limitation_reason(context)
            }
        } else {
            PacketizationLimitation::None
        };
        
        // 生成数据帧，只受流量控制限制
        let mut frames_generated = 0;
        while frames_generated < permit && !send_buffer.is_empty() {
            if let Some(chunk) = send_buffer.extract_chunk(context.max_payload_size) {
                let seq = *sequence_counter;
                *sequence_counter += 1;
                let payload_size = chunk.len();
                
                let frame = Frame::new_push(
                    context.peer_cid,
                    seq,
                    context.ack_info.0,
                    context.ack_info.1,
                    context.timestamp,
                    chunk,
                );
                
                consumed_bytes += payload_size;
                frames.push(frame);
                frames_generated += 1;
                
                trace!(
                    seq = seq,
                    payload_size = payload_size,
                    "Generated PUSH frame"
                );
            } else {
                break;
            }
        }
        
        (frames, consumed_bytes, limitation)
    }

    /// 执行0-RTT打包
    /// Perform 0-RTT packetization
    pub fn packetize_zero_rtt(
        &self,
        context: &PacketizationContext,
        send_buffer: &mut SendBufferStore,
        sequence_counter: &mut u32,
        syn_ack_frame: Frame,
    ) -> ZeroRttPacketizationResult {
        // 直接生成所有PUSH帧，不受MTU限制
        let (push_frames, consumed_bytes, limitation) = self.generate_push_frames(
            context,
            send_buffer,
            sequence_counter,
        );
        
        if push_frames.is_empty() {
            // 只有SYN-ACK，没有数据
            return ZeroRttPacketizationResult {
                packets: vec![vec![syn_ack_frame]],
                consumed_bytes: 0,
                limitation,
            };
        }
        
        // 智能分包：将SYN-ACK和PUSH帧分布到多个包中
        let packets = self.distribute_frames_to_packets(
            syn_ack_frame,
            push_frames,
            context.max_packet_size,
        );
        
        debug!(
            packets_count = packets.len(),
            consumed_bytes = consumed_bytes,
            limitation = ?limitation,
            "0-RTT packetization completed"
        );
        
        ZeroRttPacketizationResult {
            packets,
            consumed_bytes,
            limitation,
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
    
    /// 精确计算帧大小（不再是估算）
    /// Precisely calculate frame size (no longer an estimation)
    pub fn calculate_frame_size(payload_size: usize) -> usize {
        // 精确计算：PUSH帧头部 + 负载
        // Precise calculation: PUSH frame header + payload
        Self::calculate_push_frame_size(payload_size)
    }
    
    /// 获取处理器统计信息
    /// Get processor statistics
    pub fn get_statistics(&self) -> PacketizationStats {
        PacketizationStats {
            push_frame_header_size: Self::calculate_push_frame_header_size(),
        }
    }
}

/// 打包处理器统计信息
/// Packetization processor statistics
#[derive(Debug, Clone)]
pub struct PacketizationStats {
    /// PUSH帧头部的精确大小
    /// Exact size of PUSH frame header
    pub push_frame_header_size: usize,
}

impl Default for PacketizationProcessor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::reliability::data::buffer_stores::SendBufferStore;
    use crate::packet::frame::Frame;
    use bytes::Bytes;

    fn create_test_context() -> PacketizationContext {
        PacketizationContext {
            peer_cid: 1,
            timestamp: 123,
            congestion_window: 10,
            in_flight_count: 0,
            peer_recv_window: 10,
            max_payload_size: 100,
            max_packet_size: 1500,
            ack_info: (0, 1024),
        }
    }

    #[test]
    fn test_packetization_processor_basic_packetization() {
        let processor = PacketizationProcessor::new();
        let mut send_buffer = SendBufferStore::new(1024);
        let mut seq_counter = 0;
        
        send_buffer.write_data(Bytes::from("hello world"));
        
        let context = create_test_context();
        let result = processor.packetize(&context, &mut send_buffer, &mut seq_counter, None);
        
        assert_eq!(result.frames.len(), 1);
        assert_eq!(seq_counter, 1);
        assert!(result.consumed_bytes > 0);
        assert_eq!(result.limitation, PacketizationLimitation::None);
        
        if let Frame::Push { payload, header } = &result.frames[0] {
            assert_eq!(payload.as_ref(), b"hello world");
            assert_eq!(header.sequence_number, 0);
        } else {
            panic!("Expected a PUSH frame");
        }
    }

    #[test]
    fn test_packetization_processor_mtu_based_splitting() {
        let processor = PacketizationProcessor::new();
        let mut send_buffer = SendBufferStore::new(1024);
        let mut seq_counter = 0;
        
        // 添加足够的数据以超过包大小限制
        let large_data = "a".repeat(1000);
        send_buffer.write_data(Bytes::from(large_data));
        
        let mut context = create_test_context();
        context.max_packet_size = 200; // 小包大小以强制分包
        context.max_payload_size = 50;  // 小载荷大小
        
        let result = processor.packetize(&context, &mut send_buffer, &mut seq_counter, None);
        
        // 应该只生成一定数量的帧，由于MTU限制
        assert!(result.frames.len() <= 4); // 大约 (200 - 32) / (50 + 32) = ~2-3帧
        assert!(seq_counter > 0);
        
        // 验证帧大小不超过MTU
        let total_frame_size: usize = result.frames.iter().map(|f| f.encoded_size()).sum();
        assert!(total_frame_size <= context.max_packet_size);
    }

    #[test]
    fn test_packetization_processor_congestion_window_limit() {
        let processor = PacketizationProcessor::new();
        let mut send_buffer = SendBufferStore::new(1024);
        let mut seq_counter = 0;
        
        send_buffer.write_data(Bytes::from("hello world test data"));
        
        let mut context = create_test_context();
        context.congestion_window = 2; // 拥塞窗口限制
        context.in_flight_count = 0;
        
        let result = processor.packetize(&context, &mut send_buffer, &mut seq_counter, None);
        
        // 应该受拥塞窗口限制
        assert!(result.frames.len() <= 2);
        assert_eq!(result.limitation, PacketizationLimitation::None);
    }

    #[test]
    fn test_packetization_processor_flow_control_limit() {
        let processor = PacketizationProcessor::new();
        let mut send_buffer = SendBufferStore::new(1024);
        let mut seq_counter = 0;
        
        send_buffer.write_data(Bytes::from("hello world test data"));
        
        let mut context = create_test_context();
        context.peer_recv_window = 1; // 流量控制限制
        context.in_flight_count = 0;
        
        let result = processor.packetize(&context, &mut send_buffer, &mut seq_counter, None);
        
        // 应该受流量控制限制
        assert!(result.frames.len() <= 1);
        assert_eq!(result.limitation, PacketizationLimitation::None);
    }

    #[test]
    fn test_packetization_processor_empty_buffer() {
        let processor = PacketizationProcessor::new();
        let mut send_buffer = SendBufferStore::new(1024); // 空缓冲区
        let mut seq_counter = 0;
        
        let context = create_test_context();
        let result = processor.packetize(&context, &mut send_buffer, &mut seq_counter, None);
        
        assert_eq!(result.frames.len(), 0);
        assert_eq!(seq_counter, 0);
        assert_eq!(result.consumed_bytes, 0);
        assert_eq!(result.limitation, PacketizationLimitation::BufferEmpty);
    }

    #[test]
    fn test_packetization_processor_prepend_frame() {
        let processor = PacketizationProcessor::new();
        let mut send_buffer = SendBufferStore::new(1024);
        let mut seq_counter = 0;
        
        send_buffer.write_data(Bytes::from("hello"));
        
        let context = create_test_context();
        let fin_frame = Frame::new_fin(1, 99, 0, 1024, 123);
        let result = processor.packetize(&context, &mut send_buffer, &mut seq_counter, Some(fin_frame.clone()));
        
        assert_eq!(result.frames.len(), 2); // FIN + PUSH
        assert_eq!(seq_counter, 1);
        
        assert_eq!(result.frames[0], fin_frame);
        if let Frame::Push { header, .. } = &result.frames[1] {
            assert_eq!(header.sequence_number, 0);
        } else {
            panic!("Expected a PUSH frame");
        }
    }

    #[test]
    fn test_packetization_processor_zero_rtt_syn_ack_only() {
        let processor = PacketizationProcessor::new();
        let mut send_buffer = SendBufferStore::new(1024); // 空缓冲区
        let mut seq_counter = 0;
        
        let context = create_test_context();
        let syn_ack_frame = Frame::new_syn_ack(1, 2, 1);
        
        let result = processor.packetize_zero_rtt(&context, &mut send_buffer, &mut seq_counter, syn_ack_frame.clone());
        
        assert_eq!(result.packets.len(), 1);
        assert_eq!(result.packets[0].len(), 1);
        assert_eq!(result.packets[0][0], syn_ack_frame);
        assert_eq!(result.consumed_bytes, 0);
        assert_eq!(seq_counter, 0);
    }

    #[test]
    fn test_packetization_processor_zero_rtt_with_data() {
        let processor = PacketizationProcessor::new();
        let mut send_buffer = SendBufferStore::new(1024);
        let mut seq_counter = 0;
        
        send_buffer.write_data(Bytes::from("hello world"));
        
        let context = create_test_context();
        let syn_ack_frame = Frame::new_syn_ack(1, 2, 1);
        
        let result = processor.packetize_zero_rtt(&context, &mut send_buffer, &mut seq_counter, syn_ack_frame.clone());
        
        // 应该有数据包，第一个包含SYN-ACK
        assert!(!result.packets.is_empty());
        assert!(!result.packets[0].is_empty());
        assert_eq!(result.packets[0][0], syn_ack_frame);
        assert!(result.consumed_bytes > 0);
        assert!(seq_counter > 0);
        
        // 验证PUSH帧存在
        let has_push_frame = result.packets.iter().flatten().any(|frame| {
            matches!(frame, Frame::Push { .. })
        });
        assert!(has_push_frame);
    }

    #[test]
    fn test_packetization_processor_zero_rtt_multiple_packets() {
        let processor = PacketizationProcessor::new();
        let mut send_buffer = SendBufferStore::new(1024);
        let mut seq_counter = 0;
        
        // 添加大量数据以强制多包
        let large_data = "x".repeat(2000);
        send_buffer.write_data(Bytes::from(large_data));
        
        let mut context = create_test_context();
        context.max_packet_size = 500; // 小包大小
        context.max_payload_size = 100;
        
        let syn_ack_frame = Frame::new_syn_ack(1, 2, 1);
        
        let result = processor.packetize_zero_rtt(&context, &mut send_buffer, &mut seq_counter, syn_ack_frame.clone());
        
        // 应该有多个数据包
        assert!(result.packets.len() > 1);
        
        // 第一个数据包应该包含SYN-ACK
        assert_eq!(result.packets[0][0], syn_ack_frame);
        
        // 验证每个数据包的大小
        for packet in &result.packets {
            let packet_size: usize = packet.iter().map(|f| f.encoded_size()).sum();
            assert!(packet_size <= context.max_packet_size);
        }
    }

    #[test]
    fn test_packetization_processor_statistics() {
        let processor = PacketizationProcessor::new();
        let stats = processor.get_statistics();
        
        // PUSH帧头部 = 1字节命令 + ShortHeader大小
        let expected_header_size = 1 + ShortHeader::ENCODED_SIZE;
        assert_eq!(stats.push_frame_header_size, expected_header_size);
    }

    #[test]
    fn test_packetization_processor_frame_size_calculation() {
        let frame_size = PacketizationProcessor::calculate_frame_size(100);
        let expected_size = PacketizationProcessor::calculate_push_frame_header_size() + 100;
        assert_eq!(frame_size, expected_size);
    }
    
    #[test]
    fn test_packetization_processor_precise_header_calculation() {
        // 测试精确的头部大小计算
        let header_size = PacketizationProcessor::calculate_push_frame_header_size();
        
        // 验证计算是否正确：1字节命令 + ShortHeader
        assert_eq!(header_size, 1 + ShortHeader::ENCODED_SIZE);
        
        // 创建一个实际的PUSH帧来验证
        let frame = Frame::new_push(1, 0, 0, 1024, 123, Bytes::from("test"));
        let actual_header_size = frame.encoded_size() - 4; // 减去"test"的长度
        assert_eq!(header_size, actual_header_size);
    }

    #[test]
    fn test_packetization_processor_zero_permit_congestion() {
        let processor = PacketizationProcessor::new();
        let mut send_buffer = SendBufferStore::new(1024);
        let mut seq_counter = 0;
        
        send_buffer.write_data(Bytes::from("test data"));
        
        let mut context = create_test_context();
        context.congestion_window = 5;
        context.in_flight_count = 5; // 完全占用拥塞窗口
        
        let result = processor.packetize(&context, &mut send_buffer, &mut seq_counter, None);
        
        assert_eq!(result.frames.len(), 0);
        assert_eq!(result.limitation, PacketizationLimitation::CongestionWindow);
    }

    #[test]
    fn test_packetization_processor_zero_permit_flow_control() {
        let processor = PacketizationProcessor::new();
        let mut send_buffer = SendBufferStore::new(1024);
        let mut seq_counter = 0;
        
        send_buffer.write_data(Bytes::from("test data"));
        
        let mut context = create_test_context();
        context.peer_recv_window = 3;
        context.in_flight_count = 3; // 完全占用流量控制窗口
        
        let result = processor.packetize(&context, &mut send_buffer, &mut seq_counter, None);
        
        assert_eq!(result.frames.len(), 0);
        assert_eq!(result.limitation, PacketizationLimitation::FlowControl);
    }
}