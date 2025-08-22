use crate::config::Config;
use crate::core::reliability::{
    coordination::flow_control_coordinator::{
        FlowControlCoordinator, FlowControlCoordinatorConfig,
    },
    logic::congestion::vegas_controller::VegasController,
};
use std::time::Duration;
use tokio::time::Instant;

fn create_test_config() -> Config {
    Config::default()
}

fn create_test_flow_controller() -> FlowControlCoordinator<VegasController> {
    let config = create_test_config();
    let vegas_controller = VegasController::new(config);
    let flow_config = FlowControlCoordinatorConfig::default();
    FlowControlCoordinator::new(vegas_controller, flow_config)
}

#[test]
fn test_basic_flow_control_operations() {
    let mut coordinator = create_test_flow_controller();
    let now = Instant::now();
    let rtt = Duration::from_millis(50);

    // 测试初始状态
    let initial_state = coordinator.get_flow_control_state();
    assert!(initial_state.send_permit > 0);
    assert_eq!(initial_state.in_flight_count, 0);

    // 测试ACK处理
    let decision = coordinator.handle_ack(rtt, now);
    assert!(decision.send_permit > 0);
    assert!(!decision.should_delay_send);
}

#[test]
fn test_peer_receive_window_update() {
    let mut coordinator = create_test_flow_controller();

    // 更新对端接收窗口
    coordinator.update_peer_receive_window(100);
    let state = coordinator.get_flow_control_state();
    assert_eq!(state.peer_receive_window, 100);

    // 测试窗口限制对发送许可的影响
    coordinator.update_in_flight_count(20); // 使用较小的在途数据包数量
    let state = coordinator.get_flow_control_state();

    // 验证发送许可的计算逻辑：min(congestion_window - in_flight, peer_window - in_flight)
    let expected_permit = std::cmp::min(
        state
            .congestion_window
            .saturating_sub(state.in_flight_count),
        state
            .peer_receive_window
            .saturating_sub(state.in_flight_count),
    );
    assert_eq!(state.send_permit, expected_permit);
    assert!(state.send_permit > 0); // 应该有可用的发送许可
}

#[test]
fn test_packet_loss_handling() {
    let mut coordinator = create_test_flow_controller();
    let now = Instant::now();

    // 记录丢包前的状态
    let before_loss = coordinator.get_flow_control_state();

    // 处理丢包事件
    let decision = coordinator.handle_packet_loss(now);

    // 验证拥塞窗口减少
    assert!(decision.congestion_decision.significant_change);
    assert!(decision.should_delay_send);
    assert!(decision.suggested_send_interval.is_some());

    let after_loss = coordinator.get_flow_control_state();
    assert!(after_loss.congestion_window <= before_loss.congestion_window);
}

#[test]
fn test_max_send_rate_limiting() {
    let mut coordinator = create_test_flow_controller();
    let now = Instant::now();

    // 设置最大发送速率
    coordinator.set_max_send_rate(Some(10)); // 10 packets per second

    // 记录发送时间
    coordinator.record_send_time(now);

    // 立即检查是否应该延迟发送（通过公共接口）
    let _state_before = coordinator.get_flow_control_state();

    // 验证速率限制生效
    let decision = coordinator.handle_ack(Duration::from_millis(50), now);
    assert!(decision.suggested_send_interval.is_some());
}

#[test]
fn test_zero_max_rate_handling() {
    let mut coordinator = create_test_flow_controller();

    // 测试设置为0的最大速率（应该不会panic）
    coordinator.set_max_send_rate(Some(0));

    // 这应该不会导致除零错误
    let state = coordinator.get_flow_control_state();
    let _ = state.send_permit; // 确保可以正常访问状态
}

#[test]
fn test_flow_control_reset() {
    let mut coordinator = create_test_flow_controller();
    let now = Instant::now();

    // 修改状态
    coordinator.update_peer_receive_window(50);
    coordinator.update_in_flight_count(10);
    coordinator.set_max_send_rate(Some(5));
    coordinator.record_send_time(now);

    // 重置
    coordinator.reset();

    // 验证状态已重置
    let state = coordinator.get_flow_control_state();
    assert_eq!(state.peer_receive_window, u32::MAX);
    assert_eq!(state.in_flight_count, 0);

    let stats = coordinator.get_statistics();
    assert!(!stats.has_rate_limit);
}

#[test]
fn test_flow_control_statistics() {
    let mut coordinator = create_test_flow_controller();

    // 修改一些状态
    coordinator.update_peer_receive_window(200);
    coordinator.update_in_flight_count(10); // 减少在途数据包数量，确保有发送许可
    coordinator.set_max_send_rate(Some(20));

    // 获取统计信息
    let stats = coordinator.get_statistics();

    assert_eq!(stats.peer_receive_window, 200);
    assert_eq!(stats.in_flight_count, 10);
    assert_eq!(stats.max_send_rate, Some(20));
    assert!(stats.has_rate_limit);
    assert!(stats.send_permit > 0); // 现在应该有发送许可（32-10=22 或 200-10=190 的最小值）

    // 测试统计信息的显示格式
    let display_str = format!("{}", stats);
    assert!(display_str.contains("FlowControl"));
    assert!(display_str.contains("permit:"));
    assert!(display_str.contains("enabled"));
}
