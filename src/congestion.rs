//! Defines the pluggable congestion control interface.
//! 定义了可插拔的拥塞控制接口。

use std::time::{Duration, Instant};

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