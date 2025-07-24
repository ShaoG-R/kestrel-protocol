//! Tests for the Vegas congestion controller.
use super::vegas::{State, Vegas};
use super::CongestionControl;
use crate::config::Config;
use std::time::Duration;
use tokio::time::Instant;

fn test_config() -> Config {
    Config {
        initial_cwnd_packets: 2,
        min_cwnd_packets: 2,
        initial_ssthresh: 10,
        latency_threshold_ratio: 0.1, // 10%
        cwnd_decrease_factor: 0.9,    // 10% decrease
        ..Default::default()
    }
}

#[test]
fn test_vegas_slow_start() {
    let config = test_config();
    let mut vegas = Vegas::new(config);

    assert_eq!(vegas.congestion_window(), 2);
    assert_eq!(vegas.state, State::SlowStart);

    // Each ACK in slow start should increase cwnd by 1
    vegas.on_ack(Duration::from_millis(100));
    assert_eq!(vegas.congestion_window(), 3);

    vegas.on_ack(Duration::from_millis(100));
    assert_eq!(vegas.congestion_window(), 4);
}

#[test]
fn test_vegas_transition_to_congestion_avoidance() {
    let config = test_config();
    let mut vegas = Vegas::new(config);
    vegas.slow_start_threshold = 5;

    // cwnd starts at 2.
    // After 3 ACKs, cwnd will be 5.
    for _ in 0..3 {
        vegas.on_ack(Duration::from_millis(100));
    }
    // When cwnd reaches ssthresh, it should transition to CongestionAvoidance.
    assert_eq!(vegas.congestion_window(), 5);
    assert_eq!(vegas.state, State::CongestionAvoidance);
}

#[test]
fn test_vegas_congestion_avoidance_linear_increase() {
    let mut vegas = Vegas::new(test_config());
    vegas.state = State::CongestionAvoidance;
    vegas.congestion_window = 10;
    vegas.min_rtt = Duration::from_millis(100);

    // RTT is stable, cwnd should increase by 1/cwnd per RTT.
    // So, after 10 ACKs, it should increase by approximately 1.
    for _ in 0..10 {
        vegas.on_ack(Duration::from_millis(100));
    }
    assert_eq!(vegas.congestion_window(), 11);
}

#[test]
fn test_vegas_latency_based_decrease() {
    let mut vegas = Vegas::new(test_config());
    vegas.state = State::CongestionAvoidance;
    vegas.congestion_window = 20;
    vegas.min_rtt = Duration::from_millis(100);

    // RTT increases by 20ms, which is 20% > 10% threshold
    let high_rtt = Duration::from_millis(120);
    vegas.on_ack(high_rtt);

    // cwnd should decrease by 10% (0.9 factor)
    // 20 * 0.9 = 18
    assert_eq!(vegas.congestion_window(), 18);
}

#[test]
fn test_vegas_packet_loss_reaction() {
    let mut vegas = Vegas::new(test_config());
    vegas.state = State::CongestionAvoidance;
    vegas.congestion_window = 20;

    vegas.on_packet_loss(Instant::now());

    // ssthresh should be cwnd / 2 = 10
    assert_eq!(vegas.slow_start_threshold, 10);
    // cwnd should be reset to new ssthresh
    assert_eq!(vegas.congestion_window(), 10);
    // State should revert to SlowStart
    assert_eq!(vegas.state, State::SlowStart);
} 