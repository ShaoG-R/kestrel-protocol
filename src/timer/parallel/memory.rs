use std::sync::atomic::{AtomicUsize, Ordering};
use crate::timer::event::traits::EventDataTrait;
use crate::timer::{ProcessedTimerData, TimerEntry};

/// 预分配内存池
/// Pre-allocated memory pool  
pub struct MemoryPool<E: EventDataTrait> {
    /// 预分配的数据缓冲区
    /// Pre-allocated data buffers
    data_buffers: Vec<Vec<ProcessedTimerData<E>>>,
    /// 预分配的ID缓冲区  
    /// Pre-allocated ID buffers
    id_buffers: Vec<Vec<u32>>,
    /// 预分配的时间戳缓冲区
    /// Pre-allocated timestamp buffers
    timestamp_buffers: Vec<Vec<u64>>,
    /// 当前缓冲区索引
    /// Current buffer index
    current_buffer_index: AtomicUsize,
    /// 缓冲区数量
    /// Buffer count
    buffer_count: usize,
}

impl<E: EventDataTrait> MemoryPool<E> {
    pub fn new(buffer_count: usize, buffer_capacity: usize) -> Self {
        let mut data_buffers = Vec::with_capacity(buffer_count);
        let mut id_buffers = Vec::with_capacity(buffer_count);
        let mut timestamp_buffers = Vec::with_capacity(buffer_count);
        
        for _ in 0..buffer_count {
            data_buffers.push(Vec::with_capacity(buffer_capacity));
            id_buffers.push(Vec::with_capacity(buffer_capacity));
            timestamp_buffers.push(Vec::with_capacity(buffer_capacity));
        }
        
        Self {
            data_buffers,
            id_buffers,
            timestamp_buffers,
            current_buffer_index: AtomicUsize::new(0),
            buffer_count,
        }
    }
    
    /// 获取下一个可用的数据缓冲区
    /// Get next available data buffer
    pub fn get_data_buffer(&mut self) -> &mut Vec<ProcessedTimerData<E>> {
        let index = self.current_buffer_index.fetch_add(1, Ordering::Relaxed) % self.buffer_count;
        let buffer = &mut self.data_buffers[index];
        buffer.clear();
        buffer
    }
    
    /// 获取ID缓冲区和时间戳缓冲区
    /// Get ID buffer and timestamp buffer
    pub fn get_work_buffers(&mut self) -> (&mut Vec<u32>, &mut Vec<u64>) {
        let index = self.current_buffer_index.load(Ordering::Relaxed) % self.buffer_count;
        let id_buffer = &mut self.id_buffers[index];
        let timestamp_buffer = &mut self.timestamp_buffers[index];
        
        id_buffer.clear();
        timestamp_buffer.clear();
        
        (id_buffer, timestamp_buffer)
    }
    
    /// 返回缓冲区（标记为可重用）
    /// Return buffer (mark as reusable)
    pub fn return_buffers(&self) {
        // 在这个实现中，缓冲区会在下次获取时自动清理
        // In this implementation, buffers are automatically cleaned on next acquisition
    }
}

/// 零分配处理器 - 最小化内存分配
/// Zero-allocation processor - minimizing memory allocations
pub struct ZeroAllocProcessor<E: EventDataTrait> {
    /// 内存池
    /// Memory pool
    memory_pool: MemoryPool<E>,
    /// 栈分配缓冲区（用于小批量）
    /// Stack-allocated buffer (for small batches)
    stack_buffer: [ProcessedTimerData<E>; 64],
    /// 栈缓冲区使用计数
    /// Stack buffer usage count
    stack_usage: usize,
}

impl<E: EventDataTrait> Default for ZeroAllocProcessor<E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: EventDataTrait> ZeroAllocProcessor<E> {
    pub fn new() -> Self {
        // 创建默认时间戳
        let default_instant = tokio::time::Instant::now();
        
        // 使用数组映射来初始化栈缓冲区
        let stack_buffer: [ProcessedTimerData<E>; 64] = std::array::from_fn(|_| ProcessedTimerData {
            entry_id: 0,
            connection_id: 0,
            timeout_event: E::default(),
            expiry_time: default_instant,
            slot_index: 0,
        });
        
        Self {
            memory_pool: MemoryPool::new(4, 1024), // 4个缓冲区，每个1024容量
            stack_buffer,
            stack_usage: 0,
        }
    }
    
    /// 处理小批量（使用栈分配）
    /// Process small batch (using stack allocation)
    pub fn process_small_batch(&mut self, timer_entries: &[TimerEntry<E>]) -> &[ProcessedTimerData<E>] {
        if timer_entries.len() <= 64 {
            self.stack_usage = timer_entries.len();
            
            for (i, entry) in timer_entries.iter().enumerate() {
                self.stack_buffer[i] = ProcessedTimerData {
                    entry_id: entry.id,
                    connection_id: entry.event.data.connection_id,
                    timeout_event: entry.event.data.timeout_event.clone(),
                    expiry_time: entry.expiry_time,
                    slot_index: i,
                };
            }
            
            &self.stack_buffer[..self.stack_usage]
        } else {
            &[]
        }
    }
    
    /// 处理大批量（使用内存池）
    /// Process large batch (using memory pool)
    pub fn process_large_batch(&mut self, timer_entries: &[TimerEntry<E>]) -> &[ProcessedTimerData<E>] {
        let buffer = self.memory_pool.get_data_buffer();
        buffer.reserve(timer_entries.len());
        
        for (i, entry) in timer_entries.iter().enumerate() {
            buffer.push(ProcessedTimerData {
                entry_id: entry.id,
                connection_id: entry.event.data.connection_id,
                timeout_event: entry.event.data.timeout_event.clone(),
                expiry_time: entry.expiry_time,
                slot_index: i,
            });
        }
        
        buffer
    }
}