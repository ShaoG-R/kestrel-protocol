//! 全局定时器系统集成测试
//! Global timer system integration tests

use crate::core::endpoint::timing::TimeoutEvent;
use tokio::time::{sleep, Duration};

#[tokio::test]
async fn test_global_timer_integration() {
    // 启动全局定时器任务
    let timer_handle = crate::timer::start_hybrid_timer_task();
    
    // 创建定时器管理器
    let connection_id = 1;
    let mut timer_manager = crate::core::endpoint::timing::TimerManager::new(
        connection_id, 
        timer_handle.clone()
    );
    
    // 注册一个短时间的定时器
    let result = timer_manager.register_timer(
        TimeoutEvent::IdleTimeout,
        Duration::from_millis(100),
    ).await;
    
    assert!(result.is_ok(), "Failed to register timer: {:?}", result);
    println!("Timer registered successfully");
    
    // 增加等待时间以确保定时器有足够时间到期
    sleep(Duration::from_millis(300)).await;
    println!("Waited 300ms for timer expiration");
    
    // 手动触发时间轮推进 (诊断用)
    // 这是一个诊断性的调试步骤
    for i in 0..5 {
        sleep(Duration::from_millis(50)).await;
        let events = timer_manager.check_timer_events();
        println!("Check #{}: Found {} events", i + 1, events.len());
        if !events.is_empty() {
            println!("Events: {:?}", events);
            assert_eq!(events.len(), 1);
            assert_eq!(events[0], TimeoutEvent::IdleTimeout);
            let _ = timer_handle.shutdown().await;
            return;
        }
    }
    
    // 如果仍然没有事件，则失败
    let events = timer_manager.check_timer_events();
    println!("Final check: Found {} events", events.len());
    
    // 关闭定时器任务
    let _ = timer_handle.shutdown().await;
    
    // 如果没有收到任何事件，测试失败
    assert_eq!(events.len(), 1, "Expected 1 timeout event but got {}", events.len());
    assert_eq!(events[0], TimeoutEvent::IdleTimeout);
}

#[tokio::test]
async fn test_timer_cancellation() {
    let timer_handle = crate::timer::start_hybrid_timer_task();
    let connection_id = 2;
    let mut timer_manager = crate::core::endpoint::timing::TimerManager::new(
        connection_id, 
        timer_handle.clone()
    );
    
    // 注册定时器
    timer_manager.register_timer(
        TimeoutEvent::IdleTimeout,
        Duration::from_millis(200),
    ).await.unwrap();
    
    // 立即取消定时器
    let cancelled = timer_manager.cancel_timer(&TimeoutEvent::IdleTimeout).await;
    assert!(cancelled);
    
    // 等待一段时间，确保定时器不会触发
    sleep(Duration::from_millis(250)).await;
    
    // 检查没有事件
    let events = timer_manager.check_timer_events();
    assert!(events.is_empty());
    
    let _ = timer_handle.shutdown().await;
}

#[tokio::test]
async fn test_multiple_timer_types() {
    let timer_handle = crate::timer::start_hybrid_timer_task();
    let connection_id = 3;
    let mut timer_manager = crate::core::endpoint::timing::TimerManager::new(
        connection_id, 
        timer_handle.clone()
    );
    
    // 注册不同类型的定时器
    timer_manager.register_timer(
        TimeoutEvent::IdleTimeout,
        Duration::from_millis(100),
    ).await.unwrap();
    
    timer_manager.register_timer(
        TimeoutEvent::PathValidationTimeout,
        Duration::from_millis(150),
    ).await.unwrap();
    
    println!("Registered two different timer types");
    
    // 增加等待时间以确保所有定时器到期
    sleep(Duration::from_millis(400)).await;
    println!("Waited 400ms for both timers to expire");
    
    // 多次检查以收集所有事件
    let mut all_events = Vec::new();
    for i in 0..5 {
        sleep(Duration::from_millis(50)).await;
        let mut events = timer_manager.check_timer_events();
        all_events.append(&mut events);
        println!("Check #{}: Found {} new events, total: {}", i + 1, events.len(), all_events.len());
        if all_events.len() >= 2 {
            break;
        }
    }
    
    println!("Final events: {:?}", all_events);
    
    // 检查事件
    assert_eq!(all_events.len(), 2, "Expected 2 events but got {}", all_events.len());
    assert!(all_events.contains(&TimeoutEvent::IdleTimeout), "Missing IdleTimeout event");
    assert!(all_events.contains(&TimeoutEvent::PathValidationTimeout), "Missing PathValidationTimeout event");
    
    let _ = timer_handle.shutdown().await;
}

#[tokio::test]
async fn test_timer_replacement() {
    let timer_handle = crate::timer::start_hybrid_timer_task();
    let connection_id = 4;
    let mut timer_manager = crate::core::endpoint::timing::TimerManager::new(
        connection_id, 
        timer_handle.clone()
    );
    
    // 注册一个长时间定时器
    timer_manager.register_timer(
        TimeoutEvent::IdleTimeout,
        Duration::from_secs(10),
    ).await.unwrap();
    
    println!("Registered long-duration timer (10s)");
    
    // 立即用短时间定时器替换
    timer_manager.register_timer(
        TimeoutEvent::IdleTimeout,
        Duration::from_millis(100),
    ).await.unwrap();
    
    println!("Replaced with short-duration timer (100ms)");
    
    // 增加等待时间以确保短时间定时器到期
    sleep(Duration::from_millis(300)).await;
    println!("Waited 300ms for short timer to expire");
    
    // 多次检查以收集事件
    let mut all_events = Vec::new();
    for i in 0..5 {
        sleep(Duration::from_millis(50)).await;
        let mut events = timer_manager.check_timer_events();
        all_events.append(&mut events);
        println!("Check #{}: Found {} new events, total: {}", i + 1, events.len(), all_events.len());
        if !all_events.is_empty() {
            break;
        }
    }
    
    println!("Final events: {:?}", all_events);
    
    // 应该只有一个事件（短时间的）
    assert_eq!(all_events.len(), 1, "Expected 1 event but got {}", all_events.len());
    assert_eq!(all_events[0], TimeoutEvent::IdleTimeout);
    
    let _ = timer_handle.shutdown().await;
}