//! VegasController 组件的全面测试
//! Comprehensive tests for VegasController component

use crate::core::reliability::logic::congestion::vegas_controller::{VegasController, CongestionState};
use crate::config::Config;
use std::time::Duration;
use tokio::time::Instant;

/// 测试默认构造和初始状态
#[test]
fn test_vegas_controller_default_construction() {
    let controller = VegasController::default();
    
    assert_eq!(controller.congestion_window(), 32); // default initial_cwnd_packets
    assert_eq!(controller.slow_start_threshold(), u32::MAX); // default initial_ssthresh
    assert_eq!(controller.current_state(), CongestionState::SlowStart);
    assert_eq!(controller.min_rtt(), Duration::from_millis(1000));
    assert_eq!(controller.last_rtt(), Duration::from_millis(1000));
}

/// 测试自定义配置构造
#[test]
fn test_vegas_controller_custom_config() {
    let mut config = Config::default();
    config.congestion_control.initial_cwnd_packets = 16;
    config.congestion_control.initial_ssthresh = 64;
    config.congestion_control.vegas_alpha_packets = 1;
    config.congestion_control.vegas_beta_packets = 3;
    
    let controller = VegasController::new(config);
    
    assert_eq!(controller.congestion_window(), 16);
    assert_eq!(controller.slow_start_threshold(), 64);
    assert_eq!(controller.current_state(), CongestionState::SlowStart);
}

/// 测试慢启动阶段的ACK处理
#[test]
fn test_slow_start_ack_handling() {
    let mut controller = VegasController::default();
    
    // 在慢启动阶段，每个ACK应该增加窗口
    let initial_window = controller.congestion_window();
    
    let decision = controller.handle_ack(Duration::from_millis(100));
    
    assert_eq!(decision.new_congestion_window, initial_window + 1);
    assert_eq!(decision.new_state, CongestionState::SlowStart);
    assert!(decision.significant_change);
}

/// 测试慢启动到拥塞避免的状态转换
#[test]
fn test_slow_start_to_congestion_avoidance_transition() {
    let mut config = Config::default();
    config.congestion_control.initial_cwnd_packets = 8;
    config.congestion_control.initial_ssthresh = 10;
    
    let mut controller = VegasController::new(config);
    
    // 发送ACK直到达到慢启动阈值
    while controller.congestion_window() < 10 {
        controller.handle_ack(Duration::from_millis(50));
    }
    
    // 下一个ACK应该触发状态转换
    let decision = controller.handle_ack(Duration::from_millis(50));
    
    assert_eq!(decision.new_state, CongestionState::CongestionAvoidance);
    assert!(decision.significant_change);
}

/// 测试拥塞避免阶段的行为
#[test]
fn test_congestion_avoidance_behavior() {
    let mut config = Config::default();
    config.congestion_control.initial_cwnd_packets = 20;
    config.congestion_control.initial_ssthresh = 10;
    config.congestion_control.vegas_alpha_packets = 2;
    config.congestion_control.vegas_beta_packets = 4;
    
    let mut controller = VegasController::new(config);
    
    // 设置状态为拥塞避免
    controller.handle_ack(Duration::from_millis(50)); // 更新min_rtt
    
    // 模拟低RTT（没有排队）- 应该增加窗口
    let initial_window = controller.congestion_window();
    let decision = controller.handle_ack(Duration::from_millis(45));
    
    assert_eq!(decision.new_congestion_window, initial_window + 1);
    assert_eq!(decision.new_state, CongestionState::CongestionAvoidance);
}

/// 测试拥塞避免阶段的窗口减少
#[test]
fn test_congestion_avoidance_window_decrease() {
    let mut config = Config::default();
    config.congestion_control.initial_cwnd_packets = 20;
    config.congestion_control.initial_ssthresh = 10;
    config.congestion_control.vegas_alpha_packets = 2;
    config.congestion_control.vegas_beta_packets = 4;
    config.congestion_control.min_cwnd_packets = 4;
    
    let mut controller = VegasController::new(config);
    
    // 首先建立min_rtt
    controller.handle_ack(Duration::from_millis(50));
    
    let initial_window = controller.congestion_window();
    
    // 模拟高RTT（拥塞排队）- 应该减少窗口
    let decision = controller.handle_ack(Duration::from_millis(200));
    
    assert!(decision.new_congestion_window < initial_window);
    assert!(decision.new_congestion_window >= 4); // 不应低于最小值
}

/// 测试拥塞性丢包处理
#[test]
fn test_congestive_packet_loss() {
    let mut config = Config::default();
    config.congestion_control.initial_cwnd_packets = 32;
    config.congestion_control.min_cwnd_packets = 4;
    
    let mut controller = VegasController::new(config);
    
    // 建立较高的RTT以模拟拥塞
    controller.handle_ack(Duration::from_millis(100));
    controller.handle_ack(Duration::from_millis(150)); // 50%增长，应该被认为是拥塞性的
    
    let initial_window = controller.congestion_window();
    let initial_threshold = controller.slow_start_threshold();
    
    let decision = controller.handle_packet_loss(Instant::now());
    
    // 拥塞性丢包应该显著减少窗口和阈值
    assert!(decision.new_congestion_window < initial_window);
    assert!(decision.new_slow_start_threshold < initial_threshold);
    assert_eq!(decision.new_state, CongestionState::SlowStart);
    assert!(decision.significant_change);
}

