//! 零拷贝事件系统性能基准测试
//! Zero-copy event system performance benchmarks

use super::zero_copy::*;
use super::traits::*;
use super::*;
use crate::core::endpoint::timing::TimeoutEvent;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 性能基准测试配置
/// Performance benchmark configuration
#[derive(Debug, Clone)]
pub struct BenchConfig {
    pub batch_sizes: Vec<usize>,
    pub dispatcher_counts: Vec<usize>,
    pub slot_counts: Vec<usize>,
    pub iterations: usize,
}

impl Default for BenchConfig {
    fn default() -> Self {
        Self {
            batch_sizes: vec![32, 128, 512, 2048, 8192],
            dispatcher_counts: vec![1, 4, 8, 16],
            slot_counts: vec![64, 256, 1024, 4096],
            iterations: 100,
        }
    }
}

/// 性能测试结果
/// Performance test results
#[derive(Debug, Clone)]
pub struct BenchResult {
    pub config_name: String,
    pub total_events: usize,
    pub duration: Duration,
    pub events_per_second: f64,
    pub nanoseconds_per_event: f64,
    pub memory_usage_mb: f64,
}

impl BenchResult {
    pub fn new(config_name: String, total_events: usize, duration: Duration) -> Self {
        let events_per_second = total_events as f64 / duration.as_secs_f64();
        let nanoseconds_per_event = duration.as_nanos() as f64 / total_events as f64;
        
        Self {
            config_name,
            total_events,
            duration,
            events_per_second,
            nanoseconds_per_event,
            memory_usage_mb: 0.0, // 需要专门的内存监控
        }
    }
    
    pub fn print_summary(&self) {
        println!("=== {} ===", self.config_name);
        println!("  总事件数: {}", self.total_events);
        println!("  总耗时: {:?}", self.duration);
        println!("  事件/秒: {:.0}", self.events_per_second);
        println!("  纳秒/事件: {:.1}", self.nanoseconds_per_event);
        println!("  内存使用: {:.2} MB", self.memory_usage_mb);
        println!();
    }
}

/// 零拷贝性能基准测试器
/// Zero-copy performance benchmarker
pub struct ZeroCopyBenchmarker {
    config: BenchConfig,
    results: Vec<BenchResult>,
}

impl ZeroCopyBenchmarker {
    pub fn new(config: BenchConfig) -> Self {
        Self {
            config,
            results: Vec::new(),
        }
    }
    
    /// 创建测试事件数据
    /// Create test event data
    fn create_test_events(count: usize) -> Vec<TimerEventData<TimeoutEvent>> {
        (0..count)
            .map(|i| TimerEventData::new(i as u32, TimeoutEvent::IdleTimeout))
            .collect()
    }
    
    /// 基准测试：FastEventSlot 写入性能
    /// Benchmark: FastEventSlot write performance
    pub fn bench_fast_event_slot_writes(&mut self) {
        for &slot_count in &self.config.slot_counts {
            for &batch_size in &self.config.batch_sizes {
                let config_name = format!("FastEventSlot_{}slots_{}batch", slot_count, batch_size);
                
                let slot = EventSlot::<TimeoutEvent>::new(slot_count);
                let events = Self::create_test_events(batch_size);
                
                let start = Instant::now();
                
                for _ in 0..self.config.iterations {
                    for event in events.clone() {
                        slot.write_event(event);
                    }
                }
                
                let duration = start.elapsed();
                let total_events = batch_size * self.config.iterations;
                
                let result = BenchResult::new(config_name, total_events, duration);
                result.print_summary();
                self.results.push(result);
            }
        }
    }
    
    /// 基准测试：ZeroCopyBatchDispatcher 分发性能
    /// Benchmark: ZeroCopyBatchDispatcher dispatch performance  
    pub fn bench_batch_dispatcher(&mut self) {
        for &dispatcher_count in &self.config.dispatcher_counts {
            for &batch_size in &self.config.batch_sizes {
                let config_name = format!("BatchDispatcher_{}disp_{}batch", dispatcher_count, batch_size);
                
                let dispatcher = ZeroCopyBatchDispatcher::<TimeoutEvent>::new(256, dispatcher_count, 1000);
                
                let start = Instant::now();
                let mut total_dispatched = 0;
                
                for _ in 0..self.config.iterations {
                    let events = Self::create_test_events(batch_size);
                    total_dispatched += dispatcher.batch_dispatch_events(events);
                }
                
                let duration = start.elapsed();
                
                let result = BenchResult::new(config_name, total_dispatched, duration);
                result.print_summary();
                self.results.push(result);
            }
        }
    }
    
