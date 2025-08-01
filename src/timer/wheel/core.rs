//! 时间轮核心实现
//! Timing wheel core implementation

use crate::timer::event::traits::EventDataTrait;
use crate::timer::event::TimerEvent;
use crate::timer::wheel::entry::{TimerEntry, TimerEntryId};
use crate::timer::wheel::stats::TimingWheelStats;
use std::collections::{HashMap, VecDeque};
use std::time::Duration;
use tokio::time::Instant;
use tracing::{debug, trace, warn};

/// 时间轮实现
/// Timing wheel implementation
#[derive(Debug)]
pub struct TimingWheel<E: EventDataTrait> {
    /// 时间轮的槽位数量（必须是2的幂以支持位运算优化）
    /// Number of slots in the timing wheel (must be power of 2 for bitwise optimization)
    pub(super) slot_count: usize,
    /// 槽位数量的掩码，用于快速模运算 (slot_count - 1)
    /// Slot count mask for fast modulo operation (slot_count - 1)
    pub(super) slot_mask: usize,
    /// 每个槽位的时间间隔
    /// Time interval per slot
    pub(super) slot_duration: Duration,
    /// 当前指针位置
    /// Current pointer position
    pub(super) current_slot: usize,
    /// 时间轮的开始时间
    /// Start time of the timing wheel
    pub(super) start_time: Instant,
    /// 当前时间（上次推进的时间）
    /// Current time (last advanced time)
    pub(super) current_time: Instant,
    /// 槽位数组，每个槽位包含该时间点到期的定时器
    /// Slot array, each slot contains timers that expire at that time
    pub(super) slots: Vec<VecDeque<TimerEntry<E>>>,
    /// 定时器ID映射，用于快速查找和删除定时器
    /// Timer ID mapping for fast lookup and deletion
    pub(super) timer_map: HashMap<TimerEntryId, (usize, usize)>, // (slot_index, position_in_slot)
    /// 下一个分配的定时器条目ID
    /// Next timer entry ID to allocate
    pub(super) next_entry_id: TimerEntryId,
    /// 缓存的下次到期时间，避免频繁计算
    /// Cached next expiry time to avoid frequent calculation
    pub(super) cached_next_expiry: Option<Instant>,
}

impl<E: EventDataTrait> TimingWheel<E> {
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

    /// 批量添加定时器到时间轮（SIMD优化版本）
    /// Batch add timers to timing wheel (SIMD optimized version)
    ///
    /// # Arguments
    /// * `timers` - 定时器列表（延迟时间，定时器事件）
    ///
    /// # Returns
    /// 返回定时器条目ID列表
    /// Returns list of timer entry IDs
    pub fn batch_add_timers(&mut self, timers: Vec<(Duration, TimerEvent<E>)>) -> Vec<TimerEntryId> {
        if timers.is_empty() {
            return Vec::new();
        }

        let timer_count = timers.len();
        let mut result = Vec::with_capacity(timer_count);
        let mut earliest_expiry: Option<Instant> = self.cached_next_expiry;
        
        // 预分配entry IDs，减少分配开销
        // Pre-allocate entry IDs to reduce allocation overhead
        let start_id = self.next_entry_id;
        self.next_entry_id += timer_count as u64;
        
        // SIMD优化：批量生成ID序列和时间计算
        // SIMD optimization: batch generate ID sequences and time calculations
        let (entry_ids, expiry_times, slot_indices) = self.simd_calculate_batch_metadata(&timers, start_id);
        
        // 按槽位分组定时器，减少散列访问
        // Group timers by slot to reduce scattered access
        let mut slot_groups: HashMap<usize, Vec<(TimerEntryId, Instant, TimerEvent<E>)>> = HashMap::new();
        
        // SIMD优化：批量分析槽位分布，减少HashMap查找
        // SIMD optimization: batch analyze slot distribution to reduce HashMap lookups
        let _slot_distribution = self.simd_analyze_slot_distribution(&slot_indices);
        
        // 使用预计算的结果进行批量处理
        // Use pre-calculated results for batch processing
        for (index, (_delay, event)) in timers.into_iter().enumerate() {
            let entry_id = entry_ids[index];
            let expiry_time = expiry_times[index];
            let slot_index = slot_indices[index];
            
            // 更新最早到期时间
            // Update earliest expiry time
            match earliest_expiry {
                None => earliest_expiry = Some(expiry_time),
                Some(current_earliest) => {
                    if expiry_time < current_earliest {
                        earliest_expiry = Some(expiry_time);
                    }
                }
            }
            
            slot_groups
                .entry(slot_index)
                .or_insert_with(|| Vec::with_capacity(4))
                .push((entry_id, expiry_time, event));
            
            result.push(entry_id);
        }
        
        // 批量插入到各个槽位
        // Batch insert into each slot
        for (slot_index, entries) in slot_groups {
            let slot = &mut self.slots[slot_index];
            
            for (entry_id, expiry_time, event) in entries {
                let position_in_slot = slot.len();
                let entry = TimerEntry::new(entry_id, expiry_time, event);
                slot.push_back(entry);
                
                // 记录定时器位置
                // Record timer position
                self.timer_map.insert(entry_id, (slot_index, position_in_slot));
            }
        }
        
        // 更新缓存
        // Update cache
        self.cached_next_expiry = earliest_expiry;
        
        trace!(
            batch_size = result.len(),
            "Batch added timers to timing wheel"
        );
        
        result
    }

