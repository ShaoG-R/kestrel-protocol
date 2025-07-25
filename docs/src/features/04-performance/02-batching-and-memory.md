# 2: 批处理与内存优化

**功能描述:**

协议在多个关键路径上实施了批处理（Batching）和内存优化策略，以在高吞吐量场景下最大限度地减少`await`带来的上下文切换开销，并降低数据在协议栈内部流转时的内存拷贝次数。

**实现位置:**

- **批处理**:
    - `src/socket/sender.rs` (`sender_task`)
    - `src/core/endpoint/logic.rs` (`Endpoint::run`)
- **内存优化**:
    - `src/core/reliability/recv_buffer.rs` (`reassemble`)

### 1. I/O与事件批处理

批处理的核心思想是在一次异步唤醒中，尽可能多地处理积压的事件，而不是每处理一个事件就`await`一次。

- **发送批处理**:
  `SenderTask` 在从MPSC通道收到第一个发送命令后，并不会立即处理，而是会使用`try_recv()`非阻塞地尝试从通道中“榨干”所有待处理的发送命令，将它们收集到一个本地`Vec`中，然后在一个同步循环里一次性处理完毕。

- **接收与命令批处理**:
  `Endpoint`的主事件循环（`run`方法）也采用了相同的模式。当它从网络或用户`Stream`的MPSC通道中`recv()`到一个事件后，它会立刻在后面跟一个`while let Ok(...) = ... .try_recv()`循环，将该通道中所有已准备好的事件一次性处理完，然后再进入下一个`tokio::select!`等待。

```rust
// 位于 src/core/endpoint/logic.rs - Endpoint::run
// ...
Some((frame, src_addr)) = self.receiver.recv() => {
    self.handle_frame(frame, src_addr).await?;
    // 在await之后，立即尝试排空队列
    while let Ok((frame, src_addr)) = self.receiver.try_recv() {
        self.handle_frame(frame, src_addr).await?;
    }
}
// ...
```
这种模式显著降低了在高负载下任务被调度和唤醒的频率，是提升CPU效率的关键优化。

### 2. 减少内存拷贝

在网络协议栈中，不必要的内存拷贝是主要的性能瓶颈之一。本项目在接收路径上做出了关键优化。

- **机制**:
  当`ReceiveBuffer`将乱序的数据包重组为有序的数据流时，它并**不会**分配一块新的大内存，然后将每个小包的数据拷贝进去。相反，它的`reassemble`方法直接返回一个`Vec<Bytes>`。
  
  `Bytes`是Rust生态中一个强大的“零拷贝”数据结构，它可以高效地表示共享的、不可变的字节缓冲区。通过返回`Vec<Bytes>`，`ReceiveBuffer`只是将原始数据包载荷的所有权转移给了上层，整个重组过程没有任何内存拷贝。

```rust
// 位于 src/core/reliability/recv_buffer.rs
// 注意返回值类型，它直接交出了原始的Bytes对象
pub fn reassemble(&mut self) -> Option<Vec<Bytes>> {
    let mut reassembled_data = Vec::new();
    while let Some(payload) = self.try_pop_next_contiguous() {
        reassembled_data.push(payload);
    }
    // ...
}
```
数据直到最后一步，即在`Stream`的`poll_read`方法中被拷贝到用户提供的`ReadBuf`里时，才会发生第一次真正的内存拷贝。这使得数据在整个协议栈内部的流转几乎是零拷贝的。 