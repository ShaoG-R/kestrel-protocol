//! 全局定时器系统集成测试
//! Global timer system integration tests

use kestrel_protocol::{
    config::Config,
    core::endpoint::timing::{TimeoutEvent, TimingManager},
    socket::{TransportReliableUdpSocket, transport::UdpTransport},
    timer::start_hybrid_timer_task,
};
use std::net::SocketAddr;
use tokio::time::{Duration, sleep, timeout};

#[tokio::test]
async fn test_timer_system_with_real_connection() {
    // 启动全局定时器任务
    let timer_handle = start_hybrid_timer_task();

    // 创建 TimingManager
    let connection_id = 1;
    let (mut timing_manager, mut timer_rx) =
        TimingManager::new(connection_id, timer_handle.clone());

    // 测试基本的定时器功能
    let config = Config::default();

    // 注册空闲超时定时器
    let result = timing_manager.register_idle_timeout(&config).await;
    assert!(
        result.is_ok(),
        "Failed to register idle timeout: {:?}",
        result
    );

    // 注册路径验证超时定时器
    let result = timing_manager
        .register_path_validation_timeout(Duration::from_millis(200))
        .await;
    assert!(
        result.is_ok(),
        "Failed to register path validation timeout: {:?}",
        result
    );

    // 等待定时器到期
    sleep(Duration::from_millis(250)).await;

    // 通过通道接收定时器事件（符合真实事件驱动架构）
    let evt = timeout(Duration::from_millis(500), timer_rx.recv())
        .await
        .expect("Timed out waiting for timer event")
        .expect("Timer event channel closed");
    assert_eq!(evt.timeout_event, TimeoutEvent::PathValidationTimeout);

    // 关闭定时器任务
    let _ = timer_handle.shutdown().await;
}

#[tokio::test]
async fn test_timer_reset_functionality() {
    let timer_handle = start_hybrid_timer_task();
    let connection_id = 2;
    let (mut timing_manager, mut timer_rx) =
        TimingManager::new(connection_id, timer_handle.clone());

    let config = Config::default();

    // 注册空闲超时定时器
    timing_manager.register_idle_timeout(&config).await.unwrap();

    // 等待一半时间
    sleep(Duration::from_millis(
        config.connection.idle_timeout.as_millis() as u64 / 2,
    ))
    .await;

    // 重置空闲超时定时器（模拟收到数据包）
    let result = timing_manager.reset_idle_timeout(&config).await;
    assert!(result.is_ok(), "Failed to reset idle timeout: {:?}", result);

    // 再等待一半时间（总共等待了原始超时时间，但重置后应该不会超时）
    sleep(Duration::from_millis(
        config.connection.idle_timeout.as_millis() as u64 / 2 + 50,
    ))
    .await;

    // 检查事件（应该没有超时事件）——尝试非阻塞读取或短超时
    if let Ok(Some(evt)) = timeout(Duration::from_millis(50), timer_rx.recv()).await {
        panic!(
            "Unexpected timeout events after reset: {:?}",
            evt.timeout_event
        );
    }

    let _ = timer_handle.shutdown().await;
}

#[tokio::test]
async fn test_socket_layer_timer_integration() {
    // 测试 Socket 层是否正确启动了全局定时器
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();

    // 创建 Socket，这应该自动启动全局定时器任务
    let result = TransportReliableUdpSocket::<UdpTransport>::bind(addr).await;
    assert!(result.is_ok(), "Failed to bind socket");

    let (socket, mut _listener) = result.unwrap();

    // 获取实际绑定的地址
    let local_addr = socket.local_addr().await.unwrap();

    // 创建客户端连接（这会触发定时器的使用）
    let _connect_result = socket.connect(local_addr).await.unwrap();

    // 连接可能会失败（因为没有服务器监听），但重要的是定时器系统应该正常工作
    // 我们主要测试的是定时器系统没有崩溃

    // 等待一小段时间让定时器系统运行
    sleep(Duration::from_millis(100)).await;

    // 如果我们到达这里而没有崩溃，说明定时器系统基本正常
    println!("Timer system integration test completed successfully");
}

#[tokio::test]
async fn test_multiple_connections_timer_isolation() {
    let timer_handle = start_hybrid_timer_task();

    // 创建多个连接的定时器管理器
    let mut timing_managers = Vec::new();
    let mut timer_receivers = Vec::new();
    for i in 1..=5 {
        let (timing_manager, timer_rx) = TimingManager::new(i, timer_handle.clone());
        timing_managers.push(timing_manager);
        timer_receivers.push(timer_rx);
    }

    let _config = Config::default();

    // 为每个连接注册不同延迟的定时器
    for (i, timing_manager) in timing_managers.iter_mut().enumerate() {
        let delay = Duration::from_millis(100 + i as u64 * 50);
        timing_manager
            .register_path_validation_timeout(delay)
            .await
            .unwrap();
    }

    // 等待所有定时器到期
    sleep(Duration::from_millis(500)).await;

    // 检查每个连接都收到了自己的定时器事件（事件驱动）
    for timer_rx in timer_receivers.iter_mut() {
        let evt = timeout(Duration::from_millis(200), timer_rx.recv())
            .await
            .expect("Timed out waiting for timer event")
            .expect("Timer event channel closed");
        assert_eq!(evt.timeout_event, TimeoutEvent::PathValidationTimeout);
    }

    let _ = timer_handle.shutdown().await;
}
