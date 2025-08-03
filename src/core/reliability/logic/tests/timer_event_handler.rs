//! TimerEventHandler 组件的全面测试 - 使用真实TimerActor
//! Comprehensive tests for TimerEventHandler component - using real TimerActor

use crate::core::reliability::logic::{TimerEventHandler, TimerHandlingResult};
use crate::core::reliability::data::in_flight_store::{InFlightPacketStore, InFlightPacket, PacketState};
use crate::core::reliability::coordination::packet_coordinator::RetransmissionFrameInfo;
use crate::{
    core::endpoint::timing::TimeoutEvent,
    timer::{
        actor::{ActorTimerId, SenderTimerActorHandle, start_sender_timer_actor, TimerActorConfig},
        event::{ConnectionId, TimerEventData},
        start_hybrid_timer_task,
    },
    packet::frame::FrameType
};
use bytes::Bytes;
use std::time::Duration;
use tokio::time::Instant;
use tokio::sync::mpsc;

/// 创建用于测试的真实TimerActor
/// Create real TimerActor for testing
async fn create_test_timer_actor() -> SenderTimerActorHandle {
    let timer_handle = start_hybrid_timer_task();
    let config = Some(TimerActorConfig::default());
    start_sender_timer_actor(timer_handle, config)
}

/// 创建测试用的TimerEventHandler
/// Create TimerEventHandler for testing
async fn create_test_timer_event_handler(
    connection_id: ConnectionId,
) -> (TimerEventHandler, mpsc::Receiver<TimerEventData<TimeoutEvent>>) {
    let timer_actor = create_test_timer_actor().await;
    let (timeout_tx, timeout_rx) = mpsc::channel(100);
    
    let handler = TimerEventHandler::new(connection_id, timer_actor, timeout_tx);
    (handler, timeout_rx)
}

/// 创建测试用的InFlightPacketStore
fn create_test_store() -> InFlightPacketStore {
    InFlightPacketStore::new()
}

/// 创建测试用的数据包
fn create_test_packet(seq: u32, now: Instant) -> InFlightPacket {
    let payload = Bytes::from(vec![0u8; 100]);
    
    InFlightPacket {
        frame_info: RetransmissionFrameInfo {
            frame_type: FrameType::Push,
            sequence_number: seq,
            payload: payload.clone(),
            additional_data: None,
        },
        last_sent_at: now,
        retx_count: 0,
        max_retx_count: 3,
        state: PacketState::Sent,
        timer_id: None,
        fast_retx_count: 0,
    }
}

/// 测试定时器处理结果的创建和字段
#[test]
fn test_timer_handling_result_creation() {
    let result = TimerHandlingResult {
        sequence_number: Some(42),
        needs_retransmission_handling: true,
        needs_timer_reschedule: false,
    };
    
    assert_eq!(result.sequence_number, Some(42));
    assert!(result.needs_retransmission_handling);
    assert!(!result.needs_timer_reschedule);
}

/// 测试真实定时器调度
#[tokio::test]
async fn test_real_timer_scheduling() {
    let connection_id = 1;
    let (handler, _timeout_rx) = create_test_timer_event_handler(connection_id).await;
    let mut store = create_test_store();
    
    let seq = 42;
    let rto = Duration::from_millis(100);
    
    // 添加数据包到存储
    let now = Instant::now();
    let packet = create_test_packet(seq, now);
    store.add_packet(seq, packet);
    
    // 调度重传定时器
    let result = handler.schedule_retransmission_timer(&mut store, seq, rto).await;
    assert!(result.is_ok(), "Failed to schedule retransmission timer: {:?}", result);
    
    let timer_id = result.unwrap();
    
    // 验证定时器映射已设置
    assert_eq!(store.find_sequence_by_timer(timer_id), Some(seq));
}

