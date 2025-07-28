# 2: 统一的可配置化

**功能描述:**

协议的所有关键行为和参数都通过一个统一的 `Config` 结构体进行管理。为了清晰和易用，`Config` 内部被划分为几个功能内聚的子结构体：

-   `ReliabilityConfig`: 控制可靠性机制，如RTO、重传次数等。
-   `CongestionControlConfig`: 控制拥塞控制算法（如Vegas）的行为。
-   `ConnectionConfig`: 控制连接级别的参数，如缓冲区大小、超时时间等。

这种设计将可调参数与核心逻辑分离，为用户提供了极大的灵活性，允许他们根据不同的网络环境和应用场景微调协议性能。

**实现位置:**

- **文件:** `src/config.rs`

### 1. 中心化的分层 `Config` 结构体

`Config` 结构体是所有可配置参数的唯一来源。它通过包含子结构体来组织参数。

```rust
// 位于 src/config.rs
#[derive(Debug, Clone)]
pub struct Config {
    pub protocol_version: u8,
    pub reliability: ReliabilityConfig,
    pub congestion_control: CongestionControlConfig,
    pub connection: ConnectionConfig,
}

#[derive(Debug, Clone)]
pub struct ReliabilityConfig {
    pub initial_rto: Duration,
    pub min_rto: Duration,
    pub fast_retx_threshold: u16,
    // ... 其他可靠性参数
}

#[derive(Debug, Clone)]
pub struct CongestionControlConfig {
    pub initial_cwnd_packets: u32,
    pub min_cwnd_packets: u32,
    pub vegas_alpha_packets: u32,
    pub vegas_beta_packets: u32,
    // ... 其他拥塞控制参数
}

#[derive(Debug, Clone)]
pub struct ConnectionConfig {
    pub max_payload_size: usize,
    pub idle_timeout: Duration,
    pub send_buffer_capacity_bytes: usize,
    pub recv_buffer_capacity_packets: usize,
    // ... 其他连接参数
}
```

### 2. 合理的默认值

每个配置结构体（包括 `Config` 和它的所有子结构体）都实现了 `Default` trait，为所有参数提供了一套经过验证的、合理的默认值。这意味着用户可以在不了解所有参数细节的情况下，轻松地开始使用本协议。

```rust
// 位于 src/config.rs
impl Default for Config {
    fn default() -> Self {
        Self {
            protocol_version: 1,
            reliability: ReliabilityConfig::default(),
            congestion_control: CongestionControlConfig::default(),
            connection: ConnectionConfig::default(),
        }
    }
}
```

### 3. 应用配置

在创建 `ReliableUdpSocket` 或 `Endpoint` 时，用户可以选择性地传入一个 `Config` 实例。如果不提供，则会自动使用 `Config::default()`。

修改特定参数也非常方便，例如：

```rust
let mut config = Config::default();
config.connection.idle_timeout = Duration::from_secs(30);
config.reliability.initial_rto = Duration::from_millis(500);

// let (socket, listener) = ReliableUdpSocket::bind_with_config("127.0.0.1:8080", config).await?;
```

这种模式在整个库中被广泛采用，确保了所有新建的连接都遵循一致的、可预测的配置。这对于测试和生产环境的部署都至关重要。 