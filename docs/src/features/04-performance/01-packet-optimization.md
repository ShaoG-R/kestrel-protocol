# 1: 包聚合与快速应答

**功能描述:**

为了提升网络效率和响应速度，协议在发送和接收两端都实现了优化。发送端采用**包聚合（Packet Coalescing）**技术将多个小包合并发送，而接收端则采用**快速应答（Immediate ACK）**机制来及时反馈接收状态。

**实现位置:**

- **包聚合**: `src/socket/sender.rs` (`sender_task`)
- **快速应答**: `src/core/reliability.rs` (`ReliabilityLayer`)

### 1. 包聚合 (Packet Coalescing)

在高交互性场景下（例如，一个数据包紧跟着一个确认包），如果每个逻辑包都单独占用一个UDP数据报，会产生大量的IP/UDP头部开销，降低有效载荷比。包聚合正是为了解决这个问题。

- **机制**:
    1.  当 `Endpoint` 任务需要发送数据时（无论是`PUSH`, `ACK`, 还是`FIN`），它不会直接发送，而是将生成的 `Frame` 对象打包成一个 `Vec<Frame>`，并通过MPSC通道发送给全局唯一的 `SenderTask`。
    2.  `SenderTask` 在其主循环中，会将这个 `Vec<Frame>` 里的所有帧**连续地编码**到同一个字节缓冲区中。
    3.  最后，`SenderTask` 通过**一次** `socket.send_to()` 系统调用，将整个缓冲区作为一个UDP数据报发送出去。

```rust
// 位于 src/socket/sender.rs
// ...
for cmd in commands.drain(..) {
    send_buf.clear();
    // 这里是包聚合的核心：多个帧被编码进同一个缓冲区
    for frame in cmd.frames {
        frame.encode(&mut send_buf);
    }
    // ...
    // 整个缓冲区通过一次系统调用发送
    if let Err(e) = socket.send_to(&send_buf, cmd.remote_addr).await {
        // ...
    }
}
// ...
```
这种方式显著减少了系统调用次数和网络包头的开销。

### 2. 快速应答 (Immediate ACK)

传统的捎带ACK（Piggybacking）策略虽然能减少纯ACK包的数量，但在单向数据传输或数据传输暂停时，可能会导致ACK延迟，从而影响发送方的RTT计算和拥塞控制。快速应答是对此的优化。

- **机制**:
    1.  `ReliabilityLayer` 维护一个 `ack_eliciting_packets_since_last_ack` 计数器，用于记录自上次发送ACK以来，收到了多少个需要确认的包（如 `PUSH`）。
    2.  `Endpoint` 的事件循环会调用 `reliability.should_send_standalone_ack()` 方法进行检查。
    3.  该方法判断计数器的值是否达到了在 `Config` 中配置的阈值 `ack_threshold`（例如2）。
    4.  如果达到阈值，即使当前没有数据要发送，协议也会立即生成并发送一个独立的 `ACK` 包。

```rust
// 位于 src/core/reliability.rs
pub fn should_send_standalone_ack(&self) -> bool {
    self.ack_eliciting_packets_since_last_ack >= self.config.ack_threshold
        && !self.recv_buffer.get_sack_ranges().is_empty()
}
```
这确保了发送方能够及时获得网络状况的反馈，从而做出更精准的重传和拥塞控制决策。 