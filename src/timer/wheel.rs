//! 时间轮数据结构实现
//! Timing Wheel Data Structure Implementation
//!
//! 该模块实现了高效的时间轮算法，用于O(1)时间复杂度的定时器管理。
//! 时间轮将时间分为多个槽位，每个槽位包含在该时间点到期的定时器列表。
//!
//! This module implements an efficient timing wheel algorithm for O(1) timer management.
//! The timing wheel divides time into multiple slots, each containing a list of timers
//! that expire at that time point.

use crate::timer::event::TimerEvent;
use std::collections::{HashMap, VecDeque};
use std::time::Duration;
use tokio::time::Instant;
use tracing::{debug, trace, warn};

/// 定时器条目ID，用于在时间轮中唯一标识定时器条目
/// Timer entry ID, used to uniquely identify timer entries in the timing wheel
pub type TimerEntryId = u64;

/// 时间轮中的定时器条目
/// Timer entry in the timing wheel
#[derive(Debug)]
pub struct TimerEntry {
    /// 条目ID
    /// Entry ID
    pub id: TimerEntryId,
    /// 到期时间
    /// Expiration time
    pub expiry_time: Instant,
    /// 定时器事件
    /// Timer event
    pub event: TimerEvent,
}

impl TimerEntry {
    /// 创建新的定时器条目
    /// Create new timer entry
    pub fn new(id: TimerEntryId, expiry_time: Instant, event: TimerEvent) -> Self {
        Self {
            id,
            expiry_time,
            event,
        }
    }
}

/// 时间轮实现
/// Timing wheel implementation
#[derive(Debug)]
pub struct TimingWheel {
    /// 时间轮的槽位数量（必须是2的幂以支持位运算优化）
    /// Number of slots in the timing wheel (must be power of 2 for bitwise optimization)
    slot_count: usize,
    /// 槽位数量的掩码，用于快速模运算 (slot_count - 1)
    /// Slot count mask for fast modulo operation (slot_count - 1)
    slot_mask: usize,
    /// 每个槽位的时间间隔
    /// Time interval per slot
    slot_duration: Duration,
    /// 当前指针位置
    /// Current pointer position
    current_slot: usize,
    /// 时间轮的开始时间
    /// Start time of the timing wheel
    start_time: Instant,
    /// 当前时间（上次推进的时间）
    /// Current time (last advanced time)
    current_time: Instant,
    /// 槽位数组，每个槽位包含该时间点到期的定时器
    /// Slot array, each slot contains timers that expire at that time
    slots: Vec<VecDeque<TimerEntry>>,
    /// 定时器ID映射，用于快速查找和删除定时器
    /// Timer ID mapping for fast lookup and deletion
    timer_map: HashMap<TimerEntryId, (usize, usize)>, // (slot_index, position_in_slot)
    /// 下一个分配的定时器条目ID
    /// Next timer entry ID to allocate
    next_entry_id: TimerEntryId,
    /// 缓存的下次到期时间，避免频繁计算
    /// Cached next expiry time to avoid frequent calculation
    cached_next_expiry: Option<Instant>,
}

impl TimingWheel {
    /// 创建新的时间轮
    /// Create new timing wheel
    ///
    /// # Arguments
    /// * `slot_count` - 槽位数量，必须是2的幂次方以优化取模运算
    /// * `slot_duration` - 每个槽位的时间间隔
    ///
    /// # Arguments (English)
    /// * `slot_count` - Number of slots, must be power of 2 for optimized modulo
    /// * `slot_duration` - Time interval per slot
    pub fn new(slot_count: usize, slot_duration: Duration) -> Self {
        // 确保槽位数量是2的幂
        // Ensure slot count is power of 2
        assert!(slot_count.is_power_of_two(), "slot_count must be a power of 2");
        
        let now = Instant::now();
        let mut slots = Vec::with_capacity(slot_count);
        for _ in 0..slot_count {
            slots.push(VecDeque::new());
        }

        Self {
            slot_count,
            slot_mask: slot_count - 1,  // 用于快速模运算的掩码
            slot_duration,
            current_slot: 0,
            start_time: now,
            current_time: now,
            slots,
            timer_map: HashMap::new(),
            next_entry_id: 1,
            cached_next_expiry: None,
        }
    }

    /// 创建默认配置的时间轮
    /// Create timing wheel with default configuration
    /// 
    /// 默认配置：512个槽位，每个槽位100毫秒，总覆盖时间约51.2秒
    /// Default config: 512 slots, 100ms per slot, total coverage ~51.2 seconds
    pub fn new_default() -> Self {
        Self::new(512, Duration::from_millis(100))
    }

