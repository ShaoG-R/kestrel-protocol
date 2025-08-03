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
use crate::config::Config;
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
        config: &Config,
    ) -> Self {
        Self {
            send_buffer: SendBufferStore::new(config.reliability.send_buffer_capacity_bytes),
            receive_buffer: ReceiveBufferStore::new(config.reliability.recv_buffer_capacity_packets),
            packetization_processor: PacketizationProcessor::new(),
        }
    }
    
    /// 写入数据到发送缓冲区
    /// Write data to send buffer
    pub fn write_to_send_buffer(&mut self, data: Bytes) -> usize {
        let data_len = data.len();
        let written = self.send_buffer.write_data(data);
        
        if written < data_len {
            warn!(
                requested = data_len,
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
    ) -> ZeroRttPacketizationResult {
        trace!(
            available_data = self.send_buffer.data_size(),
            seq_counter = *sequence_counter,
            max_packet_size = context.max_packet_size,
            "Starting 0-RTT packetization"
        );
        
        let result = self.packetization_processor.packetize_zero_rtt(
            context,
            &mut self.send_buffer,
            sequence_counter,
            syn_ack_frame,
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
    
    /// 取出发送缓冲区中的所有数据
    /// Take all data from send buffer
    pub fn take_send_buffer_data(&mut self) -> impl Iterator<Item = Bytes> {
        info!("Taking all data from send buffer");
        self.send_buffer.take_all_data()
    }
    
    /// 检查接收缓冲区是否到达FIN
    /// Check if receive buffer has reached FIN
    pub fn is_receive_buffer_fin_reached(&self) -> bool {
        self.receive_buffer.is_fin_reached()
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
            estimated_frame_header_size: packetization_stats.estimated_frame_header_size,
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
    pub estimated_frame_header_size: usize,
}

impl std::fmt::Display for BufferCoordinatorStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "BufferCoordinator[send:{:.1}%({} chunks), recv:{:.1}%({} packets), frame_header:{}B]",
            self.send_buffer_utilization,
            self.send_buffer_chunks,
            self.receive_buffer_utilization,
            self.receive_buffer_packets,
            self.estimated_frame_header_size
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    fn create_test_coordinator() -> BufferCoordinator {
        BufferCoordinator {
            send_buffer: SendBufferStore::new(1024),
            receive_buffer: ReceiveBufferStore::new(10),
            packetization_processor: PacketizationProcessor::new(),
        }
    }

    fn create_test_context() -> PacketizationContext {
        PacketizationContext {
            peer_cid: 123,
            timestamp: 1000,
            congestion_window: 10,
            in_flight_count: 0,
            peer_recv_window: 10,
            max_payload_size: 64,
            max_packet_size: 1500, // 标准MTU大小
            ack_info: (0, 10),
        }
    }

    #[test]
    fn test_buffer_coordinator_basic_operations() {
        let mut coordinator = create_test_coordinator();
        
        // Test initial state
        assert!(coordinator.is_send_buffer_empty());
        assert!(coordinator.is_receive_buffer_empty());
        assert_eq!(coordinator.send_buffer_available_space(), 1024);
        
        // Test write to send buffer
        let data = Bytes::from("hello world");
        let written = coordinator.write_to_send_buffer(data);
        assert_eq!(written, 11);
        assert!(!coordinator.is_send_buffer_empty());
        assert_eq!(coordinator.send_buffer_available_space(), 1013);
    }

    #[test]
    fn test_buffer_coordinator_receive_operations() {
        let mut coordinator = create_test_coordinator();
        
        // Test receive packet
        assert!(coordinator.receive_packet(0, Bytes::from("hello")));
        assert!(coordinator.receive_packet(1, Bytes::from(" world")));
        assert!(!coordinator.receive_packet(0, Bytes::from("duplicate")));
        
        assert!(!coordinator.is_receive_buffer_empty());
        
        // Test reassembly
        let result = coordinator.reassemble_data();
        assert!(result.data.is_some());
        let data = result.data.unwrap();
        assert_eq!(data.len(), 2);
        assert_eq!(data[0], "hello");
        assert_eq!(data[1], " world");
        assert!(!result.fin_seen);
        assert_eq!(result.packet_count, 2);
    }

    #[test]
    fn test_buffer_coordinator_fin_handling() {
        let mut coordinator = create_test_coordinator();
        
        coordinator.receive_packet(0, Bytes::from("data"));
        coordinator.receive_fin(1);
        coordinator.receive_packet(2, Bytes::from("after fin"));
        
        let result = coordinator.reassemble_data();
        assert!(result.data.is_some());
        assert!(result.fin_seen);
        assert_eq!(result.packet_count, 2); // data + fin
        
        assert!(coordinator.is_receive_buffer_fin_reached());
        
        // After FIN, no more data should be available
        let result2 = coordinator.reassemble_data();
        assert!(result2.data.is_none());
        assert!(!result2.fin_seen);
        assert_eq!(result2.packet_count, 0);
    }

    #[test]
    fn test_buffer_coordinator_out_of_order_packets() {
        let mut coordinator = create_test_coordinator();
        
        // Receive out of order
        coordinator.receive_packet(2, Bytes::from("world"));
        coordinator.receive_packet(0, Bytes::from("hello"));
        coordinator.receive_packet(1, Bytes::from(" "));
        
        // Should reassemble in order
        let result = coordinator.reassemble_data();
        assert!(result.data.is_some());
        let data = result.data.unwrap();
        assert_eq!(data.len(), 3);
        assert_eq!(data[0], "hello");
        assert_eq!(data[1], " ");
        assert_eq!(data[2], "world");
    }

    #[test]
    fn test_buffer_coordinator_sack_ranges() {
        let mut coordinator = create_test_coordinator();
        
        // Create gaps in sequence
        coordinator.receive_packet(0, Bytes::new());
        coordinator.receive_packet(1, Bytes::new());
        coordinator.receive_packet(4, Bytes::new());
        coordinator.receive_packet(5, Bytes::new());
        coordinator.receive_packet(8, Bytes::new());
        
        // Reassemble contiguous part
        coordinator.reassemble_data();
        
        let sack_ranges = coordinator.generate_sack_ranges();
        assert_eq!(sack_ranges.len(), 2);
        assert_eq!(sack_ranges[0].start, 4);
        assert_eq!(sack_ranges[0].end, 5);
        assert_eq!(sack_ranges[1].start, 8);
        assert_eq!(sack_ranges[1].end, 8);
    }

    #[test]
    fn test_buffer_coordinator_receive_window_info() {
        let mut coordinator = create_test_coordinator();
        
        let (next_seq, window_size) = coordinator.get_receive_window_info();
        assert_eq!(next_seq, 0);
        assert_eq!(window_size, 10);
        
        coordinator.receive_packet(0, Bytes::from("test"));
        coordinator.receive_packet(2, Bytes::from("test2"));
        
        let (next_seq, window_size) = coordinator.get_receive_window_info();
        assert_eq!(next_seq, 0); // Still expecting 0 since we haven't reassembled
        assert_eq!(window_size, 8); // 2 packets buffered
        
        coordinator.reassemble_data(); // This should extract packet 0
        
        let (next_seq, window_size) = coordinator.get_receive_window_info();
        assert_eq!(next_seq, 1); // Now expecting 1
        assert_eq!(window_size, 9); // 1 packet still buffered
    }

    #[test]
    fn test_buffer_coordinator_packetization() {
        let mut coordinator = create_test_coordinator();
        let context = create_test_context();
        let mut sequence_counter = 0u32;
        
        // Add data to send buffer
        coordinator.write_to_send_buffer(Bytes::from("hello"));
        coordinator.write_to_send_buffer(Bytes::from(" world"));
        
        // Test basic packetization
        let result = coordinator.packetize(&context, &mut sequence_counter, None);
        
        assert!(!result.frames.is_empty());
        assert!(result.consumed_bytes > 0);
        assert_eq!(sequence_counter, result.frames.len() as u32);
    }

    #[test]
    fn test_buffer_coordinator_take_send_buffer_data() {
        let mut coordinator = create_test_coordinator();
        
        coordinator.write_to_send_buffer(Bytes::from("hello"));
        coordinator.write_to_send_buffer(Bytes::from(" world"));
        
        assert!(!coordinator.is_send_buffer_empty());
        
        let all_data: Vec<Bytes> = coordinator.take_send_buffer_data().collect();
        assert_eq!(all_data.len(), 2);
        assert_eq!(all_data[0], "hello");
        assert_eq!(all_data[1], " world");
        
        assert!(coordinator.is_send_buffer_empty());
    }

    #[test]
    fn test_buffer_coordinator_clear_operations() {
        let mut coordinator = create_test_coordinator();
        
        // Add data to both buffers
        coordinator.write_to_send_buffer(Bytes::from("send data"));
        coordinator.receive_packet(0, Bytes::from("receive data"));
        
        assert!(!coordinator.is_send_buffer_empty());
        assert!(!coordinator.is_receive_buffer_empty());
        
        // Test individual clears
        coordinator.clear_send_buffer();
        assert!(coordinator.is_send_buffer_empty());
        assert!(!coordinator.is_receive_buffer_empty());
        
        coordinator.clear_receive_buffer();
        assert!(coordinator.is_receive_buffer_empty());
        
        // Test clear all
        coordinator.write_to_send_buffer(Bytes::from("send data"));
        coordinator.receive_packet(0, Bytes::from("receive data"));
        
        coordinator.clear_all_buffers();
        assert!(coordinator.is_send_buffer_empty());
        assert!(coordinator.is_receive_buffer_empty());
    }

    #[test]
    fn test_buffer_coordinator_status_and_statistics() {
        let mut coordinator = create_test_coordinator();
        
        // Add some data
        coordinator.write_to_send_buffer(Bytes::from("test data"));
        coordinator.receive_packet(0, Bytes::from("recv data"));
        coordinator.receive_packet(2, Bytes::from("recv data2"));
        
        let status = coordinator.get_buffer_status();
        assert_eq!(status.send_buffer_status.used_bytes, 9);
        assert_eq!(status.send_buffer_status.total_capacity, 1024);
        assert!(!status.send_buffer_status.is_empty);
        
        assert_eq!(status.receive_buffer_status.used_slots, 2);
        assert_eq!(status.receive_buffer_status.total_capacity, 10);
        assert!(!status.receive_buffer_status.is_empty);
        assert!(!status.receive_buffer_status.fin_reached);
        
        let stats = coordinator.get_statistics();
        assert!(stats.send_buffer_utilization > 0.0);
        assert!(stats.receive_buffer_utilization > 0.0);
        assert_eq!(stats.send_buffer_chunks, 1);
        assert_eq!(stats.receive_buffer_packets, 2);
    }

    #[test]
    fn test_buffer_coordinator_send_buffer_capacity_limits() {
        let mut coordinator = BufferCoordinator {
            send_buffer: SendBufferStore::new(10), // Small send buffer
            receive_buffer: ReceiveBufferStore::new(10),
            packetization_processor: PacketizationProcessor::new(),
        };
        
        let data1 = Bytes::from("hello");
        let data2 = Bytes::from("world");
        let data3 = Bytes::from("!!!");
        
        assert_eq!(coordinator.write_to_send_buffer(data1), 5);
        assert_eq!(coordinator.write_to_send_buffer(data2), 5);
        assert_eq!(coordinator.write_to_send_buffer(data3), 0); // Should be rejected
        
        assert_eq!(coordinator.send_buffer_available_space(), 0);
    }

    #[test]
    fn test_buffer_coordinator_display_statistics() {
        let coordinator = create_test_coordinator();
        let stats = coordinator.get_statistics();
        
        let display_str = format!("{}", stats);
        assert!(display_str.contains("BufferCoordinator"));
        assert!(display_str.contains("send:"));
        assert!(display_str.contains("recv:"));
        assert!(display_str.contains("frame_header:"));
    }
}