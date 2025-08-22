//! 定义了SACK（选择性确认）相关的数据结构和逻辑。
//! Defines data structures and logic related to SACK (Selective Acknowledgment).

use bytes::{Buf, BufMut, Bytes};
use std::ops::RangeInclusive;

/// Represents a continuous range of acknowledged sequence numbers.
/// 代表一个连续的已确认序列号范围。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SackRange {
    /// The start of the acknowledged range (inclusive).
    /// 确认范围的起始（包含）。
    pub start: u32,
    /// The end of the acknowledged range (inclusive).
    /// 确认范围的结束（包含）。
    pub end: u32,
}

impl From<RangeInclusive<u32>> for SackRange {
    fn from(range: RangeInclusive<u32>) -> Self {
        Self {
            start: *range.start(),
            end: *range.end(),
        }
    }
}

/// The size of a single SACK range on the wire.
/// 单个SACK范围在网络传输中的大小。
const SACK_RANGE_SIZE: usize = 8; // u32 + u32

/// Encodes a list of SACK ranges into a buffer.
/// 将SACK范围列表编码到缓冲区中。
pub fn encode_sack_ranges<B: BufMut>(ranges: &[SackRange], buf: &mut B) {
    for range in ranges {
        buf.put_u32(range.start);
        buf.put_u32(range.end);
    }
}

/// Decodes a list of SACK ranges from a buffer.
/// The buffer is expected to only contain the SACK payload.
///
/// 从缓冲区解码SACK范围列表。
/// 缓冲区应只包含SACK的载荷。
pub fn decode_sack_ranges(mut buf: Bytes) -> Vec<SackRange> {
    let mut ranges = Vec::new();
    while buf.remaining() >= SACK_RANGE_SIZE {
        let start = buf.get_u32();
        let end = buf.get_u32();
        ranges.push(SackRange { start, end });
    }
    ranges
}
