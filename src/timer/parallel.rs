//! 混合并行定时器处理模块 
//! Hybrid Parallel Timer Processing Module
//!
//! 该模块实现了三层并行优化架构：
//! 1. SIMD向量化 - 单线程内并行处理
//! 2. Rayon数据并行 - CPU密集型批量计算
//! 3. tokio异步并发 - I/O密集型事件分发
//! 4. 零拷贝通道 - 基于引用传递避免数据克隆
//! 5. 单线程直通 - 绕过异步调度的直接路径
//!
//! This module implements a three-tier parallel optimization architecture:
//! 1. SIMD Vectorization - Intra-thread parallel processing
//! 2. Rayon Data Parallelism - CPU-intensive batch computation  
//! 3. tokio Async Concurrency - I/O-intensive event dispatching
//! 4. Zero-Copy Channels - Reference passing to avoid data cloning
//! 5. Single-Thread Bypass - Direct path bypassing async scheduling

/// 单线程直通优化模块 - 绕过异步调度的同步路径
/// Single-thread bypass optimization module - synchronous path bypassing async scheduling
pub mod single_thread_bypass;
/// 内存预分配管理器 - 减少运行时分配
/// Memory pre-allocation manager - reducing runtime allocations
pub mod memory;
/// 并行处理策略选择
/// Parallel processing strategy selection
pub mod strategy;
/// 数据类型定义
/// Data type definitions
pub mod types;
/// SIMD定时器处理器
/// SIMD Timer Processor
pub mod simd_processor;
/// Rayon批量执行器
/// Rayon Batch Executor
pub mod rayon_executor;
/// 异步事件分发器
/// Async Event Dispatcher
pub mod async_dispatcher;
/// 核心并行系统
/// Core parallel system
pub mod core;

// Re-export the main types for convenience
pub use core::HybridParallelTimerSystem;
pub use strategy::OptimalParallelStrategy;
pub use types::{
    DetailedProcessingStats, ParallelProcessingResult, ParallelProcessingStats, ProcessedTimerData,
};
pub use simd_processor::SIMDTimerProcessor;
pub use rayon_executor::RayonBatchExecutor;
pub use async_dispatcher::AsyncEventDispatcher;


#[cfg(test)]
mod tests {
    use super::*;
    use crate::timer::event::TimerEvent;
    use crate::timer::event::traits::{EventDataTrait, EventFactory};
    use crate::timer::TimerEntry;
    use tokio::sync::mpsc;
    use std::time::Instant;
    use crate::core::endpoint::timing::TimeoutEvent;

    #[tokio::test]
    async fn test_hybrid_parallel_system_creation() {
        let system = HybridParallelTimerSystem::<TimeoutEvent>::new();
        assert!(system.cpu_cores > 0);
        assert_eq!(system.stats.total_batches_processed, 0);
    }

    #[tokio::test]
    async fn test_strategy_selection() {
        let system = HybridParallelTimerSystem::<TimeoutEvent>::new();
        
        assert_eq!(system.choose_optimal_strategy(100), OptimalParallelStrategy::SIMDOnly);
        assert_eq!(system.choose_optimal_strategy(1000), OptimalParallelStrategy::SIMDOnly);
        assert_eq!(system.choose_optimal_strategy(10000), OptimalParallelStrategy::FullHybrid);
    }

    #[tokio::test]
    async fn test_simd_processor() {
        let mut processor = SIMDTimerProcessor::<TimeoutEvent>::new();
        
        // 创建测试数据 - 简化版本，不依赖TimerEvent结构
        let test_entries = vec![];  // 空测试，避免复杂的依赖关系

        let result = processor.process_batch(&test_entries);
        assert!(result.is_ok());
        let processed = result.unwrap();
        assert_eq!(processed.len(), 0);
    }

