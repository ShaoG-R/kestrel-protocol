# Endpoint时间管理 (`timing`) - 统一的节拍器

## 概述

`timing`模块是`Endpoint`的内部节拍器和时钟。它将所有与时间相关的状态和计算逻辑（如超时、心跳、RTT等）封装到一个统一的管理器中，并与全局定时器系统深度集成。这个模块通过提供一个清晰、集中的时间管理接口，极大地简化了`Endpoint`主事件循环的复杂性，并确保了协议所有定时行为的精确和高效。

**核心使命:**
- **时间状态封装**: 集中管理`start_time`、`last_recv_time`等所有时间戳。
- **全局定时器集成**: 与全局定时器系统无缝集成，提供高效的超时管理。
- **超时逻辑计算**: 提供统一的方法来检查各种超时事件，如连接空闲、路径验证超时等。
- **统一调度唤醒**: 计算出`Endpoint`下一次需要处理定时事件的最早时间点，供事件循环使用。
- **简化主循环**: 让主事件循环从复杂的、多源的时间计算中解脱出来，只需关注一个统一的"下一次唤醒"时间。

**架构实现:**
- **时间管理器**: `src/core/endpoint/timing.rs` - 包含`TimingManager`结构体，是本模块的核心。
- **定时器管理器**: `src/core/endpoint/timing.rs` - `TimerManager`，封装全局定时器的使用。
- **超时事件**: `src/core/endpoint/timing.rs` - `TimeoutEvent`枚举，定义了所有可能的超时事件类型。
- **全局定时器**: `src/timer/` - 高效的全局定时器系统，详见[定时器系统文档](../timer.md)。

## 设计原则

### 1. 状态集中化与全局定时器集成
- **单一时间源**: 所有与连接时间相关的状态都集中在`TimingManager`中，避免了时间状态分散在代码库各处导致的不一致和维护困难。
- **全局定时器集成**: 通过`TimerManager`与全局定时器系统集成，享受高效的O(1)定时器操作。
- **易于快照与调试**: 由于状态集中，可以轻易地获取连接的时间快照（如`stats_string`方法），方便调试和监控。

### 2. 计算与逻辑分离
- **计算的归一化**: `TimingManager`负责所有时间差的计算（如`time_since_last_recv`），而将配置（如`idle_timeout`的具体值）作为参数传入。这使得核心逻辑与具体配置解耦。
- **意图明确的API**: 接口名称直接反映其业务意图，如`is_idle_timeout`，调用者无需关心其内部是"当前时间减去最后接收时间"的实现细节。

### 3. 唤醒时间统一调度
- **"Pull"模式**: `Endpoint`的主循环通过调用`calculate_next_wakeup`方法，主动从`TimingManager`和`ReliabilityLayer`"拉取"下一个需要唤醒的时间点。
- **高效`select!`**: 这种模式使得主循环中的`tokio::select!`只需要一个`sleep_until`分支就能管理所有类型的定时器（RTO、心跳、空闲等），避免了维护多个`Interval`或`Sleep`实例的复杂性和开销。

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
    /// 全局定时器任务句柄
    timer_handle: GlobalTimerTaskHandle,
    /// 接收超时事件的通道
    timeout_rx: mpsc::Receiver<TimerEventData>,
    /// 活跃定时器句柄映射
    active_timers: HashMap<TimeoutEvent, TimerHandle>,
}
```

**核心功能:**
- **定时器注册**: 向全局定时器任务注册各种类型的定时器
- **事件接收**: 异步接收并处理到期的定时器事件
- **生命周期管理**: 管理定时器的创建、取消和清理

### 统一超时检查

`TimingManager`提供了两套超时检查机制：

#### 1. 传统的时间戳比较检查
```rust
impl TimingManager {
    /// 检查是否发生了空闲超时
    pub fn check_idle_timeout(&self, config: &Config, now: Instant) -> bool {
        now.saturating_duration_since(self.last_recv_time) > config.connection.idle_timeout
    }

    /// 检查所有连接级的超时情况
    pub fn check_connection_timeouts(&self, config: &Config, now: Instant) -> Vec<TimeoutEvent> {
        let mut events = Vec::new();
        if self.check_idle_timeout(config, now) {
            events.push(TimeoutEvent::IdleTimeout);
        }
        // ... check other timeouts ...
        events
    }
}
```

#### 2. 全局定时器事件检查
```rust
impl TimingManager {
    /// 检查是否有到期的定时器事件
    pub async fn check_timer_events(&mut self) -> Vec<TimeoutEvent> {
        self.timer_manager.check_timer_events().await
    }