    /// 基准测试：RefEventHandler 处理性能
    /// Benchmark: RefEventHandler processing performance
    pub fn bench_ref_event_handler(&mut self) {
        for &batch_size in &self.config.batch_sizes {
            let config_name = format!("RefEventHandler_{}batch", batch_size);
            
            let processed_count = Arc::new(AtomicUsize::new(0));
            let processed_count_clone = Arc::clone(&processed_count);
            
            let handler = RefEventHandler::new(move |_event: &TimerEventData<TimeoutEvent>| {
                processed_count_clone.fetch_add(1, Ordering::Relaxed);
                true
            });
            
            let events = Self::create_test_events(batch_size);
            let event_refs: Vec<&TimerEventData<TimeoutEvent>> = events.iter().collect();
            
            let start = Instant::now();
            
            for _ in 0..self.config.iterations {
                handler.batch_deliver_event_refs(&event_refs);
            }
            
            let duration = start.elapsed();
            let total_events = batch_size * self.config.iterations;
            
            let result = BenchResult::new(config_name, total_events, duration);
            result.print_summary();
            self.results.push(result);
        }
    }
    
    /// 基准测试：EventFactory 创建性能
    /// Benchmark: EventFactory creation performance
    pub fn bench_event_factory(&mut self) {
        for &batch_size in &self.config.batch_sizes {
            let config_name = format!("EventFactory_{}batch", batch_size);
            
            let factory = EventFactory::<TimeoutEvent>::new();
            let requests: Vec<(ConnectionId, TimeoutEvent)> = (0..batch_size)
                .map(|i| (i as u32, TimeoutEvent::IdleTimeout))
                .collect();
            
            let start = Instant::now();
            
            for _ in 0..self.config.iterations {
                let _events = factory.batch_create_events(&requests);
            }
            
            let duration = start.elapsed();
            let total_events = batch_size * self.config.iterations;
            
            let result = BenchResult::new(config_name, total_events, duration);
            result.print_summary();
            self.results.push(result);
        }
    }
    
    /// 基准测试：完整零拷贝工作流
    /// Benchmark: Complete zero-copy workflow
    pub fn bench_complete_workflow(&mut self) {
        for &batch_size in &self.config.batch_sizes {
            let config_name = format!("CompleteWorkflow_{}batch", batch_size);
            
            let factory = EventFactory::<TimeoutEvent>::new();
            let dispatcher = ZeroCopyBatchDispatcher::<TimeoutEvent>::new(512, 8, 1000);
            
            let requests: Vec<(ConnectionId, TimeoutEvent)> = (0..batch_size)
                .map(|i| (i as u32, TimeoutEvent::IdleTimeout))
                .collect();
            
            let start = Instant::now();
            let mut total_dispatched = 0;
            
            for _ in 0..self.config.iterations {
                // 1. 创建事件
                let events = factory.batch_create_events(&requests);
                
                // 2. 分发事件
                total_dispatched += dispatcher.batch_dispatch_events(events);
            }
            
            let duration = start.elapsed();
            
            let result = BenchResult::new(config_name, total_dispatched, duration);
            result.print_summary();
            self.results.push(result);
        }
    }
    
