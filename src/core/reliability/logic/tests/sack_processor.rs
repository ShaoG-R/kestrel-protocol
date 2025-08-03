//! SackProcessor 组件的全面测试
//! Comprehensive tests for SackProcessor component

use crate::core::reliability::logic::{SackProcessor, SackProcessResult};
use crate::core::reliability::data::in_flight_store::{InFlightPacketStore, InFlightPacket, PacketState};
use crate::core::reliability::coordination::packet_coordinator::RetransmissionFrameInfo;
use crate::packet::{sack::SackRange, frame::FrameType};
use bytes::Bytes;
use std::time::Duration;
use tokio::time::Instant;

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

/// 测试默认构造
#[test]
fn test_sack_processor_construction() {
    let processor = SackProcessor::new(3);
    
    // 验证可以成功创建（fast_retx_threshold是私有字段，无法直接访问）
    // 通过行为测试验证阈值设置正确
    let _ = processor; // 使用processor避免未使用警告
}

/// 测试空SACK范围处理
#[test]
fn test_empty_sack_ranges() {
    let processor = SackProcessor::new(3);
    let mut store = create_test_store();
    let now = Instant::now();
    
    let result = processor.process_sack(
        &mut store,
        5,
        &[],
        now,
    );
    
    assert!(result.cumulative_acked.is_empty());
    assert!(result.sack_acked.is_empty());
    assert!(result.rtt_samples.is_empty());
    assert!(result.fast_retx_candidates.is_empty());
}

/// 测试累积ACK处理
#[test]
fn test_cumulative_ack_processing() {
    let processor = SackProcessor::new(3);
    let mut store = create_test_store();
    let now = Instant::now();
    
    // 添加一些数据包
    for seq in 1..=5 {
        let packet = create_test_packet(seq, now - Duration::from_millis(100));
        store.add_packet(seq, packet);
    }
    
    // 累积确认到序列号3
    let result = processor.process_sack(
        &mut store,
        4, // recv_next_seq = 4，意味着1,2,3被累积确认
        &[],
        now,
    );
    
    assert_eq!(result.cumulative_acked, vec![1, 2, 3]);
    assert!(result.sack_acked.is_empty());
    assert_eq!(result.rtt_samples.len(), 3);
    
    // 检查RTT样本是否合理
    for rtt_sample in &result.rtt_samples {
        assert!(*rtt_sample >= Duration::from_millis(100));
        assert!(*rtt_sample <= Duration::from_millis(200));
    }
}

/// 测试SACK范围处理
#[test]
fn test_sack_range_processing() {
    let processor = SackProcessor::new(3);
    let mut store = create_test_store();
    let now = Instant::now();
    
    // 添加数据包1-10
    for seq in 1..=10 {
        let packet = create_test_packet(seq, now - Duration::from_millis(50));
        store.add_packet(seq, packet);
    }
    
    // SACK范围：5-7，9-10
    let sack_ranges = vec![
        SackRange { start: 5, end: 7 },
        SackRange { start: 9, end: 10 },
    ];
    
    let result = processor.process_sack(
        &mut store,
        1, // 没有累积ACK
        &sack_ranges,
        now,
    );
    
    assert!(result.cumulative_acked.is_empty());
    assert_eq!(result.sack_acked, vec![5, 6, 7, 9, 10]);
    assert_eq!(result.rtt_samples.len(), 5);
}

/// 测试无效SACK范围处理
#[test]
fn test_invalid_sack_ranges() {
    let processor = SackProcessor::new(3);
    let mut store = create_test_store();
    let now = Instant::now();
    
    // 添加数据包
    for seq in 1..=10 {
        let packet = create_test_packet(seq, now - Duration::from_millis(50));
        store.add_packet(seq, packet);
    }
    
    // 无效SACK范围：start > end
    let sack_ranges = vec![
        SackRange { start: 7, end: 5 }, // 无效
        SackRange { start: 2, end: 4 }, // 有效
    ];
    
    let result = processor.process_sack(
        &mut store,
        1,
        &sack_ranges,
        now,
    );
    
    // 只应该处理有效范围
    assert_eq!(result.sack_acked, vec![2, 3, 4]);
}

/// 测试过大SACK范围处理
#[test]
fn test_oversized_sack_ranges() {
    let processor = SackProcessor::new(3);
    let mut store = create_test_store();
    let now = Instant::now();
    
    // 添加少量数据包
    for seq in 1..=5 {
        let packet = create_test_packet(seq, now - Duration::from_millis(50));
        store.add_packet(seq, packet);
    }
    
    // 过大的SACK范围（超过1000个序列号）
    let sack_ranges = vec![
        SackRange { start: 1, end: 1001 }, // 过大，应该被跳过
        SackRange { start: 2, end: 3 },    // 正常大小
    ];
    
    let result = processor.process_sack(
        &mut store,
        1,
        &sack_ranges,
        now,
    );
    
    // 只应该处理正常大小的范围
    assert_eq!(result.sack_acked, vec![2, 3]);
}

