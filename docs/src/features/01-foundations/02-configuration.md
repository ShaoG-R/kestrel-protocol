# 2: 统一的可配置化

**功能描述:**

协议的所有关键行为和参数都通过一个统一的 `Config` 结构体进行管理。这种设计将可调参数与核心逻辑分离，为用户提供了极大的灵活性，允许他们根据不同的网络环境和应用场景微调协议性能。

**实现位置:**

- **文件:** `src/config.rs`

### 1. 中心化的 `Config` 结构体

`Config` 结构体是所有可配置参数的唯一来源。它包含了控制协议各个方面的字段，从底层的RTO（重传超时）管理到高层的缓冲区大小。

```rust
// 位于 src/config.rs
#[derive(Debug, Clone)]
pub struct Config {
    // --- 可靠性相关 ---
    pub initial_rto: Duration,
    pub min_rto: Duration,
    pub fast_retx_threshold: u16,
    
    // --- 拥塞控制相关 (Vegas) ---
    pub initial_cwnd_packets: u32,
    pub min_cwnd_packets: u32,
    pub vegas_alpha_packets: u32,
    pub vegas_beta_packets: u32,
    
    // --- 协议行为 ---
    pub protocol_version: u8,
    pub max_payload_size: usize,
    pub idle_timeout: Duration,
    
    // --- 缓冲区管理 ---
    pub send_buffer_capacity_bytes: usize,
    pub recv_buffer_capacity_packets: usize,

    // ... 其他参数
}
```

### 2. 合理的默认值

`Config` 为自身实现了 `Default` trait，为所有参数提供了一套经过验证的、合理的默认值。这意味着用户可以在不了解所有参数细节的情况下，轻松地开始使用本协议。

```rust
// 位于 src/config.rs
impl Default for Config {
    fn default() -> Self {
        Self {
            protocol_version: 1,
            initial_rto: Duration::from_millis(1000),
            fast_retx_threshold: 3,
            max_payload_size: 1200,
            initial_cwnd_packets: 32,
            vegas_alpha_packets: 2,
            vegas_beta_packets: 4,
            idle_timeout: Duration::from_secs(5),
            // ... 其他默认值
        }
    }
}
```

### 3. 应用配置

在创建 `ReliableUdpSocket` 或 `Endpoint` 时，用户可以选择性地传入一个 `Config` 实例。如果不提供，则会自动使用 `Config::default()`。

这种模式在整个库中被广泛采用，确保了所有新建的连接都遵循一致的、可预测的配置。这对于测试和生产环境的部署都至关重要。 