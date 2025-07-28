//! 连接状态验证逻辑模块
//! Connection State Validation Logic Module
//!
//! 该模块负责连接状态的验证、权限检查和活跃状态判断等功能。
//! 为生命周期管理器提供一致且可靠的状态验证服务。
//!
//! This module handles connection state validation, permission checks,
//! and activity state determination. It provides consistent and reliable
//! state validation services for the lifecycle manager.

use crate::core::endpoint::types::state::ConnectionState;

/// 状态验证器，负责所有状态相关的验证和检查逻辑
/// State validator responsible for all state-related validation and check logic
pub struct StateValidator;

impl StateValidator {
    /// 验证状态转换是否合法
    /// Validate if state transition is legal
    pub fn is_valid_transition(current_state: &ConnectionState, new_state: &ConnectionState) -> bool {
        use ConnectionState::*;

        match (current_state, new_state) {
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

    /// 检查是否可以发送数据
    /// Check if data can be sent
    pub fn can_send_data(state: &ConnectionState) -> bool {
        matches!(
            state,
            ConnectionState::Established | ConnectionState::FinWait
        )
    }

    /// 检查是否可以接收数据
    /// Check if data can be received
    pub fn can_receive_data(state: &ConnectionState) -> bool {
        matches!(
            state,
            ConnectionState::Established | ConnectionState::Closing
        )
    }

    /// 检查连接是否应该关闭
    /// Check if connection should be closed
    pub fn should_close(state: &ConnectionState) -> bool {
        matches!(state, ConnectionState::Closed)
    }

    /// 检查连接是否处于活跃状态
    /// Check if connection is in active state
    pub fn is_active(state: &ConnectionState) -> bool {
        matches!(
            state,
            ConnectionState::Established | ConnectionState::SynReceived | ConnectionState::Connecting
        )
    }

    /// 检查连接是否正在关闭
    /// Check if connection is closing
    pub fn is_closing(state: &ConnectionState) -> bool {
        matches!(
            state,
            ConnectionState::Closing | ConnectionState::ClosingWait | ConnectionState::FinWait
        )
    }

    /// 验证路径验证的前置条件
    /// Validate preconditions for path validation
    pub fn can_start_path_validation(state: &ConnectionState) -> bool {
        matches!(state, ConnectionState::Established)
    }

    /// 验证优雅关闭的前置条件
    /// Validate preconditions for graceful shutdown
    pub fn can_begin_graceful_shutdown(state: &ConnectionState) -> bool {
        !matches!(state, ConnectionState::Closed | ConnectionState::ClosingWait)
    }

    /// 验证是否正在进行路径验证
    /// Validate if path validation is in progress
    pub fn is_path_validating(state: &ConnectionState) -> bool {
        matches!(state, ConnectionState::ValidatingPath { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, SocketAddr};

    fn create_test_address() -> SocketAddr {
        SocketAddr::new(Ipv4Addr::new(127, 0, 0, 1).into(), 8080)
    }

    #[test]
    fn test_valid_transitions() {
        // 正常的连接建立流程
        assert!(StateValidator::is_valid_transition(
            &ConnectionState::Connecting,
            &ConnectionState::Established
        ));
        
        assert!(StateValidator::is_valid_transition(
            &ConnectionState::Established,
            &ConnectionState::Closing
        ));
        
        assert!(StateValidator::is_valid_transition(
            &ConnectionState::Closing,
            &ConnectionState::ClosingWait
        ));
        
        // 从任何状态都可以转到Closed（强制关闭）
        assert!(StateValidator::is_valid_transition(
            &ConnectionState::Established,
            &ConnectionState::Closed
        ));
        
        assert!(StateValidator::is_valid_transition(
            &ConnectionState::Connecting,
            &ConnectionState::Closed
        ));
    }

    #[test]
    fn test_invalid_transitions() {
        // 从Closed不能转到其他状态（除了Closed本身）
        assert!(!StateValidator::is_valid_transition(
            &ConnectionState::Closed,
            &ConnectionState::Established
        ));
        
        // 不能从Connecting直接到FinWait
        assert!(!StateValidator::is_valid_transition(
            &ConnectionState::Connecting,
            &ConnectionState::FinWait
        ));
        
        // 不能从ClosingWait到Established
        assert!(!StateValidator::is_valid_transition(
            &ConnectionState::ClosingWait,
            &ConnectionState::Established
        ));
    }

    #[test]
    fn test_data_permissions() {
        // Established状态可以发送和接收
        assert!(StateValidator::can_send_data(&ConnectionState::Established));
        assert!(StateValidator::can_receive_data(&ConnectionState::Established));
        
        // FinWait状态可以发送但不能接收
        assert!(StateValidator::can_send_data(&ConnectionState::FinWait));
        assert!(!StateValidator::can_receive_data(&ConnectionState::FinWait));
        
        // Closing状态不能发送但可以接收
        assert!(!StateValidator::can_send_data(&ConnectionState::Closing));
        assert!(StateValidator::can_receive_data(&ConnectionState::Closing));
        
        // Closed状态既不能发送也不能接收
        assert!(!StateValidator::can_send_data(&ConnectionState::Closed));
        assert!(!StateValidator::can_receive_data(&ConnectionState::Closed));
    }

    #[test]
    fn test_activity_checks() {
        // 活跃状态
        assert!(StateValidator::is_active(&ConnectionState::Connecting));
        assert!(StateValidator::is_active(&ConnectionState::SynReceived));
        assert!(StateValidator::is_active(&ConnectionState::Established));
        
        // 非活跃状态
        assert!(!StateValidator::is_active(&ConnectionState::Closing));
        assert!(!StateValidator::is_active(&ConnectionState::Closed));
        
        // 关闭状态
        assert!(StateValidator::is_closing(&ConnectionState::Closing));
        assert!(StateValidator::is_closing(&ConnectionState::FinWait));
        assert!(StateValidator::is_closing(&ConnectionState::ClosingWait));
        
        // 非关闭状态
        assert!(!StateValidator::is_closing(&ConnectionState::Established));
        assert!(!StateValidator::is_closing(&ConnectionState::Connecting));
    }

    #[test]
    fn test_path_validation_checks() {
        // 只有Established状态可以开始路径验证
        assert!(StateValidator::can_start_path_validation(&ConnectionState::Established));
        assert!(!StateValidator::can_start_path_validation(&ConnectionState::Connecting));
        assert!(!StateValidator::can_start_path_validation(&ConnectionState::Closing));
        
        // 检查是否正在进行路径验证
        let validating_state = ConnectionState::ValidatingPath {
            new_addr: create_test_address(),
            challenge_data: 12345,
            notifier: None,
        };
        assert!(StateValidator::is_path_validating(&validating_state));
        assert!(!StateValidator::is_path_validating(&ConnectionState::Established));
    }

    #[test]
    fn test_graceful_shutdown_checks() {
        // 大部分状态都可以开始优雅关闭
        assert!(StateValidator::can_begin_graceful_shutdown(&ConnectionState::Connecting));
        assert!(StateValidator::can_begin_graceful_shutdown(&ConnectionState::Established));
        assert!(StateValidator::can_begin_graceful_shutdown(&ConnectionState::FinWait));
        
        // 已经关闭或正在等待关闭的状态不能再次开始关闭
        assert!(!StateValidator::can_begin_graceful_shutdown(&ConnectionState::Closed));
        assert!(!StateValidator::can_begin_graceful_shutdown(&ConnectionState::ClosingWait));
    }

    #[test]
    fn test_should_close() {
        // 只有Closed状态应该关闭
        assert!(StateValidator::should_close(&ConnectionState::Closed));
        assert!(!StateValidator::should_close(&ConnectionState::Established));
        assert!(!StateValidator::should_close(&ConnectionState::Closing));
    }
} 