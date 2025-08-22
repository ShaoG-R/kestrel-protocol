//! 并行处理相关的数据类型定义
//! Data type definitions for parallel processing

use crate::timer::event::ConnectionId;
use crate::timer::event::traits::EventDataTrait;
use crate::timer::parallel::strategy::OptimalParallelStrategy;
use crate::timer::wheel::TimerEntryId;
use std::time::Duration;

/// 处理后的定时器数据
/// Processed timer data
#[derive(Debug, Clone)]
pub struct ProcessedTimerData<E: EventDataTrait> {
    pub entry_id: TimerEntryId,
    pub connection_id: ConnectionId,
    pub timeout_event: E,
    pub expiry_time: tokio::time::Instant,
    pub slot_index: usize,
}

/// 批量并行处理结果
/// Batch parallel processing result
#[derive(Debug)]
pub struct ParallelProcessingResult {
    /// 成功处理的定时器数量
    pub processed_count: usize,
    /// 处理耗时
    pub processing_duration: Duration,
    /// 使用的策略
    pub strategy_used: OptimalParallelStrategy,
    /// 详细统计信息
    pub detailed_stats: DetailedProcessingStats,
}

/// 详细处理统计信息
/// Detailed processing statistics
#[derive(Debug, Default)]
pub struct DetailedProcessingStats {
    pub simd_operations: usize,
    pub rayon_chunks_processed: usize,
    pub async_dispatches: usize,
    pub cache_hits: usize,
    pub memory_allocations: usize,
}

/// 并行处理性能统计
/// Parallel processing performance statistics
#[derive(Debug, Default)]
pub struct ParallelProcessingStats {
    pub total_batches_processed: u64,
    pub simd_only_count: u64,
    pub simd_rayon_count: u64,
    pub full_hybrid_count: u64,
    pub avg_processing_time_ns: f64,
    pub overall_throughput_ops_per_sec: f64,
    /// 总处理时间（纳秒），用于计算正确的平均值
    /// Total processing time in nanoseconds for accurate average calculation
    pub total_processing_time_ns: f64,
    /// 总处理的操作数，用于计算整体吞吐量
    /// Total operations processed for overall throughput calculation
    pub total_operations_processed: u64,
}

/// 内部处理结果类型
/// Internal processing result type
pub(crate) struct ProcessingResult {
    pub processed_count: usize,
    pub detailed_stats: DetailedProcessingStats,
}
