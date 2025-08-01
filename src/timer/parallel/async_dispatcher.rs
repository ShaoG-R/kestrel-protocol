//! 异步事件分发器
//! Async Event Dispatcher

use crate::timer::event::traits::EventDataTrait;
use crate::timer::event::TimerEventData;
use crate::timer::parallel::types::ProcessedTimerData;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc;

/// 异步事件分发器
/// Async Event Dispatcher
pub struct AsyncEventDispatcher<E: EventDataTrait> {
    /// 事件发送通道池
    event_channels: Vec<mpsc::Sender<TimerEventData<E>>>,
    /// 负载均衡索引 (使用原子类型)
    /// Load balancing index (using an atomic type)
    round_robin_index: AtomicUsize,
}

impl<E: EventDataTrait> Default for AsyncEventDispatcher<E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: EventDataTrait> AsyncEventDispatcher<E> {
    pub fn new() -> Self {
        // 创建多个事件通道用于负载均衡
        let channel_count = num_cpus::get().max(4);
        let mut event_channels = Vec::with_capacity(channel_count);
        
        for _ in 0..channel_count {
            let (tx, _rx) = mpsc::channel(1024);
            event_channels.push(tx);
        }

        Self {
            event_channels,
            round_robin_index: AtomicUsize::new(0),
        }
    }

    /// 异步分发定时器事件
    /// Asynchronously dispatch timer events
    pub async fn dispatch_timer_events(
        &self,
        processed_data: Vec<ProcessedTimerData<E>>,
    ) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        let mut dispatch_count = 0;

        // 并发分发事件到多个通道
        let dispatch_futures: Vec<_> = processed_data
            .into_iter()
            .map(|data| {
                // 原子地增加索引并返回增加前的值，然后取模
                let index = self.round_robin_index.fetch_add(1, Ordering::Relaxed);
                let channel_index = index % self.event_channels.len();

                let tx = self.event_channels[channel_index].clone();
                
                tokio::spawn(async move {
                    // 使用智能工厂创建事件 (Copy类型零开销，非Copy类型智能管理)
                    // Use smart factory to create event (zero-cost for Copy types, smart management for non-Copy types)
                    let factory = crate::timer::event::traits::EventFactory::new();
                    let event_data = factory.create_event(
                        data.connection_id,
                        data.timeout_event,
                    );
                    
                    // 尝试发送事件（非阻塞）
                    match tx.try_send(event_data) {
                        Ok(_) => Ok::<usize, Box<dyn std::error::Error + Send + Sync>>(1),
                        Err(_) => Ok::<usize, Box<dyn std::error::Error + Send + Sync>>(0), // 通道满了，跳过这个事件
                    }
                })
            })
            .collect();

        // 等待所有分发完成
        let results = futures::future::join_all(dispatch_futures).await;
        for result in results.into_iter().flatten() {
            dispatch_count += result.unwrap_or(0);
        }

        Ok(dispatch_count)
    }
}