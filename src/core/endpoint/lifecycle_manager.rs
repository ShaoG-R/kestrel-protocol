//! 连接生命周期管理器 - 统一管理连接的完整生命周期
//! Connection Lifecycle Manager - Unified management of connection lifecycle
//!
//! 该模块提供了统一的连接生命周期管理接口，协调状态转换、
//! 事件处理、清理逻辑等各个方面。
//!
//! This module provides a unified connection lifecycle management interface,
//! coordinating state transitions, event handling, cleanup logic, and other aspects.

use super::state::{ConnectionState};
use crate::{
    error::{Error, Result},
    config::Config,
};
use std::net::SocketAddr;
use tokio::{sync::oneshot, time::Instant};
use tracing::{debug, info, trace, warn};

/// 生命周期事件类型
/// Lifecycle event types
#[derive(Debug, Clone, PartialEq)]
pub enum LifecycleEvent {
    /// 连接初始化
    /// Connection initialization
    ConnectionInitialized { local_cid: u32, remote_addr: SocketAddr },
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
    PathValidationStarted { new_addr: SocketAddr },
    /// 路径验证完成
    /// Path validation completed
    PathValidationCompleted { success: bool },
}

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
}

/// 默认的生命周期管理器实现
/// Default lifecycle manager implementation
#[derive(Debug)]
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
        }
    }

    /// 触发生命周期事件
    /// Trigger lifecycle event
    fn trigger_event(&self, event: LifecycleEvent) {
        debug!(
            cid = self.cid,
            ?event,
            "Lifecycle event triggered"
        );
        // 这里可以添加事件回调机制
        // Event callback mechanism can be added here
    }

    /// 验证状态转换是否合法
    /// Validate if state transition is legal
    fn is_valid_transition(&self, new_state: &ConnectionState) -> bool {
        use ConnectionState::*;

        match (&self.current_state, new_state) {
            // 从任何状态都可以转换到Closed（中止连接）
            // Can transition to Closed from any state (abort connection)
            (_, Closed) => true,
            
            // Connecting状态的转换
            // Transitions from Connecting state
            (Connecting, Established) => true,
            (Connecting, Closing) => true,
            
            // SynReceived状态的转换
            // Transitions from SynReceived state
            (SynReceived, Established) => true,
            (SynReceived, FinWait) => true,
            (SynReceived, Closing) => true,
            
            // Established状态的转换
            // Transitions from Established state
            (Established, FinWait) => true,
            (Established, Closing) => true,
            (Established, ValidatingPath { .. }) => true,
            
            // ValidatingPath状态的转换
            // Transitions from ValidatingPath state
            (ValidatingPath { .. }, Established) => true,
            (ValidatingPath { .. }, FinWait) => true,
            (ValidatingPath { .. }, Closing) => true,
            
            // FinWait状态的转换
            // Transitions from FinWait state
            (FinWait, Closing) => true,
            
            // Closing状态的转换
            // Transitions from Closing state
            (Closing, ClosingWait) => true,
            
            // 同状态转换（幂等）
            // Same state transition (idempotent)
            (state1, state2) if std::mem::discriminant(state1) == std::mem::discriminant(state2) => true,
            
            // 其他转换都是无效的
            // All other transitions are invalid
            _ => false,
        }
    }
}

impl ConnectionLifecycleManager for DefaultLifecycleManager {
    fn initialize(&mut self, local_cid: u32, remote_addr: SocketAddr) -> Result<()> {
        self.cid = local_cid;
        self.remote_addr = Some(remote_addr);
        self.start_time = Instant::now();
        
        self.trigger_event(LifecycleEvent::ConnectionInitialized {
            local_cid,
            remote_addr,
        });
        
        info!(
            cid = local_cid,
            ?remote_addr,
            "Connection lifecycle initialized"
        );
        
        Ok(())
    }

