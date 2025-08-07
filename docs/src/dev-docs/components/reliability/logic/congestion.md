# 拥塞控制模块 (`congestion`) - 智能网络适应引擎

## 概述

拥塞控制模块是网络协议性能优化的核心，实现了先进的Vegas拥塞控制算法和精确的RTT估算机制。它能够智能感知网络状态，主动避免网络拥塞，确保数据传输的高效性和公平性。

**核心功能:**
- **Vegas拥塞控制**: 基于RTT的主动拥塞避免算法
- **RTT精确估算**: RFC 6298标准的RTT测量和RTO计算  
- **多算法支持**: 可扩展的拥塞控制算法框架
- **性能优化**: 针对实时网络环境的算法优化

**文件结构:**
- `vegas_controller.rs` - Vegas拥塞控制算法实现
- `rtt.rs` - RTT估算和RTO计算
- `traits.rs` - 拥塞控制通用接口定义
- `tests/` - 完整的测试套件

## Vegas拥塞控制算法

### 算法原理

Vegas算法是一种基于RTT的拥塞控制算法，通过监测RTT变化来主动检测网络拥塞，避免了传统算法依赖丢包的被动检测方式。

```rust
// Vegas算法核心逻辑
let expected_rate = cwnd / min_rtt;           // 预期发送速率
let actual_rate = cwnd / current_rtt;        // 实际发送速率  
let diff = (expected_rate - actual_rate) * min_rtt;  // 速率差异

if diff < alpha {
    // 网络空闲，可以增加发送速率
    cwnd += 1;
} else if diff > beta {
    // 网络拥塞，需要减少发送速率
    cwnd -= 1;
} else {
    // 网络状态良好，保持当前速率
    // cwnd 保持不变
}
```

### 实现特色

```rust
impl VegasController {
    pub fn on_ack(&mut self, rtt: Duration, _timestamp: Instant) -> CongestionDecision {
        // 动态更新最小RTT
        if rtt < self.min_rtt {
            self.min_rtt = rtt;
        }
        
        // Vegas核心算法
        let expected = self.congestion_window as f64 / self.min_rtt.as_secs_f64();
        let actual = self.congestion_window as f64 / rtt.as_secs_f64();
        let diff = (expected - actual) * self.min_rtt.as_secs_f64();
        
        match self.state {
            CongestionState::SlowStart => {
                if diff > self.config.vegas.gamma {
                    // 智能切换到拥塞避免
                    self.transition_to_congestion_avoidance()
                } else {
                    self.continue_slow_start()
                }
            },
            CongestionState::CongestionAvoidance => {
                self.vegas_congestion_avoidance(diff)
            },
        }
    }
}
```

## RTT估算器

### RFC 6298实现

RTT估算器严格按照RFC 6298标准实现，提供精确的RTT测量和RTO计算：

```rust
impl RttEstimator {
    pub fn update(&mut self, rtt_sample: Duration, min_rto: Duration) {
        let rtt_ms = rtt_sample.as_millis() as f64;
        
        if self.srtt.is_none() {
            // 初始化：RFC 6298 (2.2)
            self.srtt = Some(rtt_ms);
            self.rttvar = Some(rtt_ms / 2.0);
        } else {
            // 后续更新：RFC 6298 (2.3)
            let srtt = self.srtt.unwrap();
            let rttvar = self.rttvar.unwrap();
            
            // RTTVAR = (1 - beta) * RTTVAR + beta * |SRTT - R'|
            let new_rttvar = (1.0 - self.beta) * rttvar + 
                           self.beta * (srtt - rtt_ms).abs();
            
            // SRTT = (1 - alpha) * SRTT + alpha * R'  
            let new_srtt = (1.0 - self.alpha) * srtt + 
                          self.alpha * rtt_ms;
            
            self.srtt = Some(new_srtt);
            self.rttvar = Some(new_rttvar);
        }
        
        self.update_rto(min_rto);
    }
    
    fn update_rto(&mut self, min_rto: Duration) {
        // RTO = SRTT + max(G, K * RTTVAR)，RFC 6298 (2.4)
        let srtt = self.srtt.unwrap_or(1000.0);
        let rttvar = self.rttvar.unwrap_or(500.0);
        
        let rto_ms = srtt + (self.k_factor * rttvar).max(self.clock_granularity_ms);
        self.rto = Duration::from_millis(rto_ms as u64).max(min_rto);
    }
}
```