    /// 注册空闲超时定时器
    pub async fn register_idle_timeout(&mut self, config: &Config) -> Result<(), String> {
        self.timer_manager.register_idle_timeout(config).await
    }

    /// 重置空闲超时定时器（在收到数据包时调用）
    pub async fn reset_idle_timeout(&mut self, config: &Config) -> Result<(), String> {
        self.timer_manager.reset_idle_timeout(config).await
    }
}
```

### 分层超时管理架构

`Endpoint`的主循环采用分层超时管理架构，通过`check_all_timeouts`统一协调：

```rust
impl Endpoint {
    pub async fn check_all_timeouts(&mut self, now: Instant) -> Result<()> {
        // 1. 检查全局定时器事件
        let connection_timeout_events = self.timing.check_timer_events().await;
        
        // 2. 检查可靠性超时，使用帧重构
        let context = self.create_retransmission_context();
        let reliability_timeout_result = self.transport.reliability_mut()
            .check_reliability_timeouts(now, &context);
        
        // 3. 处理超时事件
        self.handle_timeout_events(connection_timeout_events, reliability_timeout_result, now).await
    }
}
```

### `calculate_next_wakeup` - 统一唤醒调度器

这是`timing`模块与`Endpoint`主循环交互的最重要接口之一：

```mermaid
graph TD
    subgraph "全局定时器系统"
        A[定时器事件检查] --> C
    end
    
    subgraph "可靠性层"
        B[RTO/重传截止时间] --> C
    end
    
    subgraph "端点"
         C{"calculate_next_wakeup_time()"} -- "频繁检查间隔" --> D[下一次唤醒时间]
    end
    
    subgraph "事件循环"
        E(tokio::select!) -- "sleep_until(下一次唤醒时间)" --> F[超时分支]
    end

    D --> E

    style A fill:#333,color:#fff
    style B fill:#333,color:#fff
    style C fill:#333,color:#fff
    style D fill:#333,color:#fff
    style E fill:#333,color:#fff
    style F fill:#333,color:#fff
```

**工作流程**:
1. `Endpoint`的`calculate_next_wakeup_time`方法被调用。
2. 它会从`ReliabilityLayer`获取下一个重传超时（RTO）的唤醒时间。
3. 由于使用全局定时器，它使用更频繁的检查间隔（50ms）来确保及时处理定时器事件。
4. 返回RTO截止时间和定时器检查间隔中的较早者。
5. `select!`中的`sleep_until`分支会精确地在那个时间点被触发。

## 全局定时器集成优势

### 1. 性能优势
- **O(1)操作**: 定时器的添加、取消和检查都是O(1)时间复杂度
- **内存高效**: 全局共享的时间轮，避免每个连接维护独立定时器的开销
- **批量处理**: 支持在单次时间推进中处理多个到期定时器

### 2. 功能优势
- **精确控制**: 毫秒级精度的定时器，满足协议对精确超时控制的需求
- **类型安全**: 通过`TimeoutEvent`枚举确保定时器类型的安全性
- **连接隔离**: 虽然使用全局任务，但每个连接的定时器在逻辑上完全隔离

### 3. 使用示例

```rust
// 在连接建立时注册初始定时器
let mut timing = TimingManager::new(connection_id, timer_handle);
timing.register_idle_timeout(&config).await?;

// 在收到数据包时重置空闲超时
timing.on_packet_received(Instant::now());
timing.reset_idle_timeout(&config).await?;

// 在事件循环中检查定时器事件
let events = timing.check_timer_events().await;
for event in events {
    match event {
        TimeoutEvent::IdleTimeout => {
            // 处理空闲超时
            self.lifecycle_manager.force_close()?;
            return Err(Error::ConnectionTimeout);
        }
        TimeoutEvent::PathValidationTimeout => {
            // 处理路径验证超时
            self.handle_path_validation_timeout().await?;
        }
        _ => {}
    }
}
```

## 总结

`timing`模块通过与全局定时器系统的深度集成，不仅保持了原有的时间状态管理和超时逻辑计算功能，还获得了高性能的定时器操作能力。它成功地扮演了`Endpoint`"节拍器"的角色，通过统一的时间管理接口和高效的全局定时器系统，为协议的各种超时机制提供了精确的计算基础和可靠的执行保障，是实现高性能、低开销异步网络系统的关键一环。