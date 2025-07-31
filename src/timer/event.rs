//! 定时器事件定义
//! Timer Event Definitions
//!
//! 该模块定义了定时器系统中使用的所有事件类型和数据结构。
//!
//! This module defines all event types and data structures used in the timer system.

use std::fmt;
use std::sync::Mutex;
use tokio::sync::mpsc;
use crate::core::endpoint::timing::TimeoutEvent;
use wide::{u32x4, u32x8};

/// 定时器事件ID，用于唯一标识一个定时器
/// Timer event ID, used to uniquely identify a timer
pub type TimerEventId = u64;

/// 连接ID，用于标识定时器属于哪个连接
/// Connection ID, used to identify which connection a timer belongs to
pub type ConnectionId = u32;

/// 高性能对象池，用于复用TimerEventData对象
/// High-performance object pool for reusing TimerEventData objects
pub struct TimerEventDataPool {
    pool: Mutex<Vec<Box<TimerEventData>>>,
    max_size: usize,
    /// 批量操作缓存，避免频繁锁定
    /// Batch operation cache to avoid frequent locking
    batch_cache: Mutex<Vec<Box<TimerEventData>>>,
    batch_cache_size: usize,
}

impl TimerEventDataPool {
    /// 创建新的对象池
    /// Create new object pool
    pub fn new(max_size: usize) -> Self {
        let batch_cache_size = (max_size / 4).max(64); // 批量缓存大小为池大小的1/4，最少64个
        Self {
            pool: Mutex::new(Vec::with_capacity(max_size)),
            max_size,
            batch_cache: Mutex::new(Vec::with_capacity(batch_cache_size)),
            batch_cache_size,
        }
    }

    /// 从池中获取对象，如果池为空则创建新对象
    /// Get object from pool, create new if pool is empty
    pub fn acquire(&self, connection_id: ConnectionId, timeout_event: TimeoutEvent) -> Box<TimerEventData> {
        if let Ok(mut pool) = self.pool.lock() {
            if let Some(mut obj) = pool.pop() {
                // 重用现有对象
                // Reuse existing object
                obj.connection_id = connection_id;
                obj.timeout_event = timeout_event;
                return obj;
            }
        }
        // 池为空或锁定失败，创建新对象
        // Pool empty or lock failed, create new object
        Box::new(TimerEventData::new(connection_id, timeout_event))
    }

    /// 将对象返回池中以供重用
    /// Return object to pool for reuse
    pub fn release(&self, obj: Box<TimerEventData>) {
        if let Ok(mut pool) = self.pool.lock() {
            if pool.len() < self.max_size {
                pool.push(obj);
            }
            // 如果池已满，对象会被自动丢弃（正常GC）
            // If pool is full, object will be automatically dropped (normal GC)
        }
    }

    /// 批量获取对象（SIMD优化版本）
    /// Batch acquire objects (SIMD optimized version)
    pub fn batch_acquire(&self, requests: &[(ConnectionId, TimeoutEvent)]) -> Vec<Box<TimerEventData>> {
        let count = requests.len();
        if count == 0 {
            return Vec::new();
        }

        let mut result = Vec::with_capacity(count);
        
        // 尝试从批量缓存获取，使用SIMD优化批量处理
        // Try to get from batch cache first, use SIMD optimized batch processing
        if let Ok(mut batch_cache) = self.batch_cache.try_lock() {
            self.simd_fill_from_cache(&mut result, requests, &mut batch_cache);
        }
        
        // 如果批量缓存不够，从主池获取，使用SIMD优化  
        // If batch cache is not enough, get from main pool with SIMD optimization
        if result.len() < count {
            if let Ok(mut pool) = self.pool.lock() {
                self.simd_fill_from_pool(&mut result, requests, &mut pool);
            }
        }
        
        // 对于剩余的请求，使用SIMD批量创建新对象
        // For remaining requests, use SIMD batch create new objects
        if result.len() < count {
            self.simd_create_remaining_objects(&mut result, requests);
        }
        
        result
    }

