//! 时间轮数据结构实现
//! Timing Wheel Data Structure Implementation
//!
//! 该模块实现了高效的时间轮算法，用于O(1)时间复杂度的定时器管理。
//! 时间轮将时间分为多个槽位，每个槽位包含在该时间点到期的定时器列表。
//!
//! This module implements an efficient timing wheel algorithm for O(1) timer management.
//! The timing wheel divides time into multiple slots, each containing a list of timers
//! that expire at that time point.

mod entry;
mod core;
mod simd;
mod stats;

// Re-export the main types for convenience
pub use entry::{TimerEntry, TimerEntryId};
pub use core::TimingWheel;
pub use stats::TimingWheelStats;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timer::event::{TimerEventData, TimerEvent};
    use crate::core::endpoint::timing::TimeoutEvent;
    use tokio::sync::mpsc;
    use tokio::time::{sleep, Duration};
    use std::collections::HashSet;

    fn create_test_timer_event(id: u64) -> TimerEvent<TimeoutEvent> {
        let (tx, _rx) = mpsc::channel(1);
        let data = TimerEventData::new(id as u32, TimeoutEvent::IdleTimeout);
        TimerEvent::new(id, data, tx)
    }

    fn create_test_timer_events(count: usize) -> Vec<TimerEvent<TimeoutEvent>> {
        (0..count)
            .map(|i| create_test_timer_event(i as u64))
            .collect()
    }

    #[test]
    fn test_timing_wheel_creation() {
        let wheel: TimingWheel<TimeoutEvent> = TimingWheel::new(8, Duration::from_millis(100));
        assert_eq!(wheel.slot_count, 8);
        assert_eq!(wheel.slot_duration, Duration::from_millis(100));
        assert_eq!(wheel.current_slot, 0);
        assert!(wheel.is_empty());
    }

    #[test]
    fn test_default_wheel_creation() {
        let wheel: TimingWheel<TimeoutEvent> = TimingWheel::new_default();
        assert_eq!(wheel.slot_count, 512);
        assert_eq!(wheel.slot_duration, Duration::from_millis(100));
        assert!(wheel.is_empty());
    }

    #[test]
    #[should_panic(expected = "slot_count must be a power of 2")]
    fn test_invalid_slot_count() {
        TimingWheel::<TimeoutEvent>::new(7, Duration::from_millis(100)); // 不是2的幂
    }

    #[test]
    fn test_add_timer() {
        let mut wheel = TimingWheel::new(8, Duration::from_millis(100));
        let event = create_test_timer_event(1);
        
        let timer_id = wheel.add_timer(Duration::from_millis(500), event);
        assert_eq!(timer_id, 1);
        assert_eq!(wheel.timer_count(), 1);
        assert!(!wheel.is_empty());
    }

    #[test]
    fn test_cancel_timer() {
        let mut wheel = TimingWheel::new(8, Duration::from_millis(100));
        let event = create_test_timer_event(1);
        
        let timer_id = wheel.add_timer(Duration::from_millis(500), event);
        assert!(wheel.cancel_timer(timer_id));
        assert_eq!(wheel.timer_count(), 0);
        assert!(wheel.is_empty());
        
        // 尝试取消不存在的定时器
        assert!(!wheel.cancel_timer(999));
    }

    #[tokio::test]
    async fn test_advance_timing_wheel() {
        let mut wheel = TimingWheel::new(8, Duration::from_millis(100));
        
        // 添加一个500ms后到期的定时器
        let event = create_test_timer_event(1);
        wheel.add_timer(Duration::from_millis(500), event);
        
        // 立即推进，不应该有到期的定时器
        let expired = wheel.advance(tokio::time::Instant::now());
        assert!(expired.is_empty());
        
        // 等待600ms后推进，应该有到期的定时器
        sleep(Duration::from_millis(600)).await;
        let expired = wheel.advance(tokio::time::Instant::now());
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].id, 1);
    }

    #[test]
    fn test_next_expiry_time() {
        let mut wheel = TimingWheel::new(8, Duration::from_millis(100));
        
        // 空时间轮应该返回None
        assert!(wheel.next_expiry_time().is_none());
        
        // 添加定时器后应该返回到期时间
        let event = create_test_timer_event(1);
        wheel.add_timer(Duration::from_millis(500), event);
        assert!(wheel.next_expiry_time().is_some());
    }

    #[test]
    fn test_timing_wheel_stats() {
        let mut wheel = TimingWheel::new(8, Duration::from_millis(100));
        
        let stats = wheel.stats();
        assert_eq!(stats.total_slots, 8);
        assert_eq!(stats.non_empty_slots, 0);
        assert_eq!(stats.total_timers, 0);
        
        // 添加定时器后统计信息应该更新
        let event = create_test_timer_event(1);
        wheel.add_timer(Duration::from_millis(500), event);
        
        let stats = wheel.stats();
        assert_eq!(stats.non_empty_slots, 1);
        assert_eq!(stats.total_timers, 1);
    }

    #[test]
    fn test_clear_timers() {
        let mut wheel = TimingWheel::new(8, Duration::from_millis(100));
        
        // 添加多个定时器
        for i in 1..=5 {
            let event = create_test_timer_event(i);
            wheel.add_timer(Duration::from_millis(i * 100), event);
        }
        
        assert_eq!(wheel.timer_count(), 5);
        
        wheel.clear();
        assert_eq!(wheel.timer_count(), 0);
        assert!(wheel.is_empty());
    }

    // ========== 扩展测试：SIMD批量操作 ==========
    
    #[test]
    fn test_batch_add_timers_small() {
        let mut wheel = TimingWheel::new(64, Duration::from_millis(10));
        let events = create_test_timer_events(8);
        
        let timer_data: Vec<(Duration, TimerEvent<TimeoutEvent>)> = events
            .into_iter()
            .enumerate()
            .map(|(i, event)| (Duration::from_millis(((i + 1) * 50) as u64), event))
            .collect();
        
        let timer_ids = wheel.batch_add_timers(timer_data);
        assert_eq!(timer_ids.len(), 8);
        assert_eq!(wheel.timer_count(), 8);
        
        // 验证所有定时器ID是连续的
        for (i, &id) in timer_ids.iter().enumerate() {
            assert_eq!(id, (i + 1) as u64);
        }
    }

    #[test]
    fn test_batch_add_timers_large() {
        let mut wheel = TimingWheel::new(512, Duration::from_millis(10));
        let events = create_test_timer_events(128);
        
        let timer_data: Vec<(Duration, TimerEvent<TimeoutEvent>)> = events
            .into_iter()
            .enumerate()
            .map(|(i, event)| (Duration::from_millis(((i + 1) * 10) as u64), event))
            .collect();
        
        let start_time = std::time::Instant::now();
        let timer_ids = wheel.batch_add_timers(timer_data);
        let duration = start_time.elapsed();
        
        assert_eq!(timer_ids.len(), 128);
        assert_eq!(wheel.timer_count(), 128);
        
        // 验证SIMD批量操作性能：应该在微秒级别完成
        println!("Batch add 128 timers took: {:?}", duration);
        assert!(duration < Duration::from_millis(1)); // 应该小于1毫秒
    }

    #[test]
    fn test_simd_u32_id_range() {
        let mut wheel = TimingWheel::new(64, Duration::from_millis(10));
        let events = create_test_timer_events(16);
        
        let timer_data: Vec<(Duration, TimerEvent<TimeoutEvent>)> = events
            .into_iter()
            .enumerate()
            .map(|(i, event)| (Duration::from_millis(((i + 1) * 10) as u64), event))
            .collect();
        
        // 测试u32x8 SIMD路径
        let timer_ids = wheel.batch_add_timers(timer_data);
        assert_eq!(timer_ids.len(), 16);
        
        // 验证ID在u32范围内
        for &id in &timer_ids {
            assert!(id <= u32::MAX as u64);
        }
    }

    // ========== 缓存机制测试 ==========
    
    #[test]
    fn test_next_expiry_cache() {
        let mut wheel = TimingWheel::new(8, Duration::from_millis(100));
        
        // 第一次查询，建立缓存
        assert!(wheel.next_expiry_time().is_none());
        
        // 添加定时器，缓存应该失效
        let event = create_test_timer_event(1);
        wheel.add_timer(Duration::from_millis(500), event);
        
        let expiry1 = wheel.next_expiry_time();
        assert!(expiry1.is_some());
        
        // 再次查询，应该使用缓存
        let expiry2 = wheel.next_expiry_time();
        assert_eq!(expiry1, expiry2);
        
        // 添加更早的定时器，缓存应该更新
        let event2 = create_test_timer_event(2);
        wheel.add_timer(Duration::from_millis(200), event2);
        
        let expiry3 = wheel.next_expiry_time();
        assert!(expiry3.is_some());
        assert!(expiry3.unwrap() < expiry1.unwrap());
    }

    // ========== 边界条件测试 ==========
    
    #[test]
    fn test_immediate_timer_basic() {
        let mut wheel = TimingWheel::new(8, Duration::from_millis(100));
        let event = create_test_timer_event(1);
        
        // 添加一个立即到期的定时器（1ms）
        let timer_id = wheel.add_timer(Duration::from_millis(1), event);
        assert_eq!(timer_id, 1);
        assert_eq!(wheel.timer_count(), 1);
        
        // 验证定时器被正确添加
        assert!(!wheel.is_empty());
        assert!(wheel.next_expiry_time().is_some());
    }

    #[test]
    fn test_very_long_duration_timer() {
        let mut wheel = TimingWheel::new(8, Duration::from_millis(100));
        let event = create_test_timer_event(1);
        
        // 添加一个非常长的定时器（超出时间轮覆盖范围）
        let long_duration = Duration::from_secs(3600); // 1小时
        let timer_id = wheel.add_timer(long_duration, event);
        assert_eq!(timer_id, 1);
        assert_eq!(wheel.timer_count(), 1);
    }

    #[test]
    fn test_multiple_timers_same_slot() {
        let mut wheel = TimingWheel::new(8, Duration::from_millis(100));
        
        // 添加多个定时器到同一个槽位
        for i in 1..=5 {
            let event = create_test_timer_event(i);
            wheel.add_timer(Duration::from_millis(500), event); // 都是500ms
        }
        
        assert_eq!(wheel.timer_count(), 5);
        
        // 推进到到期时间
        let future_time = tokio::time::Instant::now() + Duration::from_millis(600);
        let expired = wheel.advance(future_time);
        assert_eq!(expired.len(), 5);
        
        // 验证所有定时器都到期了
        let expired_ids: HashSet<u64> = expired.iter().map(|t| t.id).collect();
        let expected_ids: HashSet<u64> = (1..=5).collect();
        assert_eq!(expired_ids, expected_ids);
    }

    #[test]
    fn test_wrap_around_slot_calculation() {
        let mut wheel = TimingWheel::new(8, Duration::from_millis(100));
        
        // 添加定时器，使其槽位计算需要环绕
        for i in 0..16 {
            let event = create_test_timer_event(i);
            wheel.add_timer(Duration::from_millis(i * 100), event);
        }
        
        assert_eq!(wheel.timer_count(), 16);
        
        let stats = wheel.stats();
        // 由于环绕，定时器应该分布在所有8个槽位中
        assert_eq!(stats.non_empty_slots, 8);
    }

    // ========== 性能压力测试 ==========
    
    #[test]
    fn test_large_scale_operations() {
        let mut wheel = TimingWheel::new(512, Duration::from_millis(10));
        let timer_count = 1000;
        
        // 批量添加大量定时器
        let events = create_test_timer_events(timer_count);
        let timer_data: Vec<(Duration, TimerEvent<TimeoutEvent>)> = events
            .into_iter()
            .enumerate()
            .map(|(i, event)| (Duration::from_millis(((i % 100 + 1) * 10) as u64), event))
            .collect();
        
        let start_time = std::time::Instant::now();
        let timer_ids = wheel.batch_add_timers(timer_data);
        let add_duration = start_time.elapsed();
        
        assert_eq!(timer_ids.len(), timer_count);
        assert_eq!(wheel.timer_count(), timer_count);
        
        // 批量取消一半定时器
        let start_time = std::time::Instant::now();
        let mut cancelled_count = 0;
        for &id in timer_ids.iter().step_by(2) {
            if wheel.cancel_timer(id) {
                cancelled_count += 1;
            }
        }
        let cancel_duration = start_time.elapsed();
        
        assert_eq!(cancelled_count, timer_count / 2);
        assert_eq!(wheel.timer_count(), timer_count - cancelled_count);
        
        println!("Add {} timers: {:?}", timer_count, add_duration);
        println!("Cancel {} timers: {:?}", cancelled_count, cancel_duration);
        
        // 验证操作性能
        assert!(add_duration < Duration::from_millis(10));
        assert!(cancel_duration < Duration::from_millis(5));
    }

    // ========== 内存效率测试 ==========
    
    #[test]
    fn test_memory_efficiency() {
        let mut wheel = TimingWheel::new(64, Duration::from_millis(10));
        
        // 添加大量定时器然后清空，测试内存清理
        for _ in 0..5 {
            let events = create_test_timer_events(100);
            let timer_data: Vec<(Duration, TimerEvent<TimeoutEvent>)> = events
                .into_iter()
                .enumerate()
                .map(|(i, event)| (Duration::from_millis(((i + 1) * 10) as u64), event))
                .collect();
            
            wheel.batch_add_timers(timer_data);
            assert_eq!(wheel.timer_count(), 100);
            
            wheel.clear();
            assert_eq!(wheel.timer_count(), 0);
            assert!(wheel.is_empty());
        }
        
        // 验证统计信息正确
        let stats = wheel.stats();
        assert_eq!(stats.total_timers, 0);
        assert_eq!(stats.non_empty_slots, 0);
    }

    // ========== 并发安全性相关测试 ==========
    
    #[test]
    fn test_timer_id_uniqueness() {
        let mut wheel = TimingWheel::new(64, Duration::from_millis(10));
        let mut all_ids = HashSet::new();
        
        // 添加大量定时器，验证ID唯一性
        for i in 0..1000 {
            let event = create_test_timer_event(i);
            let timer_id = wheel.add_timer(Duration::from_millis((i % 100 + 1) * 10), event);
            assert!(all_ids.insert(timer_id), "Timer ID {} is not unique", timer_id);
        }
        
        assert_eq!(all_ids.len(), 1000);
        assert_eq!(wheel.timer_count(), 1000);
    }
}