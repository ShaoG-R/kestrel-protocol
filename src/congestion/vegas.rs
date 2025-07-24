//! An implementation of a Vegas-like, latency-based congestion control algorithm.
//!
//! 一个类Vegas、基于延迟的拥塞控制算法的实现。

use crate::config::Config;
use crate::congestion::CongestionControl;
use std::time::{Duration, Instant};
use tracing::debug;

/// The state of the congestion controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    SlowStart,
    CongestionAvoidance,
}

/// A Vegas-like congestion controller.
///
/// 一个类Vegas的拥塞控制器。
#[derive(Debug)]
pub struct Vegas {
    congestion_window: u32,
    slow_start_threshold: u32,
    state: State,
    min_rtt: Duration,
    config: Config,
}

impl Vegas {
    pub fn new(config: Config) -> Self {
        Self {
            congestion_window: config.initial_cwnd_packets,
            slow_start_threshold: config.initial_ssthresh,
            state: State::SlowStart,
            min_rtt: Duration::from_secs(u64::MAX),
            config,
        }
    }
}

impl CongestionControl for Vegas {
    fn on_ack(&mut self, rtt: Duration) {
        self.min_rtt = self.min_rtt.min(rtt);

        if self.state == State::CongestionAvoidance {
            let rtt_increase = rtt.as_secs_f32() - self.min_rtt.as_secs_f32();
            if rtt_increase > self.min_rtt.as_secs_f32() * self.config.latency_threshold_ratio {
                self.congestion_window =
                    ((self.congestion_window as f32) * self.config.cwnd_decrease_factor) as u32;
                self.congestion_window = self.congestion_window.max(self.config.min_cwnd_packets);
                debug!(
                    "Latency-based congestion avoidance triggered. New cwnd: {}",
                    self.congestion_window
                );
                return;
            }
        }

        if self.state == State::SlowStart {
            self.congestion_window += 1;
            if self.congestion_window >= self.slow_start_threshold {
                self.state = State::CongestionAvoidance;
            }
        } else {
            self.congestion_window += (1.0 / self.congestion_window as f32).max(1.0) as u32;
        }
    }

    fn on_packet_loss(&mut self, _now: Instant) {
        self.slow_start_threshold =
            (self.congestion_window / 2).max(self.config.min_cwnd_packets);
        self.congestion_window = self.slow_start_threshold;
        self.state = State::SlowStart;
        debug!(
            "Congestion event! New ssthresh: {}, New cwnd: {}",
            self.slow_start_threshold, self.congestion_window
        );
    }

    fn congestion_window(&self) -> u32 {
        self.congestion_window
    }
} 