    /// SIMD优化的批量从缓存填充
    /// SIMD optimized batch fill from cache
    fn simd_fill_from_cache(
        &self,
        result: &mut Vec<Box<TimerEventData>>,
        requests: &[(ConnectionId, TimeoutEvent)],
        batch_cache: &mut Vec<Box<TimerEventData>>,
    ) {
        let count = requests.len();
        let mut i = result.len();
        
        // 使用u32x8批量处理8个一组的连接ID
        // Use u32x8 to batch process 8 connection IDs at a time
        while i + 8 <= count && batch_cache.len() >= 8 {
            // 提取8个连接ID进行SIMD预处理
            // Extract 8 connection IDs for SIMD preprocessing
            let conn_ids = [
                requests[i].0,     requests[i + 1].0, requests[i + 2].0, requests[i + 3].0,
                requests[i + 4].0, requests[i + 5].0, requests[i + 6].0, requests[i + 7].0,
            ];
            
            // SIMD验证连接ID范围和批量预处理
            // SIMD validate connection ID range and batch preprocessing
            let conn_id_vec = u32x8::new(conn_ids);
            let validated_ids = conn_id_vec.to_array();
            
            // 批量更新8个对象
            // Batch update 8 objects
            for j in 0..8 {
                if let Some(mut obj) = batch_cache.pop() {
                    obj.connection_id = validated_ids[j];
                    obj.timeout_event = requests[i + j].1.clone();
                    result.push(obj);
                }
            }
            
            i += 8;
        }
        
        // 处理剩余的4个对象（如果有）
        // Process remaining 4 objects (if any)
        while i + 4 <= count && batch_cache.len() >= 4 {
            // 使用u32x4处理剩余的4个连接ID
            // Use u32x4 for remaining 4 connection IDs
            let conn_ids = [
                requests[i].0,
                requests[i + 1].0,
                requests[i + 2].0,
                requests[i + 3].0,
            ];
            
            let conn_id_vec = u32x4::new(conn_ids);
            let validated_ids = conn_id_vec.to_array();
            
            for j in 0..4 {
                if let Some(mut obj) = batch_cache.pop() {
                    obj.connection_id = validated_ids[j];
                    obj.timeout_event = requests[i + j].1.clone();
                    result.push(obj);
                }
            }
            
            i += 4;
        }
        
        // 处理剩余对象
        // Handle remaining objects
        while i < count && !batch_cache.is_empty() {
            if let Some(mut obj) = batch_cache.pop() {
                let (connection_id, timeout_event) = &requests[i];
                obj.connection_id = *connection_id;
                obj.timeout_event = timeout_event.clone();
                result.push(obj);
                i += 1;
            }
        }
    }

    /// SIMD优化的批量从主池填充
    /// SIMD optimized batch fill from main pool
    fn simd_fill_from_pool(
        &self,
        result: &mut Vec<Box<TimerEventData>>,
        requests: &[(ConnectionId, TimeoutEvent)],
        pool: &mut Vec<Box<TimerEventData>>,
    ) {
        let count = requests.len();
        let mut i = result.len();
        
        // 使用u32x8进行8路并行连接ID处理
        // Use u32x8 for 8-way parallel connection ID processing
        while i + 8 <= count && pool.len() >= 8 {
            // SIMD并行处理8个连接ID
            // SIMD parallel process 8 connection IDs
            let conn_ids = [
                requests[i].0,     requests[i + 1].0, requests[i + 2].0, requests[i + 3].0,
                requests[i + 4].0, requests[i + 5].0, requests[i + 6].0, requests[i + 7].0,
            ];
            let conn_id_vec = u32x8::new(conn_ids);
            let processed_ids = conn_id_vec.to_array();
            
            // 批量更新8个对象
            // Batch update 8 objects
            for j in 0..8 {
                if let Some(mut obj) = pool.pop() {
                    obj.connection_id = processed_ids[j];
                    obj.timeout_event = requests[i + j].1.clone();
                    result.push(obj);
                }
            }
            
            i += 8;
        }
        
        // 处理剩余的4个对象（如果有）
        // Process remaining 4 objects (if any)
        while i + 4 <= count && pool.len() >= 4 {
            // 使用u32x4处理剩余的连接ID
            // Use u32x4 for remaining connection IDs
            let conn_ids = [
                requests[i].0,
                requests[i + 1].0,
                requests[i + 2].0,
                requests[i + 3].0,
            ];
            let conn_id_vec = u32x4::new(conn_ids);
            let processed_ids = conn_id_vec.to_array();
            
            for j in 0..4 {
                if let Some(mut obj) = pool.pop() {
                    obj.connection_id = processed_ids[j];
                    obj.timeout_event = requests[i + j].1.clone();
                    result.push(obj);
                }
            }
            
            i += 4;
        }
        
        // 处理剩余对象
        // Handle remaining objects
        while i < count && !pool.is_empty() {
            if let Some(mut obj) = pool.pop() {
                let (connection_id, timeout_event) = &requests[i];
                obj.connection_id = *connection_id;
                obj.timeout_event = timeout_event.clone();
                result.push(obj);
                i += 1;
            }
        }
    }

