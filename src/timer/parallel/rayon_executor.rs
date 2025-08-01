//! Rayon批量执行器
//! Rayon Batch Executor

use crate::timer::event::traits::EventDataTrait;
use crate::timer::parallel::simd_processor::SIMDTimerProcessor;
use crate::timer::parallel::types::ProcessedTimerData;
use crate::timer::wheel::TimerEntry;
use rayon::prelude::*;

/// Rayon批量执行器
/// Rayon Batch Executor  
pub struct RayonBatchExecutor {
    /// 工作窃取队列的大小
    chunk_size: usize,
    /// 线程池大小
    thread_pool_size: usize,
}

impl RayonBatchExecutor {
    pub fn new(cpu_cores: usize) -> Self {
        Self {
            chunk_size: 512.max(cpu_cores * 64), // 根据CPU核心数调整块大小
            thread_pool_size: cpu_cores,
        }
    }

    /// 使用Rayon + SIMD并行处理 (异步版本)
    /// Parallel processing using Rayon + SIMD (async version)
    pub async fn parallel_process_with_simd<E: EventDataTrait>(
        &mut self,
        timer_entries: Vec<TimerEntry<E>>,
        simd_processor: &mut SIMDTimerProcessor<E>,
    ) -> Result<Vec<ProcessedTimerData<E>>, Box<dyn std::error::Error + Send + Sync>> {
        self.parallel_process_with_simd_sync(timer_entries, simd_processor)
    }

    /// 使用Rayon + SIMD并行处理 (同步版本)
    /// Parallel processing using Rayon + SIMD (sync version)
    pub fn parallel_process_with_simd_sync<E: EventDataTrait>(
        &mut self,
        timer_entries: Vec<TimerEntry<E>>,
        simd_processor: &mut SIMDTimerProcessor<E>,
    ) -> Result<Vec<ProcessedTimerData<E>>, Box<dyn std::error::Error + Send + Sync>> {
        // 使用Rayon并行处理多个块
        let results: Vec<Vec<ProcessedTimerData<E>>> = timer_entries
            .par_chunks(self.chunk_size)
            .map(|chunk| {
                // 每个线程都有自己的SIMD处理器实例
                let mut local_simd = simd_processor.clone();
                local_simd.process_batch(chunk).unwrap_or_default()
            })
            .collect();

        // 合并所有结果
        let mut final_results = Vec::with_capacity(timer_entries.len());
        for result_batch in results {
            final_results.extend(result_batch);
        }

        Ok(final_results)
    }
}

impl Clone for RayonBatchExecutor {
    fn clone(&self) -> Self {
        Self {
            chunk_size: self.chunk_size,
            thread_pool_size: self.thread_pool_size,
        }
    }
}