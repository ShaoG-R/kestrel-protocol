# 1: 清晰的协议分层

**功能描述:**

项目遵循现代网络协议栈的设计思想，实现了一套清晰、解耦的垂直分层架构。这种架构将不同的职责分离到独立的组件中，极大地提高了代码的可维护性、可测试性和可扩展性。它将复杂的协议逻辑分解为一系列内聚且可独立测试的层次。

### 1. 架构概览

协议栈从上到下可以分为六个逻辑层次，其核心设计思想是 **"分层解耦，各司其职"**。

1.  **L6: API层 (`stream`)**:
    *   **职责**: 为用户提供符合人体工程学的 `AsyncRead`/`AsyncWrite` 字节流接口，隐藏所有底层包的细节。并将用户的读写操作转换为命令，通过MPSC通道发送给对应的 `Endpoint` 任务进行处理。

2.  **L5: 连接管理层 (`socket`)**:
    *   **职责**: 协议栈的“大脑”和事件中枢，在 `SocketEventLoop` 中管理所有连接的生命周期。其核心是`FrameRouter`，它能够智能地将网络帧分派给正确的`Endpoint`实例，并原生支持连接迁移。

3.  **L4: 端点层 (`endpoint`)**:
    *   **职责**: 作为单个连接的"微观世界"，在独立的Tokio任务中运行，编排连接的生命周期，并根据状态向下层的 `ReliabilityLayer` 发出明确指令。它处理来自API层和网络层的所有事件，但不实现具体的可靠性算法。

4.  **L3: 可靠性层 (`reliability`)**:
    *   **职责**: 这是一个综合性的功能实现层，封装了基于SACK的ARQ（自动重传请求）、动态RTO计算、快速重传、滑动窗口流量控制等核心可靠性机制。它接收来自端点层的清晰指令，执行具体的算法逻辑。

5.  **L2: 拥塞控制层 (`congestion`)**:
    *   **职责**: 作为可插拔的算法层，通过`CongestionControl` Trait实现。当前默认实现了基于延迟的Vegas算法，并且该设计支持未来轻松集成其他算法（如BBR）。

6.  **L1: 传输抽象层 (`transport`)**:
    *   **职责**: 抽象底层I/O操作，提供高性能的UDP数据包收发能力。它将发送和接收逻辑解耦到独立的异步任务中，并实现批量发送优化。

```mermaid
graph TD
    subgraph "用户应用 (User Application)"
        APP[Application Code]
    end

    subgraph "L6: API层 (Stream API)"
        STREAM[Stream]
    end
    
    subgraph "L5: 连接管理层 (Socket Layer)"
        SOCKET[SocketEventLoop & FrameRouter]
    end

    subgraph "L4: 端点层 (Endpoint Layer)"
        ENDPOINT[Endpoint]
    end

    subgraph "L3: 可靠性层 (Reliability Layer)"
        RELIABILITY[ReliabilityLayer]
    end

    subgraph "L2: 拥塞控制层 (Congestion Control)"
        CONGESTION[CongestionControl Trait]
    end
    
    subgraph "L1: 传输层 (Transport Layer)"
        TRANSPORT[Transport & Batch Sender]
    end

    subgraph "网络 (Network)"
        NETWORK_IO[Physical UDP Socket]
    end

    APP -- "read()/write()" --> STREAM
    STREAM -- "StreamCommand" --> ENDPOINT
    SOCKET -- "Manages" --> ENDPOINT
    SOCKET -- "Routes Frame to" --> ENDPOINT
    ENDPOINT -- "Uses" --> RELIABILITY
    ENDPOINT -- "Submits FrameBatch to" --> SOCKET
    RELIABILITY -- "Uses" --> CONGESTION
    SOCKET -- "Sends FrameBatch via" --> TRANSPORT
    TRANSPORT -- "Reads/Writes" --> NETWORK_IO
    
    classDef l6 fill:#2C3E50,color:#ECF0F1,stroke:#BDC3C7
    classDef l5 fill:#34495E,color:#ECF0F1,stroke:#BDC3C7
    classDef l4 fill:#8E44AD,color:#FFFFFF,stroke:#BDC3C7
    classDef l3 fill:#16A085,color:#FFFFFF,stroke:#BDC3C7
    classDef l2 fill:#27AE60,color:#FFFFFF,stroke:#BDC3C7
    classDef l1 fill:#7F8C8D,color:#FFFFFF,stroke:#BDC3C7
    classDef default fill:#333,color:#fff
    
    class APP default
    class STREAM l6
    class SOCKET l5
    class ENDPOINT l4
    class RELIABILITY l3
    class CONGESTION l2
    class TRANSPORT l1
    class NETWORK_IO default
```

