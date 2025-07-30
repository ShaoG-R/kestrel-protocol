# 可靠性层 (`reliability`) - ARQ与拥塞控制核心

## 概述

`reliability`模块是协议栈的“可靠性引擎”。它位于`Endpoint`的核心逻辑之下，负责实现所有与数据可靠传输相关的机制，包括自动重传请求（ARQ）、选择性确认（SACK）、流量控制和拥塞控制。该模块将复杂的传输层协议逻辑封装成一个独立的、可测试的单元，为上层提供了“即发即忘”的可靠数据流服务。

**核心使命:**
- **保证数据可靠性**: 通过基于SACK的ARQ机制，确保所有数据包最终都能被对方接收。
- **处理网络异常**: 有效处理数据包的丢失、乱序和重复。
- **流量与拥塞控制**: 通过滑动窗口进行流量控制，并采用基于延迟的拥塞控制算法（Vegas）来避免网络拥塞，保证在恶劣网络环境下的稳定性。
- **数据流适配**: 将上层`Stream`的字节流分割成数据包（Packetization），并将接收到的数据包重组成有序的字节流（Reassembly）。

**架构实现:**
- **主协调器**: `src/core/reliability/retransmission.rs` - `ReliabilityLayer`作为模块的门面，聚合了所有子组件。
- **发送缓冲**: `src/core/reliability/send_buffer.rs` - `SendBuffer`，暂存用户待发送的字节流。
- **接收缓冲**: `src/core/reliability/recv_buffer.rs` - `ReceiveBuffer`，处理入站数据包的乱序、去重和重组。
- **打包器**: `src/core/reliability/packetizer.rs` - `Packetizer`，将字节流分割成`PUSH`帧。
- **SACK与重传**: `src/core/reliability/retransmission/sack_manager.rs` - `SackManager`，ARQ和快速重传的核心。
- **拥塞控制**: `src/core/reliability/congestion/vegas.rs` - `Vegas`算法的具体实现。

## 设计原则

### 1. 分层与聚合
- **职责分离**: 模块内部清晰地分离了数据缓冲(`Send/ReceiveBuffer`)、打包(`Packetizer`)、重传(`SackManager`)和拥塞控制(`CongestionControl` trait)的职责。
- **统一门面**: `ReliabilityLayer`作为聚合根（Facade模式），将这些独立的组件组合在一起，并向上层`Endpoint`提供了一套简洁、统一的高层API（如`handle_ack`, `packetize_stream_data`），隐藏了内部的复杂性。

### 2. 状态与逻辑分离
- **无状态组件**: `Packetizer`是一个典型的无状态辅助模块，它的所有行为都由传入的上下文决定，易于测试和推理。
- **状态组件**: `SendBuffer`, `ReceiveBuffer`, `SackManager`和`Vegas`是状态化的，它们各自维护着发送、接收、在途包和拥塞状态。`ReliabilityLayer`负责协调这些状态的更新。

### 3. 可插拔的拥塞控制
- **Trait抽象**: 拥塞控制算法通过`CongestionControl` trait进行抽象。`ReliabilityLayer`依赖于这个trait，而不是具体的`Vegas`实现。
- **灵活性**: 这种设计使得未来可以轻松地实现和替换其他拥塞控制算法（如BBR），而无需修改`ReliabilityLayer`的核心逻辑。

## 整体架构与数据流

`ReliabilityLayer`是数据在`Endpoint`内部流转的核心枢纽。

```mermaid
graph TD
    subgraph "Endpoint Layer"
        A[User Write (Bytes)] --> B[ReliabilityLayer::write_to_stream]
        C[ReliabilityLayer::packetize_stream_data] -- "PUSH Frames" --> D[Frames to Send]
        E[Network Frame (ACK)] --> F[ReliabilityLayer::handle_ack]
        G[ReliabilityLayer::reassemble] -- "Ordered Bytes" --> H[User Read]
    end

    subgraph "Reliability Layer Internal"
        B --> SB(SendBuffer)
        P[Packetizer] -- "Reads from" --> SB
        P -- "Creates" --> C
        
        F -- "Updates" --> SM(SackManager)
        F -- "Updates" --> CC(CongestionControl)
        SM -- "RTT Samples" --> RTT(RttEstimator)
        RTT -- "SRTT" --> CC
        SM -- "Triggers" --> FR[Fast Retransmission]
        
        RB(ReceiveBuffer) -- "SACK Info" --> F
        RB -- "Provides data for" --> G
        NF[Network Frame (PUSH)] --> RB
    end
    
    style SB fill:#bbf,stroke:#333,stroke-width:2px
    style RB fill:#bbf,stroke:#333,stroke-width:2px
    style SM fill:#f9f,stroke:#333,stroke-width:2px
    style CC fill:#ccf,stroke:#333,stroke-width:2px
```
**数据流解读:**
1.  **发送路径**:
    - 用户写入的数据(`Bytes`)通过`write_to_stream`进入`SendBuffer`。
    - `Endpoint`调用`packetize_stream_data`，内部的`Packetizer`会根据拥塞/流量窗口的许可，从`SendBuffer`拉取数据，并将其打包成`PUSH`帧。
    - 这些帧被`SackManager`标记为“飞行中”，然后被发送出去。

