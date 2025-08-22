//! 并行处理策略选择
//! Parallel processing strategy selection

/// 自适应并行策略选择
/// Adaptive parallel strategy selection
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OptimalParallelStrategy {
    /// 仅SIMD优化 - 小批量 (<256)
    /// SIMD Only - Small batches (<256)
    SIMDOnly,
    /// SIMD + Rayon - 中批量 (256-4096)
    /// SIMD + Rayon - Medium batches (256-4096)
    SIMDWithRayon,
    /// 完整混合策略 - 大批量 (>4096)
    /// Full Hybrid - Large batches (>4096)
    FullHybrid,
}

/// 策略选择逻辑
/// Strategy selection logic
pub struct StrategySelector {
    cpu_cores: usize,
}

impl StrategySelector {
    pub fn new(cpu_cores: usize) -> Self {
        Self { cpu_cores }
    }

    /// 选择最优的并行策略（异步开销优化版本）
    /// Choose the optimal parallel strategy (async overhead optimized version)
    pub fn choose_optimal_strategy(&self, batch_size: usize) -> OptimalParallelStrategy {
        match batch_size {
            0..=256 => OptimalParallelStrategy::SIMDOnly,
            257..=1023 => {
                // 中等批量：优先使用SIMD，避免Rayon的异步开销
                OptimalParallelStrategy::SIMDOnly
            }
            1024..=4095 => {
                // 中大批量：使用FullHybrid策略避免Rayon异步包装开销
                if self.cpu_cores >= 2 {
                    OptimalParallelStrategy::FullHybrid
                } else {
                    OptimalParallelStrategy::SIMDOnly
                }
            }
            4096.. => {
                // 大批量：使用完整混合策略获得最佳性能
                if self.cpu_cores >= 2 {
                    OptimalParallelStrategy::FullHybrid
                } else {
                    OptimalParallelStrategy::SIMDOnly
                }
            }
        }
    }
}