### 退避策略

```rust
impl RttEstimator {
    pub fn backoff(&mut self) {
        // 指数退避：RTO = RTO * 2，最大60秒
        self.rto = (self.rto * 2).min(Duration::from_secs(60));
        
        debug!(
            new_rto_ms = self.rto.as_millis(),
            "RTO backoff applied due to timeout"
        );
    }
}
```

## 可扩展架构

### 通用接口

拥塞控制模块通过trait提供通用接口，支持多种算法实现：

```rust
pub trait CongestionController: Send + Sync + std::fmt::Debug {
    type Stats: CongestionStats;
    
    /// 处理ACK
    fn on_ack(&mut self, rtt: Duration, timestamp: Instant) -> CongestionDecision;
    
    /// 处理丢包
    fn on_packet_loss(&mut self, timestamp: Instant) -> CongestionDecision;
    
    /// 获取当前状态
    fn get_state(&self) -> CongestionState;
    
    /// 获取统计信息
    fn get_stats(&self) -> Self::Stats;
}
```

### 决策结果

```rust
#[derive(Debug, Clone)]
pub struct CongestionDecision {
    /// 新的拥塞窗口大小
    pub new_congestion_window: u32,
    
    /// 新的拥塞状态
    pub new_state: CongestionState,
    
    /// 是否应该立即发送数据
    pub should_send_immediately: bool,
}
```

## 性能优化

### 数值计算优化

```rust
impl VegasController {
    #[inline]
    fn calculate_vegas_diff(&self, current_rtt: Duration) -> f64 {
        // 使用预计算的倒数避免重复除法
        let min_rtt_inv = self.min_rtt_inverse;
        let current_rtt_inv = 1.0 / current_rtt.as_secs_f64();
        
        // 优化的差值计算
        self.congestion_window as f64 * (min_rtt_inv - current_rtt_inv)
    }
    
    #[inline]  
    fn update_congestion_window(&mut self, delta: i32) -> u32 {
        // 安全的窗口更新，避免下溢
        if delta < 0 {
            self.congestion_window.saturating_sub((-delta) as u32).max(1)
        } else {
            self.congestion_window.saturating_add(delta as u32)
        }
    }
}
```

### 状态缓存

```rust
struct VegasController {
    // 预计算的常用值
    min_rtt_inverse: f64,
    alpha_threshold: f64,
    beta_threshold: f64,
    gamma_threshold: f64,
    
    // 状态缓存
    last_decision_timestamp: Instant,
    consecutive_good_rtts: u32,
}
```

## 算法对比

| 算法 | 检测方式 | 响应速度 | 公平性 | 适用场景 |
|------|----------|----------|--------|----------|
| Vegas | RTT变化 | 快速主动 | 优秀 | 低延迟网络 |
| Reno | 丢包检测 | 被动响应 | 良好 | 通用网络 |
| Cubic | 丢包+时间 | 中等 | 良好 | 高带宽网络 |

## 配置参数

### Vegas参数

```rust
pub struct VegasConfig {
    /// Alpha: 增窗阈值（通常为1-2）
    pub alpha: f64,
    
    /// Beta: 减窗阈值（通常为3-6）  
    pub beta: f64,
    
    /// Gamma: 慢启动退出阈值（通常为1）
    pub gamma: f64,
    
    /// 初始拥塞窗口
    pub initial_cwnd: u32,
    
    /// 最大拥塞窗口
    pub max_cwnd: u32,
}
```

### RTT参数

```rust
pub struct RttConfig {
    /// Alpha: SRTT平滑因子（RFC推荐1/8）
    pub alpha: f64,
    
    /// Beta: RTTVAR平滑因子（RFC推荐1/4）
    pub beta: f64,
    
    /// K: RTO计算因子（RFC推荐4）
    pub k_factor: f64,
    
    /// 时钟粒度（毫秒）
    pub clock_granularity_ms: f64,
}
```

拥塞控制模块通过其先进的算法实现和优化设计，为整个协议栈提供了智能的网络适应能力，确保在各种网络条件下都能获得最优的传输性能。
