//! Defines the connection state machine for an endpoint.
//!
//! 定义端点的连接状态机。

use std::net::SocketAddr;

/// The state of a connection.
/// 连接的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    },
    /// The endpoint has initiated closing and sent a FIN.
    /// 端点已启动关闭过程并发送了FIN。
    Closing,
    
    /// The endpoint has received a FIN from the peer.
    /// It can no longer receive data, but can still send.
    /// 端点已从对等方收到FIN。它不能再接收数据，但仍然可以发送。
    FinWait,
    
    /// The connection is fully closed.
    /// 连接已完全关闭。
    Closed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_state_traits() {
        // Test Clone and Copy
        let state1 = ConnectionState::Established;
        let state2 = state1; // Copy
        let state3 = state1.clone(); // Clone

        // Test PartialEq and Eq
        assert_eq!(state1, state2);
        assert_eq!(state1, state3);
        assert_ne!(state1, ConnectionState::Connecting);

        // Test Debug format
        assert_eq!(format!("{:?}", state1), "Established");
    }
} 