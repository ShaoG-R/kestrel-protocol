# 6: 基于SACK的高效确认

**功能描述:**

协议采用了基于SACK（Selective Acknowledgment，选择性确认）的高效确认机制，取代了传统的累积确认（Cumulative ACK）。这使得接收方可以精确地告知发送方自己收到了哪些离散的数据块，极大地提高了在乱序和丢包网络环境下的确认效率和重传精度。

**实现位置:**

- **SACK统一管理**: `src/core/reliability/sack_manager.rs` - 集中管理所有SACK相关逻辑
- **SACK范围生成**: `src/core/reliability/recv_buffer.rs` - 从接收缓冲区生成SACK范围
- **SACK序列化**: `src/packet/sack.rs` - SACK数据的编解码
- **可靠性层集成**: `src/core/reliability.rs` - 通过SackManager协调SACK功能

### 1. SACK的工作原理

当网络发生乱序或丢包时，接收方的缓冲区中会形成不连续的数据块“空洞”。

- **传统ACK**: 只能告知发送方“我已连续收到了X号之前的所有包”，无法表达“我收到了X+2，但没收到X+1”。
- **SACK**: `ACK`包的载荷中可以携带一个或多个`SackRange`（如`{start: 10, end: 20}`），明确告知发送方“我收到了序号从10到20的所有包”。

### 2. SACK管理器架构

**2.1 统一的SACK管理器**

`SackManager` 是SACK功能的核心，集中管理所有SACK相关的状态和逻辑：

```rust
// 位于 src/core/reliability/sack_manager.rs
pub struct SackManager {
    fast_retx_threshold: u16,
    in_flight_packets: BTreeMap<u32, SackInFlightPacket>,
    ack_threshold: u32,
    ack_eliciting_packets_count: u32,
}

impl SackManager {
    /// 处理接收到的ACK和SACK信息
    pub fn process_ack(
        &mut self,
        recv_next_seq: u32,
        sack_ranges: &[SackRange],
        now: Instant,
    ) -> SackProcessResult {
        // 1. 处理累积ACK
        // 2. 处理SACK范围
        // 3. 检查快速重传
        // 4. 计算RTT样本
    }
    
    /// 添加在途数据包
    pub fn add_in_flight_packet(&mut self, frame: Frame, now: Instant);
    
    /// 检查RTO重传
    pub fn check_for_rto(&mut self, rto: Duration, now: Instant) -> Vec<Frame>;
    
    /// 判断是否应发送独立ACK
    pub fn should_send_standalone_ack(&self, sack_ranges: &[SackRange]) -> bool;
}
```

**2.2 SACK范围生成**

`ReceiveBuffer` 负责从乱序接收的数据包中生成SACK范围：

```rust
// 位于 src/core/reliability/recv_buffer.rs
pub fn get_sack_ranges(&self) -> Vec<SackRange> {
    let mut ranges = Vec::new();
    let mut current_range: Option<SackRange> = None;

    for &seq in self.received.keys() {
        match current_range.as_mut() {
            Some(range) => {
                if seq == range.end + 1 {
                    // 序号是连续的，扩展当前范围
                    range.end = seq;
                } else {
                    // 出现空洞，完成当前范围，开始新范围
                    ranges.push(range.clone());
                    current_range = Some(SackRange { start: seq, end: seq });
                }
            }
            None => { // 开始第一个范围
                current_range = Some(SackRange { start: seq, end: seq });
            }
        }
    }
    // ... 添加最后一个范围 ...
    ranges
}
```

### 3. SACK信息的编码与发送

**3.1 SACK编码**

`SackManager` 提供统一的SACK编解码接口：

```rust
// 位于 src/core/reliability/sack_manager.rs
impl SackManager {
    /// 将SACK范围编码为字节流
    pub fn encode_sack_ranges(&self, ranges: &[SackRange]) -> Bytes {
        use bytes::BytesMut;
        use crate::packet::sack::encode_sack_ranges;

        let mut payload = BytesMut::with_capacity(ranges.len() * 8);
        encode_sack_ranges(ranges, &mut payload);
        payload.freeze()
    }

    /// 从字节流解码SACK范围
    pub fn decode_sack_ranges(&self, payload: Bytes) -> Vec<SackRange> {
        use crate::packet::sack::decode_sack_ranges;
        decode_sack_ranges(payload)
    }
}
```

**3.2 ACK帧构造**

发送方在构造 `ACK` 包时，通过可靠性层获取SACK信息：

```rust
// 位于 src/core/endpoint/frame_factory.rs
pub fn create_ack_frame(
    peer_cid: u32,
    reliability: &mut ReliabilityLayer,
    start_time: Instant,
) -> Frame {
    let (sack_ranges, recv_next, window_size) = reliability.get_ack_info();
    let timestamp = Instant::now().duration_since(start_time).as_millis() as u32;
    Frame::new_ack(peer_cid, recv_next, window_size, &sack_ranges, timestamp)
}
```

