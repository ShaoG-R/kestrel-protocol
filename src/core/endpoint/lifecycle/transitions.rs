//! 连接状态转换逻辑模块
//! Connection State Transition Logic Module
//!
//! 该模块负责处理连接状态的转换逻辑、状态名称管理和转换相关的事件处理。
//! 提供统一的状态转换执行机制。
//!
//! This module handles connection state transition logic, state name management,
//! and transition-related event handling. It provides a unified state transition
//! execution mechanism.

use super::validation::StateValidator;
use crate::{
    core::endpoint::types::state::ConnectionState,
    error::{Error, Result},
};
use tracing::{trace, warn};

/// 生命周期事件类型
/// Lifecycle event types
#[derive(Debug, Clone, PartialEq)]
pub enum LifecycleEvent {
    /// 连接初始化
    /// Connection initialization
    ConnectionInitialized { local_cid: u32, remote_addr: std::net::SocketAddr },
    /// 连接建立中
    /// Connection establishing
    ConnectionEstablishing,
    /// 连接已建立
    /// Connection established
    ConnectionEstablished,
    /// 连接关闭中
    /// Connection closing
    ConnectionClosing,
    /// 连接已关闭
    /// Connection closed
    ConnectionClosed,
    /// 路径验证开始
    /// Path validation started
    PathValidationStarted { new_addr: std::net::SocketAddr },
    /// 路径验证完成
    /// Path validation completed
    PathValidationCompleted { success: bool },
    /// 状态转换事件
    /// State transition event
    StateTransition { from: String, to: String },
    /// 连接超时
    /// Connection timeout
    ConnectionTimeout,
    /// 强制关闭
    /// Force close
    ForceClose,
}

/// 事件监听器类型定义
/// Event listener type definition
pub type EventListener = Box<dyn Fn(&LifecycleEvent) + Send + Sync>;

/// 状态转换执行器，负责执行状态转换和相关的事件处理
/// State transition executor responsible for executing state transitions and related event handling
pub struct StateTransitionExecutor {
    /// 连接ID，用于日志记录
    /// Connection ID for logging
    cid: u32,
    /// 事件监听器列表
    /// List of event listeners
    event_listeners: Vec<EventListener>,
}

impl std::fmt::Debug for StateTransitionExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateTransitionExecutor")
            .field("cid", &self.cid)
            .field("event_listeners_count", &self.event_listeners.len())
            .finish()
    }
}

impl StateTransitionExecutor {
    /// 创建新的状态转换执行器
    /// Create a new state transition executor
    pub fn new(cid: u32) -> Self {
        Self {
            cid,
            event_listeners: Vec::new(),
        }
    }

    /// 设置连接ID
    /// Set connection ID
    pub fn set_cid(&mut self, cid: u32) {
        self.cid = cid;
    }

    /// 执行状态转换
    /// Execute state transition
    pub fn execute_transition(
        &self,
        current_state: &ConnectionState,
        new_state: ConnectionState,
    ) -> Result<ConnectionState> {
        // 验证转换是否合法
        // Validate if transition is legal
        if !StateValidator::is_valid_transition(current_state, &new_state) {
            warn!(
                cid = self.cid,
                current_state = ?current_state,
                attempted_state = ?new_state,
                "Invalid state transition attempted"
            );
            return Err(Error::InvalidPacket);
        }

        let old_state_discriminant = std::mem::discriminant(current_state);
        let new_state_discriminant = std::mem::discriminant(&new_state);

        // 触发状态转换事件
        // Trigger state transition event
        if old_state_discriminant != new_state_discriminant {
            let from_state = Self::state_name_for(current_state);
            let to_state = Self::state_name_for(&new_state);
            
            self.trigger_event(LifecycleEvent::StateTransition {
                from: from_state.to_string(),
                to: to_state.to_string(),
            });
            
            trace!(
                cid = self.cid,
                from = from_state,
                to = to_state,
                "State transition executed"
            );
        }

        // 根据新状态触发相应的生命周期事件
        // Trigger corresponding lifecycle events based on new state
        self.trigger_lifecycle_event(&new_state);

        Ok(new_state)
    }