    /// SIMD优化的批量创建剩余对象
    /// SIMD optimized batch create remaining objects
    fn simd_create_remaining_objects(
        &self,
        result: &mut Vec<Box<TimerEventData>>,
        requests: &[(ConnectionId, TimeoutEvent)],
    ) {
        let count = requests.len();
        let mut i = result.len();
        
        // 使用u32x8进行8路并行对象创建
        // Use u32x8 for 8-way parallel object creation
        while i + 8 <= count {
            // SIMD处理8个连接ID
            // SIMD process 8 connection IDs
            let conn_ids = [
                requests[i].0,     requests[i + 1].0, requests[i + 2].0, requests[i + 3].0,
                requests[i + 4].0, requests[i + 5].0, requests[i + 6].0, requests[i + 7].0,
            ];
            let conn_id_vec = u32x8::new(conn_ids);
            let processed_ids = conn_id_vec.to_array();
            
            // 批量创建8个新对象
            // Batch create 8 new objects
            for j in 0..8 {
                let connection_id = processed_ids[j];
                let timeout_event = &requests[i + j].1;
                result.push(Box::new(TimerEventData::new(connection_id, timeout_event.clone())));
            }
            
            i += 8;
        }
        
        // 处理剩余的4个对象（如果有）
        // Process remaining 4 objects (if any)
        while i + 4 <= count {
            // 使用u32x4处理剩余的连接ID
            // Use u32x4 for remaining connection IDs
            let conn_ids = [
                requests[i].0,
                requests[i + 1].0,
                requests[i + 2].0,
                requests[i + 3].0,
            ];
            let conn_id_vec = u32x4::new(conn_ids);
            let processed_ids = conn_id_vec.to_array();
            
            for j in 0..4 {
                let connection_id = processed_ids[j];
                let timeout_event = &requests[i + j].1;
                result.push(Box::new(TimerEventData::new(connection_id, timeout_event.clone())));
            }
            
            i += 4;
        }
        
        // 创建剩余对象
        // Create remaining objects
        while i < count {
            let (connection_id, timeout_event) = &requests[i];
            result.push(Box::new(TimerEventData::new(*connection_id, timeout_event.clone())));
            i += 1;
        }
    }

    /// 批量释放对象（高性能版本）
    /// Batch release objects (high-performance version)
    pub fn batch_release(&self, objects: Vec<Box<TimerEventData>>) {
        if objects.is_empty() {
            return;
        }

        let mut remaining = objects;
        
        // 优先放入批量缓存
        // Prioritize batch cache
        if let Ok(mut batch_cache) = self.batch_cache.try_lock() {
            while batch_cache.len() < self.batch_cache_size && !remaining.is_empty() {
                batch_cache.push(remaining.pop().unwrap());
            }
        }
        
        // 剩余的放入主池
        // Put remaining into main pool
        if !remaining.is_empty() {
            if let Ok(mut pool) = self.pool.lock() {
                for obj in remaining {
                    if pool.len() < self.max_size {
                        pool.push(obj);
                    }
                    // 如果池已满，对象会被自动丢弃（正常GC）
                    // If pool is full, objects will be automatically dropped (normal GC)
                }
            }
        }
    }

    /// 获取池中当前对象数量（用于调试）
    /// Get current number of objects in pool (for debugging)
    pub fn size(&self) -> usize {
        let pool_size = self.pool.lock().map(|pool| pool.len()).unwrap_or(0);
        let cache_size = self.batch_cache.lock().map(|cache| cache.len()).unwrap_or(0);
        pool_size + cache_size
    }
}

/// 全局定时器事件数据对象池
/// Global timer event data object pool
static TIMER_EVENT_DATA_POOL: once_cell::sync::Lazy<TimerEventDataPool> = 
    once_cell::sync::Lazy::new(|| TimerEventDataPool::new(1024));

/// 定时器事件数据
/// Timer event data
#[derive(Debug, Clone)]
pub struct TimerEventData {
    /// 连接ID
    /// Connection ID
    pub connection_id: ConnectionId,
    /// 超时事件类型
    /// Timeout event type
    pub timeout_event: TimeoutEvent,
}

impl TimerEventData {
    /// 创建新的定时器事件数据
    /// Create new timer event data
    pub fn new(connection_id: ConnectionId, timeout_event: TimeoutEvent) -> Self {
        Self {
            connection_id,
            timeout_event,
        }
    }

