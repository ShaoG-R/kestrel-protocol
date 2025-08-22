//! 时间轮统计信息
//! Timing wheel statistics

use std::time::Duration;

/// 时间轮统计信息
/// Timing wheel statistics
#[derive(Debug, Clone)]
pub struct TimingWheelStats {
    /// 总槽位数
    /// Total number of slots
    pub total_slots: usize,
    /// 非空槽位数
    /// Number of non-empty slots
    pub non_empty_slots: usize,
    /// 总定时器数
    /// Total number of timers
    pub total_timers: usize,
    /// 最大槽位大小
    /// Maximum slot size
    pub max_slot_size: usize,
    /// 当前槽位
    /// Current slot
    pub current_slot: usize,
    /// 槽位持续时间
    /// Slot duration
    pub slot_duration: Duration,
}

impl std::fmt::Display for TimingWheelStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "TimingWheelStats {{ slots: {}/{}, timers: {}, max_slot: {}, current: {}, duration: {:?} }}",
            self.non_empty_slots,
            self.total_slots,
            self.total_timers,
            self.max_slot_size,
            self.current_slot,
            self.slot_duration
        )
    }
}