/// 测试异常RTT样本过滤
#[test]
fn test_abnormal_rtt_filtering() {
    let processor = SackProcessor::new(3);
    let mut store = create_test_store();
    let now = Instant::now();
    
    // 添加数据包，其中一些有异常时间戳
    let very_old_time = now - Duration::from_secs(15); // 15秒前，应该被过滤
    let normal_time = now - Duration::from_millis(100);
    
    store.add_packet(1, create_test_packet(1, very_old_time));
    store.add_packet(2, create_test_packet(2, normal_time));
    
    let result = processor.process_sack(
        &mut store,
        3, // 累积确认1和2
        &[],
        now,
    );
    
    assert_eq!(result.cumulative_acked, vec![1, 2]);
    // 应该只有一个合理的RTT样本（序列号2的）
    assert_eq!(result.rtt_samples.len(), 1);
    assert!(result.rtt_samples[0] < Duration::from_secs(1));
}

/// 测试快速重传检测 - 基本场景
#[test]
fn test_fast_retx_detection_basic() {
    let processor = SackProcessor::new(3);
    let mut store = create_test_store();
    let now = Instant::now();
    
    // 添加数据包1-6，都处于Sent状态
    for seq in 1..=6 {
        let packet = create_test_packet(seq, now);
        store.add_packet(seq, packet);
    }
    
    // SACK确认3,4,5,6（跳过了1,2）
    let sack_ranges = vec![
        SackRange { start: 3, end: 6 },
    ];
    
    let result = processor.process_sack(
        &mut store,
        1, // 没有累积ACK
        &sack_ranges,
        now,
    );
    
    // 序列号1和2应该被识别为快速重传候选（有4个更高的序列号被确认）
    assert!(result.fast_retx_candidates.contains(&1));
    assert!(result.fast_retx_candidates.contains(&2));
}

/// 测试快速重传检测 - 阈值边界
#[test]
fn test_fast_retx_detection_threshold() {
    let processor = SackProcessor::new(3); // 阈值为3
    let mut store = create_test_store();
    let now = Instant::now();
    
    // 添加数据包1-5
    for seq in 1..=5 {
        let packet = create_test_packet(seq, now);
        store.add_packet(seq, packet);
    }
    
    // SACK确认3,4,5（2个更高序列号被确认，不够阈值）
    let sack_ranges = vec![
        SackRange { start: 3, end: 5 },
    ];
    
    let result = processor.process_sack(
        &mut store,
        1,
        &sack_ranges,
        now,
    );
    
    // 应该没有快速重传候选（只有3个更高序列号，等于阈值但不大于）
    assert!(result.fast_retx_candidates.is_empty());
    
    // 再添加一个SACK确认
    store.add_packet(6, create_test_packet(6, now));
    let sack_ranges = vec![
        SackRange { start: 3, end: 6 },
    ];
    
    let result = processor.process_sack(
        &mut store,
        1,
        &sack_ranges,
        now,
    );
    
    // 现在应该有快速重传候选（4个更高序列号被确认，大于阈值3）
    assert!(result.fast_retx_candidates.contains(&1));
    assert!(result.fast_retx_candidates.contains(&2));
}

/// 测试快速重传检测 - 状态过滤
#[test]
fn test_fast_retx_detection_state_filtering() {
    let processor = SackProcessor::new(3);
    let mut store = create_test_store();
    let now = Instant::now();
    
    // 添加数据包，但状态不同
    let mut packet1 = create_test_packet(1, now);
    packet1.state = PacketState::SackAcked; // 已确认，不应该被考虑
    store.add_packet(1, packet1);
    
    let packet2 = create_test_packet(2, now); // Sent状态，应该被考虑
    store.add_packet(2, packet2);
    
    for seq in 3..=6 {
        let packet = create_test_packet(seq, now);
        store.add_packet(seq, packet);
    }
    
    let sack_ranges = vec![
        SackRange { start: 3, end: 6 },
    ];
    
    let result = processor.process_sack(
        &mut store,
        1,
        &sack_ranges,
        now,
    );
    
    // 只有序列号2应该被识别为候选（序列号1已确认）
    assert!(!result.fast_retx_candidates.contains(&1));
    assert!(result.fast_retx_candidates.contains(&2));
}