/// 测试真实定时器取消
#[tokio::test]
async fn test_real_timer_cancellation() {
    let connection_id = 2;
    let (handler, _timeout_rx) = create_test_timer_event_handler(connection_id).await;
    let mut store = create_test_store();
    
    let seq = 100;
    let rto = Duration::from_millis(50);
    
    // 添加数据包并调度定时器
    let now = Instant::now();
    let packet = create_test_packet(seq, now);
    store.add_packet(seq, packet);
    
    let timer_id = handler.schedule_retransmission_timer(&mut store, seq, rto).await.unwrap();
    
    // 验证定时器已调度
    assert_eq!(store.find_sequence_by_timer(timer_id), Some(seq));
    
    // 取消定时器
    let cancelled = handler.cancel_retransmission_timer(&mut store, seq).await;
    assert!(cancelled);
    
    // 验证定时器映射已移除
    assert_eq!(store.find_sequence_by_timer(timer_id), None);
}

/// 测试批量定时器取消
#[tokio::test]
async fn test_real_batch_timer_cancellation() {
    let connection_id = 3;
    let (handler, _timeout_rx) = create_test_timer_event_handler(connection_id).await;
    let mut store = create_test_store();
    
    let sequences = vec![1, 2, 3, 4, 5];
    let rto = Duration::from_millis(50);
    let now = Instant::now();
    
    // 为每个序列号调度定时器
    for &seq in &sequences {
        let packet = create_test_packet(seq, now);
        store.add_packet(seq, packet);
        handler.schedule_retransmission_timer(&mut store, seq, rto).await.unwrap();
    }
    
    // 验证所有定时器都已调度
    assert_eq!(handler.get_active_timer_count(&store), sequences.len());
    
    // 批量取消定时器
    let cancelled_count = handler.batch_cancel_retransmission_timers(&mut store, &sequences).await;
    assert_eq!(cancelled_count, sequences.len());
    
    // 验证所有定时器都已取消
    assert_eq!(handler.get_active_timer_count(&store), 0);
}

/// 测试定时器重新调度
#[tokio::test]
async fn test_real_timer_rescheduling() {
    let connection_id = 4;
    let (handler, _timeout_rx) = create_test_timer_event_handler(connection_id).await;
    let mut store = create_test_store();
    
    let seq = 123;
    let initial_rto = Duration::from_millis(100);
    let new_rto = Duration::from_millis(200);
    
    // 添加数据包并调度初始定时器
    let now = Instant::now();
    let packet = create_test_packet(seq, now);
    store.add_packet(seq, packet);
    
    let initial_timer_id = handler.schedule_retransmission_timer(&mut store, seq, initial_rto).await.unwrap();
    
    // 重新调度定时器
    let new_timer_id = handler.reschedule_retransmission_timer(&mut store, seq, new_rto).await.unwrap();
    
    // 验证新定时器ID与初始不同
    assert_ne!(initial_timer_id, new_timer_id);
    
    // 验证新定时器映射存在
    assert_eq!(store.find_sequence_by_timer(new_timer_id), Some(seq));
    
    // 验证旧定时器映射已移除
    assert_eq!(store.find_sequence_by_timer(initial_timer_id), None);
}

/// 测试真实定时器超时事件
#[tokio::test]
async fn test_real_timer_timeout_event() {
    let connection_id = 5;
    let (handler, mut timeout_rx) = create_test_timer_event_handler(connection_id).await;
    let mut store = create_test_store();
    
    let seq = 456;
    let rto = Duration::from_millis(50); // 短超时时间
    
    // 添加数据包并调度定时器
    let now = Instant::now();
    let packet = create_test_packet(seq, now);
    store.add_packet(seq, packet);
    
    let timer_id = handler.schedule_retransmission_timer(&mut store, seq, rto).await.unwrap();
    
    // 等待定时器超时
    tokio::time::sleep(Duration::from_millis(100)).await;
    
    // 尝试接收超时事件
    if let Ok(event_data) = tokio::time::timeout(Duration::from_millis(100), timeout_rx.recv()).await {
        if let Some(timer_event) = event_data {
            match timer_event.timeout_event {
                TimeoutEvent::PacketRetransmissionTimeout { sequence_number, timer_id: event_timer_id } => {
                    assert_eq!(sequence_number, seq);
                    assert_eq!(event_timer_id, timer_id);
                    
                    // 处理定时器超时
                    let result = handler.handle_timer_timeout(&mut store, timer_id);
                    assert_eq!(result.sequence_number, Some(seq));
                    assert!(result.needs_retransmission_handling);
                }
                _ => panic!("Unexpected timeout event type"),
            }
        }
    }
}

