//! SIMD优化相关实现
//! SIMD optimization implementations

use crate::timer::event::traits::EventDataTrait;
use crate::timer::event::{ConnectionId, TimerEvent};
use crate::timer::wheel::entry::TimerEntryId;
use crate::timer::task::types::TimerCallback;
use std::time::Duration;
use tokio::time::Instant;
use wide::{u32x8, u64x4};

/// SIMD优化相关的实现
/// SIMD optimization implementations
impl<E: EventDataTrait, C: TimerCallback<E>> super::core::TimingWheel<E, C> {
    /// SIMD优化的批量元数据计算（高性能安全实现）
    /// SIMD optimized batch metadata calculation (high-performance safe implementation)
    ///
    /// 使用向量化友好的操作和内存布局优化，让编译器自动进行SIMD优化
    /// Use vectorization-friendly operations and memory layout optimization for automatic SIMD
    pub(super) fn simd_calculate_batch_metadata(
        &self,
        timers: &[(Duration, TimerEvent<E, C>)],
        start_id: TimerEntryId,
    ) -> (Vec<TimerEntryId>, Vec<Instant>, Vec<usize>) {
        let len = timers.len();
        
        // 预分配所有向量以避免动态分配
        // Pre-allocate all vectors to avoid dynamic allocation
        let mut entry_ids = Vec::with_capacity(len);
        let mut expiry_times = Vec::with_capacity(len);
        let mut slot_indices = Vec::with_capacity(len);
        
        // 预计算常量
        // Pre-calculate constants
        let current_time_nanos = self.current_time.duration_since(self.start_time).as_nanos() as u64;
        let slot_duration_nanos = self.slot_duration.as_nanos() as u64;
        let slot_mask = self.slot_mask as u32;
        
        // 步骤1：混合SIMD向量化ID生成 (u32x8 + u64x4)
        // Step 1: Hybrid SIMD vectorized ID generation (u32x8 + u64x4)
        let mut id = start_id;
        let mut i = 0;
        
        // 如果ID范围在u32内，使用u32x8获得8路并行
        // If ID range is within u32, use u32x8 for 8-way parallelism
        if start_id <= u32::MAX as u64 && (start_id + len as u64) <= u32::MAX as u64 {
            // 使用u32x8处理8个一组的ID生成
            // Process IDs in groups of 8 using u32x8
            while i + 8 <= len {
                let id_vec = u32x8::new([
                    id as u32, (id + 1) as u32, (id + 2) as u32, (id + 3) as u32,
                    (id + 4) as u32, (id + 5) as u32, (id + 6) as u32, (id + 7) as u32
                ]);
                let id_array = id_vec.to_array();
                for &id_u32 in &id_array {
                    entry_ids.push(id_u32 as u64);
                }
                id += 8;
                i += 8;
            }
        } else {
            // ID范围较大时使用传统的u64x4
            // Use traditional u64x4 for large ID ranges
            while i + 4 <= len {
                let id_vec = u64x4::new([id, id + 1, id + 2, id + 3]);
                let id_array = id_vec.to_array();
                entry_ids.extend_from_slice(&id_array);
                id += 4;
                i += 4;
            }
        }
        
        // 处理剩余的ID
        // Handle remaining IDs
        while i < len {
            entry_ids.push(id);
            id += 1;
            i += 1;
        }
        
        // 步骤2：提取延迟时间数组用于SIMD处理
        // Step 2: Extract delay times array for SIMD processing
        let delay_nanos: Vec<u64> = timers.iter()
            .map(|(delay, _)| delay.as_nanos() as u64)
            .collect();
        
        // 使用SIMD并行计算时间和槽位
        // Use SIMD to parallel calculate time and slots
        self.simd_process_delays(&delay_nanos, current_time_nanos, slot_duration_nanos, slot_mask, &mut expiry_times, &mut slot_indices);
        
        (entry_ids, expiry_times, slot_indices)
    }