    /// 获取指定状态的字符串表示（静态方法）
    /// Gets string representation of specified state (static method)
    pub fn state_name_for(state: &ConnectionState) -> &'static str {
        match state {
            ConnectionState::Connecting => "Connecting",
            ConnectionState::SynReceived => "SynReceived",
            ConnectionState::Established => "Established",
            ConnectionState::ValidatingPath { .. } => "ValidatingPath",
            ConnectionState::Closing => "Closing",
            ConnectionState::ClosingWait => "ClosingWait",
            ConnectionState::FinWait => "FinWait",
            ConnectionState::Closed => "Closed",
        }
    }

    /// 执行优雅关闭的状态转换逻辑
    /// Execute graceful shutdown state transition logic
    pub fn execute_graceful_shutdown(&self, current_state: &ConnectionState) -> Result<ConnectionState> {
        let target_state = match current_state {
            ConnectionState::Connecting => {
                // 在连接建立前关闭，转换到Closing状态以正确发送FIN
                // Close before connection establishment, transition to Closing to properly send FIN
                ConnectionState::Closing
            }
            ConnectionState::SynReceived => {
                // 在服务端等待状态关闭，转换到Closing状态以正确发送FIN
                // Close in server waiting state, transition to Closing to properly send FIN
                ConnectionState::Closing
            }
            ConnectionState::Established => {
                ConnectionState::Closing
            }
            ConnectionState::FinWait => {
                // 如果已经在等待对方的FIN，则可以直接转换到Closing
                // If already waiting for peer's FIN, can directly transition to Closing
                ConnectionState::Closing
            }
            ConnectionState::ValidatingPath { .. } => {
                // 路径验证期间关闭，回到Closing状态
                // Close during path validation, go to Closing state
                ConnectionState::Closing
            }
            ConnectionState::Closing | ConnectionState::ClosingWait | ConnectionState::Closed => {
                // 已经在关闭过程中或已关闭，返回当前状态
                // Already in closing process or closed, return current state
                return Ok(current_state.clone());
            }
        };

        self.execute_transition(current_state, target_state)
    }

    /// 注册事件监听器
    /// Register event listener
    pub fn register_event_listener(&mut self, listener: EventListener) {
        self.event_listeners.push(listener);
    }

    /// 移除所有事件监听器
    /// Remove all event listeners
    pub fn clear_event_listeners(&mut self) {
        self.event_listeners.clear();
    }

    /// 触发生命周期事件
    /// Trigger lifecycle event
    pub fn trigger_event(&self, event: LifecycleEvent) {
        for listener in &self.event_listeners {
            listener(&event);
        }
    }

    /// 根据状态触发对应的生命周期事件
    /// Trigger corresponding lifecycle events based on state
    fn trigger_lifecycle_event(&self, state: &ConnectionState) {
        match state {
            ConnectionState::Connecting => {
                self.trigger_event(LifecycleEvent::ConnectionEstablishing);
            }
            ConnectionState::Established => {
                self.trigger_event(LifecycleEvent::ConnectionEstablished);
            }
            ConnectionState::Closing | ConnectionState::FinWait => {
                self.trigger_event(LifecycleEvent::ConnectionClosing);
            }
            ConnectionState::Closed => {
                self.trigger_event(LifecycleEvent::ConnectionClosed);
            }
            ConnectionState::ValidatingPath { new_addr, .. } => {
                self.trigger_event(LifecycleEvent::PathValidationStarted {
                    new_addr: *new_addr,
                });
            }
            _ => {}
        }
    }

    /// 触发连接初始化事件
    /// Trigger connection initialization event
    pub fn trigger_initialization_event(&self, local_cid: u32, remote_addr: std::net::SocketAddr) {
        self.trigger_event(LifecycleEvent::ConnectionInitialized {
            local_cid,
            remote_addr,
        });
    }

    /// 触发超时事件
    /// Trigger timeout event
    pub fn trigger_timeout_event(&self) {
        self.trigger_event(LifecycleEvent::ConnectionTimeout);
    }

    /// 触发强制关闭事件
    /// Trigger force close event
    pub fn trigger_force_close_event(&self) {
        self.trigger_event(LifecycleEvent::ForceClose);
    }

    /// 触发路径验证完成事件
    /// Trigger path validation completed event
    pub fn trigger_path_validation_completed(&self, success: bool) {
        self.trigger_event(LifecycleEvent::PathValidationCompleted { success });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, SocketAddr};
    use std::sync::{Arc, Mutex};

    fn create_test_address() -> SocketAddr {
        SocketAddr::new(Ipv4Addr::new(127, 0, 0, 1).into(), 8080)
    }

    #[test]
    fn test_state_name_for() {
        assert_eq!(StateTransitionExecutor::state_name_for(&ConnectionState::Connecting), "Connecting");
        assert_eq!(StateTransitionExecutor::state_name_for(&ConnectionState::Established), "Established");
        assert_eq!(StateTransitionExecutor::state_name_for(&ConnectionState::Closed), "Closed");
        
        let validating_state = ConnectionState::ValidatingPath {
            new_addr: create_test_address(),
            challenge_data: 12345,
            notifier: None,
        };
        assert_eq!(StateTransitionExecutor::state_name_for(&validating_state), "ValidatingPath");
    }

    #[test]
    fn test_execute_valid_transition() {
        let executor = StateTransitionExecutor::new(12345);
        
        let result = executor.execute_transition(
            &ConnectionState::Connecting,
            ConnectionState::Established,
        );
        
        assert!(result.is_ok());
        match result.unwrap() {
            ConnectionState::Established => {},
            _ => panic!("Expected Established state"),
        }
    }

    #[test]
    fn test_execute_invalid_transition() {
        let executor = StateTransitionExecutor::new(12345);
        
        let result = executor.execute_transition(
            &ConnectionState::Closed,
            ConnectionState::Established,
        );
        
        assert!(result.is_err());
    }

    #[test]
    fn test_graceful_shutdown_transitions() {
        let executor = StateTransitionExecutor::new(12345);

        // 从Established状态开始关闭
        let result = executor.execute_graceful_shutdown(&ConnectionState::Established);
        assert!(result.is_ok());
        match result.unwrap() {
            ConnectionState::Closing => {},
            _ => panic!("Expected Closing state"),
        }

        // 从Connecting状态开始关闭
        let result = executor.execute_graceful_shutdown(&ConnectionState::Connecting);
        assert!(result.is_ok());
        match result.unwrap() {
            ConnectionState::Closing => {},
            _ => panic!("Expected Closing state"),
        }

        // 从已关闭状态开始关闭（应该返回原状态）
        let result = executor.execute_graceful_shutdown(&ConnectionState::Closed);
        assert!(result.is_ok());
        match result.unwrap() {
            ConnectionState::Closed => {},
            _ => panic!("Expected Closed state"),
        }
    }

    #[test]
    fn test_event_listeners() {
        let mut executor = StateTransitionExecutor::new(12345);
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();

        // 注册事件监听器
        executor.register_event_listener(Box::new(move |event| {
            events_clone.lock().unwrap().push(event.clone());
        }));

        // 触发一些事件
        executor.trigger_initialization_event(12345, create_test_address());
        executor.trigger_timeout_event();
        executor.trigger_force_close_event();

        // 检查收集到的事件
        let captured_events = events.lock().unwrap();
        assert_eq!(captured_events.len(), 3);
        
        assert!(captured_events.iter().any(|e| matches!(e, LifecycleEvent::ConnectionInitialized { .. })));
        assert!(captured_events.iter().any(|e| matches!(e, LifecycleEvent::ConnectionTimeout)));
        assert!(captured_events.iter().any(|e| matches!(e, LifecycleEvent::ForceClose)));
    }

    #[test]
    fn test_clear_event_listeners() {
        let mut executor = StateTransitionExecutor::new(12345);
        
        // 注册监听器
        executor.register_event_listener(Box::new(|_| {}));
        assert_eq!(executor.event_listeners.len(), 1);
        
        // 清除监听器
        executor.clear_event_listeners();
        assert_eq!(executor.event_listeners.len(), 0);
    }

    // 注意：路径验证清理逻辑现在在 DefaultLifecycleManager 层面处理
    // Note: Path validation cleanup logic is now handled at the DefaultLifecycleManager level
} 