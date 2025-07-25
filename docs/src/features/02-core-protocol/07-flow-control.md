# 2.7: 滑动窗口流量控制

**功能描述:**

协议实现了基于滑动窗口的端到端流量控制机制，以防止快速的发送方淹没慢速的接收方，导致接收端缓冲区溢出和不必要的丢包。

**实现位置:**

- **接收窗口计算与通告**: `src/core/reliability/recv_buffer.rs`
- **发送窗口限制**: `src/core/reliability.rs`
- **窗口信息传输**: `src/packet/header.rs` (`ShortHeader`)

### 1. 流量控制原理

滑动窗口机制的核心思想是：接收方在告知自己网络状态（通过ACK）的同时，也告知自己还有多少缓冲区空间。发送方则保证自己“在途”（已发送但未确认）的数据量不会超过接收方所通告的窗口大小。

### 2. 接收窗口 (rwnd) 的计算与通告

- **计算**: `ReceiveBuffer` 负责管理接收到的数据包。它的可用窗口大小（`rwnd`）通过一个简单的公式计算得出：
  `rwnd = capacity - received_count`
  其中，`capacity` 是接收缓冲区的总容量（以包为单位），`received_count` 是当前已缓存的乱序包数量。

  ```rust
  // 位于 src/core/reliability/recv_buffer.rs
  pub fn window_size(&self) -> u16 {
      (self.capacity.saturating_sub(self.received.len())) as u16
  }
  ```

- **通告**: 发送方在发送**任何带有 `ShortHeader` 的数据包**时，都会调用 `recv_buffer.window_size()` 获取当前最新的 `rwnd`，并将其填入头部的 `recv_window_size` 字段。这意味着接收窗口的大小是被持续、动态地通告给对端的。

### 3. 发送窗口的限制

发送方的行为同时受到**两个窗口**的限制：

1.  **拥塞窗口 (`cwnd`)**: 由拥塞控制算法（如Vegas）计算得出，反映了当前网络的承载能力。
2.  **接收窗口 (`rwnd`)**: 由对端通告，反映了对端的处理能力。

- **有效发送窗口**: 发送方在任何时候可以拥有的在途数据包数量，不能超过 `min(cwnd, rwnd)`。

- **实现**: 这个核心限制逻辑在 `ReliabilityLayer` 的 `can_send_more` 方法中实现。

  ```rust
  // 位于 src/core/reliability.rs
  pub fn can_send_more(&self, peer_recv_window: u32) -> bool {
      let in_flight = self.send_buffer.in_flight_count() as u32;
      let cwnd = self.congestion_control.congestion_window();
      
      // 在途包数量必须同时小于拥塞窗口和对端的接收窗口
      in_flight < cwnd && in_flight < peer_recv_window
  }
  ```

`Endpoint` 在其主循环中，每次准备发送数据前，都会调用 `can_send_more` 来检查是否可以继续发送。这种机制确保了协议能够同时适应网络状况和对端的处理能力，实现了健壮的流量控制。 