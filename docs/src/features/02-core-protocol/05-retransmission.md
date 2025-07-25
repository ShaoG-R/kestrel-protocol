# 2.5: 动态RTO与重传机制

**功能描述:**

协议实现了一套完备的重传机制，以应对网络丢包。它结合了基于动态RTO（Retransmission Timeout）的超时重传和基于SACK的快速重传，能够在不同网络状况下高效地恢复丢失的数据。

**实现位置:**

- **动态RTO计算**: `src/core/reliability/rtt.rs` (`RttEstimator`)
- **重传逻辑**: `src/core/reliability/send_buffer.rs` (`SendBuffer`)
- **顶层协调**: `src/core/reliability.rs` (`ReliabilityLayer`)

### 1. 动态RTO计算 (`RttEstimator`)

为了适应变化的网络延迟，协议没有使用固定的重传超时，而是实现了一个遵循 **RFC 6298** 标准的动态RTO估算器。

- **核心算法**: `RttEstimator` 内部维护着 `srtt`（平滑化的RTT）和 `rttvar`（RTT方差）。每次收到一个有效的ACK并计算出新的RTT样本时，`update` 方法会使用加权移动平均算法来更新这两个值。
- **RTO计算公式**: `rto = srtt + 4 * rttvar`
- **指数退避**: 当发生超时重传时，`backoff()` 方法会被调用，将当前RTO值翻倍，以快速应对网络拥塞或恶化。

```rust
// 位于 src/core/reliability/rtt.rs
pub fn update(&mut self, rtt_sample: Duration, min_rto: Duration) {
    if self.srtt == 0.0 { // First sample
        self.srtt = rtt_sample_f64;
        self.rttvar = rtt_sample_f64 / 2.0;
    } else {
        let delta = (self.srtt - rtt_sample_f64).abs();
        self.rttvar = (1.0 - BETA) * self.rttvar + BETA * delta;
        self.srtt = (1.0 - ALPHA) * self.srtt + ALPHA * rtt_sample_f64;
    }
    // ...
}
```

### 2. 超时重传 (Timeout Retransmission)

这是最基本的重传机制，作为最后的保障。

- **机制**: `SendBuffer` 为每个已发送但未确认的包（在途包）记录一个 `last_sent_at` 时间戳。`Endpoint` 的主事件循环会定期（基于 `next_rto_deadline`）检查是否有包的发送时间超过了当前的动态RTO值。如果有，这些包将被重新发送。

```rust
// 位于 src/core/reliability/send_buffer.rs
pub fn check_for_rto(&mut self, rto: std::time::Duration, now: Instant) -> Vec<Frame> {
    let mut frames_to_resend = Vec::new();
    for packet in self.in_flight.values_mut() {
        if now.duration_since(packet.last_sent_at) > rto {
            frames_to_resend.push(packet.frame.clone());
            packet.last_sent_at = now; // 更新发送时间
        }
    }
    frames_to_resend
}
```

### 3. 快速重传 (Fast Retransmission)

为了在轻微丢包时能更快地恢复，协议实现了基于SACK的快速重传。

- **触发条件**: 当发送方收到一个ACK，该ACK确认了某个序号为 `N` 的包，但发送方发现自己的在途包列表中仍然存在序号小于 `N` 的包时，就认为这些更早的包可能已丢失。
- **丢包计数**: 每个在途包都有一个 `fast_retx_count` 计数器。每当它被一个更新的ACK“跳过”，这个计数器就加一。
- **执行重传**: 当计数器的值达到 `config.fast_retx_threshold`（通常是3）时，该包会被立即重传，而无需等待RTO计时器超时。

```rust
// 位于 src/core/reliability/send_buffer.rs - handle_ack 方法
// ...
if let Some(&highest_acked_in_this_ack) = acked_in_sack.iter().max() {
    for (&seq, _packet) in self.in_flight.range(..highest_acked_in_this_ack) {
        // ... 增加 fast_retx_count ...
        if packet.fast_retx_count >= fast_retx_threshold {
            // ... 将包加入重传队列 ...
        }
    }
}
// ...
``` 