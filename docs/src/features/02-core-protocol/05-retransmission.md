# 5: 统一重传管理器与动态RTO

**功能描述:**

协议实现了一套完备的重传机制，以应对网络丢包。它结合了基于动态RTO（Retransmission Timeout）的超时重传和基于SACK的快速重传。目前，这套机制已重构为一个统一的重传管理器，能够区分处理需要SACK完全可靠性的数据包和仅需简单重试的控制包。

**实现位置:**

- **统一管理器**: `src/core/reliability/retransmission.rs` (`RetransmissionManager`)
- **SACK可靠性逻辑**: `src/core/reliability/retransmission/sack_manager.rs` (`SackManager`)
- **简单重传逻辑**: `src/core/reliability/retransmission/simple_retx_manager.rs` (`SimpleRetransmissionManager`)
- **动态RTO计算**: `src/core/reliability/retransmission/rtt.rs` (`RttEstimator`)
- **顶层协调**: `src/core/reliability.rs` (`ReliabilityLayer`)

### 1. 动态RTO计算 (`RttEstimator`)

为了适应变化的网络延迟，协议没有使用固定的重传超时，而是实现了一个遵循 **RFC 6298** 标准的动态RTO估算器。这一部分保持不变。

- **核心算法**: `RttEstimator` 内部维护着 `srtt`（平滑化的RTT）和 `rttvar`（RTT方差）。
- **RTO计算公式**: `rto = srtt + 4 * rttvar`
- **指数退避**: 当发生超时重传时，`backoff()` 方法会被调用，将当前RTO值翻倍。

### 2. 超时重传 (Timeout Retransmission)

超时重传是最后的保障机制，现在由统一的 `RetransmissionManager` 协调。

- **机制**: `Endpoint` 的主事件循环会定期检查超时。`RetransmissionManager` 会调用其下的两个子管理器：
    - `SackManager` 根据动态RTO检查可靠数据包的超时。
    - `SimpleRetransmissionManager` 根据配置的固定间隔检查控制包的超时。

```rust
// 位于 src/core/reliability/retransmission.rs
pub fn check_for_retransmissions(&mut self, rto: Duration, now: Instant) -> Vec<Frame> {
    let mut frames_to_retx = Vec::new();
    
    // 检查SACK管理的超时
    let sack_retx = self.sack_manager.check_for_rto(rto, now);
    frames_to_retx.extend(sack_retx);
    
    // 检查简单重传的超时
    let simple_retx = self.simple_retx_manager.check_for_retransmissions(now);
    frames_to_retx.extend(simple_retx);

    frames_to_retx
}
```

### 3. 快速重传 (Fast Retransmission)

快速重传逻辑被完全封装在 `SackManager` 内部，仅作用于需要SACK保证的可靠数据包。

- **触发条件**: 当`SackManager`处理一个ACK时，如果发现该ACK确认了序列号更高的包，导致某些在途包被“跳过”，则认为可能发生了丢包。
- **丢包计数**: `SackManager` 中每个在途包都有一个 `fast_retx_count` 计数器。
- **执行重传**: 当计数器的值达到配置的阈值（`fast_retx_threshold`）时，该包被立即重传。

```rust
// 位于 src/core/reliability/retransmission/sack_manager.rs
fn check_fast_retransmission(
    &mut self,
    sack_acked_sequences: &[u32],
    now: Instant,
) -> Vec<Frame> {
    // ...
    if let Some(highest_sacked_seq) = sack_acked_sequences.iter().max().copied() {
        // ...
        for seq in keys_to_modify {
            if let Some(packet) = self.in_flight_packets.get_mut(&seq) {
                packet.fast_retx_count += 1;
                if packet.fast_retx_count >= self.fast_retx_threshold {
                    // ... 将包加入重传队列 ...
                }
            }
        }
    }
    // ...
}
``` 