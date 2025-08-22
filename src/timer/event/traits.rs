pub trait EventDataTrait: Clone + std::fmt::Debug + Default + Send + Sync + 'static {}

impl<T> EventDataTrait for T where T: Clone + std::fmt::Debug + Default + Send + Sync + 'static {}

// 从这里开始是您的代码的重构版本
// From here is the refactored version of your code
use crate::timer::event::{ConnectionId, TimerEventData, pool::TimerEventPool};
use std::marker::PhantomData;

/// 策略选择特征，用于在编译时选择最优的事件创建策略
/// Strategy selection trait for compile-time selection of optimal event creation strategy
pub trait EventCreationStrategy<E: EventDataTrait> {
    fn create_single(connection_id: ConnectionId, timeout_event: E) -> TimerEventData<E>;
    fn create_batch(requests: &[(ConnectionId, E)]) -> Vec<TimerEventData<E>>;
    fn should_use_pool() -> bool;
}

/// Copy 类型的高效策略实现
/// Efficient strategy implementation for Copy types
pub struct CopyStrategy;

impl<E: EventDataTrait + Copy> EventCreationStrategy<E> for CopyStrategy {
    #[inline(always)]
    fn create_single(connection_id: ConnectionId, timeout_event: E) -> TimerEventData<E> {
        // Copy 类型直接按位复制，无需池管理
        // Copy types use direct bitwise copying, no pool management needed
        TimerEventData::new(connection_id, timeout_event)
    }

    #[inline(always)]
    fn create_batch(requests: &[(ConnectionId, E)]) -> Vec<TimerEventData<E>> {
        // Copy 类型使用解引用，避免不必要的 clone 调用
        // Copy types use dereferencing, avoiding unnecessary clone calls
        requests
            .iter()
            .map(|(conn_id, event)| TimerEventData::new(*conn_id, *event))
            .collect()
    }

    #[inline(always)]
    fn should_use_pool() -> bool {
        // Copy 类型通常很小，池的开销大于收益
        // Copy types are usually small, pool overhead outweighs benefits
        false
    }
}

/// 非 Copy 类型的通用策略实现
/// Generic strategy implementation for non-Copy types
pub struct CloneStrategy;

impl<E: EventDataTrait> EventCreationStrategy<E> for CloneStrategy {
    #[inline(always)]
    fn create_single(connection_id: ConnectionId, timeout_event: E) -> TimerEventData<E> {
        // 非 Copy 类型使用 clone，可能受益于池管理
        // Non-Copy types use clone, may benefit from pool management
        TimerEventData::new(connection_id, timeout_event)
    }

    #[inline(always)]
    fn create_batch(requests: &[(ConnectionId, E)]) -> Vec<TimerEventData<E>> {
        // 非 Copy 类型必须使用 clone
        // Non-Copy types must use clone
        requests
            .iter()
            .map(|(conn_id, event)| TimerEventData::new(*conn_id, event.clone()))
            .collect()
    }

    #[inline(always)]
    fn should_use_pool() -> bool {
        // 非 Copy 类型通常更复杂，适合使用对象池
        // Non-Copy types are usually more complex, suitable for object pooling
        std::mem::size_of::<E>() > 64 || std::mem::needs_drop::<E>()
    }
}

/// `EventFactory` 是一个无状态的结构体，作为创建事件的入口点。
/// `EventFactory` is a stateless struct that acts as an entry point for creating events.
#[derive(Clone)]
pub struct EventFactory<E: EventDataTrait>(PhantomData<E>);

// --- 统一的智能实现：根据类型特征自动选择策略 ---
// Unified smart implementation: Automatically selects strategy based on type characteristics
impl<E: EventDataTrait> EventFactory<E> {
    /// 创建一个新的工厂实例。
    /// Creates a new factory instance.
    pub fn new() -> Self {
        Self(PhantomData)
    }

    /// 创建单个事件 (智能策略选择)
    /// Create single event (smart strategy selection)
    #[inline(always)]
    pub fn create_event(&self, connection_id: ConnectionId, timeout_event: E) -> TimerEventData<E> {
        self.dispatch_create_single(connection_id, timeout_event)
    }

    /// 批量创建事件 (智能策略选择)
    /// Batch create events (smart strategy selection)
    #[inline(always)]
    pub fn batch_create_events(&self, requests: &[(ConnectionId, E)]) -> Vec<TimerEventData<E>> {
        self.dispatch_create_batch(requests)
    }

    /// 使用对象池创建单个事件 (智能策略选择)
    /// Create single event with pool (smart strategy selection)
    #[inline(always)]
    pub fn create_with_pool(
        &self,
        pool: &TimerEventPool<E>,
        connection_id: ConnectionId,
        timeout_event: E,
    ) -> TimerEventData<E> {
        if self.should_use_pool() {
            pool.acquire(connection_id, timeout_event)
        } else {
            // 对于不适合使用池的类型（如 Copy 类型），直接创建
            // For types not suitable for pooling (like Copy types), create directly
            self.create_event(connection_id, timeout_event)
        }
    }