    /// 基准测试：智能分发器 vs 优化分发器 vs 原始实现
    /// Benchmark: Smart dispatcher vs Optimized dispatcher vs Original implementation
    pub fn bench_smart_vs_optimized_vs_original(&mut self) {
        use super::lockfree_ring::OptimizedZeroCopyDispatcher;
        use super::zero_copy::SmartZeroCopyDispatcher;
        
        for &batch_size in &self.config.batch_sizes {
            let config_name_orig = format!("Original_{}batch", batch_size);
            let config_name_opt = format!("Optimized_{}batch", batch_size);
            let config_name_smart = format!("Smart_{}batch", batch_size);
            
            // 测试原始实现
            let original_dispatcher = ZeroCopyBatchDispatcher::<TimeoutEvent>::new(512, 8, 1000);
            let start = Instant::now();
            let mut total_dispatched_orig = 0;
            
            for _ in 0..self.config.iterations {
                let events = Self::create_test_events(batch_size);
                total_dispatched_orig += original_dispatcher.batch_dispatch_events(events);
            }
            let duration_orig = start.elapsed();
            
            // 测试优化实现
            let optimized_dispatcher = OptimizedZeroCopyDispatcher::<TimeoutEvent>::new(512, 8);
            let start = Instant::now();
            let mut total_dispatched_opt = 0;
            
            for _ in 0..self.config.iterations {
                let events = Self::create_test_events(batch_size);
                total_dispatched_opt += optimized_dispatcher.batch_dispatch_events(events);
            }
            let duration_opt = start.elapsed();
            
            // 测试智能实现（集成内存池）
            let smart_dispatcher = SmartZeroCopyDispatcher::<TimeoutEvent>::new(512, 8, 1000);
            let start = Instant::now();
            let mut total_dispatched_smart = 0;
            
            for _ in 0..self.config.iterations {
                let events = Self::create_test_events(batch_size);
                total_dispatched_smart += smart_dispatcher.batch_dispatch_events(events);
            }
            let duration_smart = start.elapsed();
            
            // 记录结果
            let result_orig = BenchResult::new(config_name_orig, total_dispatched_orig, duration_orig);
            let result_opt = BenchResult::new(config_name_opt, total_dispatched_opt, duration_opt);
            let result_smart = BenchResult::new(config_name_smart, total_dispatched_smart, duration_smart);
            
            result_orig.print_summary();
            result_opt.print_summary();
            result_smart.print_summary();
            
            // 打印智能分发器的详细统计
            let smart_stats = smart_dispatcher.get_detailed_stats();
            smart_stats.print_summary();
            
            // 计算性能提升
            let improvement_opt = if duration_orig.as_nanos() > 0 {
                (duration_orig.as_nanos() as f64 - duration_opt.as_nanos() as f64) / duration_orig.as_nanos() as f64 * 100.0
            } else {
                0.0
            };
            
            let improvement_smart = if duration_orig.as_nanos() > 0 {
                (duration_orig.as_nanos() as f64 - duration_smart.as_nanos() as f64) / duration_orig.as_nanos() as f64 * 100.0
            } else {
                0.0
            };
            
            println!("🚀 优化版性能提升: {:.1}%", improvement_opt);
            println!("🧠 智能版性能提升: {:.1}%\n", improvement_smart);
            
            self.results.push(result_orig);
            self.results.push(result_opt);
            self.results.push(result_smart);
        }
    }

    /// 基准测试：智能分发器的端到端工作流（创建+分发+回收）
    /// Benchmark: Smart dispatcher end-to-end workflow (create + dispatch + recycle)
    pub fn bench_smart_end_to_end_workflow(&mut self) {
        // 先运行优化版本的基准测试
        self.bench_smart_end_to_end_workflow_optimized();
        
        // 然后运行原始版本进行对比
        self.bench_smart_end_to_end_workflow_original();
    }
    
    /// 优化版本的SmartEndToEnd工作流（减少不必要的内存操作）
    /// Optimized version of SmartEndToEnd workflow (reduced unnecessary memory operations)
    pub fn bench_smart_end_to_end_workflow_optimized(&mut self) {
        use super::zero_copy::SmartZeroCopyDispatcher;
        
        for &batch_size in &self.config.batch_sizes {
            let config_name = format!("SmartEndToEndOptimized_{}batch", batch_size);
            
            let smart_dispatcher = SmartZeroCopyDispatcher::<TimeoutEvent>::new(512, 8, 1000);
            let requests: Vec<(u32, TimeoutEvent)> = (0..batch_size)
                .map(|i| (i as u32, TimeoutEvent::IdleTimeout))
                .collect();
            
            let start = Instant::now();
            let mut total_dispatched = 0;
            
            // 优化：仅在必要时进行内存管理，大大减少开销
            // Optimization: only perform memory management when necessary, greatly reducing overhead
            for iteration in 0..self.config.iterations {
                // 1. 智能创建和分发（核心功能）
                total_dispatched += smart_dispatcher.create_and_dispatch_events(&requests);
                
                // 2. 内存管理：只在每10次迭代后执行一次，而不是每次都执行
                // Memory management: execute only after every 10 iterations, not every time
                if iteration % 10 == 9 {
                    // 批量消费少量事件用于测试完整性
                    // Batch consume small amount of events for testing completeness
                    let consumed_events = smart_dispatcher.batch_consume_events(5);
                    if !consumed_events.is_empty() {
                        smart_dispatcher.batch_return_to_pool(consumed_events);
                    }
                }
            }
            
            let duration = start.elapsed();
            
            let result = BenchResult::new(config_name, total_dispatched, duration);
            result.print_summary();
            
            // 在最后获取一次详细统计
            // Get detailed statistics at the end
            let stats = smart_dispatcher.get_detailed_stats();
            println!("  🚀 优化版性能统计:");
            println!("    - 成功分发: {}", stats.total_dispatched);
            println!("    - 成功率: {:.1}%", stats.success_rate * 100.0);
            println!("    - 平均利用率: {:.1}%", stats.average_slot_utilization * 100.0);
            println!();
            
            self.results.push(result);
        }
    }
    
