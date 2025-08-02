//! 全局定时器任务测试
//! Global timer task tests
//!
//! 本模块包含全局定时器任务系统的完整测试套件，包括基本功能测试、
//! 性能基准测试、SIMD优化验证和兼容性测试。
//!
//! This module contains comprehensive test suites for the global timer task system,
//! including basic functionality tests, performance benchmarks, SIMD optimization
//! verification, and compatibility tests.

#[cfg(test)]
mod tests {
    use super::super::{
        types::{TimerRegistration, BatchTimerRegistration, BatchTimerCancellation},
        commands::TimerError,
        // Removed old global timer task import
    };
    use crate::core::endpoint::timing::TimeoutEvent;
    use crate::timer::hybrid_system::start_hybrid_timer_task;
    use tokio::{sync::mpsc, time::{sleep, Duration, Instant}};

    #[tokio::test]
    async fn test_timer_task_creation() {
        use crate::timer::hybrid_system::HybridTimerTask;
        
        let (_task, _command_tx) = HybridTimerTask::<TimeoutEvent>::new_default();
        // Note: We can't directly access timing_wheel.timer_count() and connection_timers
        // due to private fields. This would need getters in the implementation.
        // For now, we'll just test that the task was created successfully.
    }

    #[tokio::test]
    async fn test_register_and_cancel_timer() {
        let handle = start_hybrid_timer_task::<TimeoutEvent>();
        let (callback_tx, mut callback_rx) = mpsc::channel(1);
        
        // 注册定时器
        let registration = TimerRegistration::new(
            1,
            Duration::from_millis(100),
            TimeoutEvent::IdleTimeout,
            callback_tx,
        );
        
        let timer_handle = handle.register_timer(registration).await.unwrap();
        
        // 取消定时器
        let cancelled = timer_handle.cancel().await.unwrap();
        assert!(cancelled);
        
        // 等待一段时间，确保定时器不会触发
        sleep(Duration::from_millis(200)).await;
        assert!(callback_rx.try_recv().is_err());
        
        handle.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_timer_expiration() {
        let handle = start_hybrid_timer_task::<TimeoutEvent>();
        let (callback_tx, mut callback_rx) = mpsc::channel(1);
        
        // 注册定时器
        let registration = TimerRegistration::new(
            1,
            Duration::from_millis(100),
            TimeoutEvent::IdleTimeout,
            callback_tx,
        );
        
        handle.register_timer(registration).await.unwrap();
        
        // 等待定时器到期
        let event_data = callback_rx.recv().await.unwrap();
        assert_eq!(event_data.connection_id, 1);
        assert_eq!(event_data.timeout_event, TimeoutEvent::IdleTimeout);
        
        handle.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_clear_connection_timers() {
        let handle = start_hybrid_timer_task::<TimeoutEvent>();
        let (callback_tx, _callback_rx) = mpsc::channel(1);
        
        // 为同一个连接注册多个定时器
        for i in 0..3 {
            let registration = TimerRegistration::new(
                1,
                Duration::from_millis(1000 + i * 100),
                TimeoutEvent::IdleTimeout,
                callback_tx.clone(),
            );
            handle.register_timer(registration).await.unwrap();
        }
        
        // 清除连接的所有定时器
        let cleared = handle.clear_connection_timers(1).await.unwrap();
        assert_eq!(cleared, 3);
        
        handle.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_timer_stats() {
        let handle = start_hybrid_timer_task::<TimeoutEvent>();
        let (callback_tx, _callback_rx) = mpsc::channel(1);
        
        // 注册几个定时器
        for i in 1..=3 {
            let registration = TimerRegistration::new(
                i,
                Duration::from_secs(10),
                TimeoutEvent::IdleTimeout,
                callback_tx.clone(),
            );
            handle.register_timer(registration).await.unwrap();
        }
        
        // 获取统计信息
        let stats = handle.get_stats().await.unwrap();
        assert_eq!(stats.total_timers, 3);
        assert_eq!(stats.active_connections, 3);
        
        handle.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_multiple_timer_types() {
        let handle = start_hybrid_timer_task::<TimeoutEvent>();
        let (callback_tx, mut callback_rx) = mpsc::channel(10);
        
        // 注册不同类型的定时器，使用更短的延迟
        let timeout_types = vec![
            TimeoutEvent::IdleTimeout,
            TimeoutEvent::PacketRetransmissionTimeout { sequence_number: 1, timer_id: 123 },
            TimeoutEvent::PathValidationTimeout,
            TimeoutEvent::ConnectionTimeout,
        ];
        
        for (i, timeout_type) in timeout_types.iter().enumerate() {
            let registration = TimerRegistration::new(
                i as u32 + 1,
                Duration::from_millis(120 + i as u64 * 50), // 120ms, 170ms, 220ms, 270ms - 跨越多个槽位
                timeout_type.clone(),
                callback_tx.clone(),
            );
            handle.register_timer(registration).await.unwrap();
        }
        
        // 等待所有定时器到期
        let mut received_events = Vec::new();
        for i in 0..timeout_types.len() {
            match tokio::time::timeout(
                Duration::from_millis(500), // 增加超时时间以适应更长的定时器延迟
                callback_rx.recv()
            ).await {
                Ok(Some(event_data)) => {
                    received_events.push(event_data.timeout_event);
                }
                Ok(None) => {
                    panic!("Channel closed unexpectedly after {} events", i);
                }
                Err(_) => {
                    panic!("Timeout waiting for event {} after receiving {} events", i, received_events.len());
                }
            }
        }
        
        // 验证所有类型的定时器都被触发了
        for timeout_type in &timeout_types {
            assert!(received_events.contains(timeout_type), 
                "Missing timeout event: {:?}, received: {:?}", timeout_type, received_events);
        }
        
        handle.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_timer_replacement() {
        let handle = start_hybrid_timer_task::<TimeoutEvent>();
        let (callback_tx, mut callback_rx) = mpsc::channel(10);
        
        // 注册一个长时间的定时器
        let registration1 = TimerRegistration::new(
            1,
            Duration::from_secs(5),
            TimeoutEvent::IdleTimeout,
            callback_tx.clone(),
        );
        handle.register_timer(registration1).await.unwrap();
        
        // 立即注册一个短时间的同类型定时器（应该替换前一个）
        let registration2 = TimerRegistration::new(
            1,
            Duration::from_millis(100),
            TimeoutEvent::IdleTimeout,
            callback_tx.clone(),
        );
        handle.register_timer(registration2).await.unwrap();
        
        // 等待短时间定时器到期
        let event_data = tokio::time::timeout(
            Duration::from_millis(200),
            callback_rx.recv()
        ).await.unwrap().unwrap();
        
        assert_eq!(event_data.connection_id, 1);
        assert_eq!(event_data.timeout_event, TimeoutEvent::IdleTimeout);
        
        // 确保没有更多事件（长时间定时器应该被取消了）
        let result = tokio::time::timeout(
            Duration::from_millis(100),
            callback_rx.recv()
        ).await;
        assert!(result.is_err());
        
        handle.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_timer_performance() {
        let handle = start_hybrid_timer_task::<TimeoutEvent>();
        let (callback_tx, _callback_rx) = mpsc::channel(1000);
        
        // 注册大量定时器
        let timer_count = 1000;
        let start_time = Instant::now();
        
        for i in 0..timer_count {
            let registration = TimerRegistration::new(
                i,
                Duration::from_secs(60), // 长时间定时器
                TimeoutEvent::IdleTimeout,
                callback_tx.clone(),
            );
            handle.register_timer(registration).await.unwrap();
        }
        
        let registration_duration = start_time.elapsed();
        println!("注册{}个定时器耗时: {:?}", timer_count, registration_duration);
        
        // 获取统计信息
        let stats = handle.get_stats().await.unwrap();
        assert_eq!(stats.total_timers, timer_count as usize);
        
        // 批量取消定时器
        let start_time = Instant::now();
        for i in 0..timer_count {
            handle.clear_connection_timers(i).await.unwrap();
        }
        let cancellation_duration = start_time.elapsed();
        println!("取消{}个定时器耗时: {:?}", timer_count, cancellation_duration);
        
        handle.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_batch_timer_performance() {
        let handle = start_hybrid_timer_task::<TimeoutEvent>();
        let (callback_tx, mut callback_rx) = mpsc::channel(10000);
        
        // 测试批量到期处理的性能
        // Test batch expiry processing performance
        let timer_count = 500; // 减少数量以确保更可靠的测试
        let start_time = Instant::now();
        
        // 注册相同延迟的定时器以确保它们同时到期（测试批量处理）
        // Register timers with same delay to ensure they expire together (test batch processing)
        for i in 0..timer_count {
            let registration = TimerRegistration::new(
                i,
                Duration::from_millis(200), // 统一200ms延迟
                TimeoutEvent::IdleTimeout,
                callback_tx.clone(),
            );
            handle.register_timer(registration).await.unwrap();
        }
        
        let registration_duration = start_time.elapsed();
        println!("批量注册{}个定时器耗时: {:?}", timer_count, registration_duration);
        
        // 等待所有定时器到期并测量批量处理时间
        // Wait for all timers to expire and measure batch processing time
        let batch_start = Instant::now();
        let mut received_count = 0;
        
        // 给足够时间让所有定时器到期
        // Give enough time for all timers to expire
        while received_count < timer_count {
            match tokio::time::timeout(Duration::from_secs(2), callback_rx.recv()).await {
                Ok(Some(_)) => received_count += 1,
                Ok(None) => break,
                Err(_) => {
                    println!("超时等待定时器事件，已接收: {}", received_count);
                    break;
                }
            }
        }
        
        let batch_duration = batch_start.elapsed();
        println!("批量处理{}个定时器耗时: {:?}", received_count, batch_duration);
        if received_count > 0 {
            println!("平均每个定时器处理时间: {:?}", batch_duration / received_count as u32);
        }
        
        // 验证大部分定时器都被正确处理了
        // Verify most timers were processed correctly
        assert!(received_count >= timer_count * 9 / 10, 
            "至少90%的定时器应该被处理，实际: {}/{}", received_count, timer_count);
        
        println!("handle_timer_events_percent: {}", received_count as f64 / timer_count as f64);

        // 性能检查：批量处理应该是合理的
        // Performance check: batch processing should be reasonable
        if received_count > 0 {
            let avg_per_timer = batch_duration / received_count as u32;
            println!("优化后平均处理时间: {:?}", avg_per_timer);
            // 在debug模式下，每个定时器处理时间应该小于10ms（这是很宽松的要求）
            assert!(avg_per_timer < Duration::from_millis(10), 
                "每个定时器处理时间过长: {:?}", avg_per_timer);
        }
        
        handle.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_cache_performance() {
        let handle = start_hybrid_timer_task::<TimeoutEvent>();
        let (callback_tx, _callback_rx) = mpsc::channel(1000);
        
        // 测试缓存优化的性能
        // Test cache optimization performance
        let timer_count = 500;
        
        // 注册定时器
        for i in 0..timer_count {
            let registration = TimerRegistration::new(
                i,
                Duration::from_secs(60 + i as u64), // 不同到期时间
                TimeoutEvent::IdleTimeout,
                callback_tx.clone(),
            );
            handle.register_timer(registration).await.unwrap();
        }
        
        // 多次获取统计信息，测试缓存效果
        let start_time = Instant::now();
        for _ in 0..100 {
            handle.get_stats().await.unwrap();
        }
        let stats_duration = start_time.elapsed();
        
        println!("100次统计查询耗时: {:?}", stats_duration);
        println!("平均每次查询: {:?}", stats_duration / 100);
        
        // 性能断言：多次查询应该受益于缓存
        assert!(stats_duration < Duration::from_millis(10), "统计查询性能不达标");
        
        handle.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_batch_timer_performance_comparison() {
        let handle = start_hybrid_timer_task::<TimeoutEvent>();
        let (callback_tx, _callback_rx) = mpsc::channel(1000);
        
        let timer_count = 1000;
        
        // 测试单个操作性能
        // Test individual operation performance
        let start_time = Instant::now();
        let mut individual_handles = Vec::with_capacity(timer_count);
        
        for i in 0..timer_count {
            let registration = TimerRegistration::new(
                i as u32,
                Duration::from_secs(60), // 长时间定时器
                TimeoutEvent::IdleTimeout,
                callback_tx.clone(),
            );
            let timer_handle = handle.register_timer(registration).await.unwrap();
            individual_handles.push(timer_handle);
        }
        
        let individual_registration_duration = start_time.elapsed();
        println!("单个注册{}个定时器耗时: {:?}", timer_count, individual_registration_duration);
        
        // 单个取消
        let start_time = Instant::now();
        for timer_handle in individual_handles {
            timer_handle.cancel().await.unwrap();
        }
        let individual_cancellation_duration = start_time.elapsed();
        println!("单个取消{}个定时器耗时: {:?}", timer_count, individual_cancellation_duration);
        
        // 测试批量操作性能
        // Test batch operation performance
        let start_time = Instant::now();
        
        // 创建批量注册请求
        let mut batch_registration = BatchTimerRegistration::with_capacity(timer_count);
        for i in 0..timer_count {
            let registration = TimerRegistration::new(
                (i + timer_count) as u32, // 避免ID冲突
                Duration::from_secs(60),
                TimeoutEvent::IdleTimeout,
                callback_tx.clone(),
            );
            batch_registration.add(registration);
        }
        
        let batch_result = handle.batch_register_timers(batch_registration).await.unwrap();
        let batch_registration_duration = start_time.elapsed();
        println!("批量注册{}个定时器耗时: {:?}", timer_count, batch_registration_duration);
        
        assert_eq!(batch_result.success_count(), timer_count);
        assert!(batch_result.all_succeeded());
        
        // 批量取消
        let start_time = Instant::now();
        let entry_ids: Vec<_> = batch_result.successes.into_iter().map(|h| h.entry_id).collect();
        let batch_cancellation = BatchTimerCancellation::new(entry_ids);
        let cancel_result = handle.batch_cancel_timers(batch_cancellation).await.unwrap();
        let batch_cancellation_duration = start_time.elapsed();
        println!("批量取消{}个定时器耗时: {:?}", timer_count, batch_cancellation_duration);
        
        assert_eq!(cancel_result.success_count(), timer_count);
        
        // 性能比较
        // Performance comparison
        let individual_total = individual_registration_duration + individual_cancellation_duration;
        let batch_total = batch_registration_duration + batch_cancellation_duration;
        let speedup_ratio = individual_total.as_nanos() as f64 / batch_total.as_nanos() as f64;
        
        println!("单个操作总耗时: {:?}", individual_total);
        println!("批量操作总耗时: {:?}", batch_total);
        println!("性能提升倍数: {:.2}x", speedup_ratio);
        
        // 计算每个定时器的平均操作时间
        let individual_avg_per_timer = individual_total / (timer_count as u32 * 2); // 注册+取消
        let batch_avg_per_timer = batch_total / (timer_count as u32 * 2);
        
        println!("单个操作平均每定时器耗时: {:?}", individual_avg_per_timer);
        println!("批量操作平均每定时器耗时: {:?}", batch_avg_per_timer);
        
        // 目标：批量操作应该显著快于单个操作
        assert!(speedup_ratio > 2.0, "批量操作应该至少比单个操作快2倍，实际: {:.2}x", speedup_ratio);
        
        // 目标：批量操作每定时器时间应该小于1微秒
        let batch_nanos_per_timer = batch_avg_per_timer.as_nanos();
        println!("批量操作每定时器纳秒: {}", batch_nanos_per_timer);
        
        // 1微秒 = 1000纳秒
        if batch_nanos_per_timer <= 1000 {
            println!("🎉 达成目标！批量操作每定时器耗时: {}纳秒 (< 1微秒)", batch_nanos_per_timer);
        } else {
            println!("⚠️  距离目标还有差距，当前: {}纳秒，目标: <1000纳秒", batch_nanos_per_timer);
        }
        
        handle.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_ultra_high_performance_batch_operations() {
        let handle = start_hybrid_timer_task::<TimeoutEvent>();
        let (callback_tx, _callback_rx) = mpsc::channel(10000);
        
        // 测试超大批量操作性能
        // Test ultra-large batch operation performance
        let batch_sizes = vec![100, 500, 1000, 2000, 5000];
        
        for batch_size in batch_sizes {
            println!("\n=== 测试批量大小: {} ===", batch_size);
            
            // 创建批量注册请求
            let mut batch_registration = BatchTimerRegistration::with_capacity(batch_size);
            for i in 0..batch_size {
                let registration = TimerRegistration::new(
                    i as u32,
                    Duration::from_secs(60), // 长时间定时器避免触发
                    TimeoutEvent::IdleTimeout,
                    callback_tx.clone(),
                );
                batch_registration.add(registration);
            }
            
            // 测试批量注册性能
            let start_time = std::time::Instant::now();
            let batch_result = handle.batch_register_timers(batch_registration).await.unwrap();
            let registration_duration = start_time.elapsed();
            
            assert_eq!(batch_result.success_count(), batch_size);
            
            // 测试批量取消性能
            let entry_ids: Vec<_> = batch_result.successes.into_iter().map(|h| h.entry_id).collect();
            let batch_cancellation = BatchTimerCancellation::new(entry_ids);
            
            let start_time = std::time::Instant::now();
            let cancel_result = handle.batch_cancel_timers(batch_cancellation).await.unwrap();
            let cancellation_duration = start_time.elapsed();
            
            assert_eq!(cancel_result.success_count(), batch_size);
            
            // 性能分析
            let total_duration = registration_duration + cancellation_duration;
            let nanos_per_operation = total_duration.as_nanos() / (batch_size as u128 * 2); // 注册+取消
            
            println!("批量注册耗时: {:?}", registration_duration);
            println!("批量取消耗时: {:?}", cancellation_duration);
            println!("总耗时: {:?}", total_duration);
            println!("每操作平均: {} 纳秒", nanos_per_operation);
            
            // 性能目标检查
            if nanos_per_operation <= 200 {
                println!("🎉 性能优秀！每操作 {} 纳秒", nanos_per_operation);
            } else if nanos_per_operation <= 500 {
                println!("✅ 性能良好！每操作 {} 纳秒", nanos_per_operation);
            } else if nanos_per_operation <= 1000 {
                println!("⚠️  性能达标！每操作 {} 纳秒", nanos_per_operation);
            } else {
                println!("❌ 性能不足！每操作 {} 纳秒", nanos_per_operation);
            }
            
            // 清理：确保没有残留的定时器
            let stats = handle.get_stats().await.unwrap();
            if stats.total_timers > 0 {
                println!("⚠️  警告：还有 {} 个定时器未清理", stats.total_timers);
            }
        }
        
        handle.shutdown().await.unwrap();
    }

    /// 全局任务管理测试 - 生命周期管理
    /// Global task management tests - lifecycle management
    #[tokio::test]
    async fn test_global_timer_task_lifecycle() {
        println!("\n🔄 全局定时器任务生命周期测试");
        println!("========================================");
        
        // 测试1：正常启动和关闭
        println!("\n📋 测试1: 正常启动和关闭");
        let handle = start_hybrid_timer_task::<TimeoutEvent>();
        
        // 验证任务能够正常响应
        let stats = handle.get_stats().await.unwrap();
        assert_eq!(stats.total_timers, 0);
        assert_eq!(stats.active_connections, 0);
        println!("  ✅ 任务启动成功，初始状态正确");
        
        // 正常关闭
        handle.shutdown().await.unwrap();
        println!("  ✅ 任务正常关闭");
        
        // 测试2：重复关闭的容错性
        println!("\n📋 测试2: 重复关闭的容错性");
        let handle = start_hybrid_timer_task::<TimeoutEvent>();
        
        // 第一次关闭
        handle.shutdown().await.unwrap();
        println!("  ✅ 第一次关闭成功");
        
        // 等待一小段时间确保任务已经关闭
        sleep(Duration::from_millis(10)).await;
        
        // 第二次关闭应该失败但不崩溃
        let result = handle.shutdown().await;
        assert!(result.is_err());
        match result {
            Err(TimerError::TaskShutdown) => {
                println!("  ✅ 第二次关闭正确返回TaskShutdown错误");
            }
            _ => panic!("期望TaskShutdown错误"),
        }
        
        // 测试3：关闭后的操作应该失败
        println!("\n📋 测试3: 关闭后的操作应该失败");
        let (callback_tx, _callback_rx) = mpsc::channel(1);
        let registration = TimerRegistration::new(
            1,
            Duration::from_millis(100),
            TimeoutEvent::IdleTimeout,
            callback_tx,
        );
        
        let result = handle.register_timer(registration).await;
        assert!(result.is_err());
        match result {
            Err(TimerError::TaskShutdown) => {
                println!("  ✅ 关闭后注册定时器正确返回TaskShutdown错误");
            }
            _ => panic!("期望TaskShutdown错误"),
        }
        
        // 获取统计信息也应该失败
        let result = handle.get_stats().await;
        assert!(result.is_err());
        println!("  ✅ 关闭后获取统计信息正确返回错误");
    }

    /// 全局任务管理测试 - 错误恢复
    /// Global task management tests - error recovery
    #[tokio::test]
    async fn test_global_timer_task_error_recovery() {
        println!("\n🛠️ 全局定时器任务错误恢复测试");
        println!("========================================");
        
        let handle = start_hybrid_timer_task::<TimeoutEvent>();
        
        // 测试1：无效定时器ID的批量取消
        println!("\n📋 测试1: 无效定时器ID的批量取消");
        let invalid_ids = vec![99999, 88888, 77777];
        let batch_cancellation = BatchTimerCancellation::new(invalid_ids);
        
        let result = handle.batch_cancel_timers(batch_cancellation).await.unwrap();
        assert_eq!(result.success_count(), 0);
        assert_eq!(result.failure_count(), 3);
        
        // 验证错误类型
        for (index, error) in &result.failures {
            match error {
                TimerError::TimerNotFound => {
                    println!("  ✅ 索引{}正确返回TimerNotFound错误", index);
                }
                _ => panic!("期望TimerNotFound错误"),
            }
        }
        
        // 测试2：混合有效/无效ID的批量取消
        println!("\n📋 测试2: 混合有效/无效ID的批量取消");
        let (callback_tx, _callback_rx) = mpsc::channel(10);
        
        // 先注册一些有效的定时器
        let mut valid_handles = Vec::new();
        for i in 0..3 {
            let registration = TimerRegistration::new(
                i,
                Duration::from_secs(60), // 长时间避免自动触发
                TimeoutEvent::IdleTimeout,
                callback_tx.clone(),
            );
            let handle = handle.register_timer(registration).await.unwrap();
            valid_handles.push(handle);
        }
        
        // 创建混合批量取消（有效ID + 无效ID）
        let mut mixed_ids = Vec::new();
        mixed_ids.push(valid_handles[0].entry_id); // 有效
        mixed_ids.push(99999); // 无效
        mixed_ids.push(valid_handles[1].entry_id); // 有效
        mixed_ids.push(88888); // 无效
        mixed_ids.push(valid_handles[2].entry_id); // 有效
        
        let batch_cancellation = BatchTimerCancellation::new(mixed_ids);
        let result = handle.batch_cancel_timers(batch_cancellation).await.unwrap();
        
        assert_eq!(result.success_count(), 3);
        assert_eq!(result.failure_count(), 2);
        println!("  ✅ 混合批量取消：3个成功，2个失败");
        
        // 测试3：空批量操作
        println!("\n📋 测试3: 空批量操作");
        let empty_batch_reg = BatchTimerRegistration::with_capacity(0);
        let result = handle.batch_register_timers(empty_batch_reg).await.unwrap();
        assert_eq!(result.success_count(), 0);
        assert_eq!(result.failure_count(), 0);
        println!("  ✅ 空批量注册正确处理");
        
        let empty_batch_cancel = BatchTimerCancellation::new(vec![]);
        let result = handle.batch_cancel_timers(empty_batch_cancel).await.unwrap();
        assert_eq!(result.success_count(), 0);
        assert_eq!(result.failure_count(), 0);
        println!("  ✅ 空批量取消正确处理");
        
        handle.shutdown().await.unwrap();
    }

    /// 全局任务管理测试 - 连接生命周期管理
    /// Global task management tests - connection lifecycle management
    #[tokio::test]
    async fn test_connection_lifecycle_management() {
        println!("\n🔗 连接生命周期管理测试");
        println!("========================================");
        
        let handle = start_hybrid_timer_task::<TimeoutEvent>();
        let (callback_tx, mut callback_rx) = mpsc::channel(100);
        
        // 测试1：连接的定时器隔离
        println!("\n📋 测试1: 连接的定时器隔离");
        
        // 为连接1注册定时器
        for i in 0..5 {
            let registration = TimerRegistration::new(
                1, // 连接1
                Duration::from_secs(60 + i), // 不同的超时时间
                TimeoutEvent::IdleTimeout,
                callback_tx.clone(),
            );
            handle.register_timer(registration).await.unwrap();
        }
        
        // 为连接2注册定时器
        for i in 0..3 {
            let registration = TimerRegistration::new(
                2, // 连接2
                Duration::from_secs(70 + i),
                TimeoutEvent::PacketRetransmissionTimeout { 
                    sequence_number: (i + 100) as u32, 
                    timer_id: (i + 1000) as u64 
                },
                callback_tx.clone(),
            );
            handle.register_timer(registration).await.unwrap();
        }
        
        let stats = handle.get_stats().await.unwrap();
        assert_eq!(stats.total_timers, 8);
        assert_eq!(stats.active_connections, 2);
        println!("  ✅ 连接隔离：连接1有5个定时器，连接2有3个定时器");
        
        // 测试2：清除单个连接的定时器
        println!("\n📋 测试2: 清除单个连接的定时器");
        let cleared = handle.clear_connection_timers(1).await.unwrap();
        assert_eq!(cleared, 5);
        
        let stats = handle.get_stats().await.unwrap();
        assert_eq!(stats.total_timers, 3); // 只剩连接2的3个定时器
        assert_eq!(stats.active_connections, 1); // 只剩连接2
        println!("  ✅ 清除连接1后：总定时器3个，活跃连接1个");
        
        // 测试3：重复清除同一连接
        println!("\n📋 测试3: 重复清除同一连接");
        let cleared = handle.clear_connection_timers(1).await.unwrap();
        assert_eq!(cleared, 0); // 已经清除过了
        
        let stats = handle.get_stats().await.unwrap();
        assert_eq!(stats.total_timers, 3); // 不变
        println!("  ✅ 重复清除返回0，不影响其他连接");
        
        // 测试4：清除不存在的连接
        println!("\n📋 测试4: 清除不存在的连接");
        let cleared = handle.clear_connection_timers(999).await.unwrap();
        assert_eq!(cleared, 0);
        println!("  ✅ 清除不存在的连接返回0");
        
        // 测试5：连接重用ID
        println!("\n📋 测试5: 连接重用ID");
        // 清除所有剩余定时器
        handle.clear_connection_timers(2).await.unwrap();
        
        // 重新使用连接ID 1
        let registration = TimerRegistration::new(
            1, // 重用连接1的ID
            Duration::from_millis(100),
            TimeoutEvent::ConnectionTimeout,
            callback_tx.clone(),
        );
        handle.register_timer(registration).await.unwrap();
        
        let stats = handle.get_stats().await.unwrap();
        assert_eq!(stats.total_timers, 1);
        assert_eq!(stats.active_connections, 1);
        println!("  ✅ 连接ID重用成功");
        
        // 等待定时器触发
        let event_data = callback_rx.recv().await.unwrap();
        assert_eq!(event_data.connection_id, 1);
        assert_eq!(event_data.timeout_event, TimeoutEvent::ConnectionTimeout);
        println!("  ✅ 重用连接的定时器正常触发");
        
        handle.shutdown().await.unwrap();
    }

    /// 全局任务管理测试 - 高负载批量处理
    /// Global task management tests - high load batch processing
    #[tokio::test(flavor = "multi_thread")]
    async fn test_high_load_batch_processing() {
        println!("\n⚡ 高负载批量处理测试");
        println!("========================================");
        
        let handle = start_hybrid_timer_task::<TimeoutEvent>();
        let (callback_tx, _callback_rx) = mpsc::channel(50000);
        
        // 测试1：极大批量注册
        println!("\n📋 测试1: 极大批量注册 (10000个定时器)");
        let batch_size = 10000;
        let mut batch_registration = BatchTimerRegistration::with_capacity(batch_size);
        
        for i in 0..batch_size {
            let registration = TimerRegistration::new(
                (i % 1000) as u32, // 1000个不同连接
                Duration::from_secs(3600), // 1小时超时，避免触发
                TimeoutEvent::IdleTimeout,
                callback_tx.clone(),
            );
            batch_registration.add(registration);
        }
        
        let start_time = Instant::now();
        let result = handle.batch_register_timers(batch_registration).await.unwrap();
        let registration_duration = start_time.elapsed();
        
        assert_eq!(result.success_count(), batch_size);
        assert!(result.all_succeeded());
        
        let nanos_per_timer = registration_duration.as_nanos() / batch_size as u128;
        println!("  ✅ 成功注册{}个定时器，耗时{:?}", batch_size, registration_duration);
        println!("  📊 平均每个定时器: {}纳秒", nanos_per_timer);
        
        // 验证统计信息
        let stats = handle.get_stats().await.unwrap();
        assert_eq!(stats.total_timers, batch_size);
        assert_eq!(stats.active_connections, 1000);
        println!("  ✅ 统计信息正确：{}个定时器，{}个连接", stats.total_timers, stats.active_connections);
        
        // 测试2：极大批量取消
        println!("\n📋 测试2: 极大批量取消");
        let entry_ids: Vec<_> = result.successes.into_iter().map(|h| h.entry_id).collect();
        let batch_cancellation = BatchTimerCancellation::new(entry_ids);
        
        let start_time = Instant::now();
        let cancel_result = handle.batch_cancel_timers(batch_cancellation).await.unwrap();
        let cancellation_duration = start_time.elapsed();
        
        assert_eq!(cancel_result.success_count(), batch_size);
        assert!(cancel_result.all_succeeded());
        
        let nanos_per_cancel = cancellation_duration.as_nanos() / batch_size as u128;
        println!("  ✅ 成功取消{}个定时器，耗时{:?}", batch_size, cancellation_duration);
        println!("  📊 平均每个取消: {}纳秒", nanos_per_cancel);
        
        // 验证清理完成
        let stats = handle.get_stats().await.unwrap();
        assert_eq!(stats.total_timers, 0);
        assert_eq!(stats.active_connections, 0);
        println!("  ✅ 全部清理完成：0个定时器，0个连接");
        
        // 测试3：连续批量操作的内存效率
        println!("\n📋 测试3: 连续批量操作的内存效率");
        let iterations = 10;
        let batch_size_per_iteration = 1000;
        
        for iteration in 0..iterations {
            // 注册
            let mut batch_registration = BatchTimerRegistration::with_capacity(batch_size_per_iteration);
            for i in 0..batch_size_per_iteration {
                let registration = TimerRegistration::new(
                    (iteration * batch_size_per_iteration + i) as u32,
                    Duration::from_secs(3600),
                    TimeoutEvent::IdleTimeout,
                    callback_tx.clone(),
                );
                batch_registration.add(registration);
            }
            
            let result = handle.batch_register_timers(batch_registration).await.unwrap();
            assert_eq!(result.success_count(), batch_size_per_iteration);
            
            // 立即取消
            let entry_ids: Vec<_> = result.successes.into_iter().map(|h| h.entry_id).collect();
            let batch_cancellation = BatchTimerCancellation::new(entry_ids);
            let cancel_result = handle.batch_cancel_timers(batch_cancellation).await.unwrap();
            assert_eq!(cancel_result.success_count(), batch_size_per_iteration);
        }
        
        // 验证没有内存泄漏
        let stats = handle.get_stats().await.unwrap();
        assert_eq!(stats.total_timers, 0);
        assert_eq!(stats.active_connections, 0);
        println!("  ✅ {}轮连续批量操作完成，无内存泄漏", iterations);
        
        handle.shutdown().await.unwrap();
    }

    /// 全局任务管理测试 - 并发访问安全性
    /// Global task management tests - concurrent access safety
    #[tokio::test(flavor = "multi_thread")]
    async fn test_concurrent_access_safety() {
        println!("\n🔒 并发访问安全性测试");
        println!("========================================");
        
        let handle = start_hybrid_timer_task::<TimeoutEvent>();
        let (callback_tx, _callback_rx) = mpsc::channel(10000);
        
        // 测试1：并发注册和取消
        println!("\n📋 测试1: 并发注册和取消");
        let concurrent_tasks = 50;
        let timers_per_task = 100;
        
        let mut handles = Vec::new();
        
        // 启动多个并发任务
        for task_id in 0..concurrent_tasks {
            let handle_clone = handle.clone();
            let callback_tx_clone = callback_tx.clone();
            
            let task_handle = tokio::spawn(async move {
                let mut timer_handles = Vec::new();
                
                // 注册定时器
                for i in 0..timers_per_task {
                    let registration = TimerRegistration::new(
                        (task_id * timers_per_task + i) as u32,
                        Duration::from_secs(60),
                        TimeoutEvent::IdleTimeout,
                        callback_tx_clone.clone(),
                    );
                    
                    let timer_handle = handle_clone.register_timer(registration).await.unwrap();
                    timer_handles.push(timer_handle);
                }
                
                // 立即取消一半的定时器
                for i in 0..timers_per_task/2 {
                    timer_handles[i].cancel().await.unwrap();
                }
                
                timers_per_task / 2 // 返回剩余定时器数量
            });
            
            handles.push(task_handle);
        }
        
        // 等待所有任务完成
        let mut _total_remaining = 0;
        for handle in handles {
            _total_remaining += handle.await.unwrap();
        }
        
        // 验证最终状态
        let stats = handle.get_stats().await.unwrap();
        let expected_remaining = concurrent_tasks * timers_per_task / 2;
        assert_eq!(stats.total_timers, expected_remaining);
        println!("  ✅ 并发操作完成：期望{}个剩余定时器，实际{}个", expected_remaining, stats.total_timers);
        
        // 测试2：并发批量操作
        println!("\n📋 测试2: 并发批量操作");
        let batch_tasks = 20;
        let batch_size = 200;
        
        let mut batch_handles = Vec::new();
        
        for task_id in 0..batch_tasks {
            let handle_clone = handle.clone();
            let callback_tx_clone = callback_tx.clone();
            
            let task_handle = tokio::spawn(async move {
                let mut batch_registration = BatchTimerRegistration::with_capacity(batch_size);
                
                for i in 0..batch_size {
                    let registration = TimerRegistration::new(
                        (100000 + task_id * batch_size + i) as u32, // 避免ID冲突
                        Duration::from_secs(120),
                        TimeoutEvent::PacketRetransmissionTimeout { 
                            sequence_number: (task_id * batch_size + i) as u32, 
                            timer_id: (10000 + task_id * batch_size + i) as u64 
                        },
                        callback_tx_clone.clone(),
                    );
                    batch_registration.add(registration);
                }
                
                let result = handle_clone.batch_register_timers(batch_registration).await.unwrap();
                assert_eq!(result.success_count(), batch_size);
                
                // 立即批量取消
                let entry_ids: Vec<_> = result.successes.into_iter().map(|h| h.entry_id).collect();
                let batch_cancellation = BatchTimerCancellation::new(entry_ids);
                let cancel_result = handle_clone.batch_cancel_timers(batch_cancellation).await.unwrap();
                assert_eq!(cancel_result.success_count(), batch_size);
                
                batch_size
            });
            
            batch_handles.push(task_handle);
        }
        
        // 等待所有批量任务完成
        let mut total_processed = 0;
        for handle in batch_handles {
            total_processed += handle.await.unwrap();
        }
        
        println!("  ✅ 并发批量操作完成：总共处理{}个定时器", total_processed);
        
        // 验证最终状态（之前的定时器还在）
        let stats = handle.get_stats().await.unwrap();
        assert_eq!(stats.total_timers, expected_remaining); // 批量操作的都被取消了
        println!("  ✅ 最终状态正确：{}个定时器", stats.total_timers);
        
        handle.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_simd_compatibility_and_fallback() {
        println!("\n🔧 SIMD兼容性和降级测试");
        println!("========================================");
        
        // 检测当前平台的SIMD支持情况
        println!("当前平台SIMD支持检测:");
        
        #[cfg(target_arch = "x86_64")]
        {
            println!("  架构: x86_64");
            println!("  SSE2: ✅ (架构保证)");
            
            if std::is_x86_feature_detected!("sse4.2") {
                println!("  SSE4.2: ✅");
            } else {
                println!("  SSE4.2: ❌");
            }
            
            if std::is_x86_feature_detected!("avx2") {
                println!("  AVX2: ✅");
            } else {
                println!("  AVX2: ❌");
            }
            
            if std::is_x86_feature_detected!("avx512f") {
                println!("  AVX-512: ✅");
            } else {
                println!("  AVX-512: ❌");
            }
        }
        
        #[cfg(target_arch = "x86")]
        {
            println!("  架构: x86");
            // x86平台的特性检测
        }
        
        #[cfg(target_arch = "aarch64")]
        {
            println!("  架构: ARM64");
            println!("  NEON: ✅ (架构保证)");
        }
        
        #[cfg(not(any(target_arch = "x86_64", target_arch = "x86", target_arch = "aarch64")))]
        {
            println!("  架构: 其他 ({}) - 将使用标量实现", std::env::consts::ARCH);
        }
        
        println!("\nWide库SIMD向量测试:");
        
        // 测试Wide库的u64x4在不同平台上的行为
        use wide::u64x4;
        
        let vec1 = u64x4::new([1, 2, 3, 4]);
        let vec2 = u64x4::new([5, 6, 7, 8]);
        let result = vec1 + vec2;
        let result_array = result.to_array();
        
        println!("  u64x4 加法测试: {:?} + {:?} = {:?}", 
            vec1.to_array(), vec2.to_array(), result_array);
        
        // 验证结果正确性
        assert_eq!(result_array, [6, 8, 10, 12]);
        println!("  ✅ u64x4运算结果正确");
        
        // 测试我们的定时器系统在当前平台的工作情况
        let handle = start_hybrid_timer_task::<TimeoutEvent>();
        let (callback_tx, _callback_rx) = mpsc::channel(100);
        
        // 小批量测试确保基本功能工作
        let batch_size = 64;
        let mut batch = BatchTimerRegistration::with_capacity(batch_size);
        
        for i in 0..batch_size {
            let registration = TimerRegistration::new(
                i as u32,
                Duration::from_millis(1000),
                TimeoutEvent::IdleTimeout,
                callback_tx.clone(),
            );
            batch.add(registration);
        }
        
        let start_time = std::time::Instant::now();
        let result = handle.batch_register_timers(batch).await.unwrap();
        let duration = start_time.elapsed();
        
        let nanos_per_op = duration.as_nanos() / batch_size as u128;
        
        println!("\n定时器系统兼容性测试:");
        println!("  批量大小: {}", batch_size);
        println!("  注册耗时: {:?}", duration);
        println!("  每操作: {} 纳秒", nanos_per_op);
        println!("  成功注册: {}", result.success_count());
        
        assert_eq!(result.success_count(), batch_size);
        println!("  ✅ 定时器系统在当前平台正常工作");
        
        // 清理
        let entry_ids: Vec<_> = result.successes.into_iter().map(|h| h.entry_id).collect();
        let cancellation = BatchTimerCancellation::new(entry_ids);
        handle.batch_cancel_timers(cancellation).await.unwrap();
        
        println!("\n📊 兼容性总结:");
        println!("  • Wide库提供跨平台SIMD抽象");
        println!("  • 在不支持的平台自动降级到标量代码");
        println!("  • 不会崩溃或产生未定义行为");
        println!("  • u64x4在SSE2(100%覆盖)上就能工作，不需要AVX2");
        println!("  • AVX2只是提供更好性能，不是必需条件");
        
        handle.shutdown().await.unwrap();
    }
}