//! 连接标识管理模块 - 管理连接的身份信息
//! Connection Identity Management Module - Manages connection identity information
//!
//! 该模块封装了连接的标识信息，包括连接ID、远程地址等。
//! 通过将这些字段组合到一个结构体中，我们可以更好地管理连接的身份状态。
//!
//! This module encapsulates connection identity information, including connection IDs
//! and remote addresses. By grouping these fields into a single struct, we can better
//! manage the connection's identity state.

use std::net::SocketAddr;

/// 连接标识信息
/// Connection identity information
#[derive(Debug, Clone)]
pub struct ConnectionIdentity {
    /// 本地连接ID
    /// Local connection ID
    pub(crate) local_cid: u32,

    /// 对端连接ID
    /// Peer connection ID  
    pub(crate) peer_cid: u32,

    /// 远程地址
    /// Remote address
    pub(crate) remote_addr: SocketAddr,
}

impl ConnectionIdentity {
    /// 创建新的连接标识
    /// Create new connection identity
    pub fn new(local_cid: u32, peer_cid: u32, remote_addr: SocketAddr) -> Self {
        Self {
            local_cid,
            peer_cid,
            remote_addr,
        }
    }

    /// 获取本地连接ID
    /// Get local connection ID
    pub fn local_cid(&self) -> u32 {
        self.local_cid
    }

    /// 获取对端连接ID  
    /// Get peer connection ID
    pub fn peer_cid(&self) -> u32 {
        self.peer_cid
    }

    /// 设置对端连接ID
    /// Set peer connection ID
    pub fn set_peer_cid(&mut self, peer_cid: u32) {
        self.peer_cid = peer_cid;
    }

    /// 获取远程地址
    /// Get remote address
    pub fn remote_addr(&self) -> SocketAddr {
        self.remote_addr
    }

    /// 设置远程地址
    /// Set remote address  
    pub fn set_remote_addr(&mut self, addr: SocketAddr) {
        self.remote_addr = addr;
    }

    /// 检查是否与指定的连接ID匹配
    /// Check if matches the specified connection ID
    pub fn matches_local_cid(&self, cid: u32) -> bool {
        self.local_cid == cid
    }

    /// 检查是否与指定的对端连接ID匹配
    /// Check if matches the specified peer connection ID
    pub fn matches_peer_cid(&self, cid: u32) -> bool {
        self.peer_cid == cid
    }

    /// 检查是否与指定的远程地址匹配
    /// Check if matches the specified remote address
    pub fn matches_remote_addr(&self, addr: &SocketAddr) -> bool {
        &self.remote_addr == addr
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, SocketAddr};

    fn create_test_addr() -> SocketAddr {
        SocketAddr::new(Ipv4Addr::new(127, 0, 0, 1).into(), 8080)
    }

    #[test]
    fn test_connection_identity_creation() {
        let addr = create_test_addr();
        let identity = ConnectionIdentity::new(123, 456, addr);

        assert_eq!(identity.local_cid(), 123);
        assert_eq!(identity.peer_cid(), 456);
        assert_eq!(identity.remote_addr(), addr);
    }

    #[test]
    fn test_connection_identity_setters() {
        let addr = create_test_addr();
        let mut identity = ConnectionIdentity::new(123, 0, addr);

        identity.set_peer_cid(789);
        assert_eq!(identity.peer_cid(), 789);

        let new_addr = SocketAddr::new(Ipv4Addr::new(192, 168, 1, 1).into(), 9090);
        identity.set_remote_addr(new_addr);
        assert_eq!(identity.remote_addr(), new_addr);
    }

    #[test]
    fn test_connection_identity_matching() {
        let addr = create_test_addr();
        let identity = ConnectionIdentity::new(123, 456, addr);

        assert!(identity.matches_local_cid(123));
        assert!(!identity.matches_local_cid(124));

        assert!(identity.matches_peer_cid(456));
        assert!(!identity.matches_peer_cid(457));

        assert!(identity.matches_remote_addr(&addr));
        let other_addr = SocketAddr::new(Ipv4Addr::new(192, 168, 1, 1).into(), 9090);
        assert!(!identity.matches_remote_addr(&other_addr));
    }
}
