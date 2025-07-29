//! Tests for the Vegas congestion controller.
use super::vegas::{State, Vegas};
use super::CongestionControl;
use crate::config::{Config, CongestionControlConfig};
use std::time::Duration;
use tokio::time::Instant;

fn test_config() -> Config {
    Config {
        protocol_version: 1,
        congestion_control: CongestionControlConfig {
            initial_cwnd_packets: 10,
            min_cwnd_packets: 2,
            initial_ssthresh: 100,
            vegas_alpha_packets: 2,
            vegas_beta_packets: 4,
            vegas_gentle_decrease_factor: 0.8,
        },
        ..Default::default()
    }
}

#[test]
fn test_vegas_slow_start() {
    let config = test_config();
    let mut vegas = Vegas::new(config);
    vegas.slow_start_threshold = 20;

    assert_eq!(vegas.congestion_window(), 10);
    assert_eq!(vegas.state, State::SlowStart);

    vegas.on_ack(Duration::from_millis(100));
    assert_eq!(vegas.congestion_window(), 11);
}

#[test]
fn test_vegas_transition_to_congestion_avoidance() {
    let config = test_config();
    let mut vegas = Vegas::new(config);
    vegas.congestion_window = 19;
    vegas.slow_start_threshold = 20;

    vegas.on_ack(Duration::from_millis(100));
    assert_eq!(vegas.congestion_window(), 20);
    assert_eq!(vegas.state, State::CongestionAvoidance);
}

#[test]
fn test_vegas_avoidance_increase_when_below_alpha() {
    let mut vegas = Vegas::new(test_config());
    vegas.state = State::CongestionAvoidance;
    vegas.congestion_window = 10;
    vegas.min_rtt = Duration::from_millis(100);

    // With a base RTT of 100ms, the expected throughput is 100 packets/sec.
    // If the actual RTT is also 100ms, the diff is 0, which is less than alpha (2).
    // So the window should increase.
    vegas.on_ack(Duration::from_millis(100));
    assert_eq!(vegas.congestion_window(), 11);
}

#[test]
fn test_vegas_avoidance_decrease_when_above_beta() {
    let mut vegas = Vegas::new(test_config());
    vegas.state = State::CongestionAvoidance;
    vegas.congestion_window = 10;
    vegas.min_rtt = Duration::from_millis(100);

    // With min_rtt=100ms, cwnd=10 -> expected = 100 pkts/sec
    // With actual_rtt=200ms -> actual = 50 pkts/sec
    // Diff = (100 - 50) * 0.1s = 5 packets, which is > beta (4).
    // So the window should decrease.
    vegas.on_ack(Duration::from_millis(200));
    assert_eq!(vegas.congestion_window(), 9);
}

#[test]
fn test_vegas_avoidance_stable_between_alpha_beta() {
    let mut vegas = Vegas::new(test_config());
    vegas.state = State::CongestionAvoidance;
    vegas.congestion_window = 10;
    vegas.min_rtt = Duration::from_millis(100);

    // With min_rtt=100ms, cwnd=10 -> expected = 100 pkts/sec
    // With actual_rtt=130ms -> actual = ~77 pkts/sec
    // Diff = (100 - 77) * 0.1s = 2.3 packets. This is between alpha (2) and beta (4).
    // The window should remain stable.
    vegas.on_ack(Duration::from_millis(130));
    assert_eq!(vegas.congestion_window(), 10);
}

#[test]
fn test_vegas_congestive_loss_reaction() {
    let mut vegas = Vegas::new(test_config());
    vegas.state = State::CongestionAvoidance;
    vegas.congestion_window = 20;
    vegas.min_rtt = Duration::from_millis(100);
    // last_rtt is 150ms, which is > 100ms + (100ms/5) = 120ms. This indicates congestion.
    vegas.on_ack(Duration::from_millis(150));
    
    // on_ack would have already decreased the window. For this test, we want to isolate
    // the behavior of on_packet_loss, so we reset the window to its pre-ack state.
    vegas.congestion_window = 20;

    vegas.on_packet_loss(Instant::now());

    // ssthresh should be cwnd / 2 = 10
    assert_eq!(vegas.slow_start_threshold, 10);
    // cwnd should be reset to new ssthresh
    assert_eq!(vegas.congestion_window(), 10);
    // State should revert to SlowStart
    assert_eq!(vegas.state, State::SlowStart);
}

