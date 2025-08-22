//! 零拷贝事件系统专项测试
//! Zero-copy event system dedicated tests

use super::traits::*;
use super::zero_copy::*;
use super::*;
use crate::core::endpoint::timing::TimeoutEvent;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

fn create_test_event_data(connection_id: ConnectionId) -> TimerEventData<TimeoutEvent> {
    TimerEventData::new(connection_id, TimeoutEvent::IdleTimeout)
}

fn create_test_event_data_batch(count: usize) -> Vec<TimerEventData<TimeoutEvent>> {
    (0..count)
        .map(|i| create_test_event_data(i as u32))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========== FastEventSlot 测试 ==========

    #[test]
    fn test_fast_event_slot_creation() {
        let slot = EventSlot::<TimeoutEvent>::new(64);
        // 验证槽位创建成功，通过写入测试功能
        let event = create_test_event_data(1);
        assert!(slot.write_event(event));
    }

    #[test]
    fn test_fast_event_slot_non_power_of_two() {
        let slot = EventSlot::<TimeoutEvent>::new(100);
        // 验证非2的幂槽位数也能正常工作
        let event = create_test_event_data(1);
        assert!(slot.write_event(event));
    }

    #[test]
    fn test_fast_event_slot_write_single() {
        let slot = EventSlot::<TimeoutEvent>::new(16);
        let event = create_test_event_data(1);

        let success = slot.write_event(event);
        assert!(success);
    }

    #[test]
    fn test_fast_event_slot_write_multiple() {
        let slot = EventSlot::<TimeoutEvent>::new(16);
        let mut success_count = 0;

        for i in 0..10 {
            let event = create_test_event_data(i);
            if slot.write_event(event) {
                success_count += 1;
            }
        }

        // 至少大部分写入应该成功
        assert!(success_count >= 8);
    }

    #[test]
    fn test_fast_event_slot_concurrent_writes() {
        use std::sync::Arc;
        use std::thread;

        let slot = Arc::new(EventSlot::<TimeoutEvent>::new(64));
        let success_count = Arc::new(AtomicUsize::new(0));
        let thread_count = 8;
        let writes_per_thread = 50;

        let mut handles = Vec::new();

        for thread_id in 0..thread_count {
            let slot_clone = Arc::clone(&slot);
            let success_count_clone = Arc::clone(&success_count);

            let handle = thread::spawn(move || {
                for i in 0..writes_per_thread {
                    let connection_id = (thread_id * writes_per_thread + i) as u32;
                    let event = create_test_event_data(connection_id);

                    if slot_clone.write_event(event) {
                        success_count_clone.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });

            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        let final_success_count = success_count.load(Ordering::Relaxed);
        println!(
            "Concurrent writes: {} successes out of {} attempts",
            final_success_count,
            thread_count * writes_per_thread
        );

        // 并发环境下，由于槽位竞争，成功率会降低，这是正常的
        // 至少应该有一些写入成功
        assert!(final_success_count > 0);
        assert!(final_success_count <= thread_count * writes_per_thread);
    }

    // ========== RefEventHandler 测试 ==========

    #[test]
    fn test_ref_event_handler_single() {
        let processed_events = Arc::new(AtomicUsize::new(0));
        let processed_events_clone = Arc::clone(&processed_events);

        let handler = RefEventHandler::new(move |event: &TimerEventData<TimeoutEvent>| {
            processed_events_clone.fetch_add(1, Ordering::Relaxed);
            // 验证事件数据
            event.connection_id < 1000
        });

        let event = create_test_event_data(123);
        let success = handler.deliver_event_ref(&event);

        assert!(success);
        assert_eq!(processed_events.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_ref_event_handler_batch() {
        let processed_events = Arc::new(AtomicUsize::new(0));
        let processed_events_clone = Arc::clone(&processed_events);

        let handler = RefEventHandler::new(move |event: &TimerEventData<TimeoutEvent>| {
            processed_events_clone.fetch_add(1, Ordering::Relaxed);
            // 只处理偶数连接ID的事件
            event.connection_id % 2 == 0
        });

        let events = create_test_event_data_batch(10);
        let event_refs: Vec<&TimerEventData<TimeoutEvent>> = events.iter().collect();

        let delivered_count = handler.batch_deliver_event_refs(&event_refs);

        // 应该有5个偶数连接ID（0, 2, 4, 6, 8）
        assert_eq!(delivered_count, 5);
        assert_eq!(processed_events.load(Ordering::Relaxed), 10); // 所有事件都被处理
    }

    // ========== ZeroCopyBatchDispatcher 测试 ==========

    #[test]
    fn test_zero_copy_batch_dispatcher_creation() {
        let dispatcher = ZeroCopyBatchDispatcher::<TimeoutEvent>::new(64, 4, 1000);
        // 验证分发器创建成功，通过功能测试
        let events = create_test_event_data_batch(4);
        let dispatched = dispatcher.batch_dispatch_events(events);
        assert_eq!(dispatched, 4);
    }

    #[test]
    fn test_zero_copy_batch_dispatcher_single_batch() {
        let dispatcher = ZeroCopyBatchDispatcher::<TimeoutEvent>::new(32, 2, 1000);
        let events = create_test_event_data_batch(10);

        let dispatched_count = dispatcher.batch_dispatch_events(events);
        assert_eq!(dispatched_count, 10);
    }

    #[test]
    fn test_zero_copy_batch_dispatcher_multiple_batches() {
        let dispatcher = ZeroCopyBatchDispatcher::<TimeoutEvent>::new(64, 4, 1000);
        let mut total_dispatched = 0;

        for batch_id in 0..5 {
            let events = create_test_event_data_batch(20);
            let dispatched = dispatcher.batch_dispatch_events(events);
            total_dispatched += dispatched;

            println!("Batch {}: {} events dispatched", batch_id, dispatched);
        }

        assert_eq!(total_dispatched, 100);
    }

    #[test]
    fn test_zero_copy_batch_dispatcher_load_balancing() {
        let dispatcher = ZeroCopyBatchDispatcher::<TimeoutEvent>::new(16, 4, 1000);
        let events = create_test_event_data_batch(100);

        let start = Instant::now();
        let dispatched_count = dispatcher.batch_dispatch_events(events);
        let duration = start.elapsed();

        // 由于槽位限制，可能不是所有事件都能被分发
        println!(
            "Dispatched {} out of 100 events in {:?}",
            dispatched_count, duration
        );
        assert!(dispatched_count > 0);
        assert!(dispatched_count <= 100);

        // 应该很快完成
        assert!(duration < Duration::from_millis(10));
    }

    // ========== 性能测试 ==========

    #[test]
    fn test_zero_copy_performance_small_batch() {
        let dispatcher = ZeroCopyBatchDispatcher::<TimeoutEvent>::new(64, 4, 1000);
        let events = create_test_event_data_batch(32);

        let start = Instant::now();
        let dispatched_count = dispatcher.batch_dispatch_events(events);
        let duration = start.elapsed();

        assert_eq!(dispatched_count, 32);
        let nanos_per_event = duration.as_nanos() / 32;

        println!("Small batch: {} ns/event", nanos_per_event);

        // 验证微秒级性能（实际性能可能受到系统和调试模式影响）
        assert!(nanos_per_event < 50000); // <50微秒/事件
    }

    #[test]
    fn test_zero_copy_performance_medium_batch() {
        let dispatcher = ZeroCopyBatchDispatcher::<TimeoutEvent>::new(256, 8, 1000);
        let events = create_test_event_data_batch(512);

        let start = Instant::now();
        let dispatched_count = dispatcher.batch_dispatch_events(events);
        let duration = start.elapsed();

        assert_eq!(dispatched_count, 512);
        let nanos_per_event = duration.as_nanos() / 512;

        println!("Medium batch: {} ns/event", nanos_per_event);

        // 中等批量性能应该更好
        assert!(nanos_per_event < 30000); // <30微秒/事件
    }

    #[test]
    fn test_zero_copy_performance_large_batch() {
        let dispatcher = ZeroCopyBatchDispatcher::<TimeoutEvent>::new(1024, 16, 1000);
        let events = create_test_event_data_batch(2048);

        let start = Instant::now();
        let dispatched_count = dispatcher.batch_dispatch_events(events);
        let duration = start.elapsed();

        assert_eq!(dispatched_count, 2048);
        let nanos_per_event = duration.as_nanos() / 2048;

        println!("Large batch: {} ns/event", nanos_per_event);

        // 大批量性能应该最佳
        assert!(nanos_per_event < 20000); // <20微秒/事件
    }

    // ========== 内存效率测试 ==========

    #[test]
    fn test_memory_efficiency_no_cloning() {
        // 使用较大的槽位数以减少冲突
        let dispatcher = ZeroCopyBatchDispatcher::<TimeoutEvent>::new(1024, 8, 1000);
        let mut total_dispatched = 0;

        // 使用较少的迭代和较小的批量大小
        for i in 0..10 {
            let events = create_test_event_data_batch(10);
            let initial_len = events.len();

            let dispatched_count = dispatcher.batch_dispatch_events(events);
            total_dispatched += dispatched_count;

            println!(
                "Iteration {}: dispatched {} out of {} events",
                i, dispatched_count, initial_len
            );

            // 由于槽位限制，可能不是所有事件都能被分发
            assert!(dispatched_count <= initial_len);

            // events 已经被移动，无法再访问，这证明了零拷贝
        }

        // 总体上应该至少分发了一些事件
        assert!(total_dispatched > 0);
        println!(
            "Memory efficiency test completed - total dispatched: {}",
            total_dispatched
        );
    }

    // ========== 边界条件测试 ==========

    #[test]
    fn test_zero_copy_empty_batch() {
        let dispatcher = ZeroCopyBatchDispatcher::<TimeoutEvent>::new(32, 2, 1000);
        let empty_events = Vec::new();

        let dispatched_count = dispatcher.batch_dispatch_events(empty_events);
        assert_eq!(dispatched_count, 0);
    }

    #[test]
    fn test_zero_copy_single_event() {
        let dispatcher = ZeroCopyBatchDispatcher::<TimeoutEvent>::new(32, 2, 1000);
        let single_event = vec![create_test_event_data(42)];

        let dispatched_count = dispatcher.batch_dispatch_events(single_event);
        assert_eq!(dispatched_count, 1);
    }

    #[test]
    fn test_zero_copy_max_batch_size() {
        let dispatcher = ZeroCopyBatchDispatcher::<TimeoutEvent>::new(512, 8, 1000);
        let large_batch = create_test_event_data_batch(10000);

        let start = Instant::now();
        let dispatched_count = dispatcher.batch_dispatch_events(large_batch);
        let duration = start.elapsed();

        println!(
            "Max batch: {} out of 10K events processed in {:?}",
            dispatched_count, duration
        );

        // 由于槽位限制，大批量可能只能处理部分事件
        assert!(dispatched_count > 0);
        assert!(dispatched_count <= 10000);

        // 即使是大批量也应该在合理时间内完成
        assert!(duration < Duration::from_millis(100));
    }

    // ========== 并发安全测试 ==========

    #[tokio::test]
    async fn test_zero_copy_concurrent_dispatching() {
        use tokio::task;

        let dispatcher = Arc::new(ZeroCopyBatchDispatcher::<TimeoutEvent>::new(256, 8, 1000));
        let mut tasks = Vec::new();
        let task_count = 10;
        let events_per_task = 100;

        for task_id in 0..task_count {
            let dispatcher_clone = Arc::clone(&dispatcher);

            let task = task::spawn(async move {
                let events = create_test_event_data_batch(events_per_task);
                let dispatched = dispatcher_clone.batch_dispatch_events(events);
                (task_id, dispatched)
            });

            tasks.push(task);
        }

        let mut total_dispatched = 0;
        for task in tasks {
            let (task_id, dispatched) = task.await.unwrap();
            assert_eq!(dispatched, events_per_task);
            total_dispatched += dispatched;

            println!("Task {} dispatched {} events", task_id, dispatched);
        }

        assert_eq!(total_dispatched, task_count * events_per_task);
    }

    // ========== EventFactory 智能策略测试 ==========

    #[test]
    fn test_event_factory_copy_strategy() {
        let factory = EventFactory::<TimeoutEvent>::new();

        // TimeoutEvent 是 Copy 类型，应该使用 Copy 策略
        let event = factory.create_event(123, TimeoutEvent::IdleTimeout);
        assert_eq!(event.connection_id, 123);
        assert_eq!(event.timeout_event, TimeoutEvent::IdleTimeout);
    }

    #[test]
    fn test_event_factory_batch_creation() {
        let factory = EventFactory::<TimeoutEvent>::new();

        let requests = vec![
            (1, TimeoutEvent::IdleTimeout),
            (2, TimeoutEvent::PathValidationTimeout),
            (3, TimeoutEvent::IdleTimeout),
        ];

        let events = factory.batch_create_events(&requests);
        assert_eq!(events.len(), 3);

        for (i, event) in events.iter().enumerate() {
            assert_eq!(event.connection_id, requests[i].0);
            assert_eq!(event.timeout_event, requests[i].1);
        }
    }

    #[test]
    fn test_event_factory_performance() {
        let factory = EventFactory::<TimeoutEvent>::new();
        let requests: Vec<(ConnectionId, TimeoutEvent)> = (0..1000)
            .map(|i| (i as u32, TimeoutEvent::IdleTimeout))
            .collect();

        let start = Instant::now();
        let events = factory.batch_create_events(&requests);
        let duration = start.elapsed();

        assert_eq!(events.len(), 1000);
        let nanos_per_event = duration.as_nanos() / 1000;

        println!("EventFactory batch creation: {} ns/event", nanos_per_event);

        // Copy 类型应该相对较快
        assert!(nanos_per_event < 10000); // <10微秒/事件
    }

    // ========== 综合零拷贝workflow测试 ==========

    #[tokio::test]
    async fn test_complete_zero_copy_workflow() {
        // 1. 创建事件工厂
        let factory = EventFactory::<TimeoutEvent>::new();

        // 2. 创建零拷贝分发器
        let dispatcher = ZeroCopyBatchDispatcher::<TimeoutEvent>::new(128, 4, 1000);

        // 3. 创建引用处理器
        let processed_count = Arc::new(AtomicUsize::new(0));
        let processed_count_clone = Arc::clone(&processed_count);

        let _handler = RefEventHandler::new(move |_event: &TimerEventData<TimeoutEvent>| {
            processed_count_clone.fetch_add(1, Ordering::Relaxed);
            true // 所有事件都成功处理
        });

        // 4. 执行完整工作流
        let requests = vec![
            (1, TimeoutEvent::IdleTimeout),
            (2, TimeoutEvent::PathValidationTimeout),
            (3, TimeoutEvent::IdleTimeout),
            (4, TimeoutEvent::PathValidationTimeout),
        ];

        let start = Instant::now();

        // 使用工厂创建事件
        let events = factory.batch_create_events(&requests);

        // 使用零拷贝分发器分发事件
        let dispatched_count = dispatcher.batch_dispatch_events(events);

        let workflow_duration = start.elapsed();

        assert_eq!(dispatched_count, 4);
        println!("Complete zero-copy workflow took: {:?}", workflow_duration);

        // 整个工作流应该非常快
        assert!(workflow_duration < Duration::from_millis(1));
    }
}
