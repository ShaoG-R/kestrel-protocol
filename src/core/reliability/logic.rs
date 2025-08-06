//! 逻辑层模块 - 具体的业务逻辑处理
//! Logic Layer Module - Specific business logic processing

pub mod retransmission;
pub mod timer_event_handler;
pub mod packetization_processor;
pub mod congestion;

// 测试模块 - 只在测试时编译
// Test modules - compiled only during testing
#[cfg(test)]
mod tests;

pub use retransmission::sack_processor::{SackProcessResult, SackProcessor};
pub use retransmission::{RetransmissionDecider, RetransmissionDecision, RetransmissionType};
pub use timer_event_handler::{TimerEventHandler, TimerHandlingResult};
pub use congestion::vegas_controller::{
    CongestionDecision, CongestionState, VegasController, VegasStats,
};
pub use packetization_processor::{
    PacketizationContext, PacketizationLimitation, PacketizationProcessor,
    PacketizationResult, PacketizationStats, ZeroRttPacketizationResult,
};
pub use congestion::rtt::RttEstimator;