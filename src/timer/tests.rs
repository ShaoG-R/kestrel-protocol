//! 全局定时器系统集成测试
//! Global timer system integration tests

use crate::core::endpoint::timing::TimeoutEvent;
use tokio::time::{Duration, timeout};

#[tokio::test]
async fn test_global_timer_integration() {
    // 启动全局定时器任务
    let timer_handle = crate::timer::start_hybrid_timer_task();
    
    // 创建定时器管理器（事件驱动接收）
    let connection_id = 1;
    let (mut timer_manager, mut timer_rx) = crate::core::endpoint::timing::TimerManager::new_with_receiver(
        connection_id,
        timer_handle.clone(),
    );
    
    // 注册一个短时间的定时器
    let result = timer_manager.register_timer(
        TimeoutEvent::IdleTimeout,
        Duration::from_millis(100),
    ).await;
    
    assert!(result.is_ok(), "Failed to register timer: {:?}", result);
    println!("Timer registered successfully");
    
    // 直接通过通道接收定时器事件
    let evt = timeout(Duration::from_millis(500), timer_rx.recv())
        .await
        .expect("Timed out waiting for timer event")
        .expect("Timer event channel closed");
    assert_eq!(evt.timeout_event, TimeoutEvent::IdleTimeout);
    let _ = timer_handle.shutdown().await;
}

#[tokio::test]
async fn test_timer_cancellation() {
    let timer_handle = crate::timer::start_hybrid_timer_task();
    let connection_id = 2;
    let (mut timer_manager, mut timer_rx) = crate::core::endpoint::timing::TimerManager::new_with_receiver(
        connection_id,
        timer_handle.clone(),
    );
    
    // 注册定时器
    timer_manager.register_timer(
        TimeoutEvent::IdleTimeout,
        Duration::from_millis(200),
    ).await.unwrap();
    
    // 立即取消定时器
    let cancelled = timer_manager.cancel_timer(&TimeoutEvent::IdleTimeout).await;
    assert!(cancelled);
    
    // 等待一段时间，确认没有事件到达
    if let Ok(Some(evt)) = timeout(Duration::from_millis(250), timer_rx.recv()).await {
        panic!("Unexpected event after cancellation: {:?}", evt.timeout_event);
    }
    
    let _ = timer_handle.shutdown().await;
}

#[tokio::test]
async fn test_multiple_timer_types() {
    let timer_handle = crate::timer::start_hybrid_timer_task();
    let connection_id = 3;
    let (mut timer_manager, mut timer_rx) = crate::core::endpoint::timing::TimerManager::new_with_receiver(
        connection_id,
        timer_handle.clone(),
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
    
    // 通过通道收集两个事件（顺序可能不固定）
    let mut received = Vec::new();
    for _ in 0..2 {
        let evt = timeout(Duration::from_millis(500), timer_rx.recv())
            .await
            .expect("Timed out waiting for timer event")
            .expect("Timer event channel closed");
        received.push(evt.timeout_event);
    }
    assert!(received.contains(&TimeoutEvent::IdleTimeout), "Missing IdleTimeout event");
    assert!(received.contains(&TimeoutEvent::PathValidationTimeout), "Missing PathValidationTimeout event");
    
    let _ = timer_handle.shutdown().await;
}

#[tokio::test]
async fn test_timer_replacement() {
    let timer_handle = crate::timer::start_hybrid_timer_task();
    let connection_id = 4;
    let (mut timer_manager, mut timer_rx) = crate::core::endpoint::timing::TimerManager::new_with_receiver(
        connection_id,
        timer_handle.clone(),
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
    
    // 等待短时间定时器事件到达
    let evt = timeout(Duration::from_millis(500), timer_rx.recv())
        .await
        .expect("Timed out waiting for timer event")
        .expect("Timer event channel closed");
    assert_eq!(evt.timeout_event, TimeoutEvent::IdleTimeout);
    // 不应再有额外事件
    if let Ok(Some(extra)) = timeout(Duration::from_millis(100), timer_rx.recv()).await {
        panic!("Unexpected extra event: {:?}", extra.timeout_event);
    }
    
    let _ = timer_handle.shutdown().await;
}