2.  **ACK处理路径**:
    - `Endpoint`收到`ACK`帧后，调用`handle_ack`。
    - `SackManager`解析ACK中的SACK信息，将已确认的包从“飞行中”移除，并计算出RTT样本。
    - RTT样本被送到`RttEstimator`更新SRTT（平滑往返时间）。
    - SRTT和丢包信号被传递给`CongestionControl`（Vegas）模块，以调整拥塞窗口。
    - 如果`SackManager`检测到满足快速重传条件（如收到3个冗余ACK），它会立即决定重传丢失的数据包。

3.  **接收路径**:
    - `Endpoint`收到`PUSH`帧后，将其送入`ReceiveBuffer`。
    - `ReceiveBuffer`负责处理乱序和重复，将数据包按序列号存入一个`BTreeMap`。
    - `Endpoint`周期性地调用`reassemble`，从`ReceiveBuffer`中提取出连续有序的数据，交给用户读取。
    - `ReceiveBuffer`同时能够根据内部的“空洞”生成`SACK`范围，为发送ACK帧提供依据。

## 核心组件解析

### `ReceiveBuffer` - 乱序重组与SACK生成器

`ReceiveBuffer`的核心是一个`BTreeMap<u32, PacketOrFin>`，它利用`BTreeMap`按键排序的特性来自然地处理乱序数据包。
- **接收**: 当收到一个`PUSH`帧时，如果其序列号`seq`大于等于`next_sequence`（期望的下一个连续包号），则将其存入`BTreeMap`。
- **重组**: `reassemble`方法会检查`BTreeMap`的第一个元素的序列号是否等于`next_sequence`。如果是，就将其取出，并递增`next_sequence`，然后继续检查下一个元素，直到遇到不连续的包。
- **SACK生成**: `get_sack_ranges`方法遍历`BTreeMap`中的所有键（序列号），并将连续的序列号块合并成`SackRange`，从而精确地告诉对端哪些数据包已经被接收。

### `SackManager` - ARQ大脑

`SackManager`是实现可靠性的关键。它维护一个`BTreeMap<u32, SackInFlightPacket>`来跟踪所有已发送但未确认的数据包。
- **跟踪**: 发送数据包时，记录其发送时间戳和内容。
- **确认**: 收到`ACK`时，根据累积确认号和SACK范围，将对应的包从“飞行中”移除。
- **RTT计算**: 对于每个新确认的包，用当前时间减去其发送时间戳，得到一个RTT样本。
- **快速重传**: 当一个包`seq_A`未被确认，但后续更高序列号的包`seq_B`, `seq_C`, `seq_D`都被SACK确认时，`SackManager`会认为`seq_A`很可能已经丢失，并在RTO（重传超时）定时器到期前，立即将其重传。

### `Vegas` - 基于延迟的拥塞控制器

`Vegas`算法的核心思想是**主动避免拥塞，而非被动地响应丢包**。
- **基准RTT**: 它会持续跟踪连接的最小RTT（`min_rtt`），将其作为网络未拥塞时的基准延迟。
- **队列评估**: 对于每个收到的ACK，它会计算出当前RTT与`min_rtt`的差值。这个差值反映了数据包在网络中间节点（如路由器）缓冲区中的排队延迟。
- **窗口调整**:
    - 当排队延迟很小时（`diff < alpha`），说明网络路径通畅，可以线性增加拥塞窗口。
    - 当排队延迟过大时（`diff > beta`），说明网络开始出现拥塞迹象，需要线性减小拥塞窗口。
    - 当延迟在`alpha`和`beta`之间时，保持窗口大小不变。
- **丢包处理**: 与传统算法（如Reno）在每次丢包时都将窗口减半不同，Vegas会区分**拥塞性丢包**（RTT显著增大时发生）和**随机性丢包**。对于前者，它会采取激进的窗口减半策略；对于后者，则只是温和地减小窗口，避免了在无线等高随机丢包网络下的性能骤降。

## 总结

`reliability`模块通过其精心设计的组件化架构，成功地将复杂的可靠传输逻辑分解为一系列协同工作的、职责明确的单元。它不仅实现了高效的基于SACK的ARQ机制，还集成了一个先进的、为不稳定网络优化的Vegas拥塞控制算法，共同构成了整个协议栈稳定、高效运行的坚实基础。
