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
    /// The lower bound of the `diff` value in the Vegas algorithm. If the estimated
    /// number of queued packets is below this, the window is increased.
    /// Vegas算法中 `diff` 值的下限。如果估计的排队数据包数量低于此值，则增加窗口。
    pub vegas_alpha_packets: u32,
    /// The upper bound of the `diff` value in the Vegas algorithm. If the estimated
    /// number of queued packets is above this, the window is decreased.
    /// Vegas算法中 `diff` 值的上限。如果估计的排队数据包数量高于此值，则减小窗口。
    pub vegas_beta_packets: u32,
    /// The factor by which the congestion window is decreased during non-congestive
    /// packet loss events.
    /// 在非拥塞性丢包事件期间，拥塞窗口减小的因子。
    pub vegas_gentle_decrease_factor: f32,
    /// The maximum time a connection can be idle before it's considered timed out.
    /// An idle connection is one with no packets being sent or received.
    ///
    /// 连接在被视为空闲超时之前可以处于空闲状态的最长时间。
    /// 空闲连接是指没有发送或接收数据包的连接。
    pub idle_timeout: Duration,
    /// The capacity of the user-side send buffer in bytes. Data written by the
    /// user is stored here before being packetized.
    /// 用户端发送缓冲区的容量（以字节为单位）。用户写入的数据在打包前存储在此处。
    pub send_buffer_capacity_bytes: usize,
    /// The capacity of the receive buffer in packets. This buffer stores out-of-order
    /// packets waiting for reassembly.
    ///
    /// 接收缓冲区的容量（以数据包为单位）。此缓冲区存储等待重组的乱序数据包。
    pub recv_buffer_capacity_packets: usize,
    /// The period after which a connection is truly forgotten after being closed.
    /// This is akin to TCP's TIME_WAIT state, preventing late packets from a
    /// previous connection from interfering with a new one using the same CID.
    ///
    /// 连接关闭后被真正遗忘的时间段。这类似于TCP的TIME_WAIT状态，
    /// 防止来自前一个连接的延迟数据包干扰使用相同CID的新连接。
    pub drain_timeout: Duration,
    /// The interval at which the socket actor checks for and cleans up
    /// CIDs that have completed their `drain_timeout`.
    ///
    /// 套接字actor检查并清理已完成 `drain_timeout` 的CID的间隔。
    pub draining_cleanup_interval: Duration,
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
            vegas_alpha_packets: 2,
            vegas_beta_packets: 4,
            vegas_gentle_decrease_factor: 0.8, // 20% decrease
            idle_timeout: Duration::from_secs(5),
            send_buffer_capacity_bytes: 1024 * 1024, // 1 MB
            recv_buffer_capacity_packets: 256,
            drain_timeout: Duration::from_secs(3),
            draining_cleanup_interval: Duration::from_secs(1),
        }
    }
} 