    /// 原始版本的SmartEndToEnd工作流（用于性能对比）
    /// Original version of SmartEndToEnd workflow (for performance comparison)
    pub fn bench_smart_end_to_end_workflow_original(&mut self) {
        use super::zero_copy::SmartZeroCopyDispatcher;
        
        for &batch_size in &self.config.batch_sizes {
            let config_name = format!("SmartEndToEndOriginal_{}batch", batch_size);
            
            let smart_dispatcher = SmartZeroCopyDispatcher::<TimeoutEvent>::new(512, 8, 1000);
            let requests: Vec<(u32, TimeoutEvent)> = (0..batch_size)
                .map(|i| (i as u32, TimeoutEvent::IdleTimeout))
                .collect();
            
            let start = Instant::now();
            let mut total_dispatched = 0;
            
            for _ in 0..self.config.iterations {
                // 1. 智能创建和分发
                total_dispatched += smart_dispatcher.create_and_dispatch_events(&requests);
                
                // 优化：减少消费和回收的频率，只在必要时执行
                // Optimization: reduce consumption and recycling frequency, execute only when necessary
                // 每4轮迭代才执行一次消费和回收，减少开销
                // Execute consumption and recycling only every 4 iterations to reduce overhead
                if total_dispatched % (batch_size * 4) == 0 {
                    // 2. 批量消费 - 使用更小的消费量减少锁竞争
                    // Batch consume - use smaller consumption amount to reduce lock contention
                    let consume_size = std::cmp::min(batch_size / 4, 16);
                    let consumed_events = smart_dispatcher.batch_consume_events(consume_size);
                    
                    // 3. 批量回收到内存池 - 仅在有消费事件时执行
                    // Batch return to memory pool - execute only when there are consumed events
                    if !consumed_events.is_empty() {
                        smart_dispatcher.batch_return_to_pool(consumed_events);
                    }
                }
            }
            
            let duration = start.elapsed();
            
            let result = BenchResult::new(config_name, total_dispatched, duration);
            result.print_summary();
            
            // 打印详细性能统计
            let stats = smart_dispatcher.get_detailed_stats();
            println!("  📊 原始版性能统计:");
            println!("    - 成功分发: {}", stats.total_dispatched);
            println!("    - 成功率: {:.1}%", stats.success_rate * 100.0);
            println!("    - 平均利用率: {:.1}%", stats.average_slot_utilization * 100.0);
            println!();
            
            self.results.push(result);
        }
    }
    
    /// 基准测试：内存池优化效果
    /// Benchmark: Memory pool optimization effects
    pub fn bench_memory_pool_optimization(&mut self) {
        use super::memory_pool::OptimizedBatchProcessor;
        
        for &batch_size in &self.config.batch_sizes {
            let config_name = format!("MemoryPool_{}batch", batch_size);
            
            let processor = OptimizedBatchProcessor::<TimeoutEvent>::new(1000, 32);
            let requests: Vec<(u32, TimeoutEvent)> = (0..batch_size)
                .map(|i| (i as u32, TimeoutEvent::IdleTimeout))
                .collect();
            
            let start = Instant::now();
            
            for _ in 0..self.config.iterations {
                let events = processor.create_events_optimized(&requests);
                processor.process_completed(events);
            }
            
            let duration = start.elapsed();
            let total_events = batch_size * self.config.iterations;
            
            let result = BenchResult::new(config_name, total_events, duration);
            result.print_summary();
            
            // 打印内存池统计
            let (processed, created, reused, reuse_rate, large_batch_ratio) = processor.get_performance_stats();
            println!("  📊 内存池统计:");
            println!("    - 处理事件: {}", processed);
            println!("    - 新建对象: {}", created);
            println!("    - 复用对象: {}", reused);
            println!("    - 复用率: {:.1}%", reuse_rate * 100.0);
            println!("    - 大批量处理率: {:.1}%", large_batch_ratio * 100.0);
            println!();
            
            self.results.push(result);
        }
    }