/// 测试更新快速重传候选状态
#[test]
fn test_update_fast_retx_candidates() {
    let processor = SackProcessor::new(3);
    let mut store = create_test_store();
    let now = Instant::now();
    
    // 添加数据包
    for seq in 1..=3 {
        let packet = create_test_packet(seq, now);
        store.add_packet(seq, packet);
    }
    
    let candidates = vec![1, 2];
    processor.update_fast_retx_candidates(&mut store, &candidates);
    
    // 验证状态已更新（这需要InFlightPacketStore支持相应的检查方法）
    // 这里只是确保调用不会panic
}

/// 测试混合场景 - 累积ACK + SACK + 快速重传
#[test]
fn test_mixed_scenario() {
    let processor = SackProcessor::new(2); // 降低阈值便于测试
    let mut store = create_test_store();
    let now = Instant::now();
    
    // 添加数据包1-10
    for seq in 1..=10 {
        let packet = create_test_packet(seq, now - Duration::from_millis(100));
        store.add_packet(seq, packet);
    }
    
    // 累积确认1-3，SACK确认6-8
    let sack_ranges = vec![
        SackRange { start: 6, end: 8 },
    ];
    
    let result = processor.process_sack(
        &mut store,
        4, // 累积确认1,2,3
        &sack_ranges,
        now,
    );
    
    assert_eq!(result.cumulative_acked, vec![1, 2, 3]);
    assert_eq!(result.sack_acked, vec![6, 7, 8]);
    assert_eq!(result.rtt_samples.len(), 6); // 3个累积 + 3个SACK
    
    // 序列号4和5应该被识别为快速重传候选
    // （有3个更高序列号6,7,8被SACK确认，大于阈值2）
    assert!(result.fast_retx_candidates.contains(&4));
    assert!(result.fast_retx_candidates.contains(&5));
}

/// 性能测试 - 大量SACK范围
#[test]
fn test_performance_many_sack_ranges() {
    let processor = SackProcessor::new(3);
    let mut store = create_test_store();
    let now = Instant::now();
    
    // 添加大量数据包
    for seq in 1..=1000 {
        let packet = create_test_packet(seq, now);
        store.add_packet(seq, packet);
    }
    
    // 创建多个SACK范围
    let mut sack_ranges = Vec::new();
    for i in (10..=990).step_by(20) {
        sack_ranges.push(SackRange { start: i, end: i + 10 });
    }
    
    let start = std::time::Instant::now();
    
    let _result = processor.process_sack(
        &mut store,
        1,
        &sack_ranges,
        now,
    );
    
    let elapsed = start.elapsed();
    
    // 应该在合理时间内完成
    assert!(elapsed < Duration::from_millis(100), "Performance test failed: took {:?}", elapsed);
}

/// 测试SACK范围排序
#[test]
fn test_sack_range_ordering() {
    let processor = SackProcessor::new(3);
    let mut store = create_test_store();
    let now = Instant::now();
    
    // 添加数据包
    for seq in 1..=10 {
        let packet = create_test_packet(seq, now);
        store.add_packet(seq, packet);
    }
    
    // 无序的SACK范围
    let sack_ranges = vec![
        SackRange { start: 7, end: 8 },
        SackRange { start: 2, end: 3 },
        SackRange { start: 5, end: 6 },
    ];
    
    let result = processor.process_sack(
        &mut store,
        1,
        &sack_ranges,
        now,
    );
    
    // SACK确认的序列号应该按处理顺序
    assert_eq!(result.sack_acked, vec![7, 8, 2, 3, 5, 6]);
    
    // 快速重传候选应该是有序的
    let mut sorted_candidates = result.fast_retx_candidates.clone();
    sorted_candidates.sort();
    assert_eq!(result.fast_retx_candidates, sorted_candidates);
}

/// 测试边界条件 - 空存储
#[test]
fn test_empty_store() {
    let processor = SackProcessor::new(3);
    let mut store = create_test_store();
    let now = Instant::now();
    
    let sack_ranges = vec![
        SackRange { start: 1, end: 5 },
    ];
    
    let result = processor.process_sack(
        &mut store,
        10,
        &sack_ranges,
        now,
    );
    
    // 空存储应该产生空结果
    assert!(result.cumulative_acked.is_empty());
    assert!(result.sack_acked.is_empty());
    assert!(result.rtt_samples.is_empty());
    assert!(result.fast_retx_candidates.is_empty());
}

/// 测试结果调试输出
#[test]
fn test_result_debug_output() {
    let result = SackProcessResult {
        cumulative_acked: vec![1, 2],
        sack_acked: vec![4, 5],
        rtt_samples: vec![Duration::from_millis(100)],
        fast_retx_candidates: vec![3],
    };
    
    let debug_string = format!("{:?}", result);
    
    // 应该包含所有字段
    assert!(debug_string.contains("cumulative_acked"));
    assert!(debug_string.contains("sack_acked"));
    assert!(debug_string.contains("rtt_samples"));
    assert!(debug_string.contains("fast_retx_candidates"));
}