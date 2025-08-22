//! 数据层模块 - 纯数据存储和状态管理
//! Data Layer Module - Pure data storage and state management

pub mod buffer_stores;
pub mod in_flight_store;

#[cfg(test)]
mod tests;

pub use buffer_stores::{
    PacketOrFin, ReceiveBufferStats, ReceiveBufferStore, SendBufferStats, SendBufferStore,
};
pub use in_flight_store::{InFlightPacket, InFlightPacketStore, PacketState};
