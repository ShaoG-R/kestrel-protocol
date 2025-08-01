//! 时间轮定时器条目实现
//! Timer entry implementation for timing wheel

use crate::timer::event::TimerEvent;
use crate::timer::event::traits::EventDataTrait;
use tokio::time::Instant;

/// 定时器条目ID，用于在时间轮中唯一标识定时器条目
/// Timer entry ID, used to uniquely identify timer entries in the timing wheel
pub type TimerEntryId = u64;

/// 时间轮中的定时器条目
/// Timer entry in the timing wheel
#[derive(Debug)]
pub struct TimerEntry<E: EventDataTrait> {
    /// 条目ID
    /// Entry ID
    pub id: TimerEntryId,
    /// 到期时间
    /// Expiration time
    pub expiry_time: Instant,
    /// 定时器事件
    /// Timer event
    pub event: TimerEvent<E>,
}

impl<E: EventDataTrait> TimerEntry<E> {
    /// 创建新的定时器条目
    /// Create new timer entry
    pub fn new(id: TimerEntryId, expiry_time: Instant, event: TimerEvent<E>) -> Self {
        Self {
            id,
            expiry_time,
            event,
        }
    }
}