    fn transition_to(&mut self, new_state: ConnectionState) -> Result<()> {
        if !self.is_valid_transition(&new_state) {
            warn!(
                cid = self.cid,
                current_state = ?self.current_state,
                attempted_state = ?new_state,
                "Invalid state transition attempted"
            );
            return Err(Error::InvalidPacket);
        }

        let old_state = std::mem::discriminant(&self.current_state);
        let new_state_discriminant = std::mem::discriminant(&new_state);
        
        // 在状态转换前进行清理工作
        // Perform cleanup before state transition
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
        
        self.current_state = new_state;
        
        // 只有在状态实际发生变化时才记录和触发事件
        // Only log and trigger events when state actually changes
        if old_state != new_state_discriminant {
            trace!(
                cid = self.cid,
                new_state = ?self.current_state,
                "State transition completed"
            );
            
            // 根据新状态触发相应的生命周期事件
            // Trigger corresponding lifecycle events based on new state
            match &self.current_state {
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
        
        Ok(())
    }

    fn current_state(&self) -> &ConnectionState {
        &self.current_state
    }

    fn can_send_data(&self) -> bool {
        matches!(
            self.current_state,
            ConnectionState::Established | ConnectionState::FinWait
        )
    }

    fn can_receive_data(&self) -> bool {
        matches!(
            self.current_state,
            ConnectionState::Established | ConnectionState::Closing
        )
    }

    fn should_close(&self) -> bool {
        matches!(self.current_state, ConnectionState::Closed)
    }

    fn begin_graceful_shutdown(&mut self) -> Result<()> {
        match &self.current_state {
            ConnectionState::Connecting => {
                // 在连接建立前关闭，转换到Closing状态以正确发送FIN
                // Close before connection establishment, transition to Closing to properly send FIN
                self.transition_to(ConnectionState::Closing)
            }
            ConnectionState::SynReceived => {
                // 在服务端等待状态关闭，转换到Closing状态以正确发送FIN
                // Close in server waiting state, transition to Closing to properly send FIN  
                self.transition_to(ConnectionState::Closing)
            }
            ConnectionState::Established => {
                self.transition_to(ConnectionState::Closing)
            }
            ConnectionState::FinWait => {
                // 如果已经在等待对方的FIN，则可以直接转换到Closing
                // If already waiting for peer's FIN, can directly transition to Closing
                self.transition_to(ConnectionState::Closing)
            }
            ConnectionState::ValidatingPath { .. } => {
                // 路径验证期间关闭，回到Closing状态
                // Close during path validation, go to Closing state
                self.transition_to(ConnectionState::Closing)
            }
            ConnectionState::Closing | ConnectionState::ClosingWait | ConnectionState::Closed => {
                // 已经在关闭过程中或已关闭
                // Already in closing process or closed
                Ok(())
            }
        }
    }

    fn force_close(&mut self) -> Result<()> {
        self.transition_to(ConnectionState::Closed)
    }

    fn start_path_validation(
        &mut self,
        new_addr: SocketAddr,
        challenge_data: u64,
        notifier: oneshot::Sender<Result<()>>,
    ) -> Result<()> {
        if !matches!(self.current_state, ConnectionState::Established) {
            return Err(Error::InvalidPacket);
        }

        self.transition_to(ConnectionState::ValidatingPath {
            new_addr,
            challenge_data,
            notifier: Some(notifier),
        })
    }

    fn complete_path_validation(&mut self, success: bool) -> Result<()> {
        if let ConnectionState::ValidatingPath { .. } = &self.current_state {
            self.trigger_event(LifecycleEvent::PathValidationCompleted { success });
            
            if success {
                self.transition_to(ConnectionState::Established)
            } else {
                self.transition_to(ConnectionState::Established)
            }
        } else {
            Err(Error::InvalidPacket)
        }
    }
}

impl DefaultLifecycleManager {
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
        matches!(
            self.current_state,
            ConnectionState::Established | ConnectionState::SynReceived | ConnectionState::Connecting
        )
    }

    /// 检查连接是否正在关闭
    /// Check if connection is closing
    pub fn is_closing(&self) -> bool {
        matches!(
            self.current_state,
            ConnectionState::Closing | ConnectionState::ClosingWait | ConnectionState::FinWait
        )
    }

    /// 获取当前状态的字符串表示（用于日志）
    /// Gets string representation of current state (for logging)
    pub fn state_name(&self) -> &'static str {
        match &self.current_state {
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
} 

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, SocketAddr};

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
        let mut manager2 = DefaultLifecycleManager::new(ConnectionState::Connecting, create_test_config());
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
} 