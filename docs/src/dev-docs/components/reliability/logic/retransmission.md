# 重传逻辑模块 (`retransmission`) - 智能数据包恢复系统

## 概述

重传逻辑模块是可靠性保证的核心组件，实现了智能的数据包丢失检测和恢复机制。它集成了SACK（选择性确认）处理和重传决策算法，能够快速、准确地恢复丢失的数据包，确保数据传输的完整性和效率。

**核心功能:**
- **SACK智能分析**: 基于选择性确认信息分析数据包接收状态
- **快速重传检测**: 通过重复ACK和SACK信息快速检测丢包
- **重传策略决策**: 智能决策重传时机、次数和优先级
- **RTT样本提取**: 从ACK信息中提取精确的RTT测量值

**文件结构:**
- `retransmission.rs` - 重传决策器主逻辑
- `sack_processor.rs` - SACK处理的专业实现
- `tests/` - 完整的测试套件

## SACK处理器

### SACK算法原理

SACK（Selective Acknowledgment）允许接收方告知发送方哪些数据包已经收到，哪些仍然缺失，使得发送方可以有选择地重传丢失的数据包。

```rust
// SACK处理核心逻辑
impl SackProcessor {
    pub fn process_sack(
        &mut self,
        recv_next_seq: u32,
        sack_ranges: &[SackRange],
        in_flight_store: &InFlightPacketStore,
    ) -> SackProcessResult {
        let mut result = SackProcessResult::new();
        
        // 1. 处理累积确认（所有小于recv_next_seq的数据包都已确认）
        result.newly_acked.extend(
            in_flight_store.get_sequences_before(recv_next_seq)
        );
        
        // 2. 处理选择性确认
        for range in sack_ranges {
            let acked_in_range = in_flight_store.get_sequences_in_range(
                range.start, 
                range.end
            );
            result.newly_acked.extend(acked_in_range);
        }
        
        // 3. 智能丢包检测
        result.fast_retx_candidates = self.detect_lost_packets(
            recv_next_seq,
            sack_ranges,
            in_flight_store,
        );
        
        result
    }
}
```

### 丢包检测算法

基于SACK信息的智能丢包检测，避免虚假重传：

```rust
impl SackProcessor {
    fn detect_lost_packets(
        &self,
        recv_next_seq: u32,
        sack_ranges: &[SackRange],
        in_flight_store: &InFlightPacketStore,
    ) -> Vec<u32> {
        let mut lost_packets = Vec::new();
        
        // 检查累积确认前的gap
        if recv_next_seq > 1 {
            let gap_start = recv_next_seq;
            
            // 查找第一个SACK range，中间的就是丢失的
            if let Some(first_sack) = sack_ranges.first() {
                for seq in gap_start..first_sack.start {
                    if in_flight_store.contains_packet(seq) {
                        lost_packets.push(seq);
                    }
                }
            }
        }
        
        // 检查SACK ranges之间的gaps
        for window in sack_ranges.windows(2) {
            let gap_start = window[0].end + 1;
            let gap_end = window[1].start;
            
            for seq in gap_start..gap_end {
                if in_flight_store.contains_packet(seq) {
                    // 验证是否满足快速重传条件
                    if self.should_fast_retransmit(seq, sack_ranges, in_flight_store) {
                        lost_packets.push(seq);
                    }
                }
            }
        }
        
        lost_packets
    }
    
    fn should_fast_retransmit(
        &self,
        seq: u32,
        sack_ranges: &[SackRange],
        in_flight_store: &InFlightPacketStore,
    ) -> bool {
        // 计算有多少个更高序列号的数据包被确认
        let mut higher_acked_count = 0;
        
        for range in sack_ranges {
            if range.start > seq {
                higher_acked_count += range.end - range.start + 1;
            }
        }
        
        // 快速重传阈值检查（通常为3）
        higher_acked_count >= self.fast_retx_threshold
    }
}
```

## 重传决策器

### 决策算法

重传决策器综合考虑多种因素，做出最优的重传决策：

```rust
impl RetransmissionDecider {
    pub fn decide_retransmissions(
        &mut self,
        fast_retx_candidates: &[u32],
        timeout_candidates: &[u32],
        in_flight_store: &InFlightPacketStore,
        context: &RetransmissionContext,
    ) -> RetransmissionDecision {
        let mut decision = RetransmissionDecision::new();
        
        // 1. 处理快速重传候选
        for &seq in fast_retx_candidates {
            if let Some(packet) = in_flight_store.get_packet(seq) {
                if self.should_fast_retransmit_packet(packet) {
                    if let Some(frame) = self.reconstruct_frame(packet, context) {
                        decision.frames_to_retransmit.push(frame);
                    }
                }
            }
        }
        
        // 2. 处理超时重传候选  
        for &seq in timeout_candidates {
            if let Some(packet) = in_flight_store.get_packet(seq) {
                if self.should_timeout_retransmit_packet(packet) {
                    if let Some(frame) = self.reconstruct_frame(packet, context) {
                        decision.frames_to_retransmit.push(frame);
                    } else {
                        // 达到最大重传次数，丢弃
                        decision.packets_to_drop.push(seq);
                    }
                }
            }
        }
        
        decision
    }
}
```

