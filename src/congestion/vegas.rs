//! An implementation of a Vegas-like, latency-based congestion control algorithm.
//!
//! 一个类Vegas、基于延迟的拥塞控制算法的实现。

use crate::config::Config;
use crate::congestion::CongestionControl;
use std::time::Duration;
use tokio::time::Instant;
use tracing::{debug, trace};

/// The state of the congestion controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    SlowStart,
    CongestionAvoidance,
}

/// A Vegas-like congestion controller.
///
/// 一个类Vegas的拥塞控制器。
#[derive(Debug)]
pub struct Vegas {
    pub(super) congestion_window: u32,

    pub(super) slow_start_threshold: u32,

    pub(super) state: State,

    pub(super) min_rtt: Duration,

    /// The last measured RTT. Used to help determine the nature of packet loss.
    last_rtt: Duration,

    config: Config,
}

impl Vegas {
    pub fn new(config: Config) -> Self {
        Self {
            congestion_window: config.initial_cwnd_packets,
            slow_start_threshold: config.initial_ssthresh,
            state: State::SlowStart,
            min_rtt: Duration::from_secs(u64::MAX),
            last_rtt: Duration::from_secs(u64::MAX),
            config,
        }
    }
}

impl CongestionControl for Vegas {
    fn on_ack(&mut self, rtt: Duration) {
        self.last_rtt = rtt;
        self.min_rtt = self.min_rtt.min(rtt);

        if self.state == State::SlowStart {
            self.congestion_window += 1;
            trace!(
                cwnd = self.congestion_window,
                "Slow Start: cwnd increased"
            );
            if self.congestion_window >= self.slow_start_threshold {
                self.state = State::CongestionAvoidance;
                trace!("State changed to CongestionAvoidance");
            }
        } else {
            // Congestion Avoidance using the Alpha-Beta mechanism.
            let expected_throughput = self.congestion_window as f32 / self.min_rtt.as_secs_f32();
            let actual_throughput = self.congestion_window as f32 / rtt.as_secs_f32();
            let diff_packets =
                (expected_throughput - actual_throughput) * self.min_rtt.as_secs_f32();

            if diff_packets < self.config.vegas_alpha_packets as f32 {
                self.congestion_window += 1;
                trace!(
                    cwnd = self.congestion_window,
                    diff = diff_packets,
                    alpha = self.config.vegas_alpha_packets,
                    "Congestion Avoidance: increasing cwnd"
                );
            } else if diff_packets > self.config.vegas_beta_packets as f32 {
                self.congestion_window =
                    (self.congestion_window - 1).max(self.config.min_cwnd_packets);
                trace!(
                    cwnd = self.congestion_window,
                    diff = diff_packets,
                    beta = self.config.vegas_beta_packets,
                    "Congestion Avoidance: decreasing cwnd"
                );
            } else {
                // Window is in the optimal range, do nothing.
                trace!(
                    cwnd = self.congestion_window,
                    diff = diff_packets,
                    "Congestion Avoidance: cwnd stable"
                );
            }
        }
    }

    fn on_packet_loss(&mut self, _now: Instant) {
        // To differentiate between congestive and non-congestive loss, we check
        // if the last RTT was significantly higher than the minimum RTT.
        // A simple heuristic: if RTT has increased by more than 20%, it's likely congestion.
        // If we haven't measured an RTT yet, conservatively assume it's congestive.
        let is_congestive_loss = self.last_rtt == Duration::from_secs(u64::MAX)
            || self.last_rtt > self.min_rtt + (self.min_rtt / 5);

        if is_congestive_loss {
            // Severe congestion event: Halve the window and enter slow start.
            self.slow_start_threshold =
                (self.congestion_window / 2).max(self.config.min_cwnd_packets);
            self.congestion_window = self.slow_start_threshold;
            self.state = State::SlowStart;
            debug!(
                "Congestive loss detected! New ssthresh: {}, New cwnd: {}",
                self.slow_start_threshold, self.congestion_window
            );
        } else {
            // Non-congestive (random) loss: Gentle decrease, stay in congestion avoidance.
            self.congestion_window =
                ((self.congestion_window as f32) * self.config.vegas_gentle_decrease_factor)
                    as u32;
            self.congestion_window = self.congestion_window.max(self.config.min_cwnd_packets);
            debug!(
                "Non-congestive loss detected. Gently reducing cwnd to: {}",
                self.congestion_window
            );
        }
    }

    fn congestion_window(&self) -> u32 {
        self.congestion_window
    }
} 