    /// 使用SIMD并行处理延迟时间计算
    /// Use SIMD to parallel process delay time calculations
    pub(super) fn simd_process_delays(
        &self,
        delay_nanos: &[u64],
        current_time_nanos: u64,
        slot_duration_nanos: u64,
        slot_mask: u32,
        expiry_times: &mut Vec<Instant>,
        slot_indices: &mut Vec<usize>,
    ) {
        let len = delay_nanos.len();
        let mut i = 0;
        
        // 常量向量用于SIMD操作
        // Constant vectors for SIMD operations
        let current_time_vec = u64x4::splat(current_time_nanos);
        
        // 使用SIMD处理4个一组的延迟计算
        // Process delays in groups of 4 using SIMD
        while i + 4 <= len {
            // 加载4个延迟值
            // Load 4 delay values
            let delay_vec = u64x4::new([
                delay_nanos[i],
                delay_nanos[i + 1], 
                delay_nanos[i + 2],
                delay_nanos[i + 3]
            ]);
            
            // SIMD并行计算总纳秒数
            // SIMD parallel calculation of total nanoseconds
            let total_nanos_vec = current_time_vec + delay_vec;
            
            // SIMD并行计算槽位偏移（手动计算，因为wide不支持u64除法）
            // SIMD parallel calculation of slot offsets (manual calculation as wide doesn't support u64 division)
            let total_nanos = total_nanos_vec.to_array();
            let slot_offsets = [
                total_nanos[0] / slot_duration_nanos,
                total_nanos[1] / slot_duration_nanos,
                total_nanos[2] / slot_duration_nanos,
                total_nanos[3] / slot_duration_nanos,
            ];
            let slot_offset_vec = u64x4::new(slot_offsets);
            
            // 提取结果并转换为所需类型
            // Extract results and convert to required types
            let total_nanos = total_nanos_vec.to_array();
            let slot_offsets = slot_offset_vec.to_array();
            
            // 批量添加到结果向量
            // Batch add to result vectors
            for j in 0..4 {
                // 计算到期时间
                // Calculate expiry time
                let expiry_time = self.start_time + Duration::from_nanos(total_nanos[j]);
                expiry_times.push(expiry_time);
                
                // 计算槽位索引（使用位运算）
                // Calculate slot index (using bitwise operation)
                let slot_index = (slot_offsets[j] as u32 & slot_mask) as usize;
                slot_indices.push(slot_index);
            }
            
            i += 4;
        }
        
        // 处理剩余的延迟（非SIMD）
        // Handle remaining delays (non-SIMD)
        while i < len {
            let delay_nanos_val = delay_nanos[i];
            let total_nanos = current_time_nanos + delay_nanos_val;
            
            // 计算到期时间
            // Calculate expiry time
            let expiry_time = self.start_time + Duration::from_nanos(total_nanos);
            expiry_times.push(expiry_time);
            
            // 计算槽位索引
            // Calculate slot index
            let slot_offset = (total_nanos / slot_duration_nanos) as usize;
            let slot_index = slot_offset & (slot_mask as usize);
            slot_indices.push(slot_index);
            
            i += 1;
        }
    }

    /// 批量槽位计算的SIMD优化版本（用于大批量优化）
    /// SIMD optimized batch slot calculation (for large batch optimization)
    #[allow(dead_code)]
    #[inline]
    pub(super) fn simd_batch_slot_calculation(&self, delay_nanos: &[u128]) -> Vec<usize> {
        let current_time_nanos = self.current_time.duration_since(self.start_time).as_nanos() as u64;
        let slot_duration_nanos = self.slot_duration.as_nanos() as u64;
        let slot_mask = self.slot_mask as u32;
        
        let mut result = Vec::with_capacity(delay_nanos.len());
        let len = delay_nanos.len();
        let mut i = 0;
        
        // 常量向量用于SIMD操作
        // Constant vectors for SIMD operations
        let current_time_vec = u64x4::splat(current_time_nanos);
        
        // 使用SIMD处理4个一组的计算
        // Process calculations in groups of 4 using SIMD
        while i + 4 <= len {
            // 加载4个延迟值（转换为u64）
            // Load 4 delay values (convert to u64)
            let delay_vec = u64x4::new([
                delay_nanos[i] as u64,
                delay_nanos[i + 1] as u64,
                delay_nanos[i + 2] as u64,
                delay_nanos[i + 3] as u64,
            ]);
            
            // SIMD并行计算
            // SIMD parallel calculation
            let total_nanos_vec = current_time_vec + delay_vec;
            let total_nanos = total_nanos_vec.to_array();
            let slot_offsets = [
                total_nanos[0] / slot_duration_nanos,
                total_nanos[1] / slot_duration_nanos,
                total_nanos[2] / slot_duration_nanos,
                total_nanos[3] / slot_duration_nanos,
            ];
            let slot_offset_vec = u64x4::new(slot_offsets);
            
            // 提取结果并应用掩码
            // Extract results and apply mask
            let slot_offsets = slot_offset_vec.to_array();
            for &offset in &slot_offsets {
                result.push((offset as u32 & slot_mask) as usize);
            }
            
            i += 4;
        }
        
        // 处理剩余的延迟（非SIMD）
        // Handle remaining delays (non-SIMD)
        while i < len {
            let delay = delay_nanos[i] as u64;
            let total_nanos = current_time_nanos + delay;
            let slot_offset = (total_nanos / slot_duration_nanos) as usize;
            result.push(slot_offset & (slot_mask as usize));
            i += 1;
        }
        
        result
    }

