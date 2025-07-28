//! 连接生命周期管理模块
//! Connection Lifecycle Management Module
//!
//! 该模块提供连接生命周期的统一管理，包括状态验证、转换逻辑和管理器实现。
//! 重构后采用分层设计，职责清晰分离。
//!
//! This module provides unified connection lifecycle management, including state validation,
//! transition logic, and manager implementation. After refactoring, it adopts a layered design
//! with clear separation of responsibilities.

mod manager;
mod transitions;
mod validation;

// 重新导出主要类型和特征，保持API兼容性
// Re-export main types and traits to maintain API compatibility
pub use manager::{ConnectionLifecycleManager, DefaultLifecycleManager};
pub use transitions::{LifecycleEvent, EventListener, StateTransitionExecutor};
pub use validation::StateValidator;