//! 缓冲区协调器 - 统一协调发送和接收缓冲区
//! Buffer Coordinator - Unified coordination of send and receive buffers
//!
//! 职责：
//! - 协调发送和接收缓冲区的操作
//! - 管理数据流和重组
//! - 提供统一的缓冲区接口
//! - 处理缓冲区状态同步

use super::super::{
    data::buffer_stores::{SendBufferStore, ReceiveBufferStore, PacketOrFin},
    logic::packetization_processor::{PacketizationProcessor, PacketizationContext, PacketizationResult, ZeroRttPacketizationResult},
};
use crate::packet::{frame::Frame, sack::SackRange};
use bytes::Bytes;
use tracing::{debug, info, trace, warn};

/// 数据重组结果
/// Data reassembly result
#[derive(Debug)]
pub struct ReassemblyResult {
    /// 重组的数据
    /// Reassembled data
    pub data: Option<Vec<Bytes>>,
    /// 是否遇到FIN
    /// Whether FIN was encountered
    pub fin_seen: bool,
    /// 重组的数据包数量
    /// Number of reassembled packets
    pub packet_count: usize,
}

/// 缓冲区状态
/// Buffer status
#[derive(Debug, Clone)]
pub struct BufferStatus {
    /// 发送缓冲区状态
    /// Send buffer status
    pub send_buffer_status: SendBufferStatus,
    /// 接收缓冲区状态
    /// Receive buffer status
    pub receive_buffer_status: ReceiveBufferStatus,
}

/// 发送缓冲区状态
/// Send buffer status
#[derive(Debug, Clone)]
pub struct SendBufferStatus {
    pub used_bytes: usize,
    pub available_bytes: usize,
    pub total_capacity: usize,
    pub chunk_count: usize,
    pub is_empty: bool,
}

/// 接收缓冲区状态
/// Receive buffer status
#[derive(Debug, Clone)]
pub struct ReceiveBufferStatus {
    pub used_slots: usize,
    pub available_slots: usize,
    pub total_capacity: usize,
    pub next_sequence: u32,
    pub fin_reached: bool,
    pub is_empty: bool,
}

/// 缓冲区协调器
/// Buffer coordinator
#[derive(Debug)]
pub struct BufferCoordinator {
    /// 发送缓冲区存储
    /// Send buffer store
    send_buffer: SendBufferStore,
    
    /// 接收缓冲区存储
    /// Receive buffer store
    receive_buffer: ReceiveBufferStore,
    
    /// 打包处理器
    /// Packetization processor
    packetization_processor: PacketizationProcessor,
}

impl BufferCoordinator {
    /// 创建新的缓冲区协调器
    /// Create new buffer coordinator
    pub fn new(
        send_buffer_capacity: usize,
        receive_buffer_capacity: usize,
        max_frames_per_packetization: usize,
    ) -> Self {
        Self {
            send_buffer: SendBufferStore::new(send_buffer_capacity),
            receive_buffer: ReceiveBufferStore::new(receive_buffer_capacity),
            packetization_processor: PacketizationProcessor::new(max_frames_per_packetization),
        }
    }
    
    /// 写入数据到发送缓冲区
    /// Write data to send buffer
    pub fn write_to_send_buffer(&mut self, data: Bytes) -> usize {
        let written = self.send_buffer.write_data(data.clone());
        
        if written < data.len() {
            warn!(
                requested = data.len(),
                written = written,
                "Send buffer write partially successful due to capacity limit"
            );
        }
        
        trace!(written = written, "Data written to send buffer");
        written
    }
    