### 2. Endpoint内部架构

端点层作为协议的核心编排器，内部采用了高度模块化的设计。下图展示了Endpoint内部各组件的详细结构和交互关系：

```mermaid

graph TD
    subgraph "Endpoint内部架构"
        subgraph "生命周期管理"
            LM[LifecycleManager<br/>状态转换与验证]
        end
        
        subgraph "事件处理系统"
            ED[EventDispatcher<br/>事件分发器]
            
            subgraph "帧处理器集合"
                CP[ConnectionProcessor<br/>连接控制]
                DP[DataProcessor<br/>数据处理]  
                AP[AckProcessor<br/>确认处理]
                PP[PathProcessor<br/>路径验证]
            end
        end
        
        subgraph "核心逻辑"
            LOGIC[EndpointLogic<br/>主事件循环]
            FF[FrameFactory<br/>帧构造器]
        end
        
        subgraph "外部接口"
            STREAM_IF[Stream Interface<br/>用户API通道]
            NETWORK_IF[Network Interface<br/>网络包通道]
        end
    end

    %% 数据流和控制流
    STREAM_IF -->|StreamCommand| LOGIC
    NETWORK_IF -->|IncomingFrame| ED
    
    ED -->|连接帧| CP
    ED -->|数据帧| DP
    ED -->|确认帧| AP
    ED -->|路径帧| PP
    
    CP -->|状态变更| LM
    DP -->|状态检查| LM
    AP -->|状态检查| LM
    PP -->|状态变更| LM
    
    LOGIC -->|状态转换| LM
    LOGIC -->|构造帧| FF
    FF -->|输出帧| NETWORK_IF
    
    LM -.->|状态通知| LOGIC
    
    %% 样式定义 (深色主题)
    classDef lifecycle fill:#2C3E50,color:#ECF0F1,stroke:#BDC3C7
    classDef event fill:#34495E,color:#ECF0F1,stroke:#BDC3C7
    classDef processor fill:#16A085,color:#FFFFFF,stroke:#BDC3C7
    classDef logic fill:#27AE60,color:#FFFFFF,stroke:#BDC3C7
    classDef interface fill:#7F8C8D,color:#FFFFFF,stroke:#BDC3C7

    class LM lifecycle
    class ED event
    class CP,DP,AP,PP processor
    class LOGIC,FF logic
    class STREAM_IF,NETWORK_IF interface
```

### 3. 数据流程分析

#### 3.1 写数据路径 (Write Path)

1.  **用户调用**: 用户调用 `stream.write(data)`。
2.  **命令转换**: `Stream` 将写请求转换为 `StreamCommand::SendData`，通过MPSC通道发送给 `Endpoint`。
3.  **状态检查**: `Endpoint` 通过 `LifecycleManager` 检查当前连接状态是否允许发送数据。
4.  **可靠性处理**: `Endpoint`将数据交给 `ReliabilityLayer`。`ReliabilityLayer`的`Packetizer`根据拥塞和流量窗口的许可，将数据打包成`PUSH`帧。
5.  **帧聚合**: `Endpoint`收集`ReliabilityLayer`生成的`PUSH`帧和`ACK`帧，聚合成一个`FrameBatch`。
6.  **提交发送**: `Endpoint`将`FrameBatch`提交给`Socket`任务。
7.  **批量发送**: `Socket`将`FrameBatch`转发给`transport_sender_task`，后者通过`Transport`层接口进行批量网络发送。