### 帧重构

智能重构需要重传的网络帧：

```rust
impl RetransmissionDecider {
    fn reconstruct_frame(
        &self,
        packet: &InFlightPacket,
        context: &RetransmissionContext,
    ) -> Option<Frame> {
        match packet.frame_info.frame_type {
            FrameType::Push => {
                Some(Frame::Push {
                    header: ShortHeader {
                        dest_cid: context.peer_cid,
                        sequence_number: packet.frame_info.sequence_number,
                        timestamp: context.current_timestamp,
                    },
                    payload: packet.frame_info.payload.clone(),
                })
            },
            
            FrameType::PathChallenge => {
                Some(Frame::PathChallenge {
                    header: ShortHeader {
                        dest_cid: context.peer_cid,
                        sequence_number: packet.frame_info.sequence_number,
                        timestamp: context.current_timestamp,
                    },
                    challenge_data: packet.frame_info.additional_data.unwrap_or(0),
                })
            },
            
            FrameType::Fin => {
                Some(Frame::Fin {
                    header: ShortHeader {
                        dest_cid: context.peer_cid,
                        sequence_number: packet.frame_info.sequence_number,
                        timestamp: context.current_timestamp,
                    },
                })
            },
            
            _ => {
                warn!(
                    seq = packet.frame_info.sequence_number,
                    frame_type = ?packet.frame_info.frame_type,
                    "Unsupported frame type for retransmission"
                );
                None
            }
        }
    }
}
```

## RTT样本提取

### 精确测量

从ACK处理中提取RTT样本，支持精确的网络状态评估：

```rust
impl SackProcessor {
    fn extract_rtt_samples(
        &self,
        newly_acked: &[u32],
        in_flight_store: &InFlightPacketStore,
        ack_timestamp: Instant,
    ) -> Vec<Duration> {
        let mut rtt_samples = Vec::new();
        
        for &seq in newly_acked {
            if let Some(packet) = in_flight_store.get_packet(seq) {
                // 只使用未重传的数据包计算RTT（Karn算法）
                if packet.retx_count == 0 {
                    let rtt = ack_timestamp.duration_since(packet.last_sent_at);
                    
                    // 过滤异常RTT值
                    if self.is_valid_rtt_sample(rtt) {
                        rtt_samples.push(rtt);
                    }
                }
            }
        }
        
        rtt_samples
    }
    
    fn is_valid_rtt_sample(&self, rtt: Duration) -> bool {
        // RTT合理性检查
        rtt >= Duration::from_millis(1) && rtt <= Duration::from_secs(10)
    }
}
```

## 重传策略

### 快速重传 vs 超时重传

| 重传类型 | 触发条件 | 响应速度 | 准确性 | 适用场景 |
|----------|----------|----------|--------|----------|
| 快速重传 | 重复ACK/SACK | 快速 | 高 | 偶发丢包 |
| 超时重传 | RTO超时 | 较慢 | 中等 | 连续丢包 |

### 重传次数控制

```rust
impl RetransmissionDecider {
    fn should_retransmit_packet(&self, packet: &InFlightPacket) -> bool {
        match packet.frame_info.frame_type {
            FrameType::Push => {
                // 数据帧：限制重传次数
                packet.retx_count < self.max_data_retries
            },
            FrameType::Fin => {
                // FIN帧：更高的重传次数
                packet.retx_count < self.max_control_retries
            },
            FrameType::PathChallenge => {
                // 路径探测：适中的重传次数
                packet.retx_count < self.max_probe_retries
            },
            _ => false,
        }
    }
}
```

## 性能优化

### 批量处理

```rust
impl SackProcessor {
    pub fn batch_process_acks(
        &mut self,
        ack_batch: &[AckInfo],
        in_flight_store: &InFlightPacketStore,
    ) -> Vec<SackProcessResult> {
        // 批量处理多个ACK，减少函数调用开销
        ack_batch.iter()
            .map(|ack| self.process_sack(
                ack.recv_next_seq,
                &ack.sack_ranges,
                in_flight_store,
            ))
            .collect()
    }
}
```

### 内存优化

```rust
struct SackProcessor {
    // 复用的临时向量，避免重复分配
    temp_lost_packets: Vec<u32>,
    temp_rtt_samples: Vec<Duration>,
    temp_acked_sequences: Vec<u32>,
}

impl SackProcessor {
    fn process_sack(&mut self, /* ... */) -> SackProcessResult {
        // 清理并复用临时向量
        self.temp_lost_packets.clear();
        self.temp_rtt_samples.clear();
        self.temp_acked_sequences.clear();
        
        // 使用复用的向量进行计算
        // ...
    }
}
```

重传逻辑模块通过其智能的算法实现和优化设计，为协议栈提供了高效、可靠的数据包恢复能力，确保在各种网络丢包场景下都能快速恢复数据传输。
