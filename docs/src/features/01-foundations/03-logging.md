# 3: 结构化日志记录

**功能描述:**

项目全面集成了 `tracing` 框架，用于在整个协议栈的关键路径上进行结构化、事件驱动的日志记录。这为调试、性能分析和运行时监控提供了强大的可观测性。

**实现位置:**

`tracing` 的使用贯穿于整个代码库。以下是一些最关键的实现点：

### 1. 日志记录的层级和范围

项目根据事件的重要性和详细程度，策略性地使用了不同的日志级别：

- **`info!`**: 用于记录关键的生命周期事件，如服务的启动/关闭、新连接的建立、连接迁移的成功等。这些是操作人员通常最关心的信息。
- **`debug!`**: 用于记录常规的数据流信息，例如数据包的收发、ACK的处理、窗口的更新等。这对于理解协议的正常工作流程非常有帮助。
- **`warn!`**: 用于记录潜在的问题，例如收到一个格式正确但逻辑上意外的包（如来自未知对端的包）。
- **`error!`**: 用于记录明确的错误情况，如IO错误、任务panic等。
- **`trace!`**: 用于记录最详细的内部状态变化，尤其是在拥塞控制算法 (`vegas.rs`) 中，用于追踪RTT、拥塞窗口 (`cwnd`) 等参数的精细变化。

### 2. 关键模块中的日志记录

- **`src/socket/actor.rs`**: 作为协议的“路由器”，此模块是日志记录的中心。它详细记录了：
    - Socket的创建和监听地址。
    - 每个入站UDP数据包的来源和大小。
    - 包分发逻辑：是路由到现有连接，还是作为新连接处理。
    - 连接迁移请求的处理。

    ```rust
    // 位于 src/socket/actor.rs
    // ...
    debug!(
        "Received {} bytes from {}, dispatching to connection {}",
        len, remote_addr, conn_id
    );
    // ...
    ```

- **`src/core/endpoint/logic.rs`**: 每个连接自身的状态机转换也被清晰地记录下来，便于追踪单个连接的生命周期。

- **`src/congestion/vegas.rs`**: 拥塞控制是协议中最复杂的部分之一。`tracing` 在这里被用来输出算法决策的关键内部变量。

    ```rust
    // 位于 src/congestion/vegas.rs
    // ...
    trace!(
        cwnd = self.cwnd,
        base_rtt = self.base_rtt.as_millis(),
        diff = diff,
        "Vegas cwnd increase"
    );
    // ...
    ```

### 3. 如何使用

库本身只负责产生日志事件。库的使用者（例如一个应用服务器）需要负责配置一个 `tracing` 的 `Subscriber`（如 `tracing-subscriber`）来决定如何收集、格式化和输出这些日志。

例如，一个使用者可以通过以下方式初始化一个简单的日志记录器，将所有 `info` 级别及以上的日志打印到控制台：

```rust,ignore
// 在使用库的应用程序的 main.rs 中
fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    // ... 启动你的服务器和协议栈 ...
}
```

这种设计将日志的**产生**与**消费**解耦，给予了库使用者完全的控制权。 