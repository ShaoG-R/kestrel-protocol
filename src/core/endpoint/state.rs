//! Defines the connection state machine for an endpoint.
//!
//! 定义端点的连接状态机。

use crate::error::Result;
use std::net::SocketAddr;
use tokio::sync::oneshot;

/// The state of a connection.
/// 连接的状态。
#[derive(Debug)]
pub enum ConnectionState {
    /// The client is sending the initial SYN.
    /// 客户端正在发送初始SYN。
    Connecting,
    
    /// The server has received the SYN and is waiting for the application to accept.
    /// 服务器已收到SYN，正在等待应用程序接受。
    SynReceived,
    
    /// The connection is fully established and can send/receive data.
    /// 连接已完全建立，可以发送/接收数据。
    Established,
    /// The connection is validating a new path.
    ValidatingPath {
        new_addr: SocketAddr,
        challenge_data: u64,
        notifier: Option<oneshot::Sender<Result<()>>>,
    },
    /// The local side has initiated a close. It is waiting for all in-flight
    /// data to be acknowledged before sending a FIN.
    /// 端点已启动关闭过程并发送了FIN。
    Closing,
    
    /// The endpoint has received a FIN from the peer.
    /// It can no longer receive data, but can still send.
    /// 端点已从对等方收到FIN。它不能再接收数据，但仍然可以发送。
    FinWait,
    
    /// The connection is fully closed and the endpoint should terminate.
    /// 连接已完全关闭。
    Closed,
}

impl Clone for ConnectionState {
    fn clone(&self) -> Self {
        match self {
            Self::Connecting => Self::Connecting,
            Self::SynReceived => Self::SynReceived,
            Self::Established => Self::Established,
            Self::ValidatingPath {
                new_addr,
                challenge_data,
                ..
            } => Self::ValidatingPath {
                new_addr: *new_addr,
                challenge_data: *challenge_data,
                notifier: None, // Notifier cannot be cloned
            },
            Self::Closing => Self::Closing,
            Self::FinWait => Self::FinWait,
            Self::Closed => Self::Closed,
        }
    }
}

impl PartialEq for ConnectionState {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Connecting, Self::Connecting) => true,
            (Self::SynReceived, Self::SynReceived) => true,
            (Self::Established, Self::Established) => true,
            (
                Self::ValidatingPath {
                    new_addr: l_addr,
                    challenge_data: l_data,
                    ..
                },
                Self::ValidatingPath {
                    new_addr: r_addr,
                    challenge_data: r_data,
                    ..
                },
            ) => l_addr == r_addr && l_data == r_data,
            (Self::Closing, Self::Closing) => true,
            (Self::FinWait, Self::FinWait) => true,
            (Self::Closed, Self::Closed) => true,
            _ => false,
        }
    }
}

impl Eq for ConnectionState {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_state_traits() {
        // Test Clone and PartialEq for non-ValidatingPath states
        let state1 = ConnectionState::Established;
        let state2 = state1.clone();
        assert_eq!(state1, state2);

        // Test Clone and PartialEq for ValidatingPath state
        let addr: SocketAddr = "127.0.0.1:1234".parse().unwrap();
        let (tx, _rx) = oneshot::channel();
        let mut state_with_notifier = ConnectionState::ValidatingPath {
            new_addr: addr,
            challenge_data: 12345,
            notifier: Some(tx),
        };

        let state_cloned = state_with_notifier.clone();

        // The cloned state should not have the notifier
        assert!(matches!(
            state_cloned,
            ConnectionState::ValidatingPath { notifier: None, .. }
        ));

        // For PartialEq, we ignore the notifier.
        // Let's create another state that is identical except for the notifier.
        let (tx2, _rx2) = oneshot::channel();
        let state_with_other_notifier = ConnectionState::ValidatingPath {
            new_addr: addr,
            challenge_data: 12345,
            notifier: Some(tx2),
        };
        assert_eq!(state_with_notifier, state_with_other_notifier);

        // Make the original state comparable with the cloned one.
        if let ConnectionState::ValidatingPath { notifier, .. } = &mut state_with_notifier {
            *notifier = None;
        }
        assert_eq!(state_with_notifier, state_cloned);

        // Test Debug format
        assert_eq!(format!("{:?}", ConnectionState::Established), "Established");
    }
} 