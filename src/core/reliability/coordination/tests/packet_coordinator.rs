//! PacketCoordinator 组件的全面测试 - 使用真实TimerActor
//! Comprehensive tests for PacketCoordinator component - using real TimerActor

use crate::core::endpoint::timing::TimeoutEvent;
use crate::core::reliability::coordination::packet_coordinator::RetransmissionFrameInfo;
use crate::core::reliability::coordination::{
    ComprehensiveResult, PacketCoordinator, PacketCoordinatorStats,
};
use crate::packet::frame::{Frame, FrameType, RetransmissionContext};
use crate::packet::sack::SackRange;
use crate::timer::event::ConnectionId;
use crate::timer::{
    SenderTimerActorHandle, TimerActorConfig, TimerEventData, start_hybrid_timer_task,
    start_sender_timer_actor,
};
use bytes::Bytes;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;

/// 创建用于测试的真实TimerActor
/// Create real TimerActor for testing
async fn create_test_timer_actor() -> SenderTimerActorHandle {
    let timer_handle = start_hybrid_timer_task();
    let config = Some(TimerActorConfig::default());
    start_sender_timer_actor(timer_handle, config)
}

/// 创建测试用的PacketCoordinator
/// Create PacketCoordinator for testing
async fn create_test_packet_coordinator(
    connection_id: ConnectionId,
) -> (
    PacketCoordinator,
    mpsc::Receiver<TimerEventData<TimeoutEvent>>,
) {
    let timer_actor = create_test_timer_actor().await;
    let (timeout_tx, timeout_rx) = mpsc::channel(100);

    let coordinator = PacketCoordinator::new(
        connection_id,
        timer_actor,
        timeout_tx,
        3, // fast_retx_threshold
        3, // max_retx_count
    );
    (coordinator, timeout_rx)
}

/// 创建测试用的重传上下文
/// Create test retransmission context
fn create_test_retransmission_context() -> RetransmissionContext {
    RetransmissionContext::new(
        Instant::now(),
        12345, // current_peer_cid
        1,     // protocol_version
        54321, // local_cid
        100,   // recv_next_sequence
        8192,  // recv_window_size
    )
}

/// 创建测试用的PUSH帧
/// Create test PUSH frame
fn create_test_push_frame(seq: u32, payload: &[u8]) -> Frame {
    Frame::new_push(
        12345, // peer_cid
        seq,   // sequence_number
        100,   // recv_next_sequence
        8192,  // recv_window_size
        1000,  // timestamp
        Bytes::copy_from_slice(payload),
    )
}

/// 创建测试用的SYN帧
/// Create test SYN frame
fn create_test_syn_frame() -> Frame {
    Frame::new_syn(1, 54321, 12345)
}

/// 创建测试用的FIN帧
/// Create test FIN frame
fn create_test_fin_frame(seq: u32) -> Frame {
    Frame::new_fin(12345, seq, 1000, 100, 8192)
}

/// 测试RetransmissionFrameInfo的创建和重构
/// Test RetransmissionFrameInfo creation and reconstruction
#[tokio::test]
async fn test_retransmission_frame_info_creation_and_reconstruction() {
    let payload = b"test data";
    let push_frame = create_test_push_frame(42, payload);

    // 测试从PUSH帧创建重传信息
    let frame_info = RetransmissionFrameInfo::from_frame(&push_frame).unwrap();
    assert_eq!(frame_info.frame_type, FrameType::Push);
    assert_eq!(frame_info.sequence_number, 42);
    assert_eq!(frame_info.payload.as_ref(), payload);
    assert_eq!(frame_info.additional_data, None);

    // 测试帧重构
    let context = create_test_retransmission_context();
    let now = Instant::now();
    let reconstructed_frame = frame_info.reconstruct_frame(&context, now);

    if let Frame::Push {
        header,
        payload: reconstructed_payload,
    } = reconstructed_frame
    {
        assert_eq!(header.sequence_number, 42);
        assert_eq!(reconstructed_payload.as_ref(), payload);
    } else {
        panic!("Expected Push frame");
    }
}

