//! 定时器事件数据池管理器
//! Timer Event Data Pool Manager
//!
//! 提供局部手动管理的对象池，支持不同类型的定时器事件数据复用
//! Provides locally managed object pools for different types of timer event data reuse

use std::sync::OnceLock;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::any::TypeId;
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

/// 类型化对象池
/// Typed object pool
pub struct TypedPool<E: EventDataTrait> {
    pool: Arc<SegQueue<TimerEventData<E>>>,
    config: PoolConfig,
}

impl<E: EventDataTrait> TypedPool<E> {
    /// 创建新的类型化池
    /// Create new typed pool
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
                .map(|(conn_id, event)| self.acquire(*conn_id, *event))
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
            let (conn_id, timeout_event) = requests[i];
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
            let (conn_id, timeout_event) = requests[j];
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
            chunk[k].timeout_event = requests[start_index + k].1;
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
            chunk[k].timeout_event = requests[start_index + k].1;
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
            target_vec.push(TimerEventData::new(processed_ids[k], requests[start_index + k].1));
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
            target_vec.push(TimerEventData::new(processed_ids[k], requests[start_index + k].1));
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
}

/// 池统计信息
/// Pool statistics
#[derive(Debug, Clone)]
pub struct PoolStats {
    pub current_size: usize,
    pub max_size: usize,
    pub initial_size: usize,
}

/// 池管理器，支持多种类型的对象池
/// Pool manager, supporting multiple types of object pools
pub struct PoolManager {
    pools: RwLock<HashMap<TypeId, Box<dyn std::any::Any + Send + Sync>>>,
    default_config: PoolConfig,
}

impl PoolManager {
    /// 创建新的池管理器
    /// Create new pool manager
    pub fn new(default_config: PoolConfig) -> Self {
        Self {
            pools: RwLock::new(HashMap::new()),
            default_config,
        }
    }

    /// 获取或创建指定类型的池
    /// Get or create pool for specified type
    pub fn get_or_create_pool<E: EventDataTrait>(&self) -> Arc<TypedPool<E>> {
        let type_id = TypeId::of::<E>();
        
        // 尝试从已有池中获取
        // Try to get from existing pools
        {
            let pools = self.pools.read().unwrap();
            if let Some(pool_any) = pools.get(&type_id) {
                if let Some(pool) = pool_any.downcast_ref::<Arc<TypedPool<E>>>() {
                    return pool.clone();
                }
            }
        }

        // 创建新池
        // Create new pool
        let new_pool = Arc::new(TypedPool::new(self.default_config.clone()));
        let pool_clone = new_pool.clone();
        
        {
            let mut pools = self.pools.write().unwrap();
            pools.insert(type_id, Box::new(new_pool));
        }
        
        pool_clone
    }

    /// 获取指定类型池的统计信息
    /// Get statistics for specified type pool
    pub fn get_pool_stats<E: EventDataTrait>(&self) -> Option<PoolStats> {
        let type_id = TypeId::of::<E>();
        let pools = self.pools.read().unwrap();
        
        pools.get(&type_id)
            .and_then(|pool_any| pool_any.downcast_ref::<Arc<TypedPool<E>>>())
            .map(|pool| pool.stats())
    }
}

/// 全局池管理器实例
/// Global pool manager instance
static GLOBAL_POOL_MANAGER: OnceLock<PoolManager> = OnceLock::new();

/// 获取全局池管理器
/// Get global pool manager
pub fn global_pool_manager() -> &'static PoolManager {
    GLOBAL_POOL_MANAGER.get_or_init(|| {
        PoolManager::new(PoolConfig::default())
    })
}

/// 获取指定类型的对象池
/// Get object pool for specified type
pub fn get_pool<E: EventDataTrait>() -> Arc<TypedPool<E>> {
    global_pool_manager().get_or_create_pool::<E>()
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[derive(Debug, Clone, Copy, Default, PartialEq)]
    struct TestEvent(u32);
    
    #[test]
    fn test_typed_pool_basic_operations() {
        let pool = TypedPool::new(PoolConfig::default());
        
        // 测试获取和释放
        let data = pool.acquire(123, TestEvent(456));
        assert_eq!(data.connection_id, 123);
        assert_eq!(data.timeout_event, TestEvent(456));
        
        pool.release(data);
        assert!(pool.stats().current_size > 0);
    }
    
    #[test]
    fn test_pool_manager() {
        let manager = PoolManager::new(PoolConfig::default());
        
        // 获取不同类型的池
        let pool1 = manager.get_or_create_pool::<TestEvent>();
        let pool2 = manager.get_or_create_pool::<TestEvent>();
        
        // 应该是同一个池的引用
        assert!(Arc::ptr_eq(&pool1, &pool2));
    }
    
    #[test]
    fn test_global_pool_manager() {
        let pool1 = get_pool::<TestEvent>();
        let pool2 = get_pool::<TestEvent>();
        
        // 全局管理器应该返回同一个池
        assert!(Arc::ptr_eq(&pool1, &pool2));
    }
}