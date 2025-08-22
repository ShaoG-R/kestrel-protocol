//! 连接生命周期管理器 - 统一管理连接的完整生命周期
//! Connection Lifecycle Manager - Unified management of connection lifecycle
//!
//! 该模块提供了统一的连接生命周期管理接口，协调状态转换、
//! 事件处理、清理逻辑等各个方面。重构后使用分离的验证和转换模块。
//!
//! This module provides a unified connection lifecycle management interface,
//! coordinating state transitions, event handling, cleanup logic, and other aspects.
//! After refactoring, it uses separated validation and transition modules.

use super::{
    transitions::{EventListener, StateTransitionExecutor},
    validation::StateValidator,
};
use crate::{
    config::Config,
    core::endpoint::types::state::ConnectionState,
    error::{Error, Result},
};
use std::net::SocketAddr;
use tokio::{sync::oneshot, time::Instant};
use tracing::{debug, info};

/// 生命周期管理器的接口
/// Interface for lifecycle manager
pub trait ConnectionLifecycleManager {
    /// 初始化连接生命周期
    /// Initialize connection lifecycle
    fn initialize(&mut self, local_cid: u32, remote_addr: SocketAddr) -> Result<()>;

    /// 尝试转换到新状态
    /// Attempt to transition to a new state
    fn transition_to(&mut self, new_state: ConnectionState) -> Result<()>;

    /// 获取当前状态
    /// Get current state
    fn current_state(&self) -> &ConnectionState;

    /// 检查是否可以发送数据
    /// Check if data can be sent
    fn can_send_data(&self) -> bool;

    /// 检查是否可以接收数据
    /// Check if data can be received
    fn can_receive_data(&self) -> bool;

    /// 检查连接是否应该关闭
    /// Check if connection should be closed
    fn should_close(&self) -> bool;

    /// 开始优雅关闭
    /// Start graceful shutdown
    fn begin_graceful_shutdown(&mut self) -> Result<()>;

    /// 强制关闭连接
    /// Force close connection
    fn force_close(&mut self) -> Result<()>;

    /// 开始路径验证
    /// Start path validation
    fn start_path_validation(
        &mut self,
        new_addr: SocketAddr,
        challenge_data: u64,
        notifier: oneshot::Sender<Result<()>>,
    ) -> Result<()>;

    /// 完成路径验证
    /// Complete path validation
    fn complete_path_validation(&mut self, success: bool) -> Result<()>;

    /// 注册事件监听器
    /// Register event listener
    fn register_event_listener(&mut self, listener: EventListener);

    /// 移除所有事件监听器
    /// Remove all event listeners
    fn clear_event_listeners(&mut self);
}

/// 默认的生命周期管理器实现
/// Default lifecycle manager implementation
pub struct DefaultLifecycleManager {
    /// 当前连接状态
    /// Current connection state
    current_state: ConnectionState,
    /// 连接ID，用于日志记录
    /// Connection ID for logging
    cid: u32,
    /// 远程地址
    /// Remote address
    remote_addr: Option<SocketAddr>,
    /// 配置
    /// Configuration
    config: Config,
    /// 连接开始时间
    /// Connection start time
    start_time: Instant,
    /// 状态转换执行器
    /// State transition executor
    transition_executor: StateTransitionExecutor,
}

impl std::fmt::Debug for DefaultLifecycleManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefaultLifecycleManager")
            .field("current_state", &self.current_state)
            .field("cid", &self.cid)
            .field("remote_addr", &self.remote_addr)
            .field("config", &self.config)
            .field("start_time", &self.start_time)
            .field("transition_executor", &self.transition_executor)
            .finish()
    }
}

impl DefaultLifecycleManager {
    /// 创建新的生命周期管理器
    /// Create a new lifecycle manager
    pub fn new(initial_state: ConnectionState, config: Config) -> Self {
        Self {
            current_state: initial_state,
            cid: 0, // 将在初始化时设置
            remote_addr: None,
            config,
            start_time: Instant::now(),
            transition_executor: StateTransitionExecutor::new(0), // CID将在初始化时更新
        }
    }

    /// 获取连接开始时间
    /// Gets the connection start time
    pub fn start_time(&self) -> Instant {
        self.start_time
    }

    /// 获取连接ID
    /// Gets the connection ID
    pub fn connection_id(&self) -> u32 {
        self.cid
    }

    /// 获取远程地址
    /// Gets the remote address
    pub fn remote_addr(&self) -> Option<SocketAddr> {
        self.remote_addr
    }

