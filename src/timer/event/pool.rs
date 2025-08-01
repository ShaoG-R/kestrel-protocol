//! 定时器事件数据对象池
//! Timer Event Data Object Pool
//!
//! 提供局部管理的高性能对象池，直接在使用组件中持有和管理
//! Provides locally managed high-performance object pools, held and managed directly in using components

use std::sync::Arc;
use crossbeam_queue::SegQueue;
use wide::{u32x4, u32x8};
use crate::timer::event::traits::EventDataTrait;
use crate::timer::event::{TimerEventData, ConnectionId};

/// 池配置
/// Pool configuration
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// 最大池大小
    /// Maximum pool size
    pub max_size: usize,
    /// 初始预分配大小
    /// Initial pre-allocation size
    pub initial_size: usize,
    /// 是否启用批量操作优化
    /// Whether to enable batch operation optimization
    pub enable_batch_optimization: bool,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_size: 1024,
            initial_size: 64,
            enable_batch_optimization: true,
        }
    }
}

/// 定时器事件数据对象池
/// Timer event data object pool
pub struct TimerEventPool<E: EventDataTrait> {
    pool: Arc<SegQueue<TimerEventData<E>>>,
    config: PoolConfig,
}

impl<E: EventDataTrait> TimerEventPool<E> {
    /// 创建新的对象池
    /// Create new object pool
    pub fn new(config: PoolConfig) -> Self {
        let pool = Arc::new(SegQueue::new());
        
        // 预分配对象
        // Pre-allocate objects
        for _ in 0..config.initial_size {
            let dummy_data = TimerEventData::new(0, E::default());
            pool.push(dummy_data);
        }
        
        Self { pool, config }
    }

    /// 使用默认配置创建对象池
    /// Create object pool with default configuration
    pub fn new_default() -> Self {
        Self::new(PoolConfig::default())
    }

    /// 使用指定容量创建对象池
    /// Create object pool with specified capacity
    pub fn with_capacity(max_size: usize) -> Self {
        Self::new(PoolConfig {
            max_size,
            initial_size: (max_size / 16).max(4).min(64), // 合理的初始大小
            enable_batch_optimization: true,
        })
    }

    /// 从池中获取对象
    /// Acquire object from pool
    pub fn acquire(&self, connection_id: ConnectionId, timeout_event: E) -> TimerEventData<E> {
        match self.pool.pop() {
            Some(mut obj) => {
                obj.connection_id = connection_id;
                obj.timeout_event = timeout_event;
                obj
            }
            None => TimerEventData::new(connection_id, timeout_event),
        }
    }

    /// 释放对象到池中
    /// Release object to pool
    pub fn release(&self, obj: TimerEventData<E>) {
        if self.pool.len() < self.config.max_size {
            self.pool.push(obj);
        }
    }

    /// 批量获取对象（SIMD优化）
    /// Batch acquire objects (SIMD optimized)
    pub fn batch_acquire(&self, requests: &[(ConnectionId, E)]) -> Vec<TimerEventData<E>> {
        if !self.config.enable_batch_optimization || requests.is_empty() {
            return requests.iter()
                .map(|(conn_id, event)| self.acquire(*conn_id, event.clone()))
                .collect();
        }

        let count = requests.len();
        let mut result = Vec::with_capacity(count);
        
        // 从池中获取尽可能多的对象
        // Acquire as many objects as possible from pool
        for _ in 0..count {
            if let Some(obj) = self.pool.pop() {
                result.push(obj);
            } else {
                break;
            }
        }

        // SIMD优化的批量更新和创建
        // SIMD optimized batch update and create
        self.simd_update_and_create(&mut result, requests);
        result
    }

    /// 批量释放对象
    /// Batch release objects
    pub fn batch_release(&self, objects: Vec<TimerEventData<E>>) {
        let current_len = self.pool.len();
        let available_capacity = self.config.max_size.saturating_sub(current_len);
        let to_release = objects.len().min(available_capacity);

        for obj in objects.into_iter().take(to_release) {
            self.pool.push(obj);
        }
    }

    /// SIMD优化的批量更新与创建
    /// SIMD optimized batch update and create
    fn simd_update_and_create(
        &self,
        result: &mut Vec<TimerEventData<E>>,
        requests: &[(ConnectionId, E)],
    ) {
        let total_requests = requests.len();
        let acquired_count = result.len();
        let mut i = 0;

        // SIMD更新已从池中获取的对象
        // SIMD update for objects acquired from the pool
        while i + 8 <= acquired_count {
            self.simd_process_chunk(i, requests, &mut result[i..i+8]);
            i += 8;
        }
        while i + 4 <= acquired_count {
            self.simd_process_chunk_4(i, requests, &mut result[i..i+4]);
            i += 4;
        }

        // 标量更新剩余的已获取对象
        // Scalar update for remaining acquired objects
        while i < acquired_count {
            let (conn_id, timeout_event) = requests[i].clone();
            result[i].connection_id = conn_id;
            result[i].timeout_event = timeout_event;
            i += 1;
        }
        
        // 创建剩余的新对象
        // Create remaining new objects
        let mut new_creations = Vec::with_capacity(total_requests - acquired_count);
        let mut j = acquired_count;
        while j + 8 <= total_requests {
            self.simd_create_chunk(j, requests, &mut new_creations);
            j += 8;
        }
        while j + 4 <= total_requests {
            self.simd_create_chunk_4(j, requests, &mut new_creations);
            j += 4;
        }

        // 标量创建最后剩余的对象
        // Scalar create for the final remaining objects
        while j < total_requests {
            let (conn_id, timeout_event) = requests[j].clone();
            new_creations.push(TimerEventData::new(conn_id, timeout_event));
            j += 1;
        }
        
        result.append(&mut new_creations);
    }

