//! 缓冲区数据存储 - 纯数据管理层
//! Buffer Data Stores - Pure data management layer
//!
//! 职责：
//! - 发送和接收缓冲区的数据存储
//! - 无业务逻辑，只管理数据状态
//! - 为上层逻辑提供数据访问接口

use crate::packet::sack::SackRange;
use bytes::Bytes;
use std::collections::{btree_map::Entry, BTreeMap, VecDeque};
use tracing::{debug, trace};

/// 数据包或FIN标记
/// Packet or FIN marker
#[derive(Debug, Clone)]
pub enum PacketOrFin {
    /// 普通数据包
    /// Regular data packet
    Push(Bytes),
    /// 流结束标记
    /// End of stream marker
    Fin,
}

/// 发送缓冲区存储
/// Send buffer store
#[derive(Debug)]
pub struct SendBufferStore {
    /// 待打包的数据队列
    /// Queue of data waiting to be packetized
    stream_buffer: VecDeque<Bytes>,
    /// 缓冲区中数据的总大小
    /// Total size of data in buffer
    stream_buffer_size: usize,
    /// 缓冲区容量
    /// Buffer capacity
    stream_buffer_capacity: usize,
}

impl SendBufferStore {
    /// 创建新的发送缓冲区存储
    /// Create new send buffer store
    pub fn new(capacity_bytes: usize) -> Self {
        Self {
            stream_buffer: VecDeque::new(),
            stream_buffer_size: 0,
            stream_buffer_capacity: capacity_bytes,
        }
    }
    
    /// 写入数据到缓冲区
    /// Write data to buffer
    pub fn write_data(&mut self, data: Bytes) -> usize {
        let space_available = self.stream_buffer_capacity
            .saturating_sub(self.stream_buffer_size);
        
        if space_available == 0 {
            return 0;
        }
        
        let bytes_to_write = std::cmp::min(data.len(), space_available);
        if bytes_to_write < data.len() {
            let chunk = data.slice(..bytes_to_write);
            self.stream_buffer.push_back(chunk);
        } else {
            self.stream_buffer.push_back(data);
        }
        
        self.stream_buffer_size += bytes_to_write;
        trace!(written = bytes_to_write, total_size = self.stream_buffer_size, "Data written to send buffer store");
        bytes_to_write
    }
    
    /// 从缓冲区提取数据块
    /// Extract data chunk from buffer
    pub fn extract_chunk(&mut self, max_size: usize) -> Option<Bytes> {
        let first_chunk = self.stream_buffer.front_mut()?;
        let chunk_size = std::cmp::min(first_chunk.len(), max_size);
        
        if chunk_size == 0 {
            self.stream_buffer.pop_front();
            return self.extract_chunk(max_size);
        }
        
        let chunk = if chunk_size >= first_chunk.len() {
            self.stream_buffer.pop_front()?
        } else {
            first_chunk.split_to(chunk_size)
        };
        
        self.stream_buffer_size -= chunk.len();
        trace!(extracted = chunk.len(), remaining_size = self.stream_buffer_size, "Data extracted from send buffer store");
        Some(chunk)
    }
    
    /// 获取缓冲区可用空间
    /// Get available buffer space
    pub fn available_space(&self) -> usize {
        self.stream_buffer_capacity.saturating_sub(self.stream_buffer_size)
    }
    
    /// 获取缓冲区数据大小
    /// Get buffer data size
    pub fn data_size(&self) -> usize {
        self.stream_buffer_size
    }
    
    /// 检查缓冲区是否为空
    /// Check if buffer is empty
    pub fn is_empty(&self) -> bool {
        self.stream_buffer.is_empty()
    }
    
    /// 清空缓冲区
    /// Clear buffer
    pub fn clear(&mut self) {
        self.stream_buffer.clear();
        self.stream_buffer_size = 0;
        debug!("Send buffer store cleared");
    }
    
    /// 获取缓冲区统计信息
    /// Get buffer statistics
    pub fn get_stats(&self) -> SendBufferStats {
        SendBufferStats {
            total_capacity: self.stream_buffer_capacity,
            used_size: self.stream_buffer_size,
            available_size: self.available_space(),
            chunk_count: self.stream_buffer.len(),
        }
    }
}

/// 接收缓冲区存储
/// Receive buffer store
#[derive(Debug)]
pub struct ReceiveBufferStore {
    /// 下一个期望的序列号
    /// Next expected sequence number
    next_sequence: u32,
    /// 乱序数据包存储
    /// Out-of-order packet storage
    received: BTreeMap<u32, PacketOrFin>,
    /// 缓冲区容量（按数据包数量）
    /// Buffer capacity (in packets)
    capacity: usize,
    /// FIN是否已处理
    /// Whether FIN has been processed
    fin_reached: bool,
}

impl ReceiveBufferStore {
    /// 创建新的接收缓冲区存储
    /// Create new receive buffer store
    pub fn new(capacity_packets: usize) -> Self {
        Self {
            next_sequence: 0,
            received: BTreeMap::new(),
            capacity: capacity_packets,
            fin_reached: false,
        }
    }
    
