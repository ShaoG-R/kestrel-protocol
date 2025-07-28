//! 状态管理器 - 负责管理连接的状态转换
//! State Manager - Manages connection state transitions

use super::state::ConnectionState;
use crate::error::{Error, Result};
use std::net::SocketAddr;
use tokio::sync::oneshot;
use tracing::{info, trace, warn};

/// 状态管理器，负责管理连接状态的转换和验证
/// State manager responsible for managing connection state transitions and validation
pub struct StateManager {
    /// 当前连接状态
    /// Current connection state
    current_state: ConnectionState,
    /// 连接ID，用于日志记录
    /// Connection ID for logging
    cid: u32,
}

impl StateManager {
    /// 创建新的状态管理器
    /// Creates a new state manager
    pub fn new(initial_state: ConnectionState, cid: u32) -> Self {
        Self {
            current_state: initial_state,
            cid,
        }
    }

    /// 获取当前状态
    /// Gets the current state
    pub fn current_state(&self) -> &ConnectionState {
        &self.current_state
    }

    /// 尝试转换到新状态
    /// Attempts to transition to a new state
    pub fn transition_to(&mut self, new_state: ConnectionState) -> Result<()> {
        if self.is_valid_transition(&new_state) {
            let old_state = self.current_state.clone();
            self.current_state = new_state;
            trace!(
                cid = self.cid,
                ?old_state,
                new_state = ?self.current_state,
                "State transition successful"
            );
            Ok(())
        } else {
            warn!(
                cid = self.cid,
                current_state = ?self.current_state,
                attempted_state = ?new_state,
                "Invalid state transition attempted"
            );
            Err(Error::InvalidPacket) // 使用现有的错误类型
        }
    }

    /// 处理连接建立（客户端侧）
    /// Handles connection establishment (client side)
    pub fn handle_connection_established(&mut self, peer_cid: u32) -> Result<()> {
        if self.current_state == ConnectionState::Connecting {
            self.transition_to(ConnectionState::Established)?;
            info!(cid = self.cid, peer_cid, "Connection established (client-side)");
            Ok(())
        } else {
            Err(Error::InvalidPacket)
        }
    }

    /// 处理服务端连接接受
    /// Handles server-side connection acceptance
    pub fn handle_connection_accepted(&mut self) -> Result<()> {
        if self.current_state == ConnectionState::SynReceived {
            self.transition_to(ConnectionState::Established)?;
            info!(cid = self.cid, "Connection accepted by user, preparing 0-RTT SYN-ACK with data.");
            Ok(())
        } else {
            Err(Error::InvalidPacket)
        }
    }

    /// 处理FIN接收后的状态转换
    /// Handles state transition after FIN reception
    pub fn handle_fin_received(&mut self) -> Result<()> {
        match self.current_state {
            ConnectionState::Established => {
                self.transition_to(ConnectionState::FinWait)?;
                trace!(cid = self.cid, "FIN processed by reassemble, moving to FinWait state.");
                Ok(())
            }
            ConnectionState::Closing => {
                self.transition_to(ConnectionState::ClosingWait)?;
                Ok(())
            }
            ConnectionState::SynReceived => {
                // 在0-RTT场景中，客户端可能在SynReceived状态就发送FIN
                self.transition_to(ConnectionState::FinWait)?;
                Ok(())
            }
            _ => Ok(()), // 其他状态下忽略FIN
        }
    }

    /// 处理用户主动关闭
    /// Handles user-initiated close
    pub fn handle_user_close(&mut self, has_pending_data: bool) -> Result<()> {
        match self.current_state {
            ConnectionState::Established | ConnectionState::FinWait => {
                self.transition_to(ConnectionState::Closing)?;
                Ok(())
            }
            ConnectionState::Connecting if has_pending_data => {
                info!(cid = self.cid, "Close requested on connecting stream with pending data; proceeding to graceful shutdown.");
                self.transition_to(ConnectionState::Closing)?;
                Ok(())
            }
            ConnectionState::Closing | ConnectionState::ClosingWait | ConnectionState::Closed => {
                // 已经在关闭状态，无需操作
                Ok(())
            }
            _ => {
                // 其他非建立状态，直接中止
                info!(
                    cid = self.cid,
                    state = ?self.current_state,
                    "Connection closed by user during non-established state, aborting."
                );
                self.transition_to(ConnectionState::Closed)?;
                Ok(())
            }
        }
    }