#[test]
fn test_vegas_random_loss_reaction() {
    let mut vegas = Vegas::new(test_config());
    vegas.state = State::CongestionAvoidance;
    vegas.congestion_window = 20;
    vegas.min_rtt = Duration::from_millis(100);
    // last_rtt is 100ms, indicating no congestion before the loss.
    vegas.on_ack(Duration::from_millis(100));
    let original_ssthresh = vegas.slow_start_threshold;

    vegas.on_packet_loss(Instant::now());

    // ssthresh should NOT change.
    assert_eq!(vegas.slow_start_threshold, original_ssthresh);
    // cwnd should be gently reduced (20 * 0.8 = 16)
    assert_eq!(vegas.congestion_window(), 16);
    // State should remain in CongestionAvoidance
    assert_eq!(vegas.state, State::CongestionAvoidance);
} 

#[test]
fn test_vegas_initial_state() {
    let config = test_config();
    let vegas = Vegas::new(config);

    assert_eq!(vegas.congestion_window(), 10);
    assert_eq!(vegas.slow_start_threshold, 100);
    assert_eq!(vegas.state, State::SlowStart);
    assert_eq!(vegas.min_rtt, Duration::from_secs(u64::MAX));
}

#[test]
fn test_vegas_min_rtt_tracking() {
    let mut vegas = Vegas::new(test_config());

    vegas.on_ack(Duration::from_millis(200));
    assert_eq!(vegas.min_rtt, Duration::from_millis(200));

    vegas.on_ack(Duration::from_millis(150));
    assert_eq!(vegas.min_rtt, Duration::from_millis(150));

    vegas.on_ack(Duration::from_millis(180));
    assert_eq!(vegas.min_rtt, Duration::from_millis(150));
}

#[test]
fn test_vegas_avoidance_decrease_clamp_to_min() {
    let mut config = test_config();
    config.congestion_control.min_cwnd_packets = 5;
    let mut vegas = Vegas::new(config);

    vegas.state = State::CongestionAvoidance;
    vegas.congestion_window = 5;
    vegas.min_rtt = Duration::from_millis(100);

    // This would cause a decrease if not for the minimum clamp.
    vegas.on_ack(Duration::from_millis(200));

    assert_eq!(vegas.congestion_window(), 5);
}

#[test]
fn test_vegas_congestive_loss_clamp_to_min() {
    let mut config = test_config();
    config.congestion_control.min_cwnd_packets = 5;
    let mut vegas = Vegas::new(config);
    vegas.state = State::CongestionAvoidance;
    // Set cwnd so that cwnd/2 < min_cwnd_packets
    vegas.congestion_window = 8;
    vegas.min_rtt = Duration::from_millis(100);
    // Trigger congestive loss condition
    vegas.on_ack(Duration::from_millis(130));

    vegas.on_packet_loss(Instant::now());

    // ssthresh should be max(8/2, 5) = 5
    assert_eq!(vegas.slow_start_threshold, 5);
    assert_eq!(vegas.congestion_window(), 5);
}

#[test]
fn test_vegas_loss_with_no_rtt_sample() {
    let config = test_config();
    let mut vegas = Vegas::new(config);
    vegas.congestion_window = 20;
    let original_ssthresh = vegas.slow_start_threshold;

    // A loss event before any ACK/RTT sample should be treated as congestive.
    vegas.on_packet_loss(Instant::now());

    // ssthresh should be halved
    assert_eq!(vegas.slow_start_threshold, 10);
    // cwnd should be reset to new ssthresh
    assert_eq!(vegas.congestion_window(), 10);
    // State should be SlowStart
    assert_eq!(vegas.state, State::SlowStart);
    // Ensure ssthresh was actually changed
    assert_ne!(vegas.slow_start_threshold, original_ssthresh);
}

#[test]
fn test_vegas_random_loss_clamp_to_min() {
    let mut config = test_config();
    config.congestion_control.min_cwnd_packets = 15;
    let mut vegas = Vegas::new(config);
    vegas.state = State::CongestionAvoidance;
    vegas.congestion_window = 16; // 16 * 0.8 = 12.8, which is < 15
    vegas.min_rtt = Duration::from_millis(100);
    // Trigger non-congestive loss condition
    vegas.on_ack(Duration::from_millis(100));

    vegas.on_packet_loss(Instant::now());

    // cwnd should be clamped at min_cwnd_packets
    assert_eq!(vegas.congestion_window(), 15);
} 