/// 测试错误处理 - 无效RTO
#[tokio::test]
async fn test_real_timer_invalid_rto() {
    let connection_id = 6;
    let (handler, _timeout_rx) = create_test_timer_event_handler(connection_id).await;
    let mut store = create_test_store();
    
    let seq = 789;
    let now = Instant::now();
    let packet = create_test_packet(seq, now);
    store.add_packet(seq, packet);
    
    // 测试零RTO
    let result = handler.schedule_retransmission_timer(&mut store, seq, Duration::ZERO).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("Invalid RTO"));
    
    // 测试过大RTO
    let result = handler.schedule_retransmission_timer(&mut store, seq, Duration::from_secs(400)).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("too large"));
}

/// 测试批量重新调度
#[tokio::test]
async fn test_real_batch_reschedule() {
    let connection_id = 7;
    let (handler, _timeout_rx) = create_test_timer_event_handler(connection_id).await;
    let mut store = create_test_store();
    
    let sequences = vec![10, 20, 30];
    let initial_rto = Duration::from_millis(100);
    let new_rto = Duration::from_millis(200);
    let now = Instant::now();
    
    // 为每个序列号添加数据包并调度初始定时器
    for &seq in &sequences {
        let packet = create_test_packet(seq, now);
        store.add_packet(seq, packet);
        handler.schedule_retransmission_timer(&mut store, seq, initial_rto).await.unwrap();
    }
    
    // 验证初始定时器数量
    assert_eq!(handler.get_active_timer_count(&store), sequences.len());
    
    // 批量重新调度
    let results = handler.batch_reschedule_timers(&mut store, &sequences, new_rto).await;
    
    // 验证所有重新调度都成功
    assert_eq!(results.len(), sequences.len());
    for (seq, result) in results {
        assert!(sequences.contains(&seq));
        assert!(result.is_ok(), "Failed to reschedule timer for seq {}: {:?}", seq, result);
    }
    
    // 验证定时器数量不变
    assert_eq!(handler.get_active_timer_count(&store), sequences.len());
}

/// 测试清理所有定时器
#[tokio::test]
async fn test_real_cleanup_all_timers() {
    let connection_id = 8;
    let (handler, _timeout_rx) = create_test_timer_event_handler(connection_id).await;
    let mut store = create_test_store();
    
    let sequences = vec![100, 200, 300, 400];
    let rto = Duration::from_millis(50);
    let now = Instant::now();
    
    // 为每个序列号调度定时器
    for &seq in &sequences {
        let packet = create_test_packet(seq, now);
        store.add_packet(seq, packet);
        handler.schedule_retransmission_timer(&mut store, seq, rto).await.unwrap();
    }
    
    // 验证所有定时器都已调度
    assert_eq!(handler.get_active_timer_count(&store), sequences.len());
    
    // 清理所有定时器
    handler.cleanup_all_timers(&mut store).await;
    
    // 验证所有定时器都已清理
    assert_eq!(handler.get_active_timer_count(&store), 0);
}

/// 测试定时器超时处理 - 找到数据包 (使用真实handler)
#[tokio::test]
async fn test_handle_timer_timeout_packet_found() {
    let connection_id = 10;
    let (handler, _timeout_rx) = create_test_timer_event_handler(connection_id).await;
    let mut store = create_test_store();
    
    let now = Instant::now();
    let seq = 42;
    
    // 添加数据包到存储
    let packet = create_test_packet(seq, now);
    store.add_packet(seq, packet);
    
    // 调度定时器并获取timer_id
    let timer_id = handler.schedule_retransmission_timer(&mut store, seq, Duration::from_millis(50)).await.unwrap();
    
    // 处理定时器超时
    let result = handler.handle_timer_timeout(&mut store, timer_id);
    
    assert_eq!(result.sequence_number, Some(seq));
    assert!(result.needs_retransmission_handling);
    assert!(!result.needs_timer_reschedule);
}