    /// 添加定时器到时间轮
    /// Add timer to timing wheel
    ///
    /// # Arguments
    /// * `delay` - 延迟时间
    /// * `event` - 定时器事件
    ///
    /// # Returns
    /// 返回定时器条目ID，可用于取消定时器
    /// Returns timer entry ID that can be used to cancel the timer
    pub fn add_timer(&mut self, delay: Duration, event: TimerEvent) -> TimerEntryId {
        let entry_id = self.next_entry_id;
        self.next_entry_id += 1;

        let expiry_time = self.current_time + delay;
        let slot_index = self.calculate_slot_index(expiry_time);

        let entry = TimerEntry::new(entry_id, expiry_time, event);
        
        // 将定时器添加到对应槽位
        // Add timer to corresponding slot
        let position_in_slot = self.slots[slot_index].len();
        self.slots[slot_index].push_back(entry);
        
        // 记录定时器位置以便快速删除
        // Record timer position for fast deletion
        self.timer_map.insert(entry_id, (slot_index, position_in_slot));

        // 更新缓存的下次到期时间
        // Update cached next expiry time
        match self.cached_next_expiry {
            None => self.cached_next_expiry = Some(expiry_time),
            Some(current_earliest) => {
                if expiry_time < current_earliest {
                    self.cached_next_expiry = Some(expiry_time);
                }
            }
        }

        trace!(
            entry_id,
            slot_index,
            delay_ms = delay.as_millis(),
            "Added timer to timing wheel"
        );

        entry_id
    }

    /// 取消定时器
    /// Cancel timer
    ///
    /// # Arguments
    /// * `entry_id` - 定时器条目ID
    ///
    /// # Returns
    /// 如果成功取消返回true，如果定时器不存在返回false
    /// Returns true if successfully cancelled, false if timer doesn't exist
    pub fn cancel_timer(&mut self, entry_id: TimerEntryId) -> bool {
        if let Some((slot_index, position_in_slot)) = self.timer_map.remove(&entry_id) {
            // 检查槽位和位置是否有效
            // Check if slot and position are valid
            if slot_index < self.slots.len() && position_in_slot < self.slots[slot_index].len() {
                // 从槽位中移除定时器
                // Remove timer from slot
                let slot = &mut self.slots[slot_index];
                if position_in_slot == slot.len() - 1 {
                    // 如果是最后一个元素，直接弹出
                    // If it's the last element, just pop
                    slot.pop_back();
                } else {
                    // 否则与最后一个元素交换后弹出
                    // Otherwise swap with last element and pop
                    slot.swap(position_in_slot, slot.len() - 1);
                    slot.pop_back();
                    
                    // 更新被交换元素的位置信息
                    // Update position info for swapped element
                    if let Some(swapped_entry) = slot.get(position_in_slot) {
                        self.timer_map.insert(swapped_entry.id, (slot_index, position_in_slot));
                    }
                }

                // 如果取消的是缓存的最早定时器，清除缓存
                // If cancelling the cached earliest timer, clear cache
                if self.cached_next_expiry.is_some() {
                    // 简单策略：如果取消任何定时器，都清除缓存（避免复杂的比较逻辑）
                    // Simple strategy: clear cache if any timer is cancelled (avoids complex comparison logic)
                    self.cached_next_expiry = None;
                }
                
                trace!(entry_id, "Timer cancelled successfully");
                true
            } else {
                warn!(entry_id, slot_index, position_in_slot, "Invalid timer position");
                false
            }
        } else {
            trace!(entry_id, "Timer not found for cancellation");
            false
        }
    }

    /// 推进时间轮并返回到期的定时器
    /// Advance timing wheel and return expired timers
    ///
    /// # Arguments
    /// * `now` - 当前时间
    ///
    /// # Returns
    /// 返回已到期的定时器列表
    /// Returns list of expired timers
    pub fn advance(&mut self, now: Instant) -> Vec<TimerEntry> {
        let mut expired_timers = Vec::new();
        
        // 计算需要推进多少个槽位
        // Calculate how many slots to advance
        let elapsed = now.saturating_duration_since(self.current_time);
        let slots_to_advance = (elapsed.as_nanos() / self.slot_duration.as_nanos()) as usize;
        
        if slots_to_advance == 0 {
            return expired_timers;
        }

        // 限制一次推进的槽位数量，避免处理过多的定时器
        // Limit slots to advance in one go to avoid processing too many timers
        let max_advance = self.slot_count.min(slots_to_advance);

        debug!(
            elapsed_ms = elapsed.as_millis(),
            slots_to_advance,
            max_advance,
            current_slot = self.current_slot,
            "Advancing timing wheel"
        );

        for _ in 0..max_advance {
            // 推进到下一个槽位，使用位运算优化
            // Advance to next slot, using bitwise optimization
            self.current_slot = (self.current_slot + 1) & self.slot_mask;
            self.current_time += self.slot_duration;

            // 处理当前槽位的所有定时器
            // Process all timers in current slot
            let mut entries_to_process = Vec::new();
            {
                let slot = &mut self.slots[self.current_slot];
                while let Some(entry) = slot.pop_front() {
                    entries_to_process.push(entry);
                }
            }
            
            for entry in entries_to_process {
                // 从映射中移除
                // Remove from mapping
                self.timer_map.remove(&entry.id);
                
                // 检查定时器是否真的到期了
                // Check if timer has actually expired
                if entry.expiry_time <= now {
                    trace!(
                        entry_id = entry.id,
                        expiry_time = ?entry.expiry_time,
                        "Timer expired"
                    );
                    expired_timers.push(entry);
                } else {
                    // 如果还没到期，重新添加到正确的槽位
                    // If not expired yet, re-add to correct slot
                    let new_slot_index = self.calculate_slot_index(entry.expiry_time);
                    let entry_id = entry.id;
                    let position_in_slot = self.slots[new_slot_index].len();
                    self.slots[new_slot_index].push_back(entry);
                    self.timer_map.insert(entry_id, (new_slot_index, position_in_slot));
                    
                    trace!(
                        entry_id,
                        new_slot_index,
                        "Timer moved to new slot"
                    );
                }
            }
        }

        // 如果有定时器到期，清除缓存
        // If any timers expired, clear cache
        if !expired_timers.is_empty() {
            self.cached_next_expiry = None;
            debug!(
                expired_count = expired_timers.len(),
                "Timing wheel advance completed"
            );
        }

        expired_timers
    }