### 4. SACK信息的处理

**4.1 统一的ACK处理流程**

当发送方收到一个 `ACK` 包时，`ReliabilityLayer` 通过 `SackManager` 进行统一处理：

```rust
// 位于 src/core/reliability.rs
pub fn handle_ack(
    &mut self,
    recv_next_seq: u32,
    sack_ranges: Vec<SackRange>,
    now: Instant,
) -> Vec<Frame> {
    // 使用集中式SACK管理器处理ACK
    let result = self.sack_manager.process_ack(recv_next_seq, &sack_ranges, now);

    // 更新RTT和拥塞控制
    for rtt_sample in result.rtt_samples {
        self.rto_estimator.update(rtt_sample, self.config.min_rto);
        self.congestion_control.on_ack(rtt_sample);
    }

    // 处理丢包事件
    if !result.frames_to_retransmit.is_empty() {
        self.congestion_control.on_packet_loss(now);
    }

    result.frames_to_retransmit
}
```

**4.2 SACK处理的核心逻辑**

`SackManager::process_ack` 实现了完整的SACK处理流程：

```rust
// 位于 src/core/reliability/sack_manager.rs
pub fn process_ack(
    &mut self,
    recv_next_seq: u32,
    sack_ranges: &[SackRange],
    now: Instant,
) -> SackProcessResult {
    let mut rtt_samples = Vec::new();
    let mut newly_acked_sequences = Vec::new();

    // 步骤1: 处理累积ACK
    let mut cumulative_acked_keys = Vec::new();
    for (&seq, packet) in self.in_flight_packets.iter() {
        if seq < recv_next_seq {
            cumulative_acked_keys.push(seq);
            rtt_samples.push(now.saturating_duration_since(packet.last_sent_at));
            newly_acked_sequences.push(seq);
        } else {
            break; // BTreeMap是有序的
        }
    }

    for key in cumulative_acked_keys {
        self.in_flight_packets.remove(&key);
    }

    // 步骤2: 处理SACK范围
    let mut sack_acked_sequences = Vec::new();
    for range in sack_ranges {
        for seq in range.start..=range.end {
            if let Some(packet) = self.in_flight_packets.remove(&seq) {
                rtt_samples.push(now.saturating_duration_since(packet.last_sent_at));
                newly_acked_sequences.push(seq);
                sack_acked_sequences.push(seq);
            }
        }
    }

    // 步骤3: 检查快速重传
    let frames_to_retransmit = self.check_fast_retransmission(
        &sack_acked_sequences,
        now,
    );

    SackProcessResult {
        frames_to_retransmit,
        rtt_samples,
        newly_acked_sequences,
    }
}
```

**4.3 快速重传逻辑**

基于SACK信息的快速重传检测：

```rust
fn check_fast_retransmission(
    &mut self,
    sack_acked_sequences: &[u32],
    now: Instant,
) -> Vec<Frame> {
    let mut frames_to_retransmit = Vec::new();

    // 找到本次ACK中SACK确认的最高序列号
    if let Some(highest_sacked_seq) = sack_acked_sequences.iter().max().copied() {
        // 检查所有序列号小于最高SACK序列号的在途包
        let mut keys_to_modify = Vec::new();
        for (&seq, _) in self.in_flight_packets.range(..highest_sacked_seq) {
            keys_to_modify.push(seq);
        }

        // 增加快速重传计数器，达到阈值时触发重传
        for seq in keys_to_modify {
            if let Some(packet) = self.in_flight_packets.get_mut(&seq) {
                packet.fast_retx_count += 1;
                
                if packet.fast_retx_count >= self.fast_retx_threshold {
                    frames_to_retransmit.push(packet.frame.clone());
                    packet.last_sent_at = now;
                    packet.fast_retx_count = 0; // 重传后重置计数器
                }
            }
        }
    }

    frames_to_retransmit
}
```

### 5. 架构优势

**5.1 集中化管理**
- 所有SACK相关逻辑集中在 `SackManager` 中，便于维护和测试
- 统一的状态管理，避免了逻辑分散导致的不一致问题

**5.2 清晰的职责分离**
- `ReceiveBuffer`: 负责SACK范围生成
- `SackManager`: 负责SACK处理和在途包管理
- `ReliabilityLayer`: 负责协调各组件并处理上层逻辑
- `SendBuffer`: 专注于流缓冲区管理

**5.3 高效的处理流程**
- 双重确认机制（累积ACK + SACK）最大化信息利用
- 精确的快速重传检测，减少不必要的重传
- 统一的RTT计算和拥塞控制集成

这种重构后的架构确保了SACK功能的高效性和可维护性，同时为未来的扩展提供了良好的基础。 