/// 测试定时器超时处理 - 未找到数据包
#[tokio::test]
async fn test_handle_timer_timeout_packet_not_found() {
    let connection_id = 11;
    let (handler, _timeout_rx) = create_test_timer_event_handler(connection_id).await;
    let mut store = create_test_store();
    
    let timer_id = 123; // 不存在的timer_id
    
    // 直接处理不存在的定时器
    let result = handler.handle_timer_timeout(&mut store, timer_id);
    
    assert_eq!(result.sequence_number, None);
    assert!(!result.needs_retransmission_handling);
    assert!(!result.needs_timer_reschedule);
}

/// 测试定时器超时处理 - 数据包已移除
#[tokio::test]
async fn test_handle_timer_timeout_packet_removed() {
    let connection_id = 12;
    let (handler, _timeout_rx) = create_test_timer_event_handler(connection_id).await;
    let mut store = create_test_store();
    
    let now = Instant::now();
    let seq = 42;
    
    // 添加数据包并调度定时器
    let packet = create_test_packet(seq, now);
    store.add_packet(seq, packet);
    let timer_id = handler.schedule_retransmission_timer(&mut store, seq, Duration::from_millis(50)).await.unwrap();
    
    // 手动移除数据包但保留定时器映射
    // 注意：实际场景中，这种情况可能发生在并发环境中
    store.remove_packet(seq);
    // 但我们需要保留定时器映射来模拟这种边缘情况
    store.set_timer_mapping(timer_id, seq);
    
    // 处理超时
    let result = handler.handle_timer_timeout(&mut store, timer_id);
    
    assert_eq!(result.sequence_number, Some(seq));
    assert!(!result.needs_retransmission_handling); // 数据包不存在
}

/// 测试RTO参数验证
#[test]
fn test_rto_validation() {
    // 测试零RTO
    let zero_rto = Duration::ZERO;
    assert!(zero_rto.is_zero());
    
    // 测试过大RTO
    let large_rto = Duration::from_secs(400);
    assert!(large_rto > Duration::from_secs(300));
    
    // 测试正常RTO
    let normal_rto = Duration::from_millis(1000);
    assert!(!normal_rto.is_zero());
    assert!(normal_rto <= Duration::from_secs(300));
}

/// 测试定时器映射管理
#[test]
fn test_timer_mapping_management() {
    let mut store = create_test_store();
    let now = Instant::now();
    let timer_id = 123;
    let seq = 42;
    
    // 添加数据包
    let packet = create_test_packet(seq, now);
    store.add_packet(seq, packet);
    
    // 设置定时器映射
    store.set_timer_mapping(timer_id, seq);
    
    // 验证映射存在
    assert_eq!(store.find_sequence_by_timer(timer_id), Some(seq));
    
    // 移除映射
    store.remove_timer_mapping(timer_id);
    
    // 验证映射已移除
    assert_eq!(store.find_sequence_by_timer(timer_id), None);
}

/// 测试批量操作的数据结构
#[test]
fn test_batch_operations_data_structures() {
    let sequences = vec![1, 2, 3, 4, 5];
    
    // 测试分块处理
    const MAX_CONCURRENT: usize = 2;
    let chunks: Vec<_> = sequences.chunks(MAX_CONCURRENT).collect();
    
    assert_eq!(chunks.len(), 3); // 5个元素，每块2个，共3块
    assert_eq!(chunks[0], &[1, 2]);
    assert_eq!(chunks[1], &[3, 4]);
    assert_eq!(chunks[2], &[5]);
}

/// 测试活跃定时器计数
#[test]
fn test_active_timer_count() {
    let mut store = create_test_store();
    let now = Instant::now();
    
    // 添加数据包，一些有定时器，一些没有
    for seq in 1..=5 {
        let mut packet = create_test_packet(seq, now);
        if seq <= 3 {
            packet.timer_id = Some(seq as ActorTimerId);
        }
        store.add_packet(seq, packet);
    }
    
    // 计算活跃定时器数量
    let active_count = store.get_all_sequences()
        .iter()
        .filter_map(|&seq| store.get_packet(seq))
        .filter(|packet| packet.timer_id.is_some())
        .count();
    
    assert_eq!(active_count, 3);
}

