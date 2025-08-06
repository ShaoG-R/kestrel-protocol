use tokio::time::Instant;
use bytes::Bytes;
use crate::core::reliability::coordination::packet_coordinator::RetransmissionFrameInfo;
use crate::core::reliability::data::{InFlightPacket, InFlightPacketStore, PacketState};
use crate::packet::frame::FrameType;
use crate::timer::actor::ActorTimerId;

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