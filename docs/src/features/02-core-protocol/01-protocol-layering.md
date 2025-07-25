# 2.1: 清晰的协议分层

**功能描述:**

项目遵循现代网络协议栈的设计思想，实现了一套清晰、解耦的垂直分层架构。这种架构将不同的职责分离到独立的组件中，极大地提高了代码的可维护性、可测试性和可扩展性。

**实现位置:**

- **顶层 - `Endpoint`**: `src/core/endpoint.rs`
- **中层 - `ReliabilityLayer`**: `src/core/reliability.rs`
- **底层 - `CongestionControl` Trait**: `src/congestion.rs`

### 1. 架构概览

协议的核心逻辑被划分为三个层次：

1.  **`Endpoint` (连接端点层)**: 作为协议的“大脑”，负责管理单个连接的完整生命周期（握手、通信、关闭）、处理用户API的命令 (`StreamCommand`) 以及与底层Socket的任务进行通信。它**拥有**一个 `ReliabilityLayer` 实例。

2.  **`ReliabilityLayer` (可靠性层)**: 负责所有与可靠传输相关的核心算法。这包括数据包的序列化、基于SACK的确认、RTO（重传超时）计算、快速重传以及乱序重组。它是一个纯粹的逻辑层，不执行任何I/O操作。它**拥有**一个实现了 `CongestionControl` trait 的实例。

3.  **`CongestionControl` (拥塞控制层)**: 这是一个Trait（接口），定义了拥塞控制算法所需满足的契约。具体的算法（如 `Vegas`）作为该Trait的实现。这使得拥塞控制算法可以被轻松替换，而无需改动上层逻辑。

### 2. 交互流程与设计模式

这种分层结构采用了典型的**策略模式 (Strategy Pattern)** 和**委托模式 (Delegation Pattern)**。

```mermaid
graph TD
    subgraph Endpoint [Endpoint Layer]
        A[Connection Logic]
    end

    subgraph ReliabilityLayer [Reliability Layer]
        B[ARQ, SACK, RTO Logic]
    end

    subgraph CongestionControl [Congestion Control Layer]
        C[Congestion Algorithm (e.g., Vegas)]
    end

    A -- "Delegate reliability tasks" --> B
    B -- "Notify network events (ACK, Loss)" --> C
    C -- "Provide congestion window (cwnd)" --> B
```

- **`Endpoint` -> `ReliabilityLayer`**: `Endpoint` 将所有需要可靠传输的数据块和收到的ACK信息全部委托给 `ReliabilityLayer` 处理。

- **`ReliabilityLayer` <-> `CongestionControl`**:
    - **事件通知**: 当 `ReliabilityLayer` 收到ACK或检测到丢包时，它会调用 `CongestionControl` trait 的 `on_ack()` 或 `on_packet_loss()` 方法，将网络事件通知给拥塞控制算法。
    - **获取许可**: 当 `ReliabilityLayer` 准备发送数据时，它会调用 `CongestionControl` trait 的 `congestion_window()` 方法来获取当前允许发送的数据量（拥塞窗口大小），以判断自己是否可以继续发送。

### 3. 代码实现

**`Endpoint` 持有 `ReliabilityLayer`:**

```rust
// 位于 src/core/endpoint.rs
pub struct Endpoint<S: AsyncUdpSocket> {
    // ... 其他字段
    reliability: ReliabilityLayer,
    // ... 其他字段
}
```

**`ReliabilityLayer` 持有 `CongestionControl` Trait 对象:**

```rust
// 位于 src/core/reliability.rs
pub struct ReliabilityLayer {
    // ... 其他字段
    congestion_control: Box<dyn CongestionControl>,
    // ... 其他字段
}

impl ReliabilityLayer {
    pub fn new(config: Config, congestion_control: Box<dyn CongestionControl>) -> Self {
        // ...
    }
    
    pub fn handle_ack(/* ... */) {
        // ...
        self.congestion_control.on_ack(rtt_sample);
        // ...
    }
    
    pub fn can_send_more(&self, peer_recv_window: u32) -> bool {
        // ...
        let cwnd = self.congestion_control.congestion_window();
        // ...
    }
}
```

这种设计是整个协议库能够保持高度模块化和灵活性的基石。 