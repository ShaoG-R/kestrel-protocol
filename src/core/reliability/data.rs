//! 数据层模块 - 纯数据存储和状态管理
//! Data Layer Module - Pure data storage and state management

pub mod in_flight_store;
pub mod buffer_stores;

pub use in_flight_store::{InFlightPacket, InFlightPacketStore, PacketState};
pub use buffer_stores::{
    SendBufferStore, ReceiveBufferStore, PacketOrFin,
    SendBufferStats, ReceiveBufferStats,
};