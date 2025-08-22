//! 协调层模块 - 统一协调所有组件
//! Coordination Layer Module - Unified coordination of all components

pub mod buffer_coordinator;
pub mod flow_control_coordinator;
pub mod packet_coordinator;
pub mod traits;

pub use buffer_coordinator::{
    BufferCoordinator, BufferCoordinatorStats, BufferStatus, ReassemblyResult, ReceiveBufferStatus,
    SendBufferStatus,
};
pub use flow_control_coordinator::{
    FlowControlCoordinator, FlowControlCoordinatorConfig, FlowControlStats,
    VegasFlowControlCoordinator,
};
pub use packet_coordinator::{ComprehensiveResult, PacketCoordinator, PacketCoordinatorStats};
pub use traits::{
    AdaptiveParameters, AdvancedFlowControlCoordinator, ConfigurableFlowControlCoordinator,
    FlowControlCoordinator as FlowControlCoordinatorTrait, FlowControlCoordinatorFactory,
    FlowControlDecision, FlowControlState, FlowControlStats as FlowControlStatsTrait,
    NetworkCondition,
};

#[cfg(test)]
mod tests;