#### 3.2 读数据路径 (Read Path)

1.  **网络接收**: `Transport`层的接收任务从UDP socket接收到原始包，解码成`Frame`。
2.  **路由分发**: `Transport`将解码后的数据报发送给`SocketEventLoop`。`FrameRouter`根据连接ID，将`Frame`路由到正确的`Endpoint`任务。
3.  **事件处理**: `Endpoint`收到`Frame`，交由内部的`EventDispatcher`分发给相应的`FrameProcessor`。
4.  **可靠性处理**:
    *   `DataProcessor`将`PUSH`帧交给`ReliabilityLayer`的`ReceiveBuffer`进行去重和排序。
    *   `AckProcessor`将`ACK`帧交给`ReliabilityLayer`的`SackManager`更新在途包列表和RTT。
5.  **数据重组**: `Endpoint`调用`ReliabilityLayer`的`reassemble`方法，从`ReceiveBuffer`中提取出连续有序的字节流。
6.  **用户接收**: `Endpoint`通过MPSC通道将重组后的数据发送给 `Stream`，用户通过 `stream.read()` 获取。

#### 3.3 生命周期管理 (Lifecycle Management)

`LifecycleManager` 统一管理连接的整个生命周期：

*   **连接建立**: 处理握手过程中的状态转换 (`Connecting` → `SynReceived` → `Established`)
*   **正常通信**: 维护 `Established` 状态，协调数据传输
*   **连接迁移**: 处理路径验证和地址迁移 (`Established` → `PathValidating` → `Established`)
*   **优雅关闭**: 管理四次挥手过程 (`Established` → `Closing` → `ClosingWait` → `Closed`)
*   **异常处理**: 处理错误情况下的状态转换和资源清理

### 4. 设计优势

#### 4.1 职责分离 (Separation of Concerns)

*   **状态管理**: `LifecycleManager` 专注于连接状态的一致性和转换逻辑
*   **事件处理**: `EventDispatcher` 和 `FrameProcessors` 专注于不同类型帧的解析和处理
*   **可靠性保证**: `ReliabilityLayer` 专注于数据传输的可靠性算法
*   **拥塞控制**: 独立的算法层，易于替换和扩展

#### 4.2 高度模块化 (High Modularity)

*   每个组件都有明确的接口和职责边界
*   组件间通过明确的API进行交互，降低耦合度
*   便于单独测试和验证每个组件的功能
*   支持组件的独立演进和优化

#### 4.3 代码复用性 (Code Reusability)

*   `FrameProcessors` 可以在不同类型的连接中复用
*   `CongestionControl` trait允许插入不同的拥塞控制算法
*   `LifecycleManager` 的状态转换逻辑对所有连接通用

#### 4.4 可扩展性 (Extensibility)

*   易于添加新的帧类型和对应的处理器
*   可以轻松集成新的拥塞控制算法
*   支持协议的向后兼容升级

### 5. 实现细节

#### 5.1 无锁并发模型

*   每个 `Endpoint` 运行在独立的Tokio任务中
*   通过MPSC通道进行跨任务通信，避免共享状态锁
*   所有状态修改都在单一任务内进行，保证线程安全

#### 5.2 现代化状态管理

*   `LifecycleManager` 提供统一的状态转换接口
*   支持复杂的状态转换验证和约束
*   完全替代了旧的StateManager，消除了状态管理的双重性

#### 5.3 性能优化

*   帧处理器采用零拷贝设计，减少内存分配
*   支持包聚合和批处理，提高网络效率
*   事件驱动的架构，减少不必要的轮询