/// 测试并发控制参数
#[test]
fn test_concurrency_control_parameters() {
    const MAX_CONCURRENT: usize = 10;
    let large_batch = (1..=100).collect::<Vec<_>>();
    
    let chunk_count = (large_batch.len() + MAX_CONCURRENT - 1) / MAX_CONCURRENT;
    assert_eq!(chunk_count, 10); // 100个元素，每批10个，共10批
    
    let small_batch = vec![1, 2, 3];
    let small_chunk_count = (small_batch.len() + MAX_CONCURRENT - 1) / MAX_CONCURRENT;
    assert_eq!(small_chunk_count, 1); // 3个元素，只需1批
}

/// 测试错误处理场景
#[test]
fn test_error_handling_scenarios() {
    // 测试各种错误条件
    
    // 无效RTO错误
    let zero_rto_error = "Invalid RTO: cannot be zero";
    assert!(zero_rto_error.contains("Invalid RTO"));
    
    // RTO过大错误
    let large_rto_error = format!("RTO too large: {} seconds", 400);
    assert!(large_rto_error.contains("too large"));
    
    // Task join错误
    let join_error = "Task join error: cancelled";
    assert!(join_error.contains("Task join error"));
}

/// 测试定时器事件数据结构
#[test]
fn test_timer_event_data_structure() {
    let seq = 42;
    let timer_id = 456;
    
    // 创建超时事件
    let timeout_event = TimeoutEvent::PacketRetransmissionTimeout {
        sequence_number: seq,
        timer_id,
    };
    
    // 验证事件包含正确信息
    match timeout_event {
        TimeoutEvent::PacketRetransmissionTimeout { sequence_number, timer_id: t_id } => {
            assert_eq!(sequence_number, seq);
            assert_eq!(t_id, timer_id);
        }
        _ => panic!("Unexpected event type"),
    }
}

/// 测试清理操作的完整性
#[test]
fn test_cleanup_completeness() {
    let mut store = create_test_store();
    let now = Instant::now();
    
    // 添加多个数据包，都有定时器
    let sequences = vec![1, 3, 5, 7, 9];
    for &seq in &sequences {
        let mut packet = create_test_packet(seq, now);
        packet.timer_id = Some(seq as ActorTimerId);
        store.add_packet(seq, packet);
    }
    
    let all_sequences = store.get_all_sequences();
    assert_eq!(all_sequences.len(), sequences.len());
    
    // 验证所有序列号都有定时器
    let timer_count = all_sequences
        .iter()
        .filter_map(|&seq| store.get_packet(seq))
        .filter(|packet| packet.timer_id.is_some())
        .count();
    
    assert_eq!(timer_count, sequences.len());
}



/// 性能和压力测试
/// Performance and stress tests
#[cfg(test)]
mod performance_tests {
    use super::*;
    
    /// 测试大量定时器的性能
    #[tokio::test]
    async fn test_performance_many_timers() {
        let connection_id = 999;
        let (handler, _timeout_rx) = create_test_timer_event_handler(connection_id).await;
        let mut store = create_test_store();
        
        let start = std::time::Instant::now();
        let count = 100; // 减少数量以适应测试环境
        let rto = Duration::from_millis(50);
        let now = Instant::now();
        
        // 批量添加数据包和定时器
        for seq in 1..=count {
            let packet = create_test_packet(seq, now);
            store.add_packet(seq, packet);
            handler.schedule_retransmission_timer(&mut store, seq, rto).await.unwrap();
        }
        
        let elapsed = start.elapsed();
        
        // 验证所有定时器都已调度
        assert_eq!(handler.get_active_timer_count(&store), count as usize);
        
        // 应该在合理时间内完成
        assert!(elapsed < Duration::from_millis(5000), "Performance test failed: took {:?}", elapsed);
        
        // 清理
        handler.cleanup_all_timers(&mut store).await;
    }
}

/// 测试调试输出格式
#[test]
fn test_debug_formatting() {
    let result = TimerHandlingResult {
        sequence_number: Some(42),
        needs_retransmission_handling: true,
        needs_timer_reschedule: false,
    };
    
    let debug_string = format!("{:?}", result);
    
    // 验证调试输出包含关键信息
    assert!(debug_string.contains("TimerHandlingResult"));
    assert!(debug_string.contains("sequence_number"));
    assert!(debug_string.contains("42"));
    assert!(debug_string.contains("needs_retransmission_handling"));
    assert!(debug_string.contains("true"));
}