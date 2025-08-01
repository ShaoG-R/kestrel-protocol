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

    fn create_test_timer_event(id: u64) -> TimerEvent<TimeoutEvent> {
        let (tx, _rx) = mpsc::channel(1);
        let data = TimerEventData::new(id as u32, TimeoutEvent::IdleTimeout);
        TimerEvent::new(id, data, tx)
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
}