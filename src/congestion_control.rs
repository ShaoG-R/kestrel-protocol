//! Implements a congestion control algorithm.
//!
//! The algorithm is a blend of traditional TCP congestion control (slow start,
//! congestion avoidance) with a latency-based approach inspired by TCP Vegas.
//! It aims to be stable in networks with high jitter and packet loss.
//!
//! 实现拥塞控制算法。
//!
//! 该算法融合了传统的TCP拥塞控制（慢启动、拥塞避免）和受TCP Vegas启发的
//! 基于延迟的方法。它旨在在高抖动和高丢包率的网络中保持稳定。

use crate::config::Config;
use std::time::Duration;
use tracing::debug;

/// The state of the congestion controller.
/// 拥塞控制器的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// In slow start, the congestion window grows exponentially.
    /// 慢启动阶段，拥塞窗口指数级增长。
    SlowStart,
    /// In congestion avoidance, the congestion window grows linearly.
    /// 拥塞避免阶段，拥塞窗口线性增长。
    CongestionAvoidance,
}

/// A congestion controller.
///
/// 拥塞控制器。
#[derive(Debug)]
pub struct CongestionController {
    /// The congestion window, in packets.
    /// 拥塞窗口（以包为单位）。
    congestion_window: u32,
    /// The slow start threshold, in packets.
    /// 慢启动阈值（以包为单位）。
    slow_start_threshold: u32,
    /// The current state of the controller.
    /// 控制器的当前状态。
    state: State,
    /// The minimum RTT observed so far.
    /// 到目前为止观察到的最小RTT。
    min_rtt: Duration,
    /// A reference to the connection's configuration.
    /// 对连接配置的引用。
    config: Config,
}

impl CongestionController {
    /// Creates a new `CongestionController`.
    /// 创建一个新的 `CongestionController`。
    pub fn new(config: Config) -> Self {
        Self {
            congestion_window: config.initial_cwnd_packets,
            slow_start_threshold: config.initial_ssthresh,
            state: State::SlowStart,
            min_rtt: Duration::from_secs(u64::MAX),
            config,
        }
    }

    /// Gets the current congestion window size in packets.
    /// 获取当前的拥塞窗口大小（以包为单位）。
    pub fn congestion_window(&self) -> u32 {
        self.congestion_window
    }

    /// Called when a packet is acknowledged.
    ///
    /// The controller updates its state based on the acknowledgement and the
    /// round-trip time sample.
    ///
    /// 当一个包被确认时调用。
    ///
    /// 控制器根据确认和RTT样本更新其状态。
    pub fn on_ack(&mut self, rtt: Duration) {
        // First, update the minimum RTT.
        // 首先，更新最小RTT。
        self.min_rtt = self.min_rtt.min(rtt);

        // Latency-based congestion control check
        // We only do this in CongestionAvoidance to avoid shrinking the window too early.
        if self.state == State::CongestionAvoidance {
            let rtt_increase = rtt.as_secs_f32() - self.min_rtt.as_secs_f32();
            if rtt_increase > self.min_rtt.as_secs_f32() * self.config.latency_threshold_ratio {
                // RTT is increasing, potential congestion.
                // Gently reduce the congestion window.
                self.congestion_window =
                    ((self.congestion_window as f32) * self.config.cwnd_decrease_factor) as u32;
                self.congestion_window = self.congestion_window.max(self.config.min_cwnd_packets);
                debug!(
                    "Latency-based congestion avoidance triggered. New cwnd: {}",
                    self.congestion_window
                );
                // After reducing, we don't increase it further in this ACK.
                return;
            }
        }

        if self.state == State::SlowStart {
            // In slow start, cwnd increases by 1 for each acked packet.
            // 在慢启动阶段，每确认一个包，cwnd增加1。
            self.congestion_window += 1;
            // If we've crossed the threshold, enter congestion avoidance.
            // 如果超过了阈值，则进入拥塞避免阶段。
            if self.congestion_window >= self.slow_start_threshold {
                self.state = State::CongestionAvoidance;
            }
        } else {
            // In congestion avoidance, cwnd increases by 1 per RTT.
            // We approximate this by increasing by 1/cwnd for each acked packet.
            // 在拥塞避免阶段，每个RTT内cwnd增加1。
            // 我们通过为每个确认的包增加1/cwnd来近似实现。
            // (Note: integer division will make this increase very slow for large cwnd)
            self.congestion_window += (1.0 / self.congestion_window as f32).max(1.0) as u32;
        }
    }

    /// Called when a packet loss is detected (e.g., via RTO or fast retransmit).
    ///
    /// The controller reacts to the congestion signal by reducing its sending rate.
    ///
    /// 当检测到丢包时调用（例如，通过RTO或快速重传）。
    ///
    /// 控制器通过降低其发送速率来响应拥塞信号。
    pub fn on_packet_loss(&mut self) {
        // This is a congestion event.
        // 这是一个拥塞事件。
        self.slow_start_threshold =
            (self.congestion_window / 2).max(self.config.min_cwnd_packets);
        self.congestion_window = self.slow_start_threshold;
        self.state = State::SlowStart;
        debug!(
            "Congestion event! New ssthresh: {}, New cwnd: {}",
            self.slow_start_threshold, self.congestion_window
        );
    }
} 