    #[inline]
    fn simd_process_chunk(&self, start_index: usize, requests: &[(ConnectionId, E)], chunk: &mut [TimerEventData<E>]) {
        let conn_ids = [
            requests[start_index].0, requests[start_index + 1].0, 
            requests[start_index + 2].0, requests[start_index + 3].0,
            requests[start_index + 4].0, requests[start_index + 5].0, 
            requests[start_index + 6].0, requests[start_index + 7].0,
        ];
        let conn_id_vec = u32x8::new(conn_ids);
        let processed_ids = conn_id_vec.to_array();
        
        for k in 0..8 {
            chunk[k].connection_id = processed_ids[k];
            chunk[k].timeout_event = requests[start_index + k].1.clone();
        }
    }

    #[inline]
    fn simd_process_chunk_4(&self, start_index: usize, requests: &[(ConnectionId, E)], chunk: &mut [TimerEventData<E>]) {
        let conn_ids = [
            requests[start_index].0, requests[start_index + 1].0, 
            requests[start_index + 2].0, requests[start_index + 3].0,
        ];
        let conn_id_vec = u32x4::new(conn_ids);
        let processed_ids = conn_id_vec.to_array();
        
        for k in 0..4 {
            chunk[k].connection_id = processed_ids[k];
            chunk[k].timeout_event = requests[start_index + k].1.clone();
        }
    }

    #[inline]
    fn simd_create_chunk(&self, start_index: usize, requests: &[(ConnectionId, E)], target_vec: &mut Vec<TimerEventData<E>>) {
        let conn_ids = [
            requests[start_index].0, requests[start_index + 1].0, 
            requests[start_index + 2].0, requests[start_index + 3].0,
            requests[start_index + 4].0, requests[start_index + 5].0, 
            requests[start_index + 6].0, requests[start_index + 7].0,
        ];
        let conn_id_vec = u32x8::new(conn_ids);
        let processed_ids = conn_id_vec.to_array();
        
        for k in 0..8 {
            target_vec.push(TimerEventData::new(processed_ids[k], requests[start_index + k].1.clone()));
        }
    }

    #[inline]
    fn simd_create_chunk_4(&self, start_index: usize, requests: &[(ConnectionId, E)], target_vec: &mut Vec<TimerEventData<E>>) {
        let conn_ids = [
            requests[start_index].0, requests[start_index + 1].0, 
            requests[start_index + 2].0, requests[start_index + 3].0,
        ];
        let conn_id_vec = u32x4::new(conn_ids);
        let processed_ids = conn_id_vec.to_array();
        
        for k in 0..4 {
            target_vec.push(TimerEventData::new(processed_ids[k], requests[start_index + k].1.clone()));
        }
    }

    /// 获取池状态统计
    /// Get pool statistics
    pub fn stats(&self) -> PoolStats {
        PoolStats {
            current_size: self.pool.len(),
            max_size: self.config.max_size,
            initial_size: self.config.initial_size,
        }
    }

    /// 克隆池引用（用于在多个地方共享同一个池）
    /// Clone pool reference (for sharing the same pool across multiple places)
    pub fn clone_ref(&self) -> Self {
        Self {
            pool: self.pool.clone(),
            config: self.config.clone(),
        }
    }

    /// 获取池配置
    /// Get pool configuration
    pub fn config(&self) -> &PoolConfig {
        &self.config
    }
}

/// 池统计信息
/// Pool statistics
#[derive(Debug, Clone)]
pub struct PoolStats {
    pub current_size: usize,
    pub max_size: usize,
    pub initial_size: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[derive(Debug, Clone, Copy, Default, PartialEq)]
    struct TestEvent(u32);
    
    #[test]
    fn test_timer_event_pool_basic_operations() {
        let pool = TimerEventPool::new(PoolConfig::default());
        
        // 测试获取和释放
        let data = pool.acquire(123, TestEvent(456));
        assert_eq!(data.connection_id, 123);
        assert_eq!(data.timeout_event, TestEvent(456));
        
        pool.release(data);
        assert!(pool.stats().current_size > 0);
    }
    
    #[test]
    fn test_pool_with_capacity() {
        let pool = TimerEventPool::<TestEvent>::with_capacity(512);
        
        assert_eq!(pool.config().max_size, 512);
        assert!(pool.config().initial_size > 0);
        assert!(pool.config().enable_batch_optimization);
    }
    
    #[test]
    fn test_pool_clone_ref() {
        let pool1 = TimerEventPool::<TestEvent>::new_default();
        let pool2 = pool1.clone_ref();
        
        // 应该共享同一个底层队列
        let data = pool1.acquire(123, TestEvent(456));
        pool2.release(data);
        
        // 从 pool1 获取，应该能获取到刚刚 pool2 释放的对象
        assert!(pool1.stats().current_size > 0);
    }
    
    #[test]
    fn test_batch_operations() {
        let pool = TimerEventPool::<TestEvent>::new_default();
        
        let requests = vec![
            (1, TestEvent(100)),
            (2, TestEvent(200)),
            (3, TestEvent(300)),
        ];
        
        let objects = pool.batch_acquire(&requests);
        assert_eq!(objects.len(), 3);
        
        for (i, obj) in objects.iter().enumerate() {
            assert_eq!(obj.connection_id, requests[i].0);
            assert_eq!(obj.timeout_event, requests[i].1);
        }
        
        pool.batch_release(objects);
        assert!(pool.stats().current_size > 0);
    }
}