    /// 使用对象池批量创建事件 (智能策略选择)
    /// Batch create events with pool (smart strategy selection)
    #[inline(always)]
    pub fn batch_create_with_pool(
        &self,
        pool: &TimerEventPool<E>,
        requests: &[(ConnectionId, E)],
    ) -> Vec<TimerEventData<E>> {
        if self.should_use_pool() {
            pool.batch_acquire(requests)
        } else {
            // 对于不适合使用池的类型，直接批量创建
            // For types not suitable for pooling, create batch directly
            self.batch_create_events(requests)
        }
    }

    /// 返回事件到池中 (智能策略选择)
    /// Return event to pool (smart strategy selection)
    #[inline(always)]
    pub fn return_to_pool(&self, event: TimerEventData<E>, pool: &TimerEventPool<E>) {
        if self.should_use_pool() {
            pool.release(event);
        }
        // 对于不使用池的类型，事件会自动销毁
        // For types that don't use pools, events are automatically destroyed
    }

    /// 批量返回事件到池中 (智能策略选择)
    /// Batch return events to pool (smart strategy selection)
    #[inline(always)]
    pub fn batch_return_to_pool(&self, events: Vec<TimerEventData<E>>, pool: &TimerEventPool<E>) {
        if self.should_use_pool() {
            pool.batch_release(events);
        }
        // 对于不使用池的类型，Vec 会自动清理
        // For types that don't use pools, Vec will be automatically cleaned up
    }

    // --- 策略分发方法 ---
    // Strategy dispatch methods

    /// 智能分发单个事件创建
    /// Smart dispatch for single event creation
    #[inline(always)]
    fn dispatch_create_single(
        &self,
        connection_id: ConnectionId,
        timeout_event: E,
    ) -> TimerEventData<E> {
        // 运行时检查是否为 Copy 类型（基于类型特征）
        // Runtime check if it's a Copy type (based on type characteristics)
        if self.is_copy_like() {
            // 对于 Copy-like 类型，使用 CopyStrategy 的逻辑
            // For Copy-like types, use CopyStrategy logic
            TimerEventData::new(connection_id, timeout_event)
        } else {
            // 对于非 Copy 类型，使用 CloneStrategy 的逻辑
            // For non-Copy types, use CloneStrategy logic
            TimerEventData::new(connection_id, timeout_event)
        }
    }

    /// 智能分发批量事件创建
    /// Smart dispatch for batch event creation
    #[inline(always)]
    fn dispatch_create_batch(&self, requests: &[(ConnectionId, E)]) -> Vec<TimerEventData<E>> {
        if self.is_copy_like() {
            // 对于 Copy-like 类型，尽量避免 clone 调用
            // For Copy-like types, try to avoid clone calls
            self.create_batch_copy_optimized(requests)
        } else {
            // 对于非 Copy 类型，使用 clone
            // For non-Copy types, use clone
            self.create_batch_clone_based(requests)
        }
    }

    /// 检查是否应该使用对象池
    /// Check if object pool should be used
    #[inline(always)]
    fn should_use_pool(&self) -> bool {
        // 基于类型特征决定是否使用池
        // Decide whether to use pool based on type characteristics
        !self.is_copy_like() && (std::mem::size_of::<E>() > 64 || std::mem::needs_drop::<E>())
    }

    // --- 辅助方法 ---
    // Helper methods

    /// 检查类型是否为 Copy-like（小且简单）
    /// Check if type is Copy-like (small and simple)
    #[inline(always)]
    fn is_copy_like(&self) -> bool {
        std::mem::size_of::<E>() <= 64 && !std::mem::needs_drop::<E>()
    }

    /// Copy 优化的批量创建
    /// Copy-optimized batch creation
    #[inline(always)]
    fn create_batch_copy_optimized(
        &self,
        requests: &[(ConnectionId, E)],
    ) -> Vec<TimerEventData<E>> {
        // 对于小且简单的类型，clone 实际上是按位复制
        // For small and simple types, clone is actually bitwise copy
        requests
            .iter()
            .map(|(conn_id, event)| TimerEventData::new(*conn_id, event.clone()))
            .collect()
    }