    /// 执行打包操作
    /// Perform packetization
    pub fn packetize(
        &mut self,
        context: &PacketizationContext,
        sequence_counter: &mut u32,
        prepend_frame: Option<Frame>,
    ) -> PacketizationResult {
        trace!(
            available_data = self.send_buffer.data_size(),
            seq_counter = *sequence_counter,
            "Starting packetization"
        );
        
        let result = self.packetization_processor.packetize(
            context,
            &mut self.send_buffer,
            sequence_counter,
            prepend_frame,
        );
        
        debug!(
            frames_generated = result.frames.len(),
            bytes_consumed = result.consumed_bytes,
            limitation = ?result.limitation,
            "Packetization completed"
        );
        
        result
    }
    
    /// 执行0-RTT打包操作
    /// Perform 0-RTT packetization
    pub fn packetize_zero_rtt(
        &mut self,
        context: &PacketizationContext,
        sequence_counter: &mut u32,
        syn_ack_frame: Frame,
        max_packet_size: usize,
    ) -> ZeroRttPacketizationResult {
        trace!(
            available_data = self.send_buffer.data_size(),
            seq_counter = *sequence_counter,
            max_packet_size = max_packet_size,
            "Starting 0-RTT packetization"
        );
        
        let result = self.packetization_processor.packetize_zero_rtt(
            context,
            &mut self.send_buffer,
            sequence_counter,
            syn_ack_frame,
            max_packet_size,
        );
        
        debug!(
            packets_generated = result.packets.len(),
            bytes_consumed = result.consumed_bytes,
            limitation = ?result.limitation,
            "0-RTT packetization completed"
        );
        
        result
    }
    
    /// 接收数据包到接收缓冲区
    /// Receive packet to receive buffer
    pub fn receive_packet(&mut self, sequence_number: u32, payload: Bytes) -> bool {
        let accepted = self.receive_buffer.store_packet(sequence_number, payload.clone());
        
        trace!(
            seq = sequence_number,
            payload_len = payload.len(),
            accepted = accepted,
            "Packet received"
        );
        
        accepted
    }
    
    /// 接收FIN到接收缓冲区
    /// Receive FIN to receive buffer
    pub fn receive_fin(&mut self, sequence_number: u32) -> bool {
        let accepted = self.receive_buffer.store_fin(sequence_number);
        
        trace!(
            seq = sequence_number,
            accepted = accepted,
            "FIN received"
        );
        
        accepted
    }
    
    /// 重组接收到的数据
    /// Reassemble received data
    pub fn reassemble_data(&mut self) -> ReassemblyResult {
        let mut reassembled_data = Vec::new();
        let mut fin_seen = false;
        let mut packet_count = 0;
        
        while let Some(packet) = self.receive_buffer.extract_next_contiguous() {
            packet_count += 1;
            
            match packet {
                PacketOrFin::Push(payload) => {
                    reassembled_data.push(payload);
                }
                PacketOrFin::Fin => {
                    fin_seen = true;
                    debug!("FIN encountered during data reassembly");
                    break;
                }
            }
        }
        
        let data_option = if reassembled_data.is_empty() {
            None
        } else {
            Some(reassembled_data)
        };
        
        if packet_count > 0 {
            debug!(
                packet_count = packet_count,
                fin_seen = fin_seen,
                "Data reassembly completed"
            );
        }
        
        ReassemblyResult {
            data: data_option,
            fin_seen,
            packet_count,
        }
    }
    
    /// 生成SACK范围
    /// Generate SACK ranges
    pub fn generate_sack_ranges(&self) -> Vec<SackRange> {
        let ranges = self.receive_buffer.generate_sack_ranges();
        
        trace!(ranges_count = ranges.len(), "SACK ranges generated");
        ranges
    }
    
    /// 获取接收窗口信息
    /// Get receive window information
    pub fn get_receive_window_info(&self) -> (u32, u16) {
        let next_seq = self.receive_buffer.next_sequence();
        let window_size = self.receive_buffer.window_size();
        
        (next_seq, window_size)
    }
    
    /// 检查发送缓冲区是否为空
    /// Check if send buffer is empty
    pub fn is_send_buffer_empty(&self) -> bool {
        self.send_buffer.is_empty()
    }
    
