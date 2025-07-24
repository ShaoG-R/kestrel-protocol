//! An estimator for the round-trip time (RTT).
//! RTT 估算器。

use std::time::Duration;

const ALPHA: f64 = 1.0 / 8.0;
const BETA: f64 = 1.0 / 4.0;

/// An estimator for the round-trip time (RTT), based on RFC 6298.
///
/// 一个基于 RFC 6298 的 RTT 估算器。
#[derive(Debug, Clone)]
pub struct RttEstimator {
    /// The smoothed round-trip time, in seconds.
    /// 平滑的往返时间（秒）。
    srtt: f64,
    /// The round-trip time variation, in seconds.
    /// 往返时间变化量（秒）。
    rttvar: f64,
    /// The retransmission timeout.
    /// 重传超时时间。
    rto: Duration,
}

impl RttEstimator {
    /// Creates a new RTT estimator with a given initial RTO.
    ///
    /// 使用给定的初始 RTO 创建一个新的 RTT 估算器。
    pub fn new(initial_rto: Duration) -> Self {
        Self {
            srtt: 0.0,
            rttvar: 0.0,
            rto: initial_rto,
        }
    }

    /// Returns the current RTO value.
    ///
    /// 返回当前的 RTO 值。
    pub fn rto(&self) -> Duration {
        self.rto
    }

    /// Updates the RTT estimator with a new sample.
    ///
    /// 使用一个新的样本更新 RTT 估算器。
    pub fn update(&mut self, rtt_sample: Duration, min_rto: Duration) {
        let rtt_sample_f64 = rtt_sample.as_secs_f64();

        if self.srtt == 0.0 {
            // First sample
            self.srtt = rtt_sample_f64;
            self.rttvar = rtt_sample_f64 / 2.0;
        } else {
            // Subsequent samples using RFC 6298 formulas
            let delta = (self.srtt - rtt_sample_f64).abs();
            self.rttvar = (1.0 - BETA) * self.rttvar + BETA * delta;
            self.srtt = (1.0 - ALPHA) * self.srtt + ALPHA * rtt_sample_f64;
        }

        let rto_f64 = self.srtt + (4.0 * self.rttvar);
        self.rto = Duration::from_secs_f64(rto_f64).max(min_rto);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn assert_f64_eq(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "Floats not equal: {} vs {}", a, b);
    }

    #[test]
    fn test_rtt_estimator_initialization() {
        let initial_rto = Duration::from_millis(500);
        let estimator = RttEstimator::new(initial_rto);
        assert_eq!(estimator.rto(), initial_rto);
    }

    #[test]
    fn test_rtt_estimator_first_sample() {
        let initial_rto = Duration::from_millis(500);
        let min_rto = Duration::from_millis(100);
        let mut estimator = RttEstimator::new(initial_rto);

        let rtt_sample = Duration::from_millis(100);
        estimator.update(rtt_sample, min_rto);

        assert_f64_eq(estimator.srtt, 0.1);
        assert_f64_eq(estimator.rttvar, 0.05);
        assert_eq!(estimator.rto(), Duration::from_millis(300));
    }

    #[test]
    fn test_rtt_estimator_subsequent_samples() {
        let initial_rto = Duration::from_millis(500);
        let min_rto = Duration::from_millis(100);
        let mut estimator = RttEstimator::new(initial_rto);

        // First sample
        estimator.update(Duration::from_millis(100), min_rto);
        assert_eq!(estimator.rto(), Duration::from_millis(300));

        // Second sample, stable RTT
        estimator.update(Duration::from_millis(100), min_rto);
        assert_f64_eq(estimator.srtt, 0.1);
        assert_f64_eq(estimator.rttvar, 0.0375);
        assert_eq!(estimator.rto(), Duration::from_millis(250));

        // Third sample, RTT increases
        estimator.update(Duration::from_millis(200), min_rto);
        assert_f64_eq(estimator.srtt, 0.1125);
        assert_f64_eq(estimator.rttvar, 0.053125);
        assert_eq!(estimator.rto(), Duration::from_millis(325));
    }

    #[test]
    fn test_rtt_estimator_min_rto_enforced() {
        let initial_rto = Duration::from_millis(500);
        let min_rto = Duration::from_millis(200);
        let mut estimator = RttEstimator::new(initial_rto);

        // A very small RTT sample
        estimator.update(Duration::from_millis(10), min_rto);

        // RTO calculation would be 0.01 + 4 * 0.005 = 0.03s (30ms), but min_rto is 200ms
        assert!(estimator.rto() >= min_rto);
        assert_eq!(estimator.rto(), min_rto);
    }
} 