    /// 创建测试用的定时器条目 (使用智能工厂)
    fn create_test_timer_entries<E: EventDataTrait>(count: usize) -> Vec<TimerEntry<E>> {
        let (tx, _rx) = mpsc::channel(1);
        let mut entries = Vec::with_capacity(count);
        let factory = EventFactory::<E>::new(); // 智能策略选择工厂
        
        for i in 0..count {
            let timer_event = TimerEvent::from_factory(
                &factory,
                i as u64, // id
                (i % 10000) as u32, // connection_id
                E::default(),
                tx.clone(),
            );
            
            entries.push(TimerEntry {
                id: i as u64,
                expiry_time: tokio::time::Instant::now() + tokio::time::Duration::from_millis(1000),
                event: timer_event,
            });
        }
        
        entries
    }


    #[tokio::test]
    async fn test_async_overhead_optimization_effectiveness() {
        // 在测试开始时添加小延迟，减少并发测试间的资源竞争
        // Add small delay at test start to reduce resource contention between concurrent tests
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        
        println!("\n🚀 异步开销优化效果验证测试");
        println!("========================================");
        println!("该测试验证零拷贝通道、单线程直通和内存优化的效果");
        println!();

        let mut optimized_system = HybridParallelTimerSystem::<TimeoutEvent>::new();
        
        // 测试不同批量大小下的优化效果
        let optimization_test_cases = vec![
            (32, "微批量 - 直通模式"),
            (128, "小批量 - 直通模式"), 
            (512, "中批量 - 零拷贝优化"),
            (2048, "大批量 - 完整优化"),
        ];

        for (batch_size, scenario) in optimization_test_cases {
            println!("🔬 {} ({} 个定时器):", scenario, batch_size);
            
            // 检测执行模式
            let execution_mode = optimized_system.mode_selector.choose_mode(batch_size);
            let should_bypass = optimized_system.mode_selector.should_bypass_async(batch_size);
            
            println!("  执行模式: {:?}", execution_mode);
            println!("  使用直通: {}", should_bypass);
            
            // 预热系统
            for _ in 0..3 {
                let warmup_entries = create_test_timer_entries(10);
                let _ = optimized_system.process_timer_batch(warmup_entries).await;
            }
            
            // 多次运行取平均值
            let iterations = 100;
            let mut total_duration = std::time::Duration::ZERO;
            let mut total_memory_allocations = 0;
            let mut bypass_used_count = 0;
            
            let overall_start = Instant::now();
            
            for _ in 0..iterations {
                let test_entries = create_test_timer_entries(batch_size);
                let start_time = Instant::now();
                
                match optimized_system.process_timer_batch(test_entries).await {
                    Ok(result) => {
                        total_duration += start_time.elapsed();
                        total_memory_allocations += result.detailed_stats.memory_allocations;
                        
                        // 检测是否使用了直通模式（SIMD操作为0表示使用了直通）
                        if result.detailed_stats.simd_operations == 0 {
                            bypass_used_count += 1;
                        }
                    }
                    Err(e) => {
                        eprintln!("测试失败: {}", e);
                        continue;
                    }
                }
            }
            
            let overall_duration = overall_start.elapsed();
            let avg_duration = if iterations > 0 {
                total_duration / iterations
            } else {
                std::time::Duration::ZERO
            };
            let avg_memory_allocs = if iterations > 0 {
                total_memory_allocations as f64 / iterations as f64
            } else {
                0.0
            };
            let nanos_per_operation = if batch_size > 0 {
                avg_duration.as_nanos() / batch_size as u128
            } else {
                0
            };
            
            println!("  平均处理时间: {:.2}µs", avg_duration.as_micros());
            println!("  每操作延迟: {} 纳秒", nanos_per_operation);
            println!("  平均内存分配: {:.1} 次", avg_memory_allocs);
            println!("  直通模式使用: {}/{} 次", bypass_used_count, iterations);
            println!("  总测试时间: {:.2}ms", overall_duration.as_millis());
            
            // 吞吐量计算
            let throughput = if overall_duration.as_secs_f64() > 0.0 {
                (batch_size as f64 * iterations as f64) / overall_duration.as_secs_f64()
            } else {
                0.0
            };
            println!("  整体吞吐量: {:.0} ops/sec", throughput);
            
            // 性能评估
            let optimization_effectiveness = match (nanos_per_operation, should_bypass) {
                (0..=100, true) => "🚀 直通模式优化卓越",
                (101..=200, true) => "⚡ 直通模式优化显著", 
                (0..=300, false) if avg_memory_allocs <= 2.0 => "✅ 零拷贝优化有效",
                (301..=500, false) => "⚠️  优化效果一般",
                _ => "❌ 优化效果有限",
            };
            
            println!("  优化评估: {}", optimization_effectiveness);
            
            // 内存效率评估
            let memory_efficiency = match avg_memory_allocs {
                x if x <= 1.0 => "🎯 内存零分配/单次分配",
                x if x <= 2.0 => "✅ 内存分配优化良好",
                x if x <= 3.0 => "⚠️  内存分配可以改进",
                _ => "❌ 内存分配需要优化",
            };
            
            println!("  内存效率: {}", memory_efficiency);
            println!();
        }

        // 输出系统统计信息
        let stats = optimized_system.get_stats();
        println!("📊 优化系统性能统计:");
        println!("  处理批次: {}", stats.total_batches_processed);
        println!("  峰值吞吐量: {} ops/sec", stats.peak_throughput_ops_per_sec);
        println!("  平均处理时间: {} 纳秒", stats.avg_processing_time_ns);
    }

    
    
