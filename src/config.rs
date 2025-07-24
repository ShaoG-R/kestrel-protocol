//! 定义了连接和协议的可配置参数。
//! Defines configurable parameters for connections and the protocol.

use std::time::Duration;

/// A structure containing all configurable parameters for a connection.
///
/// 包含所有连接可配置参数的结构体。
#[derive(Debug, Clone)]
pub struct Config {
    /// The protocol version number. Used in the long header.
    /// 协议版本号。用于长头中。
    pub protocol_version: u8,
    /// The initial round-trip time (RTO) for a new connection.
    /// 新连接的初始往返时间 (RTO)。
    pub initial_rto: Duration,
    /// The minimum RTO value. The RTO will not be allowed to fall below this.
    /// 最小RTO值。RTO不允许低于此值。
    pub min_rto: Duration,
    /// The number of unacknowledged packets with higher sequence numbers that
    /// must be received before a packet is considered lost and fast-retransmitted.
    /// 在一个包被认为丢失并进行快速重传之前，必须收到的具有更高序列号的未确认数据包的数量。
    pub fast_retx_threshold: u16,
    /// The maximum size of the payload for a single PUSH packet.
    /// 单个PUSH包的最大载荷大小。
    pub max_payload_size: usize,
    /// The number of ACK-eliciting packets to receive before sending an immediate ACK.
    /// 在发送即时ACK之前要接收的触发ACK的包的数量。
    pub ack_threshold: u16,
    /// The initial congestion window size in packets.
    /// 初始拥塞窗口大小（以包为单位）。
    pub initial_cwnd_packets: u32,
    /// The minimum congestion window size in packets.
    /// 最小拥塞窗口大小（以包为单位）。
    pub min_cwnd_packets: u32,
    /// The initial slow start threshold in packets.
    /// 初始慢启动阈值（以包为单位）。
    pub initial_ssthresh: u32,
    /// The ratio of RTT increase over the minimum RTT that triggers latency-based
    /// congestion control to reduce the window.
    /// RTT增量与最小RTT的比率，该比率会触发基于延迟的拥塞控制以减小窗口。
    pub latency_threshold_ratio: f32,
    /// The factor by which the congestion window is decreased during latency-based
    /// congestion avoidance.
    /// 在基于延迟的拥塞避免期间，拥塞窗口减小的因子。
    pub cwnd_decrease_factor: f32,
    /// The maximum time a connection can be idle before it's considered timed out.
    /// An idle connection is one with no packets being sent or received.
    ///
    /// 连接在被视为空闲超时之前可以处于空闲状态的最长时间。
    /// 空闲连接是指没有发送或接收数据包的连接。
    pub idle_timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            protocol_version: 1,
            initial_rto: Duration::from_millis(1000),
            min_rto: Duration::from_millis(500),
            fast_retx_threshold: 3,
            max_payload_size: 1200,
            ack_threshold: 2,
            initial_cwnd_packets: 32,
            min_cwnd_packets: 4,
            initial_ssthresh: u32::MAX,
            latency_threshold_ratio: 0.1, // 10%
            cwnd_decrease_factor: 0.9,    // 10% decrease
            idle_timeout: Duration::from_secs(5),
        }
    }
} 