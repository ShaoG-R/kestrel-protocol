//! 逻辑层模块 - 具体的业务逻辑处理
//! Logic Layer Module - Specific business logic processing

pub mod sack_processor;
pub mod retransmission_decider;
pub mod timer_event_handler;
pub mod vegas_controller;
pub mod packetization_processor;
pub mod rtt;

// 测试模块 - 只在测试时编译
// Test modules - compiled only during testing
#[cfg(test)]
mod tests;

pub use sack_processor::{SackProcessor, SackProcessResult};
pub use retransmission_decider::{RetransmissionDecider, RetransmissionDecision, RetransmissionType};
pub use timer_event_handler::{TimerEventHandler, TimerHandlingResult};
pub use vegas_controller::{
    VegasController, CongestionDecision, CongestionState, VegasStats,
};
pub use packetization_processor::{
    PacketizationProcessor, PacketizationContext, PacketizationResult, 
    ZeroRttPacketizationResult, PacketizationLimitation, PacketizationStats,
};
pub use rtt::RttEstimator;