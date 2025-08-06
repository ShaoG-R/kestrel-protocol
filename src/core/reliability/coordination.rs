//! 协调层模块 - 统一协调所有组件
//! Coordination Layer Module - Unified coordination of all components

pub mod traits;
pub mod packet_coordinator;
pub mod buffer_coordinator;
pub mod flow_control_coordinator;

pub use traits::{
    FlowControlCoordinator as FlowControlCoordinatorTrait,
    FlowControlDecision, FlowControlState, FlowControlStats as FlowControlStatsTrait,
    ConfigurableFlowControlCoordinator, FlowControlCoordinatorFactory,
    AdvancedFlowControlCoordinator, NetworkCondition, AdaptiveParameters,
};
pub use packet_coordinator::{PacketCoordinator, ComprehensiveResult, PacketCoordinatorStats};
pub use buffer_coordinator::{
    BufferCoordinator, ReassemblyResult, BufferStatus, SendBufferStatus, 
    ReceiveBufferStatus, BufferCoordinatorStats,
};
pub use flow_control_coordinator::{
    FlowControlCoordinator, FlowControlCoordinatorConfig, FlowControlStats,
    VegasFlowControlCoordinator,
};

#[cfg(test)]
mod tests;