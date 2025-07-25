# 2.8: 基于延迟的拥塞控制 (Vegas)

**功能描述:**

为实现在不稳定网络环境下的高性能，协议采用了类似于 **TCP Vegas** 的、基于延迟的拥塞控制算法。与传统的基于丢包的算法（如Reno）不同，Vegas是一种**主动预测**并避免拥塞的算法，它通过监控RTT的变化来调节发送速率，而不是被动地等到丢包发生后再做反应。

**实现位置:**

- **Trait定义**: `src/congestion.rs` (`CongestionControl`)
- **Vegas实现**: `src/congestion/vegas.rs` (`Vegas`)
- **配置参数**: `src/config.rs` (`vegas_alpha_packets`, `vegas_beta_packets`)

### 1. Vegas的核心思想

Vegas的核心思想是，通过比较**期望吞吐率**和**实际吞吐率**，来估算当前网络路径中的**排队数据包数量**，并以此作为网络拥塞的信号。

- **`BaseRTT`**: 算法会持续记录连接建立以来观测到的最小RTT (`min_rtt`)，将其作为网络的基准线路延迟。
- **期望吞吐率**: `Expected = CongestionWindow / BaseRTT`
- **实际吞吐率**: `Actual = CongestionWindow / CurrentRTT`
- **排队量估算**: `Diff = (Expected - Actual) * BaseRTT`。这个 `Diff` 值就约等于当前在网络路由器队列中排队的数据包数量。

### 2. 窗口调整策略

Vegas的窗口调整非常精细，它定义了两个阈值 `alpha` 和 `beta`（在`Config`中配置，单位是包的数量）：

1.  **增加窗口**: 如果 `Diff < alpha`，说明网络中几乎没有排队，非常通畅。此时算法会线性增加拥塞窗口（`cwnd += 1`），主动探测更多可用带宽。
2.  **减小窗口**: 如果 `Diff > beta`，说明网络中积压的数据包已经过多，有拥塞风险。算法会线性减小拥塞窗口（`cwnd -= 1`），主动缓解压力。
3.  **保持不变**: 如果 `alpha <= Diff <= beta`，说明当前的发送速率与网络处理能力基本匹配，窗口大小保持不变。

```rust
// 位于 src/congestion/vegas.rs
fn on_ack(&mut self, rtt: Duration) {
    // ...
    if self.state == State::CongestionAvoidance {
        let expected_throughput = self.congestion_window as f32 / self.min_rtt.as_secs_f32();
        let actual_throughput = self.congestion_window as f32 / rtt.as_secs_f32();
        let diff_packets = (expected_throughput - actual_throughput) * self.min_rtt.as_secs_f32();

        if diff_packets < self.config.vegas_alpha_packets as f32 {
            self.congestion_window += 1;
        } else if diff_packets > self.config.vegas_beta_packets as f32 {
            self.congestion_window = (self.congestion_window - 1).max(self.config.min_cwnd_packets);
        }
    }
    // ...
}
```

### 3. 区分拥塞性丢包与随机丢包

Vegas在处理丢包时也更智能。它会检查丢包发生前的RTT：
- **拥塞性丢包**: 如果丢包前的RTT远大于`BaseRTT`，则认为是网络拥塞导致的。此时会采取激进行为，将拥塞窗口减半。
- **随机性丢包**: 如果丢包前的RTT与`BaseRTT`相差不大，则认为是无线链路干扰等原因造成的随机丢包。此时仅对拥塞窗口做**温和的乘法降低** (`vegas_gentle_decrease_factor`)，避免因非拥塞事件而错杀吞吐率。

这种设计使得协议在Wi-Fi、蜂窝网络等高随机丢包率的环境下，依然能维持相对较高的性能。 