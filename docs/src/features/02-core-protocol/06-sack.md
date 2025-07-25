# 2.6: 基于SACK的高效确认

**功能描述:**

协议采用了基于SACK（Selective Acknowledgment，选择性确认）的高效确认机制，取代了传统的累积确认（Cumulative ACK）。这使得接收方可以精确地告知发送方自己收到了哪些离散的数据块，极大地提高了在乱序和丢包网络环境下的确认效率和重传精度。

**实现位置:**

- **SACK范围生成**: `src/core/reliability/recv_buffer.rs`
- **SACK信息处理**: `src/core/reliability/send_buffer.rs`
- **SACK序列化**: `src/packet/sack.rs`

### 1. SACK的工作原理

当网络发生乱序或丢包时，接收方的缓冲区中会形成不连续的数据块“空洞”。

- **传统ACK**: 只能告知发送方“我已连续收到了X号之前的所有包”，无法表达“我收到了X+2，但没收到X+1”。
- **SACK**: `ACK`包的载荷中可以携带一个或多个`SackRange`（如`{start: 10, end: 20}`），明确告知发送方“我收到了序号从10到20的所有包”。

### 2. SACK范围的生成

- **机制**: `ReceiveBuffer` 使用 `BTreeMap` 来存储所有已接收但尚未按序提交给上层的包。`get_sack_ranges` 方法会遍历这个有序的Map，将所有连续的序列号区间合并成 `SackRange` 对象。

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

发送方在构造 `ACK` 包时，会调用 `get_sack_ranges()`，并将返回的范围列表通过 `src/packet/sack.rs` 中的 `encode_sack_ranges` 函数序列化为字节，放入 `ACK` 帧的载荷中。

### 4. SACK信息的处理

当发送方收到一个 `ACK` 包时，其 `SendBuffer` 的 `handle_ack` 方法会：

1.  **处理累积ACK**: 首先处理 `ACK` 头部的 `recv_next_sequence` 字段，将所有序号小于此值的在途包标记为已确认。
2.  **处理SACK范围**: 然后，解码 `ACK` 载荷中的所有 `SackRange`，并将在这些范围内的所有在途包也标记为已确认。

```rust
// 位于 src/core/reliability/send_buffer.rs
pub fn handle_ack(
    &mut self,
    recv_next_seq: u32,
    sack_ranges: &[SackRange],
    // ...
) -> (Vec<Frame>, Vec<Duration>) {
    // ... 处理累积ACK ...
    for seq in recv_next_seq.. {
        // ...
    }
    
    // ... 处理SACK范围 ...
    for range in sack_ranges {
        for seq in range.start..=range.end {
            if let Some(packet) = self.in_flight.remove(&seq) {
                // ... 包已确认，计算RTT ...
            }
        }
    }
    
    // ... 检查快速重传 ...
}
```

这种双重处理机制确保了最大程度的信息利用，使得发送方能最快地清理其在途包列表、更新RTT估算，并精确地识别出真正需要重传的包。 