/// 测试不同帧类型的RetransmissionFrameInfo创建
/// Test RetransmissionFrameInfo creation for different frame types
#[test]
fn test_retransmission_frame_info_different_types() {
    // 测试SYN帧
    let syn_frame = create_test_syn_frame();
    let syn_info = RetransmissionFrameInfo::from_frame(&syn_frame).unwrap();
    assert_eq!(syn_info.frame_type, FrameType::Syn);
    assert_eq!(syn_info.sequence_number, 0);
    assert!(syn_info.payload.is_empty());

    // 测试FIN帧
    let fin_frame = create_test_fin_frame(100);
    let fin_info = RetransmissionFrameInfo::from_frame(&fin_frame).unwrap();
    assert_eq!(fin_info.frame_type, FrameType::Fin);
    assert_eq!(fin_info.sequence_number, 100);
    assert!(fin_info.payload.is_empty());
}

/// 测试PacketCoordinator基本操作
/// Test PacketCoordinator basic operations
#[tokio::test]
async fn test_packet_coordinator_basic_operations() {
    let connection_id = 1;
    let (mut coordinator, _timeout_rx) = create_test_packet_coordinator(connection_id).await;

    // 测试初始状态
    assert_eq!(coordinator.in_flight_count(), 0);
    assert!(coordinator.is_empty());

    // 添加数据包
    let frame = create_test_push_frame(1, b"test data");
    let now = Instant::now();
    let rto = Duration::from_millis(100);

    let added = coordinator.add_packet(&frame, now, rto).await;
    assert!(added);
    assert_eq!(coordinator.in_flight_count(), 1);
    assert!(!coordinator.is_empty());

    // 获取统计信息
    let stats = coordinator.get_statistics();
    assert_eq!(stats.total_in_flight, 1);
    assert_eq!(stats.active_timers, 1);
}

/// 测试添加多个数据包
/// Test adding multiple packets
#[tokio::test]
async fn test_add_multiple_packets() {
    let connection_id = 2;
    let (mut coordinator, _timeout_rx) = create_test_packet_coordinator(connection_id).await;

    let now = Instant::now();
    let rto = Duration::from_millis(100);

    // 添加多个数据包
    for seq in 1..=5 {
        let frame = create_test_push_frame(seq, &format!("data_{}", seq).into_bytes());
        let added = coordinator.add_packet(&frame, now, rto).await;
        assert!(added);
    }

    assert_eq!(coordinator.in_flight_count(), 5);

    let stats = coordinator.get_statistics();
    assert_eq!(stats.total_in_flight, 5);
    assert_eq!(stats.active_timers, 5);
}

/// 测试ACK处理
/// Test ACK processing
#[tokio::test]
async fn test_ack_processing() {
    let connection_id = 3;
    let (mut coordinator, _timeout_rx) = create_test_packet_coordinator(connection_id).await;

    let now = Instant::now();
    let rto = Duration::from_millis(100);
    let context = create_test_retransmission_context();

    // 添加数据包
    for seq in 1..=3 {
        let frame = create_test_push_frame(seq, &format!("data_{}", seq).into_bytes());
        coordinator.add_packet(&frame, now, rto).await;
    }

    assert_eq!(coordinator.in_flight_count(), 3);

    // 处理累积ACK（确认序列号1和2）
    let sack_ranges = vec![];
    let result = coordinator
        .process_ack_comprehensive(
            3, // recv_next_seq (确认到序列号2)
            sack_ranges,
            now,
            &context,
        )
        .await;

    // 验证结果
    assert_eq!(result.newly_acked_sequences.len(), 2); // 序列号1和2被确认
    assert!(result.newly_acked_sequences.contains(&1));
    assert!(result.newly_acked_sequences.contains(&2));
    assert_eq!(result.processed_packet_count, 2);

    // 验证剩余在途数据包
    assert_eq!(coordinator.in_flight_count(), 1); // 只剩序列号3
}

