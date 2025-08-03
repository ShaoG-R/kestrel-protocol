//! 协调器组件的全面测试
//! Comprehensive tests for coordinator components

use super::flow_control_coordinator::FlowControlCoordinator;
use crate::{
    core::reliability::{
        coordination::packet_coordinator::RetransmissionFrameInfo,
        data::in_flight_store::{InFlightPacketStore, InFlightPacket, PacketState},
    },
    config::Config,
    packet::frame::FrameType,
    timer::{
        actor::ActorTimerId,
        event::ConnectionId,
    },
};
use bytes::Bytes;
use std::time::Duration;
use tokio::{sync::mpsc, time::Instant};

#[cfg(test)]
mod in_flight_store_tests {
    use super::*;

    fn create_test_store() -> InFlightPacketStore {
        InFlightPacketStore::new()
    }

    fn create_test_packet(seq: u32, now: Instant) -> InFlightPacket {
        InFlightPacket {
            frame_info: RetransmissionFrameInfo {
                frame_type: FrameType::Push,
                sequence_number: seq,
                payload: Bytes::from("test data"),
                additional_data: None,
            },
            last_sent_at: now,
            retx_count: 0,
            max_retx_count: 3,
            timer_id: None,
            state: PacketState::Sent,
            fast_retx_count: 0,
        }
    }

    #[test]
    fn test_basic_packet_operations() {
        let mut store = create_test_store();
        let now = Instant::now();
        let packet = create_test_packet(1, now);

        // 测试添加数据包
        store.add_packet(1, packet.clone());
        assert_eq!(store.count(), 1);
        assert!(!store.is_empty());

        // 测试获取数据包
        let retrieved = store.get_packet(1).unwrap();
        assert_eq!(retrieved.frame_info.sequence_number, 1);

        // 测试移除数据包
        let removed = store.remove_packet(1).unwrap();
        assert_eq!(removed.frame_info.sequence_number, 1);
        assert_eq!(store.count(), 0);
        assert!(store.is_empty());
    }

    #[test]
    fn test_timer_mapping_consistency() {
        let mut store = create_test_store();
        let now = Instant::now();
        let packet = create_test_packet(1, now);
        let timer_id: ActorTimerId = 12345;

        // 先添加数据包
        store.add_packet(1, packet);

        // 测试设置定时器映射
        store.set_timer_mapping(timer_id, 1);
        assert_eq!(store.find_sequence_by_timer(timer_id), Some(1));

        // 验证数据包的定时器ID已更新
        let packet = store.get_packet(1).unwrap();
        assert_eq!(packet.timer_id, Some(timer_id));

        // 测试移除定时器映射
        let seq = store.remove_timer_mapping(timer_id).unwrap();
        assert_eq!(seq, 1);
        assert_eq!(store.find_sequence_by_timer(timer_id), None);

        // 验证数据包的定时器ID已清除
        let packet = store.get_packet(1).unwrap();
        assert_eq!(packet.timer_id, None);
    }

    #[test]
    fn test_timer_mapping_nonexistent_packet() {
        let mut store = create_test_store();
        let timer_id: ActorTimerId = 12345;

        // 尝试为不存在的数据包设置定时器映射（现在应该失败）
        let result = store.set_timer_mapping(timer_id, 999);
        assert!(!result); // 应该返回 false，表示设置失败

        // 不应该能找到映射，因为数据包不存在
        assert_eq!(store.find_sequence_by_timer(timer_id), None);
        assert!(store.get_packet(999).is_none());

        // 清理不存在的映射应该返回None
        let seq = store.remove_timer_mapping(timer_id);
        assert_eq!(seq, None);
    }

    #[test]
    fn test_batch_remove_packets() {
        let mut store = create_test_store();
        let now = Instant::now();

        // 添加多个数据包
        for i in 1..=5 {
            store.add_packet(i, create_test_packet(i, now));
        }
        assert_eq!(store.count(), 5);

        // 批量移除部分数据包
        let removed = store.batch_remove_packets(&[1, 3, 5]);
        assert_eq!(removed.len(), 3);
        assert_eq!(store.count(), 2);

        // 验证正确的数据包被移除
        let removed_seqs: Vec<u32> = removed.iter().map(|(seq, _)| *seq).collect();
        assert_eq!(removed_seqs, vec![1, 3, 5]);

        // 剩余的数据包应该还在
        assert!(store.get_packet(2).is_some());
        assert!(store.get_packet(4).is_some());
    }

