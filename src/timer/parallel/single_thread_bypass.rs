use crate::timer::event::traits::EventDataTrait;
use crate::timer::event::zero_copy::ZeroCopyEventDelivery;
use crate::timer::{ProcessedTimerData, TimerEventData};
use std::marker::PhantomData;
use std::matches;
use std::sync::atomic::{AtomicBool, Ordering};

/// 单线程执行模式判断
/// Single-thread execution mode detection
#[derive(Debug, Clone, Copy)]
pub enum ExecutionMode {
    /// 单线程直接执行（零异步开销）
    /// Single-thread direct execution (zero async overhead)
    SingleThreadDirect,
    /// 单线程但异步调度
    /// Single-thread with async scheduling  
    SingleThreadAsync,
    /// 多线程并行执行
    /// Multi-thread parallel execution
    MultiThreadParallel,
}

/// 智能执行模式选择器
/// Smart execution mode selector
pub struct ExecutionModeSelector {
    /// 当前线程数
    /// Current thread count
    thread_count: usize,
    /// 是否在tokio运行时内
    /// Whether inside tokio runtime
    in_tokio_runtime: AtomicBool,
    /// 性能阈值：小于此批量大小时使用直通模式
    /// Performance threshold: use bypass mode for batches smaller than this
    bypass_threshold: usize,
}

impl Default for ExecutionModeSelector {
    fn default() -> Self {
        Self::new()
    }
}

impl ExecutionModeSelector {
    pub fn new() -> Self {
        Self {
            thread_count: num_cpus::get(),
            in_tokio_runtime: AtomicBool::new(false),
            bypass_threshold: 128, // 小批量使用直通模式
        }
    }

    /// 根据批量大小和系统状态选择最优执行模式
    /// Choose optimal execution mode based on batch size and system state
    pub fn choose_mode(&self, batch_size: usize) -> ExecutionMode {
        // 检测是否在tokio运行时中
        let in_runtime = tokio::runtime::Handle::try_current().is_ok();
        self.in_tokio_runtime.store(in_runtime, Ordering::Relaxed);

        match (batch_size, self.thread_count, in_runtime) {
            // 小批量 + 单核心 + 非异步环境 = 直通
            (size, 1, false) if size <= self.bypass_threshold => ExecutionMode::SingleThreadDirect,

            // 小批量 + 无需并行 = 直通
            (size, _, _) if size <= 64 => ExecutionMode::SingleThreadDirect,

            // 中等批量 + 多核心 = 并行
            (size, cores, _) if size > self.bypass_threshold && cores > 1 => {
                ExecutionMode::MultiThreadParallel
            }

            // 其他情况：单线程异步
            _ => ExecutionMode::SingleThreadAsync,
        }
    }

    /// 检查是否应该使用直通模式
    /// Check if bypass mode should be used
    pub fn should_bypass_async(&self, batch_size: usize) -> bool {
        matches!(
            self.choose_mode(batch_size),
            ExecutionMode::SingleThreadDirect
        )
    }
}

/// 直通式定时器处理器（零异步开销）
/// Bypass timer processor (zero async overhead)
pub struct BypassTimerProcessor<E: EventDataTrait> {
    _marker: PhantomData<E>,
}

impl<E: EventDataTrait> Default for BypassTimerProcessor<E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: EventDataTrait> BypassTimerProcessor<E> {
    pub fn new() -> Self {
        Self {
            _marker: PhantomData,
        }
    }

    /// 直通式事件分发（同步，使用引用传递）
    /// Bypass event dispatch (synchronous, using reference passing)
    pub fn dispatch_events_bypass<H>(
        &self,
        processed_data: &[ProcessedTimerData<E>],
        handler: &H,
    ) -> usize
    where
        H: ZeroCopyEventDelivery<E>,
    {
        // 构建事件引用数组，避免克隆
        // Build event reference array, avoiding clones
        let event_refs: Vec<TimerEventData<E>> = processed_data
            .iter()
            .map(|data| TimerEventData::new(data.connection_id, data.timeout_event.clone()))
            .collect();

        let event_ref_ptrs: Vec<&TimerEventData<E>> = event_refs.iter().collect();

        // 批量传递引用，零拷贝
        // Batch deliver references, zero-copy
        handler.batch_deliver_event_refs(&event_ref_ptrs)
    }
}
