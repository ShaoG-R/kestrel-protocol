//! 在途数据包存储层 - 纯数据管理
//! In-Flight Packet Store - Pure Data Management
//!
//! 职责：
//! - 数据包状态存储和查询
//! - 定时器映射管理  
//! - 序列号索引维护
//! - 无业务逻辑，只管理数据

use crate::{
    core::reliability::coordination::packet_coordinator::RetransmissionFrameInfo,
    timer::actor::ActorTimerId,
};
use std::collections::{BTreeMap, HashMap};
use tokio::time::Instant;
use tracing::{debug, trace};

/// 在途数据包的状态
/// In-flight packet state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PacketState {
    /// 已发送，等待ACK
    /// Sent, waiting for ACK
    Sent,
    /// 标记为快速重传候选
    /// Marked for fast retransmission candidate
    FastRetxCandidate,
    /// 已被SACK确认
    /// SACK acknowledged
    SackAcked,
    /// 需要重传
    /// Needs retransmission
    NeedsRetx,
}

/// 在途数据包数据结构
/// In-flight packet data structure
#[derive(Debug, Clone)]
pub struct InFlightPacket {
    /// 数据包的重传信息
    /// Packet retransmission info
    pub frame_info: RetransmissionFrameInfo,
    
    /// 最后发送时间
    /// Last sent time
    pub last_sent_at: Instant,
    
    /// 重传次数
    /// Retransmission count
    pub retx_count: u8,
    
    /// 最大重传次数
    /// Maximum retransmission count
    pub max_retx_count: u8,
    
    /// 当前的定时器ID（如果有）
    /// Current timer ID (if any)
    pub timer_id: Option<ActorTimerId>,
    
    /// 数据包状态
    /// Packet state
    pub state: PacketState,
    
    /// 快速重传计数
    /// Fast retransmission count
    pub fast_retx_count: u8,
}

/// 在途数据包存储器
/// In-flight packet store
#[derive(Debug)]
pub struct InFlightPacketStore {
    /// 主存储：序列号 -> 数据包
    /// Main storage: sequence number -> packet
    packets: BTreeMap<u32, InFlightPacket>,
    
    /// 定时器映射：定时器ID -> 序列号
    /// Timer mapping: timer ID -> sequence number
    timer_to_sequence: BTreeMap<ActorTimerId, u32>,
    
    /// 快速重传候选缓存
    /// Fast retransmission candidate cache
    fast_retx_candidates: BTreeMap<u32, u8>, // seq -> duplicate_ack_count
    
    /// 性能优化：按状态的索引
    /// Performance optimization: index by state
    state_index: HashMap<PacketState, Vec<u32>>,
}

impl InFlightPacketStore {
    /// 创建新的存储器
    /// Create new store
    pub fn new() -> Self {
        Self {
            packets: BTreeMap::new(),
            timer_to_sequence: BTreeMap::new(),
            fast_retx_candidates: BTreeMap::new(),
            state_index: HashMap::new(),
        }
    }
    
    /// 添加数据包
    /// Add packet
    pub fn add_packet(&mut self, seq: u32, packet: InFlightPacket) {
        trace!(seq = seq, "Adding packet to store");
        
        // 更新状态索引
        self.add_to_state_index(packet.state, seq);
        
        self.packets.insert(seq, packet);
    }
    
    /// 获取数据包
    /// Get packet
    pub fn get_packet(&self, seq: u32) -> Option<&InFlightPacket> {
        self.packets.get(&seq)
    }
    
    /// 获取可变数据包引用
    /// Get mutable packet reference
    pub fn get_packet_mut(&mut self, seq: u32) -> Option<&mut InFlightPacket> {
        self.packets.get_mut(&seq)
    }
    
    /// 移除数据包
    /// Remove packet
    pub fn remove_packet(&mut self, seq: u32) -> Option<InFlightPacket> {
        if let Some(packet) = self.packets.remove(&seq) {
            // 清理状态索引
            self.remove_from_state_index(packet.state, seq);
            
            // 清理定时器映射
            if let Some(timer_id) = packet.timer_id {
                self.timer_to_sequence.remove(&timer_id);
            }
            // 清理快速重传候选
            self.fast_retx_candidates.remove(&seq);
            
            trace!(seq = seq, "Removed packet from store");
            Some(packet)
        } else {
            None
        }
    }
    
    /// 批量移除数据包
    /// Batch remove packets
    pub fn batch_remove_packets(&mut self, sequences: &[u32]) -> Vec<(u32, InFlightPacket)> {
        let mut removed = Vec::new();
        
        for &seq in sequences {
            if let Some(packet) = self.remove_packet(seq) {
                removed.push((seq, packet));
            }
        }
        
        debug!(removed_count = removed.len(), "Batch removed packets from store");
        removed
    }
    