    /// 检查连接是否处于活跃状态
    /// Check if connection is in active state
    pub fn is_active(&self) -> bool {
        StateValidator::is_active(&self.current_state)
    }

    /// 检查连接是否正在关闭
    /// Check if connection is closing
    pub fn is_closing(&self) -> bool {
        StateValidator::is_closing(&self.current_state)
    }

    /// 获取当前状态的字符串表示（用于日志）
    /// Gets string representation of current state (for logging)
    pub fn state_name(&self) -> &'static str {
        StateTransitionExecutor::state_name_for(&self.current_state)
    }

    /// 触发超时事件
    /// Trigger timeout event
    pub fn trigger_timeout(&self) {
        self.transition_executor.trigger_timeout_event();
    }
}

impl ConnectionLifecycleManager for DefaultLifecycleManager {
    fn initialize(&mut self, local_cid: u32, remote_addr: SocketAddr) -> Result<()> {
        self.cid = local_cid;
        self.remote_addr = Some(remote_addr);
        self.start_time = Instant::now();

        // 更新状态转换执行器的连接ID
        // Update the connection ID in the state transition executor
        self.transition_executor.set_cid(local_cid);

        // 触发初始化事件
        // Trigger initialization event
        self.transition_executor
            .trigger_initialization_event(local_cid, remote_addr);

        info!(
            cid = local_cid,
            ?remote_addr,
            "Connection lifecycle initialized"
        );

        Ok(())
    }

