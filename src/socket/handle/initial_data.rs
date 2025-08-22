//! A wrapper type for 0-RTT data to enforce size limits at the API level.
//!
//! 一个用于0-RTT数据的包装类型，以在API级别强制执行大小限制。

use crate::{
    config::Config,
    error::{Error, Result},
    packet::header::{LongHeader, ShortHeader},
};
use bytes::Bytes;

/// Data for a 0-RTT connection attempt, guaranteed to fit within a single UDP datagram.
///
/// This type is created using [`InitialData::new`], which validates that the
/// provided data, plus the necessary protocol overhead for a `SYN` and `PUSH`
/// frames, does not exceed the `max_packet_size` defined in the [`Config`].
/// This provides a compile-time-like guarantee that the initial connection
/// packet will not be fragmented.
///
/// 用于0-RTT连接尝试的数据，保证能容纳在单个UDP数据报内。
///
/// 此类型使用 [`InitialData::new`] 创建，该函数会验证所提供的数据，
/// 加上`SYN`和`PUSH`帧所需的协议开销后，不超过在[`Config`]中定义的
/// `max_packet_size`。这提供了一个类似编译时的保证，确保初始连接包不会被分片。
#[derive(Debug, Clone)]
pub struct InitialData(Bytes);

impl InitialData {
    /// Creates new `InitialData`, checking if it respects the single-packet limit.
    ///
    /// This function calculates the total size of a UDP packet containing a `SYN`
    /// frame and the `PUSH` frames required to carry `data`. If this total size
    /// exceeds `config.max_packet_size`, an `Error::InitialDataTooLarge` is returned.
    ///
    /// 创建新的 `InitialData`，检查其是否遵守单包限制。
    ///
    /// 此函数计算包含一个 `SYN` 帧和承载 `data` 所需的 `PUSH` 帧的UDP包的总大小。
    /// 如果总大小超过 `config.max_packet_size`，则返回 `Error::InitialDataTooLarge`。
    pub fn new(data: &[u8], config: &Config) -> Result<Self> {
        if data.is_empty() {
            return Ok(Self(Bytes::new()));
        }

        // Calculate the overhead of the SYN frame.
        // Command (1) + LongHeader
        // 计算SYN帧的开销。
        // 命令 (1) + 长头
        let syn_overhead = 1 + LongHeader::ENCODED_SIZE;

        // Calculate the number of PUSH frames and their total header overhead.
        // 计算PUSH帧的数量及其总头部开销。
        let max_payload_per_push = config.connection.max_payload_size;
        let num_push_frames = data.len().div_ceil(max_payload_per_push);
        // Each PUSH frame has Command (1) + ShortHeader
        // 每个PUSH帧都有 命令 (1) + 短头
        let push_header_overhead = num_push_frames * (1 + ShortHeader::ENCODED_SIZE);

        let total_size = syn_overhead + push_header_overhead + data.len();

        if total_size > config.connection.max_packet_size {
            return Err(Error::InitialDataTooLarge);
        }

        Ok(Self(Bytes::copy_from_slice(data)))
    }

    /// Consumes the `InitialData` and returns the inner `Bytes`.
    ///
    /// 消费 `InitialData` 并返回内部的 `Bytes`。
    pub(crate) fn into_bytes(self) -> Bytes {
        self.0
    }
}