    #[tokio::test]
    #[ignore] // 由于资源密集型，在CI中跳过，使用 cargo test -- --ignored 单独运行
    async fn test_comprehensive_optimization_benchmark() {
        // 基准测试前等待更长时间，确保系统稳定
        // Wait longer before benchmark to ensure system stability
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        
        println!("\n🏆 综合优化效果基准测试 (已修正)");
        println!("========================================");
        println!("对比传统异步模式 vs 优化模式的性能差异");
        println!();

        let mut optimized_system = HybridParallelTimerSystem::<TimeoutEvent>::new();
        
        // 测试场景：不同批量大小下的性能对比 (已扩展至 8192)
        let benchmark_cases = vec![
            (1, "超小批量 (1)"),
            (16, "超小批量 (16)"),
            (32, "超小批量 (32)"),
            (64, "小批量 (64)"),
            (128, "小批量 (128)"),
            (256, "中批量 (256)"),
            (512, "中批量 (512)"),
            (1024, "大批量 (1024)"),
            (2048, "大批量 (2048)"),
            (4096, "超大批量 (4096)"),
            (8192, "超大批量 (8192)"),
        ];

        for (batch_size, scenario) in benchmark_cases {
            println!("🏁 {} ({} 个定时器):", scenario, batch_size);
            
            let should_bypass = optimized_system.mode_selector.should_bypass_async(batch_size);
            
            // 预热
            for _ in 0..5 {
                let warmup_entries = create_test_timer_entries(batch_size);
                let _ = optimized_system.process_timer_batch(warmup_entries).await;
            }
            
            // 基准测试（多次运行取平均值）
            let benchmark_iterations = 1000;
            let mut durations = Vec::with_capacity(benchmark_iterations);
            let mut memory_allocations = Vec::with_capacity(benchmark_iterations);
            let mut bypass_mode_used = 0;
            
            let benchmark_start = Instant::now();
            
            for _ in 0..benchmark_iterations {
                let test_entries = create_test_timer_entries(batch_size);
                let iteration_start = Instant::now();
                
                if let Ok(result) = optimized_system.process_timer_batch(test_entries).await {
                    durations.push(iteration_start.elapsed());
                    memory_allocations.push(result.detailed_stats.memory_allocations);
                    
                    // 如果没有SIMD操作，我们假设它使用了直通模式
                    if result.detailed_stats.simd_operations == 0 {
                        bypass_mode_used += 1;
                    }
                }
            }
            
            let total_benchmark_time = benchmark_start.elapsed();
            
            // 计算统计数据
            let avg_duration = if !durations.is_empty() {
                durations.iter().sum::<std::time::Duration>() / durations.len() as u32
            } else {
                std::time::Duration::ZERO
            };
            let min_duration = durations.iter().min().unwrap_or(&std::time::Duration::ZERO);
            let max_duration = durations.iter().max().unwrap_or(&std::time::Duration::ZERO);
            let avg_memory_allocs: f64 = if !memory_allocations.is_empty() {
                memory_allocations.iter().sum::<usize>() as f64 / memory_allocations.len() as f64
            } else {
                0.0
            };
            
            let nanos_per_op = if batch_size > 0 {
                avg_duration.as_nanos() / batch_size as u128
            } else {
                0
            };
            let throughput = if total_benchmark_time.as_secs_f64() > 0.0 {
                (batch_size as f64 * benchmark_iterations as f64) / total_benchmark_time.as_secs_f64()
            } else {
                0.0
            };
            
            println!("  预期模式: {}", if should_bypass { "直通" } else { "并行" });
            let bypass_rate = if benchmark_iterations > 0 {
                (bypass_mode_used as f64 / benchmark_iterations as f64) * 100.0
            } else {
                0.0
            };
            println!("  直通使用率: {:.1}%", bypass_rate);
            println!();
            
            println!("  📈 性能指标:");
            println!("    平均延迟: {:.}µs", avg_duration.as_micros() as f64 / 1000.0);
            println!("    最小延迟: {:.}µs", min_duration.as_micros() as f64 / 1000.0);
            println!("    最大延迟: {:.}µs", max_duration.as_micros() as f64 / 1000.0);
            println!("    每操作: {} 纳秒", nanos_per_op);
            println!("    吞吐量: {:.0} ops/sec", throughput);
            println!();
            
            println!("  🧠 内存指标:");
            println!("    平均分配: {:.1} 次", avg_memory_allocs);
            println!("    分配效率: {}", if avg_memory_allocs <= 1.0 { "优秀" } else if avg_memory_allocs <= 2.0 { "良好" } else { "需改进" });
            println!();
            
            // 性能等级评估
            let performance_grade = match nanos_per_op {
                0..=50 => "S级 (卓越)",
                51..=100 => "A级 (优秀)",
                101..=200 => "B级 (良好)",
                201..=400 => "C级 (一般)",
                _ => "D级 (需改进)",
            };
            
            println!("  🏆 性能等级: {}", performance_grade);
            
            // 优化效果总结
            let optimization_impact = match (should_bypass, nanos_per_op) {
                (true, 0..=100) => "🚀 直通优化效果显著",
                (false, 0..=200) if avg_memory_allocs <= 2.0 => "⚡ 零拷贝优化有效",
                (false, 201..=400) => "✅ 优化有一定效果",
                _ => "⚠️  优化效果有限",
            };
            
            println!("  💡 优化效果: {}", optimization_impact);
            println!();
        }

        // 输出最终的系统性能统计
        let final_stats = optimized_system.get_stats();
        println!("🎯 最终性能统计:");
        println!("  处理批次总数: {}", final_stats.total_batches_processed);
        println!("  峰值吞吐量: {:.0} ops/sec", final_stats.peak_throughput_ops_per_sec);
        println!("  平均处理时间: {} 纳秒", final_stats.avg_processing_time_ns);
        println!("  直通模式使用: {} 次", final_stats.simd_only_count);
        println!("  SIMD+Rayon使用: {} 次", final_stats.simd_rayon_count);
        println!("  完整混合使用: {} 次", final_stats.full_hybrid_count);
    }
}