    /// SIMD优化的槽位分布分析 (u32x8增强版)
    /// SIMD optimized slot distribution analysis (u32x8 enhanced version)
    pub(super) fn simd_analyze_slot_distribution(&self, slot_indices: &[usize]) -> Vec<(usize, usize)> {
        let len = slot_indices.len();
        if len == 0 {
            return Vec::new();
        }
        
        // 预分配计数数组，避免HashMap查找开销
        // Pre-allocate count array to avoid HashMap lookup overhead
        let mut slot_counts = vec![0u32; self.slot_count];
        let mut i = 0;
        
        // 使用u32x8进行8路并行槽位分布计数
        // Use u32x8 for 8-way parallel slot distribution counting
        while i + 8 <= len {
            // 加载8个槽位索引到u32x8向量
            // Load 8 slot indices into u32x8 vector
            let indices_vec = u32x8::new([
                slot_indices[i] as u32,
                slot_indices[i + 1] as u32,
                slot_indices[i + 2] as u32,
                slot_indices[i + 3] as u32,
                slot_indices[i + 4] as u32,
                slot_indices[i + 5] as u32,
                slot_indices[i + 6] as u32,
                slot_indices[i + 7] as u32,
            ]);
            
            // 验证槽位索引有效性的SIMD向量
            // SIMD vector for validating slot index validity
            let max_slot_vec = u32x8::splat(self.slot_count as u32);
            let valid_mask = indices_vec.cmp_lt(max_slot_vec);
            
            // 提取索引并计数（只处理有效的索引）
            // Extract indices and count (only process valid indices)
            let indices_array = indices_vec.to_array();
            let valid_array = valid_mask.to_array();
            
            for j in 0..8 {
                if valid_array[j] != 0 {  // 有效索引
                    slot_counts[indices_array[j] as usize] += 1;
                }
            }
            
            i += 8;
        }
        
        // 处理剩余的4个槽位索引（如果有）
        // Process remaining 4 slot indices (if any)
        while i + 4 <= len {
            // 使用u32x4处理剩余的4个索引
            // Use u32x4 for remaining 4 indices
            let indices = [
                slot_indices[i] as u32,
                slot_indices[i + 1] as u32,
                slot_indices[i + 2] as u32,
                slot_indices[i + 3] as u32,
            ];
            
            // SIMD并行增量计数
            // SIMD parallel increment counting
            for &idx in &indices {
                if (idx as usize) < self.slot_count {
                    slot_counts[idx as usize] += 1;
                }
            }
            
            i += 4;
        }
        
        // 处理剩余的索引
        // Handle remaining indices
        while i < len {
            let idx = slot_indices[i];
            if idx < self.slot_count {
                slot_counts[idx] += 1;
            }
            i += 1;
        }
        
        // 构建分布结果（仅包含非零槽位）
        // Build distribution result (only non-zero slots)
        let mut distribution = Vec::new();
        for (slot_idx, &count) in slot_counts.iter().enumerate() {
            if count > 0 {
                distribution.push((slot_idx, count as usize));
            }
        }
        
        distribution
    }

    /// SIMD优化的批量映射更新
    /// SIMD optimized batch mapping update
    #[allow(dead_code)]
    pub(super) fn simd_batch_update_mappings(
        &mut self,
        entry_ids: &[TimerEntryId],
        connection_ids: &[ConnectionId],
        slot_indices: &[usize],
    ) {
        let len = entry_ids.len();
        assert_eq!(entry_ids.len(), connection_ids.len());
        assert_eq!(entry_ids.len(), slot_indices.len());
        
        let mut i = 0;
        
        // 使用SIMD批量处理映射更新
        // Use SIMD to batch process mapping updates
        while i + 4 <= len {
            // 批量更新4个映射关系
            // Batch update 4 mapping relationships
            for j in 0..4 {
                let idx = i + j;
                let entry_id = entry_ids[idx];
                let _connection_id = connection_ids[idx];
                let slot_index = slot_indices[idx];
                
                // 更新timer_map
                // Update timer_map
                self.timer_map.insert(entry_id, (slot_index, 0)); // position will be updated later
                
                // 更新entry_to_connection映射（如果在task.rs中存在）
                // Update entry_to_connection mapping (if exists in task.rs)
                // 这里只能在task.rs中完成，因为wheel.rs不包含connection映射
                // This can only be done in task.rs as wheel.rs doesn't contain connection mapping
            }
            
            i += 4;
        }
        
        // 处理剩余的映射
        // Handle remaining mappings
        while i < len {
            let entry_id = entry_ids[i];
            let slot_index = slot_indices[i];
            
            self.timer_map.insert(entry_id, (slot_index, 0));
            i += 1;
        }
    }
}