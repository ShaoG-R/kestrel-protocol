# Endpoint时间管理 (`timing`) - 统一的节拍器

## 概述

`timing`模块是`Endpoint`的内部节拍器和时钟。它将所有与时间相关的状态和计算逻辑（如超时、心跳、RTT等）封装到一个统一的管理器中，并与全局定时器系统深度集成。这个模块通过提供一个清晰、集中的时间管理接口，极大地简化了`Endpoint`主事件循环的复杂性，并确保了协议所有定时行为的精确和高效。

**核心使命:**
- **时间状态封装**: 集中管理`start_time`、`last_recv_time`等所有时间戳。
- **全局定时器集成**: 与全局定时器系统无缝集成，提供高效的超时管理。
- **事件驱动处理**: 通过注册定时器并接收事件，驱动所有超时处理逻辑。
- **简化主循环**: 主事件循环通过通道接收超时事件，无需手动比较时间戳。

**架构实现:**
- **时间管理器**: `src/core/endpoint/timing.rs` - 包含`TimingManager`结构体，是本模块的核心。
- **定时器管理器**: `src/core/endpoint/timing.rs` - `TimerManager`，封装全局定时器的使用。
- **超时事件**: `src/core/endpoint/timing.rs` - `TimeoutEvent`枚举，定义了所有可能的超时事件类型。
- **事件驱动**: 通过定时器事件通道传递到期事件，统一由上层处理。
- **全局定时器**: `src/timer/` - 高效的全局定时器系统，详见[定时器系统文档](../timer.md)。

## 设计原则

### 1. 状态集中化与全局定时器集成
- **单一时间源**: 所有与连接时间相关的状态都集中在`TimingManager`中，避免了时间状态分散在代码库各处导致的不一致和维护困难。
- **全局定时器集成**: 通过`TimerManager`与全局定时器系统集成，享受高效的O(1)定时器操作。
- **易于快照与调试**: 由于状态集中，可以轻易地获取连接的时间快照（如`stats_string`方法），方便调试和监控。

### 2. 计算与逻辑分离
- **计算的归一化**: `TimingManager`负责所有时间差的计算（如`time_since_last_recv`），而将配置（如`idle_timeout`的具体值）作为参数传入。这使得核心逻辑与具体配置解耦。
- **意图明确的API**: 接口名称直接反映其业务意图，如接收时间更新、FIN处理调度等，无需暴露手动比较时间的细节。

### 3. 事件驱动的超时处理
- **"Push"模式**: 通过`TimerManager`注册定时器，到期后通过通道推送事件到上层。
- **高效`select!`**: 主循环在`tokio::select!`里监听网络事件与定时器事件通道，无需手动比较时间。

## 核心组件与逻辑

### `TimingManager` - 时间状态中心

`TimingManager`是本模块的核心结构体，它像一个专职会计，记录着连接的所有关键时间点，并集成了全局定时器管理功能。

```rust
// In src/core/endpoint/timing.rs
pub struct TimingManager {
    /// 连接开始时间
    start_time: Instant,
    /// 最后接收数据的时间
    last_recv_time: Instant,
    /// FIN挂起EOF标志
    fin_pending_eof: bool,
    /// 定时器管理器
    timer_manager: TimerManager,
}
```

### `TimerManager` - 全局定时器集成

`TimerManager`封装了与全局定时器系统的所有交互，为每个连接提供独立的定时器管理：

```rust
pub struct TimerManager {
    /// 连接ID，用于全局定时器注册
    connection_id: ConnectionId,
    /// 定时器actor句柄
    timer_actor: TimerActorHandle<SenderCallback<TimeoutEvent>>,
    /// 发送超时事件的通道
    timeout_tx: mpsc::Sender<TimerEventData<TimeoutEvent>>,
    /// 活跃定时器类型集合
    active_timer_types: HashMap<TimeoutEvent, bool>,
}
```

**核心功能:**
- **定时器注册**: 向全局定时器任务注册各种类型的定时器
- **事件接收**: 通过通道接收到期的定时器事件
- **生命周期管理**: 管理定时器的创建、取消和清理

### 事件驱动超时

```rust
impl TimingManager {
    /// 注册空闲超时定时器
    pub async fn register_idle_timeout(&mut self, config: &Config) -> Result<(), &'static str> {
        self.timer_manager.register_idle_timeout(config).await
    }

    /// 在收到数据包时更新最后接收时间并可选择重置超时
    pub fn on_packet_received(&mut self, now: Instant) {
        self.last_recv_time = now;
    }
}
```

### 使用示例

```rust
// 在连接建立时注册初始定时器
let (mut timing, mut timer_rx) = TimingManager::new(connection_id, timer_handle);
let mut config = Config::default();
config.connection.idle_timeout = Duration::from_millis(50);

timing.register_idle_timeout(&config).await?;

// 在事件循环中接收超时事件
if let Some(evt) = timer_rx.recv().await {
    match evt.timeout_event {
        TimeoutEvent::IdleTimeout => {
            // 处理空闲超时
        }
        TimeoutEvent::PathValidationTimeout => {
            // 处理路径验证超时
        }
        _ => {}
    }
}
```

## 全局定时器与事件驱动优势

### 1. 性能优势
- **O(1)操作**: 定时器的添加、取消和检查都是O(1)时间复杂度
- **内存高效**: 全局共享的时间轮，避免每个连接维护独立定时器的开销
- **批量处理**: 支持在单次时间推进中处理多个到期定时器
- **事件驱动**: 通过事件通道统一驱动定时处理，简化模型
- **⚡ 系统调用优化**: 时间缓存减少系统调用

### 2. 功能优势
- **精确控制**: 毫秒级精度的定时器，满足协议对精确超时控制的需求
- **类型安全**: 通过`TimeoutEvent`枚举确保定时器类型的安全性
- **连接隔离**: 虽然使用全局任务，但每个连接的定时器在逻辑上完全隔离

### 3. 架构优势
- **跨层协作**: 各层通过事件协作，无需统一调度器绑定
- **可扩展性**: 新的超时点通过注册新的事件即可集成
- **监控友好**: 通过模块化统计与日志进行监控

## 总结

`timing`模块通过与全局定时器系统的深度集成，并采用**事件驱动**模型，保持了时间状态管理与高性能定时器操作能力。作为`Endpoint`的“节拍器”，它让主事件循环更加简洁与高效，适用于严苛生产环境。