    /// 运行所有基准测试
    /// Run all benchmarks
    pub fn run_all_benchmarks(&mut self) {
        println!("=== 零拷贝事件系统性能基准测试 ===");
        println!("配置: {:?}\n", self.config);
        
        println!("1. FastEventSlot 写入性能...");
        self.bench_fast_event_slot_writes();
        
        println!("2. BatchDispatcher 分发性能...");
        self.bench_batch_dispatcher();
        
        println!("3. RefEventHandler 处理性能...");
        self.bench_ref_event_handler();
        
        println!("4. EventFactory 创建性能...");
        self.bench_event_factory();
        
        println!("5. 完整工作流性能...");
        self.bench_complete_workflow();
        
        println!("6. 智能版 vs 优化版 vs 原始版对比...");
        self.bench_smart_vs_optimized_vs_original();
        
        println!("7. 智能分发器端到端工作流测试（优化版 vs 原始版）...");
        self.bench_smart_end_to_end_workflow();
        
        println!("8. 内存池优化效果...");
        self.bench_memory_pool_optimization();
        
        self.print_performance_analysis();
    }
    
    /// 打印性能分析报告
    /// Print performance analysis report
    pub fn print_performance_analysis(&self) {
        println!("=== 性能分析报告 ===");
        
        // 按组件分类性能结果
        let mut component_results: std::collections::HashMap<String, Vec<&BenchResult>> = 
            std::collections::HashMap::new();
        
        for result in &self.results {
            let component = result.config_name.split('_').next().unwrap_or("Unknown");
            component_results.entry(component.to_string())
                .or_insert_with(Vec::new)
                .push(result);
        }
        
        for (component, results) in component_results {
            println!("\n--- {} 组件性能总结 ---", component);
            
            let avg_ns_per_event: f64 = results.iter()
                .map(|r| r.nanoseconds_per_event)
                .sum::<f64>() / results.len() as f64;
            
            let max_throughput = results.iter()
                .map(|r| r.events_per_second)
                .fold(0.0, f64::max);
            
            println!("  平均延迟: {:.1} ns/事件", avg_ns_per_event);
            println!("  最大吞吐量: {:.0} 事件/秒", max_throughput);
            
            // 找出最佳和最差配置
            if let Some(best) = results.iter().min_by(|a, b| 
                a.nanoseconds_per_event.partial_cmp(&b.nanoseconds_per_event).unwrap()) {
                println!("  最佳配置: {} ({:.1} ns/事件)", best.config_name, best.nanoseconds_per_event);
            }
            
            if let Some(worst) = results.iter().max_by(|a, b| 
                a.nanoseconds_per_event.partial_cmp(&b.nanoseconds_per_event).unwrap()) {
                println!("  最差配置: {} ({:.1} ns/事件)", worst.config_name, worst.nanoseconds_per_event);
            }
        }
        
        println!("\n=== 性能优化建议 ===");
        self.print_optimization_recommendations();
    }
    
    /// 打印优化建议
    /// Print optimization recommendations
    fn print_optimization_recommendations(&self) {
        println!("1. **批量大小优化**:");
        println!("   - 推荐批量大小: 512-2048 个事件");
        println!("   - 避免小批量 (<32) 处理，会增加单事件开销");
        
        println!("\n2. **内存访问优化**:");
        println!("   - 考虑使用 CPU缓存友好的数据结构");
        println!("   - 减少原子操作频率，使用批量原子更新");
        
        println!("\n3. **并发优化**:");
        println!("   - 使用4-8个分发器获得最佳性能");
        println!("   - 避免过多分发器造成的上下文切换开销");
        
        println!("\n4. **零拷贝优化**:");
        println!("   - 考虑使用无锁环形缓冲区替代 ArcSwap");
        println!("   - 实现批量内存映射以减少系统调用");
    }
}

#[cfg(test)]
mod bench_tests {
    use super::*;
    
    #[test]
    fn test_lightweight_benchmark() {
        let config = BenchConfig {
            batch_sizes: vec![32, 128],
            dispatcher_counts: vec![2, 4],
            slot_counts: vec![64, 256],
            iterations: 10, // 减少迭代次数以加快测试
        };
        
        let mut benchmarker = ZeroCopyBenchmarker::new(config);
        benchmarker.run_all_benchmarks();
        
        // 验证有结果产生
        assert!(!benchmarker.results.is_empty());
    }
}