    fn transition_to(&mut self, new_state: ConnectionState) -> Result<()> {
        debug!(
            cid = self.cid,
            current_state = ?self.current_state,
            target_state = ?new_state,
            "Attempting state transition"
        );

        // 在状态转换前进行清理工作（保留原先的实现）
        // Perform cleanup before state transition (keep original implementation)
        if let ConnectionState::ValidatingPath { notifier, .. } = self.current_state.clone() {
            // 如果从ValidatingPath状态转换到其他状态，通知调用者连接被中断
            // If transitioning from ValidatingPath to another state, notify caller of interruption
            if !matches!(new_state, ConnectionState::ValidatingPath { .. }) {
                if let Some(notifier) = notifier {
                    // 根据目标状态决定错误类型
                    // Determine error type based on target state
                    let error = match new_state {
                        ConnectionState::Closing | ConnectionState::Closed => {
                            crate::error::Error::ConnectionClosed
                        }
                        _ => crate::error::Error::PathValidationTimeout,
                    };
                    let _ = notifier.send(Err(error));
                }
            }
        }

        // 使用状态转换执行器执行转换
        // Use state transition executor to execute transition
        match self
            .transition_executor
            .execute_transition(&self.current_state, new_state)
        {
            Ok(resulting_state) => {
                self.current_state = resulting_state;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    fn current_state(&self) -> &ConnectionState {
        &self.current_state
    }

    fn can_send_data(&self) -> bool {
        StateValidator::can_send_data(&self.current_state)
    }

    fn can_receive_data(&self) -> bool {
        StateValidator::can_receive_data(&self.current_state)
    }

    fn should_close(&self) -> bool {
        StateValidator::should_close(&self.current_state)
    }

    fn begin_graceful_shutdown(&mut self) -> Result<()> {
        debug!(
            cid = self.cid,
            current_state = ?self.current_state,
            "Beginning graceful shutdown"
        );

        // 使用状态转换执行器处理优雅关闭
        // Use state transition executor to handle graceful shutdown
        match self
            .transition_executor
            .execute_graceful_shutdown(&self.current_state)
        {
            Ok(resulting_state) => {
                self.current_state = resulting_state;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    fn force_close(&mut self) -> Result<()> {
        debug!(
            cid = self.cid,
            current_state = ?self.current_state,
            "Force closing connection"
        );

        // 触发强制关闭事件
        // Trigger force close event
        self.transition_executor.trigger_force_close_event();

        // 执行转换到Closed状态
        // Execute transition to Closed state
        self.transition_to(ConnectionState::Closed)
    }

    fn start_path_validation(
        &mut self,
        new_addr: SocketAddr,
        challenge_data: u64,
        notifier: oneshot::Sender<Result<()>>,
    ) -> Result<()> {
        debug!(
            cid = self.cid,
            ?new_addr,
            challenge_data,
            "Starting path validation"
        );

        // 验证前置条件
        // Validate preconditions
        if !StateValidator::can_start_path_validation(&self.current_state) {
            return Err(Error::InvalidPacket);
        }

        self.transition_to(ConnectionState::ValidatingPath {
            new_addr,
            challenge_data,
            notifier: Some(notifier),
        })
    }

    fn complete_path_validation(&mut self, success: bool) -> Result<()> {
        debug!(cid = self.cid, success, "Completing path validation");

        // 验证当前是否正在进行路径验证
        // Validate if path validation is currently in progress
        if !StateValidator::is_path_validating(&self.current_state) {
            return Err(Error::InvalidPacket);
        }

        // 触发路径验证完成事件
        // Trigger path validation completed event
        self.transition_executor
            .trigger_path_validation_completed(success);

        // 无论成功还是失败，都转换回Established状态
        // Transition back to Established state regardless of success or failure
        self.transition_to(ConnectionState::Established)
    }

    fn register_event_listener(&mut self, listener: EventListener) {
        self.transition_executor.register_event_listener(listener);
    }

    fn clear_event_listeners(&mut self) {
        self.transition_executor.clear_event_listeners();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::endpoint::lifecycle::transitions::LifecycleEvent;
    use std::net::{Ipv4Addr, SocketAddr};
    use std::sync::{Arc, Mutex};

    fn create_test_config() -> Config {
        Config::default()
    }

    fn create_test_address() -> SocketAddr {
        SocketAddr::new(Ipv4Addr::new(127, 0, 0, 1).into(), 8080)
    }

    #[test]
    fn test_lifecycle_manager_initialization() {
        let config = create_test_config();
        let mut manager = DefaultLifecycleManager::new(ConnectionState::Connecting, config);
        let addr = create_test_address();
        let cid = 12345;

        // 测试初始化
        assert!(manager.initialize(cid, addr).is_ok());
        assert_eq!(manager.connection_id(), cid);
        assert_eq!(manager.remote_addr(), Some(addr));
        assert_eq!(manager.state_name(), "Connecting");
    }

    #[test]
    fn test_state_transitions() {
        let config = create_test_config();
        let mut manager = DefaultLifecycleManager::new(ConnectionState::Connecting, config);
        let addr = create_test_address();

        manager.initialize(12345, addr).unwrap();

        // 测试正常的状态转换
        assert!(manager.transition_to(ConnectionState::Established).is_ok());
        assert_eq!(manager.state_name(), "Established");

        assert!(manager.transition_to(ConnectionState::Closing).is_ok());
        assert_eq!(manager.state_name(), "Closing");

        assert!(manager.transition_to(ConnectionState::Closed).is_ok());
        assert_eq!(manager.state_name(), "Closed");
    }

    #[test]
    fn test_invalid_state_transitions() {
        let config = create_test_config();
        let mut manager = DefaultLifecycleManager::new(ConnectionState::Closed, config);
        let addr = create_test_address();

        manager.initialize(12345, addr).unwrap();

        // 测试无效的状态转换（从Closed到其他状态应该失败，除了到Closed本身）
        assert!(manager.transition_to(ConnectionState::Established).is_err());
        assert!(manager.transition_to(ConnectionState::Connecting).is_err());
    }

    #[test]
    fn test_data_permission_checks() {
        let config = create_test_config();
        let mut manager = DefaultLifecycleManager::new(ConnectionState::Connecting, config);
        let addr = create_test_address();

        manager.initialize(12345, addr).unwrap();

        // 在Connecting状态下不能发送数据
        assert!(!manager.can_send_data());
        assert!(!manager.can_receive_data());

        // 转换到Established状态
        manager.transition_to(ConnectionState::Established).unwrap();
        assert!(manager.can_send_data());
        assert!(manager.can_receive_data());

        // 转换到Closing状态
        manager.transition_to(ConnectionState::Closing).unwrap();
        assert!(!manager.can_send_data());
        assert!(manager.can_receive_data());

        // 转换到Closed状态
        manager.transition_to(ConnectionState::Closed).unwrap();
        assert!(!manager.can_send_data());
        assert!(!manager.can_receive_data());
    }

    #[test]
    fn test_graceful_shutdown() {
        let config = create_test_config();
        let mut manager = DefaultLifecycleManager::new(ConnectionState::Established, config);
        let addr = create_test_address();

        manager.initialize(12345, addr).unwrap();

        // 从Established状态开始优雅关闭
        assert!(manager.begin_graceful_shutdown().is_ok());
        assert_eq!(manager.state_name(), "Closing");
        assert!(manager.is_closing());

        // 从Connecting状态开始优雅关闭应该转换到Closing状态
        let mut manager2 =
            DefaultLifecycleManager::new(ConnectionState::Connecting, create_test_config());
        manager2.initialize(12346, addr).unwrap();
        assert!(manager2.begin_graceful_shutdown().is_ok());
        assert_eq!(manager2.state_name(), "Closing");
        assert!(manager2.is_closing());
    }

    #[test]
    fn test_force_close() {
        let config = create_test_config();
        let mut manager = DefaultLifecycleManager::new(ConnectionState::Established, config);
        let addr = create_test_address();

        manager.initialize(12345, addr).unwrap();

        // 强制关闭应该总是成功的
        assert!(manager.force_close().is_ok());
        assert_eq!(manager.state_name(), "Closed");
        assert!(manager.should_close());
    }

    #[test]
    fn test_activity_checks() {
        let config = create_test_config();
        let mut manager = DefaultLifecycleManager::new(ConnectionState::Connecting, config);
        let addr = create_test_address();

        manager.initialize(12345, addr).unwrap();

        // 测试活跃状态检查
        assert!(manager.is_active());
        assert!(!manager.is_closing());

        manager.transition_to(ConnectionState::Established).unwrap();
        assert!(manager.is_active());
        assert!(!manager.is_closing());

        manager.transition_to(ConnectionState::Closing).unwrap();
        assert!(!manager.is_active());
        assert!(manager.is_closing());

        manager.transition_to(ConnectionState::Closed).unwrap();
        assert!(!manager.is_active());
        assert!(!manager.is_closing());
    }

    #[test]
    fn test_path_validation() {
        let config = create_test_config();
        let mut manager = DefaultLifecycleManager::new(ConnectionState::Established, config);
        let addr = create_test_address();
        let new_addr = SocketAddr::new(Ipv4Addr::new(192, 168, 1, 1).into(), 9090);

        manager.initialize(12345, addr).unwrap();

        // 创建一个dummy的oneshot channel
        let (tx, _rx) = tokio::sync::oneshot::channel();

        // 开始路径验证
        assert!(manager.start_path_validation(new_addr, 12345, tx).is_ok());
        assert_eq!(manager.state_name(), "ValidatingPath");

        // 完成路径验证（成功）
        assert!(manager.complete_path_validation(true).is_ok());
        assert_eq!(manager.state_name(), "Established");
    }

    #[test]
    fn test_event_callbacks() {
        let config = create_test_config();
        let mut manager = DefaultLifecycleManager::new(ConnectionState::Connecting, config);
        let addr = create_test_address();

        // 创建事件收集器
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();

        // 注册事件监听器
        manager.register_event_listener(Box::new(move |event| {
            events_clone.lock().unwrap().push(event.clone());
        }));

        // 初始化应该触发事件
        manager.initialize(12345, addr).unwrap();

        // 状态转换应该触发事件
        manager.transition_to(ConnectionState::Established).unwrap();
        manager.transition_to(ConnectionState::Closing).unwrap();

        // 强制关闭应该触发事件
        manager.force_close().unwrap();

        // 检查收集到的事件
        let captured_events = events.lock().unwrap();
        assert!(!captured_events.is_empty());

        // 应该包含初始化事件
        assert!(
            captured_events
                .iter()
                .any(|e| matches!(e, LifecycleEvent::ConnectionInitialized { .. }))
        );

        // 应该包含状态转换事件
        assert!(
            captured_events
                .iter()
                .any(|e| matches!(e, LifecycleEvent::StateTransition { .. }))
        );

        // 应该包含强制关闭事件
        assert!(
            captured_events
                .iter()
                .any(|e| matches!(e, LifecycleEvent::ForceClose))
        );

        // 应该包含连接关闭事件
        assert!(
            captured_events
                .iter()
                .any(|e| matches!(e, LifecycleEvent::ConnectionClosed))
        );
    }

    #[test]
    fn test_clear_event_listeners() {
        let config = create_test_config();
        let mut manager = DefaultLifecycleManager::new(ConnectionState::Connecting, config);

        // 注册监听器
        manager.register_event_listener(Box::new(|_| {}));

        // 清除监听器
        manager.clear_event_listeners();

        // 验证清除成功（通过触发事件，检查没有panic或其他问题）
        manager.trigger_timeout();
    }
}