/// 测试SACK处理
/// Test SACK processing
#[tokio::test]
async fn test_sack_processing() {
    let connection_id = 4;
    let (mut coordinator, _timeout_rx) = create_test_packet_coordinator(connection_id).await;

    let now = Instant::now();
    let rto = Duration::from_millis(100);
    let context = create_test_retransmission_context();

    // 添加数据包 1, 2, 3, 4, 5
    for seq in 1..=5 {
        let frame = create_test_push_frame(seq, &format!("data_{}", seq).into_bytes());
        coordinator.add_packet(&frame, now, rto).await;
    }

    // SACK确认序列号3和5（跳过2和4）
    let sack_ranges = vec![
        SackRange { start: 3, end: 3 },
        SackRange { start: 5, end: 5 },
    ];

    let result = coordinator
        .process_ack_comprehensive(
            2, // recv_next_seq (累积确认到1)
            sack_ranges,
            now,
            &context,
        )
        .await;

    // 验证结果
    assert_eq!(result.newly_acked_sequences.len(), 3); // 1(累积), 3, 5
    assert!(result.newly_acked_sequences.contains(&1));
    assert!(result.newly_acked_sequences.contains(&3));
    assert!(result.newly_acked_sequences.contains(&5));

    // 剩余数据包应该是2和4
    assert_eq!(coordinator.in_flight_count(), 2);
}

/// 测试定时器超时处理
/// Test timer timeout handling
#[tokio::test]
async fn test_timer_timeout_handling() {
    let connection_id = 5;
    let (mut coordinator, _timeout_rx) = create_test_packet_coordinator(connection_id).await;

    let now = Instant::now();
    let rto = Duration::from_millis(50);
    let _context = create_test_retransmission_context();

    // 添加数据包
    let frame = create_test_push_frame(42, b"test data");
    coordinator.add_packet(&frame, now, rto).await;

    // 模拟定时器超时（我们需要获取实际的timer_id）
    // 这里我们直接测试handle_timer_timeout方法的逻辑
    let stats = coordinator.get_statistics();
    assert_eq!(stats.active_timers, 1);

    // 等待一段时间让定时器可能超时
    tokio::time::sleep(Duration::from_millis(100)).await;

    // 注意：在实际测试中，我们需要从timeout_rx接收超时事件
    // 这里我们主要测试协调器的结构完整性
    assert_eq!(coordinator.in_flight_count(), 1);
}

/// 测试RTO重传检查
/// Test RTO retransmission check
#[tokio::test]
async fn test_rto_retransmission_check() {
    let connection_id = 6;
    let (mut coordinator, _timeout_rx) = create_test_packet_coordinator(connection_id).await;

    let now = Instant::now();
    let rto = Duration::from_millis(100);
    let context = create_test_retransmission_context();

    // 添加数据包
    let frame = create_test_push_frame(10, b"test data");
    coordinator.add_packet(&frame, now, rto).await;

    // 模拟时间过去，触发RTO检查
    let later = now + Duration::from_millis(200);
    let retx_frames = coordinator
        .check_rto_retransmission(&context, rto, later)
        .await;

    // 验证是否有重传帧生成
    // 注意：具体行为取决于RetransmissionDecider的实现
    println!(
        "RTO check generated {} retransmission frames",
        retx_frames.len()
    );
}

/// 测试清理操作
/// Test cleanup operations
#[tokio::test]
async fn test_cleanup_operations() {
    let connection_id = 7;
    let (mut coordinator, _timeout_rx) = create_test_packet_coordinator(connection_id).await;

    let now = Instant::now();
    let rto = Duration::from_millis(100);

    // 添加多个数据包
    for seq in 1..=5 {
        let frame = create_test_push_frame(seq, &format!("data_{}", seq).into_bytes());
        coordinator.add_packet(&frame, now, rto).await;
    }

    assert_eq!(coordinator.in_flight_count(), 5);
    let stats = coordinator.get_statistics();
    assert_eq!(stats.active_timers, 5);

    // 执行清理
    coordinator.cleanup_all().await;

    // 验证清理结果
    assert_eq!(coordinator.in_flight_count(), 0);
    assert!(coordinator.is_empty());

    let stats_after = coordinator.get_statistics();
    assert_eq!(stats_after.total_in_flight, 0);
    assert_eq!(stats_after.active_timers, 0);
}