    /// 批量取消定时器（高性能版本）
    /// Batch cancel timers (high-performance version)
    ///
    /// # Arguments
    /// * `entry_ids` - 要取消的定时器条目ID列表
    ///
    /// # Returns
    /// 返回成功取消的定时器数量
    /// Returns number of successfully cancelled timers
    pub fn batch_cancel_timers(&mut self, entry_ids: &[TimerEntryId]) -> usize {
        if entry_ids.is_empty() {
            return 0;
        }

        let mut cancelled_count = 0;
        let mut need_cache_invalidation = false;
        
        // 按槽位分组要取消的定时器，提高cache locality
        // Group timers to cancel by slot for better cache locality
        let mut slot_groups: HashMap<usize, Vec<(TimerEntryId, usize)>> = HashMap::new();
        
        for &entry_id in entry_ids {
            if let Some((slot_index, position_in_slot)) = self.timer_map.get(&entry_id) {
                slot_groups
                    .entry(*slot_index)
                    .or_insert_with(|| Vec::with_capacity(4))
                    .push((entry_id, *position_in_slot));
            }
        }
        
        // 批量处理每个槽位的取消操作
        // Batch process cancellation for each slot
        for (slot_index, entries) in slot_groups {
            if slot_index >= self.slots.len() {
                continue;
            }
            
            let slot = &mut self.slots[slot_index];
            
            // 按位置倒序排序，从后往前删除避免位置偏移
            // Sort by position in reverse order, delete from back to front to avoid position shifts
            let mut entries = entries;
            entries.sort_by(|a, b| b.1.cmp(&a.1));
            
            for (entry_id, position_in_slot) in entries {
                if position_in_slot < slot.len() {
                    // 检查是否需要缓存失效
                    // Check if cache invalidation is needed
                    if let Some(cached_expiry) = self.cached_next_expiry {
                        if let Some(entry) = slot.get(position_in_slot) {
                            if entry.expiry_time <= cached_expiry + Duration::from_millis(1) {
                                need_cache_invalidation = true;
                            }
                        }
                    }
                    
                    // 智能删除：如果是最后一个元素，直接弹出；否则交换后弹出
                    // Smart deletion: if last element, pop directly; otherwise swap and pop
                    if position_in_slot == slot.len() - 1 {
                        slot.pop_back();
                    } else {
                        slot.swap(position_in_slot, slot.len() - 1);
                        slot.pop_back();
                        
                        // 更新被交换元素的位置信息
                        // Update position info for swapped element
                        if let Some(swapped_entry) = slot.get(position_in_slot) {
                            self.timer_map.insert(swapped_entry.id, (slot_index, position_in_slot));
                        }
                    }
                    
                    self.timer_map.remove(&entry_id);
                    cancelled_count += 1;
                }
            }
        }
        
        // 智能缓存失效
        // Smart cache invalidation
        if need_cache_invalidation {
            self.cached_next_expiry = None;
        }
        
        trace!(
            batch_size = entry_ids.len(),
            cancelled_count,
            "Batch cancelled timers"
        );
        
        cancelled_count
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
    pub fn add_timer(&mut self, delay: Duration, event: TimerEvent<E>) -> TimerEntryId {
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
                let slot = &mut self.slots[slot_index];
                
                // 智能缓存失效：在移除前先获取被取消定时器的到期时间
                // Smart cache invalidation: get cancelled timer's expiry before removal
                let cancelled_expiry = slot.get(position_in_slot).map(|entry| entry.expiry_time);
                
                // 从槽位中移除定时器
                // Remove timer from slot
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

                // 智能缓存失效：只有当取消的定时器可能影响最早时间时才清除缓存
                // Smart cache invalidation: only clear cache if cancelled timer might affect earliest time
                if let (Some(cached_expiry), Some(cancelled_expiry)) = (self.cached_next_expiry, cancelled_expiry) {
                    // 只有当被取消的定时器到期时间小于等于缓存时间时才清除缓存
                    // Only clear cache if cancelled timer's expiry is less than or equal to cached time
                    if cancelled_expiry <= cached_expiry + Duration::from_millis(1) {
                        self.cached_next_expiry = None;
                    }
                    // 否则缓存仍然有效，因为被取消的定时器不是最早的
                    // Otherwise cache is still valid as cancelled timer wasn't the earliest
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
    pub fn advance(&mut self, now: Instant) -> Vec<TimerEntry<E>> {
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
        
        // 如果没有定时器，直接返回None
        // If no timers, return None directly
        if self.timer_map.is_empty() {
            return None;
        }
        
        // 使用优化算法：按时间顺序检查槽位，大幅提升性能
        // Use optimized algorithm: check slots in time order, significantly improve performance
        let mut earliest_time: Option<Instant> = None;
        
        // 从当前槽位开始，按时间顺序检查最多一轮槽位
        // Starting from current slot, check at most one round of slots in time order
        for offset in 0..self.slot_count {
            let slot_index = (self.current_slot + offset) & self.slot_mask;
            let slot = &self.slots[slot_index];
            
            if slot.is_empty() {
                continue; // 跳过空槽位，O(1)操作
            }
            
            // 找到第一个非空槽位，在其中寻找最早的定时器
            // Found first non-empty slot, find earliest timer in it
            let mut slot_earliest: Option<Instant> = None;
            for entry in slot {
                match slot_earliest {
                    None => slot_earliest = Some(entry.expiry_time),
                    Some(current) => {
                        if entry.expiry_time < current {
                            slot_earliest = Some(entry.expiry_time);
                        }
                    }
                }
            }
            
            // 比较当前槽位的最早时间和全局最早时间
            // Compare current slot's earliest time with global earliest time
            match (earliest_time, slot_earliest) {
                (None, Some(slot_time)) => earliest_time = Some(slot_time),
                (Some(global_time), Some(slot_time)) => {
                    if slot_time < global_time {
                        earliest_time = Some(slot_time);
                    }
                }
                _ => {}
            }
            
            // 早期退出优化：如果找到的时间在当前或更早的时间槽位，
            // 且该时间小于等于下一个槽位的基准时间，则可以确定这是最早的
            // Early exit optimization: if found time is in current or earlier slot,
            // and it's <= next slot's baseline time, we can confirm it's the earliest
            if let Some(time) = earliest_time {
                let next_slot_baseline = self.current_time + self.slot_duration * offset as u32;
                if time <= next_slot_baseline + self.slot_duration {
                    break; // 提前退出，找到了最优解
                }
            }
        }

        // 缓存结果以供后续快速访问
        // Cache the result for subsequent fast access
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
    pub(super) fn calculate_slot_index(&self, expiry_time: Instant) -> usize {
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