    /// 基于 Clone 的批量创建
    /// Clone-based batch creation
    #[inline(always)]
    fn create_batch_clone_based(&self, requests: &[(ConnectionId, E)]) -> Vec<TimerEventData<E>> {
        requests
            .iter()
            .map(|(conn_id, event)| TimerEventData::new(*conn_id, event.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timer::event::pool::PoolConfig;

    // 测试用的简单 Copy 类型
    #[derive(Debug, Clone, Copy, Default, PartialEq)]
    struct SimpleCopyEvent(u32);

    // 测试用的复杂非 Copy 类型
    #[derive(Debug, Clone, Default, PartialEq)]
    struct ComplexCloneEvent {
        data: Vec<u8>,
        id: String,
    }

    #[test]
    fn test_copy_type_strategy_selection() {
        let factory = EventFactory::<SimpleCopyEvent>::new();

        // 验证 Copy 类型被识别为 copy-like
        assert!(factory.is_copy_like());
        assert!(!factory.should_use_pool());

        // 测试单个事件创建
        let event = factory.create_event(123, SimpleCopyEvent(456));
        assert_eq!(event.connection_id, 123);
        assert_eq!(event.timeout_event, SimpleCopyEvent(456));
    }

    #[test]
    fn test_clone_type_strategy_selection() {
        let factory = EventFactory::<ComplexCloneEvent>::new();

        // 验证复杂类型不被识别为 copy-like
        assert!(!factory.is_copy_like());
        assert!(factory.should_use_pool());

        // 测试单个事件创建
        let complex_event = ComplexCloneEvent {
            data: vec![1, 2, 3],
            id: "test".to_string(),
        };
        let event = factory.create_event(123, complex_event.clone());
        assert_eq!(event.connection_id, 123);
        assert_eq!(event.timeout_event, complex_event);
    }

    #[test]
    fn test_batch_creation_copy_type() {
        let factory = EventFactory::<SimpleCopyEvent>::new();

        let requests = vec![
            (1, SimpleCopyEvent(100)),
            (2, SimpleCopyEvent(200)),
            (3, SimpleCopyEvent(300)),
        ];

        let events = factory.batch_create_events(&requests);
        assert_eq!(events.len(), 3);

        for (i, event) in events.iter().enumerate() {
            assert_eq!(event.connection_id, requests[i].0);
            assert_eq!(event.timeout_event, requests[i].1);
        }
    }

    #[test]
    fn test_batch_creation_clone_type() {
        let factory = EventFactory::<ComplexCloneEvent>::new();

        let requests = vec![
            (
                1,
                ComplexCloneEvent {
                    data: vec![1],
                    id: "first".to_string(),
                },
            ),
            (
                2,
                ComplexCloneEvent {
                    data: vec![2],
                    id: "second".to_string(),
                },
            ),
        ];

        let events = factory.batch_create_events(&requests);
        assert_eq!(events.len(), 2);

        for (i, event) in events.iter().enumerate() {
            assert_eq!(event.connection_id, requests[i].0);
            assert_eq!(event.timeout_event, requests[i].1);
        }
    }

    #[test]
    fn test_pool_usage_copy_type() {
        let factory = EventFactory::<SimpleCopyEvent>::new();
        let pool = TimerEventPool::new(PoolConfig::default());

        // Copy 类型应该忽略池，直接创建
        let event = factory.create_with_pool(&pool, 123, SimpleCopyEvent(456));
        assert_eq!(event.connection_id, 123);
        assert_eq!(event.timeout_event, SimpleCopyEvent(456));

        // 返回到池中应该是无操作
        factory.return_to_pool(event, &pool);
    }

    #[test]
    fn test_pool_usage_clone_type() {
        let factory = EventFactory::<ComplexCloneEvent>::new();
        let pool = TimerEventPool::new(PoolConfig::default());

        let complex_event = ComplexCloneEvent {
            data: vec![1, 2, 3],
            id: "test".to_string(),
        };

        // 复杂类型应该使用池
        let event = factory.create_with_pool(&pool, 123, complex_event.clone());
        assert_eq!(event.connection_id, 123);
        assert_eq!(event.timeout_event, complex_event);

        // 返回到池中应该真正释放到池
        factory.return_to_pool(event, &pool);
        assert!(pool.stats().current_size > 0);
    }

    #[test]
    fn test_strategy_traits_copy() {
        // 测试 CopyStrategy 的行为
        let event = <CopyStrategy as EventCreationStrategy<SimpleCopyEvent>>::create_single(
            123,
            SimpleCopyEvent(456),
        );
        assert_eq!(event.connection_id, 123);
        assert_eq!(event.timeout_event, SimpleCopyEvent(456));

        let requests = vec![(1, SimpleCopyEvent(100)), (2, SimpleCopyEvent(200))];
        let events =
            <CopyStrategy as EventCreationStrategy<SimpleCopyEvent>>::create_batch(&requests);
        assert_eq!(events.len(), 2);

        assert!(!<CopyStrategy as EventCreationStrategy<SimpleCopyEvent>>::should_use_pool());
    }

    #[test]
    fn test_strategy_traits_clone() {
        // 测试 CloneStrategy 的行为
        let complex_event = ComplexCloneEvent {
            data: vec![1, 2, 3],
            id: "test".to_string(),
        };

        let event = <CloneStrategy as EventCreationStrategy<ComplexCloneEvent>>::create_single(
            123,
            complex_event.clone(),
        );
        assert_eq!(event.connection_id, 123);
        assert_eq!(event.timeout_event, complex_event);

        let requests = vec![(1, complex_event.clone())];
        let events =
            <CloneStrategy as EventCreationStrategy<ComplexCloneEvent>>::create_batch(&requests);
        assert_eq!(events.len(), 1);

        // 对于复杂的 ComplexCloneEvent，应该建议使用池
        assert!(<CloneStrategy as EventCreationStrategy<
            ComplexCloneEvent,
        >>::should_use_pool());
    }
}