    /// 获取下一个定时器的到期时间
    /// Get expiration time of next timer
    ///
    /// # Returns
    /// 返回最早的定时器到期时间，如果没有定时器则返回None
    /// Returns earliest timer expiration time, None if no timers
    pub fn next_expiry_time(&mut self) -> Option<Instant> {
        // 如果有缓存的到期时间，直接返回
        // If there's cached expiry time, return it directly
        if let Some(cached) = self.cached_next_expiry {
            return Some(cached);
        }
        
        // 如果没有缓存，计算并缓存最早的定时器到期时间
        // If no cache, calculate and cache earliest timer expiry time
        let mut earliest_time: Option<Instant> = None;

        // 遍历所有槽位寻找最早的定时器
        // Iterate through all slots to find earliest timer
        for slot in &self.slots {
            for entry in slot {
                match earliest_time {
                    None => earliest_time = Some(entry.expiry_time),
                    Some(current_earliest) => {
                        if entry.expiry_time < current_earliest {
                            earliest_time = Some(entry.expiry_time);
                        }
                    }
                }
            }
        }

        // 缓存结果
        // Cache the result
        self.cached_next_expiry = earliest_time;
        earliest_time
    }

    /// 获取定时器总数
    /// Get total number of timers
    pub fn timer_count(&self) -> usize {
        self.timer_map.len()
    }

    /// 检查是否有定时器
    /// Check if there are any timers
    pub fn is_empty(&self) -> bool {
        self.timer_map.is_empty()
    }

    /// 清除所有定时器
    /// Clear all timers
    pub fn clear(&mut self) {
        for slot in &mut self.slots {
            slot.clear();
        }
        self.timer_map.clear();
        self.cached_next_expiry = None;
        debug!("All timers cleared from timing wheel");
    }

    /// 计算给定时间对应的槽位索引
    /// Calculate slot index for given time
    fn calculate_slot_index(&self, expiry_time: Instant) -> usize {
        let elapsed_since_start = expiry_time.saturating_duration_since(self.start_time);
        let slot_offset = (elapsed_since_start.as_nanos() / self.slot_duration.as_nanos()) as usize;
        // 使用位运算替代模运算，提高性能
        // Use bitwise operation instead of modulo for better performance
        slot_offset & self.slot_mask
    }

    /// 获取时间轮统计信息
    /// Get timing wheel statistics
    pub fn stats(&self) -> TimingWheelStats {
        let mut non_empty_slots = 0;
        let mut max_slot_size = 0;
        let mut total_timers = 0;

        for slot in &self.slots {
            if !slot.is_empty() {
                non_empty_slots += 1;
                max_slot_size = max_slot_size.max(slot.len());
            }
            total_timers += slot.len();
        }

        TimingWheelStats {
            total_slots: self.slot_count,
            non_empty_slots,
            total_timers,
            max_slot_size,
            current_slot: self.current_slot,
            slot_duration: self.slot_duration,
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timer::event::{TimerEventData, TimerEvent};
    use crate::core::endpoint::timing::TimeoutEvent;
    use tokio::sync::mpsc;
    use tokio::time::{sleep, Duration};

    fn create_test_timer_event(id: u64) -> TimerEvent {
        let (tx, _rx) = mpsc::channel(1);
        let data = TimerEventData::new(id as u32, TimeoutEvent::IdleTimeout);
        TimerEvent::new(id, data, tx)
    }

    #[test]
    fn test_timing_wheel_creation() {
        let wheel = TimingWheel::new(8, Duration::from_millis(100));
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
        let expired = wheel.advance(Instant::now());
        assert!(expired.is_empty());
        
        // 等待600ms后推进，应该有到期的定时器
        sleep(Duration::from_millis(600)).await;
        let expired = wheel.advance(Instant::now());
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