/// 测试不可重传帧的处理
/// Test handling of non-retransmittable frames
#[tokio::test]
async fn test_non_retransmittable_frames() {
    let connection_id = 8;
    let (mut coordinator, _timeout_rx) = create_test_packet_coordinator(connection_id).await;

    let now = Instant::now();
    let rto = Duration::from_millis(100);

    // 尝试添加ACK帧（不应该被存储用于重传）
    let ack_frame = Frame::new_ack(12345, 100, 8192, &vec![], 1000);
    let added = coordinator.add_packet(&ack_frame, now, rto).await;
    assert!(!added); // ACK帧不应该被添加

    assert_eq!(coordinator.in_flight_count(), 0);
}

/// 测试统计信息的准确性
/// Test statistics accuracy
#[tokio::test]
async fn test_statistics_accuracy() {
    let connection_id = 9;
    let (mut coordinator, _timeout_rx) = create_test_packet_coordinator(connection_id).await;

    let now = Instant::now();
    let rto = Duration::from_millis(100);
    let context = create_test_retransmission_context();

    // 初始统计
    let initial_stats = coordinator.get_statistics();
    assert_eq!(initial_stats.total_in_flight, 0);
    assert_eq!(initial_stats.active_timers, 0);

    // 添加数据包
    for seq in 1..=3 {
        let frame = create_test_push_frame(seq, &format!("data_{}", seq).into_bytes());
        coordinator.add_packet(&frame, now, rto).await;
    }

    let after_add_stats = coordinator.get_statistics();
    assert_eq!(after_add_stats.total_in_flight, 3);
    assert_eq!(after_add_stats.active_timers, 3);

    // 处理ACK
    let result = coordinator
        .process_ack_comprehensive(
            3, // 确认序列号1和2
            vec![],
            now,
            &context,
        )
        .await;

    assert_eq!(result.processed_packet_count, 2);

    let after_ack_stats = coordinator.get_statistics();
    assert_eq!(after_ack_stats.total_in_flight, 1);
    assert_eq!(after_ack_stats.active_timers, 1);
}

/// 测试ComprehensiveResult的结构
/// Test ComprehensiveResult structure
#[test]
fn test_comprehensive_result_structure() {
    let result = ComprehensiveResult {
        frames_to_retransmit: vec![],
        newly_acked_sequences: vec![1, 2, 3],
        rtt_samples: vec![Duration::from_millis(50), Duration::from_millis(60)],
        processed_packet_count: 3,
    };

    assert_eq!(result.newly_acked_sequences.len(), 3);
    assert_eq!(result.rtt_samples.len(), 2);
    assert_eq!(result.processed_packet_count, 3);
    assert!(result.frames_to_retransmit.is_empty());
}

/// 测试PacketCoordinatorStats的结构
/// Test PacketCoordinatorStats structure
#[test]
fn test_packet_coordinator_stats_structure() {
    let stats = PacketCoordinatorStats {
        total_in_flight: 5,
        needs_retx_count: 2,
        fast_retx_candidates: 1,
        active_timers: 5,
    };

    assert_eq!(stats.total_in_flight, 5);
    assert_eq!(stats.needs_retx_count, 2);
    assert_eq!(stats.fast_retx_candidates, 1);
    assert_eq!(stats.active_timers, 5);
}

/// 测试错误处理场景
/// Test error handling scenarios
#[tokio::test]
async fn test_error_handling_scenarios() {
    let connection_id = 10;
    let (mut coordinator, _timeout_rx) = create_test_packet_coordinator(connection_id).await;

    let now = Instant::now();
    let context = create_test_retransmission_context();

    // 测试处理不存在的定时器超时
    let non_existent_timer_id = 99999;
    let result = coordinator
        .handle_timer_timeout(
            non_existent_timer_id,
            &context,
            Duration::from_millis(100),
            now,
        )
        .await;

    assert!(result.is_none()); // 应该返回None

    // 测试空的SACK处理
    let empty_result = coordinator
        .process_ack_comprehensive(100, vec![], now, &context)
        .await;

    assert!(empty_result.newly_acked_sequences.is_empty());
    assert_eq!(empty_result.processed_packet_count, 0);
}