    /// 存储接收到的数据包
    /// Store received data packet
    pub fn store_packet(&mut self, sequence_number: u32, payload: Bytes) -> bool {
        if self.fin_reached || sequence_number < self.next_sequence {
            return false;
        }
        
        match self.received.entry(sequence_number) {
            Entry::Vacant(entry) => {
                let len = payload.len();
                entry.insert(PacketOrFin::Push(payload));
                trace!(seq = sequence_number, payload_len = len, "Packet stored in receive buffer");
                true
            }
            Entry::Occupied(_) => {
                trace!(seq = sequence_number, "Duplicate packet ignored");
                false
            }
        }
    }
    
    /// 存储FIN标记
    /// Store FIN marker
    pub fn store_fin(&mut self, sequence_number: u32) -> bool {
        if self.fin_reached || sequence_number < self.next_sequence {
            return false;
        }
        
        match self.received.entry(sequence_number) {
            Entry::Vacant(entry) => {
                entry.insert(PacketOrFin::Fin);
                trace!(seq = sequence_number, "FIN stored in receive buffer");
                true
            }
            Entry::Occupied(_) => {
                trace!(seq = sequence_number, "Duplicate FIN ignored");
                false
            }
        }
    }
    
    /// 尝试提取下一个连续的数据包
    /// Try to extract next contiguous packet
    pub fn extract_next_contiguous(&mut self) -> Option<PacketOrFin> {
        if let Some((&seq, _)) = self.received.first_key_value() {
            if seq == self.next_sequence {
                self.next_sequence += 1;
                let packet = self.received.pop_first().map(|(_, packet)| packet)?;
                
                if matches!(packet, PacketOrFin::Fin) {
                    self.fin_reached = true;
                    self.received.clear();
                    debug!("FIN processed, receive buffer cleared");
                }
                
                trace!(seq = seq, "Extracted contiguous packet from receive buffer");
                return Some(packet);
            }
        }
        None
    }
    
    /// 生成SACK范围
    /// Generate SACK ranges
    pub fn generate_sack_ranges(&self) -> Vec<SackRange> {
        let mut ranges = Vec::new();
        let mut current_range: Option<SackRange> = None;
        
        for &seq in self.received.keys() {
            match current_range.as_mut() {
                Some(range) => {
                    if seq == range.end + 1 {
                        range.end = seq;
                    } else {
                        ranges.push(range.clone());
                        current_range = Some(SackRange { start: seq, end: seq });
                    }
                }
                None => {
                    current_range = Some(SackRange { start: seq, end: seq });
                }
            }
        }
        
        if let Some(range) = current_range {
            ranges.push(range);
        }
        
        trace!(ranges_count = ranges.len(), "Generated SACK ranges");
        ranges
    }
    
    /// 获取接收窗口大小
    /// Get receive window size
    pub fn window_size(&self) -> u16 {
        (self.capacity.saturating_sub(self.received.len())) as u16
    }
    
    /// 获取下一个期望序列号
    /// Get next expected sequence number
    pub fn next_sequence(&self) -> u32 {
        self.next_sequence
    }
    
    /// 检查是否已到达FIN
    /// Check if FIN has been reached
    pub fn is_fin_reached(&self) -> bool {
        self.fin_reached
    }
    
    /// 检查是否为空
    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.received.is_empty()
    }
    
    /// 清空缓冲区
    /// Clear buffer
    pub fn clear(&mut self) {
        self.received.clear();
        self.next_sequence = 0;
        self.fin_reached = false;
        debug!("Receive buffer store cleared");
    }
    
    /// 获取缓冲区统计信息
    /// Get buffer statistics
    pub fn get_stats(&self) -> ReceiveBufferStats {
        ReceiveBufferStats {
            total_capacity: self.capacity,
            used_slots: self.received.len(),
            available_slots: self.capacity.saturating_sub(self.received.len()),
            next_sequence: self.next_sequence,
            fin_reached: self.fin_reached,
        }
    }
}

/// 发送缓冲区统计信息
/// Send buffer statistics
#[derive(Debug, Clone)]
pub struct SendBufferStats {
    pub total_capacity: usize,
    pub used_size: usize,
    pub available_size: usize,
    pub chunk_count: usize,
}

/// 接收缓冲区统计信息
/// Receive buffer statistics
#[derive(Debug, Clone)]
pub struct ReceiveBufferStats {
    pub total_capacity: usize,
    pub used_slots: usize,
    pub available_slots: usize,
    pub next_sequence: u32,
    pub fin_reached: bool,
}

impl Default for SendBufferStore {
    fn default() -> Self {
        Self::new(1024 * 1024) // 1MB default capacity
    }
}

impl Default for ReceiveBufferStore {
    fn default() -> Self {
        Self::new(256) // 256 packets default capacity
    }
}