//! Defines the pluggable congestion control interface.
//! 定义了可插拔的拥塞控制接口。

use std::time::Duration;
use tokio::time::Instant;

pub mod vegas;

/// A trait for congestion control algorithms.
///
/// 拥塞控制算法的 trait。
pub trait CongestionControl: Send + Sync + 'static {
    /// Called when a packet is acknowledged.
    ///
    /// 当一个包被确认时调用。
    fn on_ack(&mut self, rtt: Duration);

    /// Called when a packet loss is detected.
    ///
    /// 当检测到丢包时调用。
    fn on_packet_loss(&mut self, now: Instant);

    /// Gets the current congestion window size in packets.
    ///
    /// 获取当前的拥塞窗口大小（以包为单位）。
    fn congestion_window(&self) -> u32;
}

#[cfg(test)]
pub use self::testing::NoCongestionControl;

#[cfg(test)]
mod testing {
    use super::{CongestionControl, Duration, Instant};

    /// A congestion controller that does nothing.
    /// Useful for testing other parts of the system.
    pub struct NoCongestionControl;

    impl NoCongestionControl {
        pub fn new() -> Self {
            Self
        }
    }

    impl CongestionControl for NoCongestionControl {
        fn on_ack(&mut self, _rtt: Duration) {}

        fn on_packet_loss(&mut self, _now: Instant) {}

        fn congestion_window(&self) -> u32 {
            // Effectively disable congestion control for tests
            u32::MAX
        }
    }
}

#[cfg(test)]
mod tests; 