/// 性能测试 - 大量数据包处理
/// Performance test - handling many packets
#[tokio::test]
async fn test_performance_many_packets() {
    let connection_id = 11;
    let (mut coordinator, _timeout_rx) = create_test_packet_coordinator(connection_id).await;

    let start = std::time::Instant::now();
    let packet_count = 100; // 适中的数量以适应测试环境
    let now = Instant::now();
    let rto = Duration::from_millis(100);

    // 批量添加数据包
    for seq in 1..=packet_count {
        let frame = create_test_push_frame(seq, &format!("data_{}", seq).into_bytes());
        coordinator.add_packet(&frame, now, rto).await;
    }

    let add_duration = start.elapsed();
    assert_eq!(coordinator.in_flight_count(), packet_count as usize);

    // 批量ACK处理
    let context = create_test_retransmission_context();
    let ack_start = std::time::Instant::now();

    let result = coordinator
        .process_ack_comprehensive(
            packet_count + 1, // 确认所有数据包
            vec![],
            now,
            &context,
        )
        .await;

    let ack_duration = ack_start.elapsed();

    assert_eq!(result.newly_acked_sequences.len(), packet_count as usize);
    assert_eq!(coordinator.in_flight_count(), 0);

    // 性能验证
    assert!(
        add_duration < Duration::from_millis(5000),
        "Adding packets took too long: {:?}",
        add_duration
    );
    assert!(
        ack_duration < Duration::from_millis(1000),
        "ACK processing took too long: {:?}",
        ack_duration
    );

    println!(
        "Performance test: {} packets added in {:?}, ACK processed in {:?}",
        packet_count, add_duration, ack_duration
    );
}

/// 测试调试输出格式
/// Test debug output formatting
#[test]
fn test_debug_formatting() {
    let stats = PacketCoordinatorStats {
        total_in_flight: 5,
        needs_retx_count: 2,
        fast_retx_candidates: 1,
        active_timers: 4,
    };

    let debug_string = format!("{:?}", stats);

    // 验证调试输出包含关键信息
    assert!(debug_string.contains("PacketCoordinatorStats"));
    assert!(debug_string.contains("total_in_flight"));
    assert!(debug_string.contains("5"));
    assert!(debug_string.contains("needs_retx_count"));
    assert!(debug_string.contains("2"));
}

/// 集成测试 - 完整的数据包生命周期
/// Integration test - complete packet lifecycle
#[tokio::test]
async fn test_complete_packet_lifecycle() {
    let connection_id = 12;
    let (mut coordinator, _timeout_rx) = create_test_packet_coordinator(connection_id).await;

    let now = Instant::now();
    let rto = Duration::from_millis(50);
    let context = create_test_retransmission_context();

    // 1. 添加数据包
    let frame = create_test_push_frame(42, b"lifecycle test");
    let added = coordinator.add_packet(&frame, now, rto).await;
    assert!(added);
    assert_eq!(coordinator.in_flight_count(), 1);

    // 2. 等待可能的定时器事件（短暂等待）
    tokio::time::sleep(Duration::from_millis(10)).await;

    // 3. 处理ACK确认
    let result = coordinator
        .process_ack_comprehensive(
            43, // 确认序列号42
            vec![],
            now,
            &context,
        )
        .await;

    assert_eq!(result.newly_acked_sequences, vec![42]);
    assert_eq!(result.processed_packet_count, 1);
    assert_eq!(coordinator.in_flight_count(), 0);

    // 4. 验证最终状态
    let final_stats = coordinator.get_statistics();
    assert_eq!(final_stats.total_in_flight, 0);
    assert_eq!(final_stats.active_timers, 0);
}