    /// 设置定时器映射
    /// Set timer mapping
    pub fn set_timer_mapping(&mut self, timer_id: ActorTimerId, seq: u32) -> bool {
        // 只有当数据包存在时才创建映射，避免孤立映射
        if let Some(packet) = self.packets.get_mut(&seq) {
            // 清理旧的定时器映射（如果存在）
            if let Some(old_timer_id) = packet.timer_id {
                self.timer_to_sequence.remove(&old_timer_id);
            }
            
            self.timer_to_sequence.insert(timer_id, seq);
            packet.timer_id = Some(timer_id);
            trace!(seq = seq, timer_id = timer_id, "Timer mapping set successfully");
            true
        } else {
            debug!(seq = seq, timer_id = timer_id, "Cannot set timer mapping: packet not found");
            false
        }
    }
    
    /// 通过定时器ID查找序列号
    /// Find sequence number by timer ID
    pub fn find_sequence_by_timer(&self, timer_id: ActorTimerId) -> Option<u32> {
        self.timer_to_sequence.get(&timer_id).copied()
    }
    
    /// 移除定时器映射
    /// Remove timer mapping
    pub fn remove_timer_mapping(&mut self, timer_id: ActorTimerId) -> Option<u32> {
        if let Some(seq) = self.timer_to_sequence.remove(&timer_id) {
            // 清除数据包的定时器ID
            if let Some(packet) = self.packets.get_mut(&seq) {
                packet.timer_id = None;
            }
            Some(seq)
        } else {
            None
        }
    }
    
    /// 获取所有在途数据包序列号
    /// Get all in-flight packet sequence numbers
    pub fn get_all_sequences(&self) -> Vec<u32> {
        self.packets.keys().copied().collect()
    }
    
    /// 获取指定状态的数据包 (优化版本，使用状态索引)
    /// Get packets with specific state (optimized version using state index)
    pub fn get_packets_by_state(&self, state: PacketState) -> Vec<(u32, &InFlightPacket)> {
        if let Some(seq_list) = self.state_index.get(&state) {
            seq_list
                .iter()
                .filter_map(|&seq| {
                    self.packets.get(&seq).map(|packet| (seq, packet))
                })
                .collect()
        } else {
            Vec::new()
        }
    }
    
    /// 获取指定状态的数据包数量 (新增方法，高效计数)
    /// Get count of packets with specific state (new method for efficient counting)
    pub fn count_packets_by_state(&self, state: PacketState) -> usize {
        self.state_index.get(&state).map_or(0, |seq_list| seq_list.len())
    }
    
    /// 获取在途数据包数量
    /// Get in-flight packet count
    pub fn count(&self) -> usize {
        self.packets.len()
    }
    
    /// 检查是否为空
    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.packets.is_empty()
    }
    
    /// 清空所有数据
    /// Clear all data
    pub fn clear(&mut self) {
        debug!(packet_count = self.packets.len(), "Clearing all packets from store");
        self.packets.clear();
        self.timer_to_sequence.clear();
        self.fast_retx_candidates.clear();
        self.state_index.clear();
    }
    
    /// 更新快速重传候选状态
    /// Update fast retransmission candidate state
    pub fn update_fast_retx_candidate(&mut self, seq: u32, duplicate_count: u8) {
        self.fast_retx_candidates.insert(seq, duplicate_count);
        
        if let Some(packet) = self.packets.get_mut(&seq) {
            let old_state = packet.state;
            packet.state = PacketState::FastRetxCandidate;
            packet.fast_retx_count = duplicate_count;
            
            // 更新状态索引
            if old_state != PacketState::FastRetxCandidate {
                self.update_state_index(old_state, PacketState::FastRetxCandidate, seq);
            }
        }
    }
    
    /// 获取快速重传候选
    /// Get fast retransmission candidates
    pub fn get_fast_retx_candidates(&self) -> Vec<(u32, u8)> {
        self.fast_retx_candidates.iter().map(|(&seq, &count)| (seq, count)).collect()
    }
    
    /// 添加序列号到状态索引
    /// Add sequence number to state index
    fn add_to_state_index(&mut self, state: PacketState, seq: u32) {
        self.state_index.entry(state).or_insert_with(Vec::new).push(seq);
    }
    
    /// 从状态索引中移除序列号
    /// Remove sequence number from state index
    fn remove_from_state_index(&mut self, state: PacketState, seq: u32) {
        if let Some(seq_list) = self.state_index.get_mut(&state) {
            seq_list.retain(|&s| s != seq);
            if seq_list.is_empty() {
                self.state_index.remove(&state);
            }
        }
    }
    
    /// 更新状态索引（从旧状态移动到新状态）
    /// Update state index (move from old state to new state)
    fn update_state_index(&mut self, old_state: PacketState, new_state: PacketState, seq: u32) {
        self.remove_from_state_index(old_state, seq);
        self.add_to_state_index(new_state, seq);
    }
}

impl Default for InFlightPacketStore {
    fn default() -> Self {
        Self::new()
    }
}