//! 全局定时器系统集成测试
//! Global timer system integration tests

use crate::core::endpoint::timing::TimeoutEvent;
use tokio::time::{sleep, Duration};

#[tokio::test]
async fn test_global_timer_integration() {
    // 启动全局定时器任务
    let timer_handle = crate::timer::task::start_global_timer_task();
    
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
    
    // 等待定时器到期
    sleep(Duration::from_millis(150)).await;
    
    // 检查定时器事件
    let events = timer_manager.check_timer_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0], TimeoutEvent::IdleTimeout);
    
    // 关闭定时器任务
    let _ = timer_handle.shutdown().await;
}

#[tokio::test]
async fn test_timer_cancellation() {
    let timer_handle = crate::timer::task::start_global_timer_task();
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
    let timer_handle = crate::timer::task::start_global_timer_task();
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
    
    // 等待所有定时器到期
    sleep(Duration::from_millis(200)).await;
    
    // 检查事件
    let events = timer_manager.check_timer_events();
    assert_eq!(events.len(), 2);
    assert!(events.contains(&TimeoutEvent::IdleTimeout));
    assert!(events.contains(&TimeoutEvent::PathValidationTimeout));
    
    let _ = timer_handle.shutdown().await;
}

#[tokio::test]
async fn test_timer_replacement() {
    let timer_handle = crate::timer::task::start_global_timer_task();
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
    
    // 立即用短时间定时器替换
    timer_manager.register_timer(
        TimeoutEvent::IdleTimeout,
        Duration::from_millis(100),
    ).await.unwrap();
    
    // 等待短时间定时器到期
    sleep(Duration::from_millis(150)).await;
    
    // 应该只有一个事件（短时间的）
    let events = timer_manager.check_timer_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0], TimeoutEvent::IdleTimeout);
    
    let _ = timer_handle.shutdown().await;
}