//! SACK处理器 - 专门处理SACK逻辑
//! SACK Processor - Specialized SACK logic processing
//!
//! 职责：
//! - 解析和处理SACK范围
//! - 识别已确认和丢失的数据包
//! - 计算RTT样本
//! - 快速重传检测逻辑

use super::super::data::in_flight_store::{InFlightPacketStore, PacketState};
use crate::packet::sack::SackRange;
use std::time::Duration;
use tokio::time::Instant;
use tracing::{debug, trace};

/// SACK处理结果
/// SACK processing result
#[derive(Debug)]
pub struct SackProcessResult {
    /// 累积确认的序列号
    /// Cumulatively acknowledged sequence numbers
    pub cumulative_acked: Vec<u32>,
    
    /// SACK确认的序列号
    /// SACK acknowledged sequence numbers
    pub sack_acked: Vec<u32>,
    
    /// RTT样本
    /// RTT samples
    pub rtt_samples: Vec<Duration>,
    
    /// 快速重传候选
    /// Fast retransmission candidates
    pub fast_retx_candidates: Vec<u32>,
}

/// SACK处理器
/// SACK processor
#[derive(Debug)]
pub struct SackProcessor {
    /// 快速重传阈值
    /// Fast retransmission threshold
    fast_retx_threshold: u8,
}

impl SackProcessor {
    /// 创建新的SACK处理器
    /// Create new SACK processor
    pub fn new(fast_retx_threshold: u8) -> Self {
        Self {
            fast_retx_threshold,
        }
    }
    
    /// 处理SACK信息
    /// Process SACK information
    pub fn process_sack(
        &self,
        store: &mut InFlightPacketStore,
        recv_next_seq: u32,
        sack_ranges: &[SackRange],
        now: Instant,
    ) -> SackProcessResult {
        let mut result = SackProcessResult {
            cumulative_acked: Vec::new(),
            sack_acked: Vec::new(),
            rtt_samples: Vec::new(),
            fast_retx_candidates: Vec::new(),
        };
        
        // 步骤1：处理累积ACK
        self.process_cumulative_ack(store, recv_next_seq, now, &mut result);
        
        // 步骤2：处理SACK范围
        self.process_sack_ranges(store, sack_ranges, now, &mut result);
        
        // 步骤3：检测快速重传候选
        self.detect_fast_retx_candidates(store, &mut result);
        
        debug!(
            cumulative_count = result.cumulative_acked.len(),
            sack_count = result.sack_acked.len(),
            rtt_samples = result.rtt_samples.len(),
            fast_retx_candidates = result.fast_retx_candidates.len(),
            "SACK processing completed"
        );
        
        result
    }
    
    /// 处理累积ACK
    /// Process cumulative ACK
    fn process_cumulative_ack(
        &self,
        store: &mut InFlightPacketStore,
        recv_next_seq: u32,
        now: Instant,
        result: &mut SackProcessResult,
    ) {
        let all_sequences = store.get_all_sequences();
        
        for seq in all_sequences {
            if seq < recv_next_seq {
                if let Some(packet) = store.get_packet(seq) {
                    // 计算RTT样本
                    let rtt_sample = now.saturating_duration_since(packet.last_sent_at);
                    result.rtt_samples.push(rtt_sample);
                    result.cumulative_acked.push(seq);
                    
                    trace!(seq = seq, rtt_ms = rtt_sample.as_millis(), "Cumulative ACK processed");
                }
            }
        }
    }
    
    /// 处理SACK范围
    /// Process SACK ranges
    fn process_sack_ranges(
        &self,
        store: &mut InFlightPacketStore,
        sack_ranges: &[SackRange],
        now: Instant,
        result: &mut SackProcessResult,
    ) {
        for range in sack_ranges {
            for seq in range.start..=range.end {
                if let Some(packet) = store.get_packet(seq) {
                    // 计算RTT样本
                    let rtt_sample = now.saturating_duration_since(packet.last_sent_at);
                    result.rtt_samples.push(rtt_sample);
                    result.sack_acked.push(seq);
                    
                    trace!(seq = seq, rtt_ms = rtt_sample.as_millis(), "SACK range processed");
                }
            }
        }
    }
    
    /// 检测快速重传候选
    /// Detect fast retransmission candidates
    fn detect_fast_retx_candidates(
        &self,
        store: &mut InFlightPacketStore,
        result: &mut SackProcessResult,
    ) {
        if result.sack_acked.is_empty() {
            return;
        }
        
        // 寻找被跳过的数据包（可能丢失）
        // Find skipped packets (potentially lost)
        let all_sequences = store.get_all_sequences();
        
        for seq in all_sequences {
            // 检查是否有更高序列号的数据包已被SACK确认
            let higher_acked_count = result.sack_acked.iter()
                .filter(|&&acked_seq| acked_seq > seq)
                .count() as u8;
            
            if higher_acked_count >= self.fast_retx_threshold {
                if let Some(packet) = store.get_packet(seq) {
                    // 只有处于Sent状态的数据包才考虑快速重传
                    if packet.state == PacketState::Sent {
                        result.fast_retx_candidates.push(seq);
                        
                        trace!(
                            seq = seq,
                            higher_acked_count = higher_acked_count,
                            threshold = self.fast_retx_threshold,
                            "Fast retransmission candidate detected"
                        );
                    }
                }
            }
        }
    }
    
    /// 更新快速重传候选状态
    /// Update fast retransmission candidate state
    pub fn update_fast_retx_candidates(
        &self,
        store: &mut InFlightPacketStore,
        candidates: &[u32],
    ) {
        for &seq in candidates {
            store.update_fast_retx_candidate(seq, self.fast_retx_threshold);
        }
    }
}