/// 测试随机丢包处理
#[test]
fn test_random_packet_loss() {
    let mut config = Config::default();
    config.congestion_control.initial_cwnd_packets = 32;
    config.congestion_control.vegas_gentle_decrease_factor = 0.8;
    config.congestion_control.min_cwnd_packets = 4;
    
    let mut controller = VegasController::new(config);
    
    // 建立稳定的RTT（无拥塞）
    controller.handle_ack(Duration::from_millis(50));
    controller.handle_ack(Duration::from_millis(50));
    
    let initial_window = controller.congestion_window();
    let initial_threshold = controller.slow_start_threshold();
    
    let decision = controller.handle_packet_loss(Instant::now());
    
    // 随机丢包应该温和减少窗口，不改变阈值
    assert!(decision.new_congestion_window < initial_window);
    assert_eq!(decision.new_slow_start_threshold, initial_threshold);
    
    // 检查减少因子
    let expected_window = ((initial_window as f32) * 0.8).max(4.0) as u32;
    assert_eq!(decision.new_congestion_window, expected_window);
}

/// 测试RTT统计更新
#[test]
fn test_rtt_statistics_update() {
    let mut controller = VegasController::default();
    
    // 第一个RTT样本
    controller.handle_ack(Duration::from_millis(100));
    assert_eq!(controller.min_rtt(), Duration::from_millis(100));
    assert_eq!(controller.last_rtt(), Duration::from_millis(100));
    
    // 更小的RTT样本
    controller.handle_ack(Duration::from_millis(80));
    assert_eq!(controller.min_rtt(), Duration::from_millis(80));
    assert_eq!(controller.last_rtt(), Duration::from_millis(80));
    
    // 更大的RTT样本（min_rtt不变）
    controller.handle_ack(Duration::from_millis(120));
    assert_eq!(controller.min_rtt(), Duration::from_millis(80));
    assert_eq!(controller.last_rtt(), Duration::from_millis(120));
}

/// 测试重置功能
#[test]
fn test_controller_reset() {
    let mut config = Config::default();
    config.congestion_control.initial_cwnd_packets = 16;
    config.congestion_control.initial_ssthresh = 32;
    
    let mut controller = VegasController::new(config.clone());
    
    // 修改状态
    controller.handle_ack(Duration::from_millis(50));
    controller.handle_packet_loss(Instant::now());
    
    // 重置
    controller.reset();
    
    // 验证状态恢复到初始值
    assert_eq!(controller.congestion_window(), 16);
    assert_eq!(controller.slow_start_threshold(), 32);
    assert_eq!(controller.current_state(), CongestionState::SlowStart);
    assert_eq!(controller.min_rtt(), Duration::from_millis(1000));
    assert_eq!(controller.last_rtt(), Duration::from_millis(1000));
}

/// 测试边界条件 - 零RTT处理
#[test]
fn test_zero_rtt_handling() {
    let mut controller = VegasController::default();
    
    // 零RTT应该被安全处理
    let decision = controller.handle_ack(Duration::ZERO);
    
    // 应该仍然可以正常决策，不会panic
    assert!(decision.new_congestion_window > 0);
}

/// 测试边界条件 - 最小窗口保护
#[test]
fn test_minimum_window_protection() {
    let mut config = Config::default();
    config.congestion_control.initial_cwnd_packets = 4;
    config.congestion_control.min_cwnd_packets = 2;
    config.congestion_control.vegas_gentle_decrease_factor = 0.1; // 极端减少
    
    let mut controller = VegasController::new(config);
    
    // 触发多次丢包
    for _ in 0..10 {
        controller.handle_packet_loss(Instant::now());
    }
    
    // 窗口不应低于最小值
    assert!(controller.congestion_window() >= 2);
}

/// 测试统计信息获取
#[test]
fn test_statistics_retrieval() {
    let mut controller = VegasController::default();
    
    controller.handle_ack(Duration::from_millis(100));
    controller.handle_packet_loss(Instant::now());
    
    let stats = controller.get_statistics();
    
    assert_eq!(stats.congestion_window, controller.congestion_window());
    assert_eq!(stats.slow_start_threshold, controller.slow_start_threshold());
    assert_eq!(stats.state, controller.current_state());
    assert_eq!(stats.min_rtt, controller.min_rtt());
    assert_eq!(stats.last_rtt, controller.last_rtt());
}

/// 性能测试 - 大量ACK处理
#[test]
fn test_performance_many_acks() {
    let mut controller = VegasController::default();
    
    let start = std::time::Instant::now();
    
    // 处理1000个ACK
    for i in 0..1000 {
        let rtt = Duration::from_millis(50 + (i % 50) as u64);
        controller.handle_ack(rtt);
    }
    
    let elapsed = start.elapsed();
    
    // 应该在合理时间内完成（比如100毫秒）
    assert!(elapsed < Duration::from_millis(100), "Performance test failed: took {:?}", elapsed);
}

/// 测试显示格式化
#[test]
fn test_display_formatting() {
    let controller = VegasController::default();
    let stats = controller.get_statistics();
    
    let display_string = format!("{}", stats);
    
    // 应该包含关键信息
    assert!(display_string.contains("Vegas"));
    assert!(display_string.contains("cwnd"));
    assert!(display_string.contains("ssthresh"));
}