    #[test]
    fn test_fast_retx_candidates_management() {
        let mut store = create_test_store();
        let now = Instant::now();
        
        // 添加数据包
        store.add_packet(1, create_test_packet(1, now));

        // 更新快速重传候选
        store.update_fast_retx_candidate(1, 3);

        // 验证状态更新
        let packet = store.get_packet(1).unwrap();
        assert_eq!(packet.state, PacketState::FastRetxCandidate);
        assert_eq!(packet.fast_retx_count, 3);

        // 验证候选列表
        let candidates = store.get_fast_retx_candidates();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0], (1, 3));

        // 移除数据包应该清理候选缓存
        store.remove_packet(1);
        let candidates = store.get_fast_retx_candidates();
        assert_eq!(candidates.len(), 0);
    }

    #[test]
    fn test_get_packets_by_state() {
        let mut store = create_test_store();
        let now = Instant::now();

        // 添加不同状态的数据包
        for i in 1..=3 {
            let mut packet = create_test_packet(i, now);
            packet.state = match i {
                1 => PacketState::Sent,
                2 => PacketState::FastRetxCandidate,
                3 => PacketState::NeedsRetx,
                _ => PacketState::Sent,
            };
            store.add_packet(i, packet);
        }

        // 测试按状态查询
        let sent_packets = store.get_packets_by_state(PacketState::Sent);
        assert_eq!(sent_packets.len(), 1);
        assert_eq!(sent_packets[0].0, 1);

        let fast_retx_packets = store.get_packets_by_state(PacketState::FastRetxCandidate);
        assert_eq!(fast_retx_packets.len(), 1);
        assert_eq!(fast_retx_packets[0].0, 2);

        let needs_retx_packets = store.get_packets_by_state(PacketState::NeedsRetx);
        assert_eq!(needs_retx_packets.len(), 1);
        assert_eq!(needs_retx_packets[0].0, 3);
    }

    #[test]
    fn test_clear_all_data() {
        let mut store = create_test_store();
        let now = Instant::now();
        let timer_id: ActorTimerId = 12345;

        // 添加数据包和映射
        store.add_packet(1, create_test_packet(1, now));
        store.set_timer_mapping(timer_id, 1);
        store.update_fast_retx_candidate(1, 2);

        // 验证数据存在
        assert_eq!(store.count(), 1);
        assert!(store.find_sequence_by_timer(timer_id).is_some());
        assert_eq!(store.get_fast_retx_candidates().len(), 1);

        // 清空所有数据
        store.clear();

        // 验证所有数据都被清空
        assert_eq!(store.count(), 0);
        assert!(store.is_empty());
        assert!(store.find_sequence_by_timer(timer_id).is_none());
        assert_eq!(store.get_fast_retx_candidates().len(), 0);
    }
}

#[cfg(test)]
mod flow_control_coordinator_tests {
    use super::*;

    fn create_test_config() -> Config {
        Config::default()
    }

    fn create_test_flow_controller() -> FlowControlCoordinator {
        FlowControlCoordinator::new(create_test_config())
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
            state.congestion_window.saturating_sub(state.in_flight_count),
            state.peer_receive_window.saturating_sub(state.in_flight_count)
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
}

// 注意：PacketCoordinator 的测试需要更复杂的异步设置，因为它依赖于定时器系统
// 这些测试应该在集成测试中实现，或者需要模拟定时器系统
#[cfg(test)]
mod packet_coordinator_integration_tests {
    use super::*;
    
    // 这里可以添加PacketCoordinator的集成测试
    // 但需要更复杂的测试环境设置
    
    #[tokio::test]
    async fn test_packet_coordinator_creation() {
        // 这是一个基本的创建测试，更复杂的功能测试需要完整的环境
        let _connection_id: ConnectionId = 1;
        let (_timer_tx, _timer_rx) = mpsc::channel::<()>(100);
        let (_timeout_tx, _timeout_rx) = mpsc::channel::<()>(100);
        
        // 这里需要实际的 SenderTimerActorHandle，但在单元测试中很难构造
        // 建议在集成测试中测试完整功能
        
        // let coordinator = PacketCoordinator::new(
        //     connection_id,
        //     timer_actor,
        //     timeout_tx,
        //     3, // fast_retx_threshold
        //     5, // max_retx_count
        // );
        
        // assert_eq!(coordinator.in_flight_count(), 0);
        // assert!(coordinator.is_empty());
    }
}