    /// 使用对象池高效创建定时器事件数据
    /// Efficiently create timer event data using object pool
    pub fn from_pool(connection_id: ConnectionId, timeout_event: TimeoutEvent) -> Box<Self> {
        TIMER_EVENT_DATA_POOL.acquire(connection_id, timeout_event)
    }

    /// 批量创建定时器事件数据（高性能版本）
    /// Batch create timer event data (high-performance version)
    pub fn batch_from_pool(requests: &[(ConnectionId, TimeoutEvent)]) -> Vec<Box<Self>> {
        TIMER_EVENT_DATA_POOL.batch_acquire(requests)
    }

    /// 将对象返回到对象池中
    /// Return object to object pool
    pub fn return_to_pool(self: Box<Self>) {
        TIMER_EVENT_DATA_POOL.release(self);
    }

    /// 批量将对象返回到对象池中
    /// Batch return objects to object pool
    pub fn batch_return_to_pool(objects: Vec<Box<Self>>) {
        TIMER_EVENT_DATA_POOL.batch_release(objects);
    }
}

/// 定时器事件（优化版本，支持对象池）
/// Timer event (optimized version with object pool support)
#[derive(Debug)]
pub struct TimerEvent {
    /// 事件ID
    /// Event ID
    pub id: TimerEventId,
    /// 事件数据（使用Box以支持对象池）
    /// Event data (using Box for object pool support)
    pub data: Box<TimerEventData>,
    /// 回调通道，用于向注册者发送超时通知
    /// Callback channel, used to send timeout notifications to registrant
    pub callback_tx: mpsc::Sender<TimerEventData>,
}

impl TimerEvent {
    /// 创建新的定时器事件（传统方式）
    /// Create new timer event (traditional way)
    pub fn new(
        id: TimerEventId,
        data: TimerEventData,
        callback_tx: mpsc::Sender<TimerEventData>,
    ) -> Self {
        Self {
            id,
            data: Box::new(data),
            callback_tx,
        }
    }

    /// 使用对象池高效创建定时器事件
    /// Efficiently create timer event using object pool
    pub fn from_pool(
        id: TimerEventId,
        connection_id: ConnectionId,
        timeout_event: TimeoutEvent,
        callback_tx: mpsc::Sender<TimerEventData>,
    ) -> Self {
        Self {
            id,
            data: TimerEventData::from_pool(connection_id, timeout_event),
            callback_tx,
        }
    }

    /// 批量创建定时器事件（高性能版本）
    /// Batch create timer events (high-performance version)
    pub fn batch_from_pool(
        start_id: TimerEventId,
        requests: &[(ConnectionId, TimeoutEvent)],
        callback_txs: &[mpsc::Sender<TimerEventData>],
    ) -> Vec<Self> {
        if requests.len() != callback_txs.len() {
            panic!("Requests and callback_txs must have the same length");
        }

        // 批量获取数据对象
        // Batch acquire data objects
        let data_objects = TimerEventData::batch_from_pool(requests);
        
        // 构建定时器事件
        // Build timer events
        data_objects
            .into_iter()
            .zip(callback_txs.iter())
            .enumerate()
            .map(|(index, (data, callback_tx))| Self {
                id: start_id + index as u64,
                data,
                callback_tx: callback_tx.clone(),
            })
            .collect()
    }

    /// 触发定时器事件，向注册者发送超时通知（优化版本）
    /// Trigger timer event, send timeout notification to registrant (optimized version)
    pub async fn trigger(self) {
        let timer_id = self.id;
        let connection_id = self.data.connection_id;
        let event_type = self.data.timeout_event.clone();
        
        // 克隆数据用于发送，原对象将被回收到池中
        // Clone data for sending, original object will be recycled to pool
        let data_for_send = TimerEventData::new(connection_id, event_type.clone());
        
        if let Err(err) = self.callback_tx.send(data_for_send).await {
            tracing::warn!(
                timer_id,
                error = %err,
                "Failed to send timer event to callback channel"
            );
        } else {
            tracing::trace!(
                timer_id,
                connection_id,
                event_type = ?event_type,
                "Timer event triggered successfully"
            );
        }
        
        // 将数据对象返回到对象池中以供重用
        // Return data object to object pool for reuse
        self.data.return_to_pool();
    }
}

impl fmt::Display for TimerEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "TimerEvent(id: {}, conn: {}, event: {:?})",
            self.id, self.data.connection_id, self.data.timeout_event
        )
    }
}