    /// 处理路径验证开始
    /// Handles path validation initiation
    pub fn handle_path_validation_start(
        &mut self,
        new_addr: SocketAddr,
        challenge_data: u64,
        notifier: Option<oneshot::Sender<Result<()>>>,
    ) -> Result<()> {
        if self.current_state == ConnectionState::Established {
            self.transition_to(ConnectionState::ValidatingPath {
                new_addr,
                challenge_data,
                notifier,
            })?;
            Ok(())
        } else {
            Err(Error::NotConnected)
        }
    }

    /// 处理路径验证成功
    /// Handles successful path validation
    pub fn handle_path_validation_success(&mut self, new_addr: SocketAddr) -> Result<SocketAddr> {
        if let ConnectionState::ValidatingPath { new_addr: expected_addr, .. } = &self.current_state {
            if *expected_addr == new_addr {
                let old_addr = *expected_addr; // 保存旧地址用于返回
                self.transition_to(ConnectionState::Established)?;
                info!(cid = self.cid, old_addr = %old_addr, new_addr = %new_addr, "Path validation successful, migrating connection.");
                Ok(old_addr)
            } else {
                Err(Error::InvalidPacket)
            }
        } else {
            Err(Error::InvalidPacket)
        }
    }

    /// 处理路径验证超时
    /// Handles path validation timeout
    pub fn handle_path_validation_timeout(&mut self) -> Result<()> {
        if matches!(self.current_state, ConnectionState::ValidatingPath { .. }) {
            info!(cid = self.cid, "Path validation timed out.");
            self.transition_to(ConnectionState::Established)?; // 恢复到建立状态
            Ok(())
        } else {
            Ok(())
        }
    }

    /// 处理连接超时
    /// Handles connection timeout
    pub fn handle_connection_timeout(&mut self) -> Result<()> {
        info!(cid = self.cid, "Connection timed out");
        self.transition_to(ConnectionState::Closed)?;
        Ok(())
    }

    /// 处理所有数据确认完成
    /// Handles all data acknowledged
    pub fn handle_all_data_acked(&mut self) -> Result<bool> {
        match self.current_state {
            ConnectionState::Closing | ConnectionState::ClosingWait => {
                info!(cid = self.cid, "All data ACKed, entering Closed state.");
                self.transition_to(ConnectionState::Closed)?;
                Ok(true) // 返回true表示应该关闭连接
            }
            _ => Ok(false),
        }
    }

    /// 检查是否应该关闭连接
    /// Checks if the connection should be closed
    pub fn should_close(&self) -> bool {
        self.current_state == ConnectionState::Closed
    }

    /// 检查状态转换是否有效
    /// Checks if a state transition is valid
    fn is_valid_transition(&self, new_state: &ConnectionState) -> bool {
        use ConnectionState::*;

        match (&self.current_state, new_state) {
            // 从任何状态都可以转换到Closed（中止连接）
            (_, Closed) => true,
            
            // Connecting状态的转换
            (Connecting, Established) => true,
            (Connecting, Closing) => true, // 有待发送数据时的优雅关闭
            
            // SynReceived状态的转换
            (SynReceived, Established) => true,
            (SynReceived, FinWait) => true, // 0-RTT场景
            
            // Established状态的转换
            (Established, FinWait) => true,
            (Established, Closing) => true,
            (Established, ValidatingPath { .. }) => true,
            
            // ValidatingPath状态的转换
            (ValidatingPath { .. }, Established) => true,
            (ValidatingPath { .. }, FinWait) => true, // 路径验证期间收到FIN
            
            // FinWait状态的转换
            (FinWait, Closing) => true,
            
            // Closing状态的转换
            (Closing, ClosingWait) => true,
            
            // ClosingWait状态的转换已被上面的通用规则覆盖
            
            // 同状态转换（幂等）
            (state1, state2) if std::mem::discriminant(state1) == std::mem::discriminant(state2) => true,
            
            // 其他转换都是无效的
            _ => false,
        }
    }
}