    /// 检查接收缓冲区是否为空
    /// Check if receive buffer is empty
    pub fn is_receive_buffer_empty(&self) -> bool {
        self.receive_buffer.is_empty()
    }
    
    /// 获取发送缓冲区可用空间
    /// Get send buffer available space
    pub fn send_buffer_available_space(&self) -> usize {
        self.send_buffer.available_space()
    }
    
    /// 清空发送缓冲区
    /// Clear send buffer
    pub fn clear_send_buffer(&mut self) {
        self.send_buffer.clear();
        info!("Send buffer cleared");
    }
    
    /// 清空接收缓冲区
    /// Clear receive buffer
    pub fn clear_receive_buffer(&mut self) {
        self.receive_buffer.clear();
        info!("Receive buffer cleared");
    }
    
    /// 清空所有缓冲区
    /// Clear all buffers
    pub fn clear_all_buffers(&mut self) {
        self.clear_send_buffer();
        self.clear_receive_buffer();
        info!("All buffers cleared");
    }
    
    /// 获取缓冲区状态
    /// Get buffer status
    pub fn get_buffer_status(&self) -> BufferStatus {
        let send_stats = self.send_buffer.get_stats();
        let receive_stats = self.receive_buffer.get_stats();
        
        BufferStatus {
            send_buffer_status: SendBufferStatus {
                used_bytes: send_stats.used_size,
                available_bytes: send_stats.available_size,
                total_capacity: send_stats.total_capacity,
                chunk_count: send_stats.chunk_count,
                is_empty: self.send_buffer.is_empty(),
            },
            receive_buffer_status: ReceiveBufferStatus {
                used_slots: receive_stats.used_slots,
                available_slots: receive_stats.available_slots,
                total_capacity: receive_stats.total_capacity,
                next_sequence: receive_stats.next_sequence,
                fin_reached: receive_stats.fin_reached,
                is_empty: self.receive_buffer.is_empty(),
            },
        }
    }
    
    /// 获取统计信息
    /// Get statistics
    pub fn get_statistics(&self) -> BufferCoordinatorStats {
        let status = self.get_buffer_status();
        let packetization_stats = self.packetization_processor.get_statistics();
        
        BufferCoordinatorStats {
            send_buffer_utilization: if status.send_buffer_status.total_capacity > 0 {
                (status.send_buffer_status.used_bytes as f64 / status.send_buffer_status.total_capacity as f64) * 100.0
            } else {
                0.0
            },
            receive_buffer_utilization: if status.receive_buffer_status.total_capacity > 0 {
                (status.receive_buffer_status.used_slots as f64 / status.receive_buffer_status.total_capacity as f64) * 100.0
            } else {
                0.0
            },
            send_buffer_chunks: status.send_buffer_status.chunk_count,
            receive_buffer_packets: status.receive_buffer_status.used_slots,
            max_frames_per_packetization: packetization_stats.max_frames_per_call,
        }
    }
}

/// 缓冲区协调器统计信息
/// Buffer coordinator statistics
#[derive(Debug, Clone)]
pub struct BufferCoordinatorStats {
    pub send_buffer_utilization: f64,
    pub receive_buffer_utilization: f64,
    pub send_buffer_chunks: usize,
    pub receive_buffer_packets: usize,
    pub max_frames_per_packetization: usize,
}

impl std::fmt::Display for BufferCoordinatorStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "BufferCoordinator[send:{:.1}%({} chunks), recv:{:.1}%({} packets), max_frames:{}]",
            self.send_buffer_utilization,
            self.send_buffer_chunks,
            self.receive_buffer_utilization,
            self.receive_buffer_packets,
            self.max_frames_per_packetization
        )
    }
}

impl Default for BufferCoordinator {
    fn default() -> Self {
        Self::new(
            1024 * 1024, // 1MB send buffer
            256,          // 256 packets receive buffer
            64,           // 64 frames per packetization
        )
    }
}