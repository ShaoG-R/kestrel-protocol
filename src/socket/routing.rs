//! 帧路由分发层 - 负责帧的智能路由和连接映射管理
//! Frame Routing Layer - Handles intelligent frame routing and connection mapping

use super::{actor::ConnectionMeta, draining::DrainingPool};
use crate::packet::frame::Frame;
use std::{collections::HashMap, net::SocketAddr};
use tracing::debug;

/// 帧路由管理器 - 负责帧的智能路由和连接映射管理
/// Frame Router Manager - Handles intelligent frame routing and connection mapping
///
/// 该组件管理所有连接的路由状态，包括活跃连接映射、地址路由表和排水连接池。
/// 它提供统一的帧分发接口，支持连接迁移和智能路由决策。
///
/// This component manages routing state for all connections, including active connection
/// mappings, address routing tables, and draining connection pools. It provides a unified
/// frame dispatch interface with support for connection migration and intelligent routing decisions.
#[derive(Debug)]
pub(crate) struct FrameRouter {
    /// 活跃连接映射：本地CID -> 连接元数据
    /// Active connections: Local CID -> Connection metadata
    active_connections: HashMap<u32, ConnectionMeta>,
    /// 地址路由表：远程地址 -> 本地CID (握手期间使用)
    /// Address routing table: Remote address -> Local CID (used during handshake)
    address_routing: HashMap<SocketAddr, u32>,
    /// 正在排水的连接ID池
    /// Pool of connection IDs being drained
    draining_pool: DrainingPool,
}

/// 路由操作结果
/// Routing operation result
#[derive(Debug)]
pub(crate) enum RoutingResult {
    /// 成功分发到现有连接
    /// Successfully dispatched to existing connection
    Dispatched,
    /// 连接未找到，需要上层处理
    /// Connection not found, needs upper layer handling
    ConnectionNotFound,
    /// 连接已在排水状态，已忽略
    /// Connection is in draining state, ignored
    ConnectionDraining,
}

impl FrameRouter {
    /// 创建新的帧路由管理器
    /// Creates a new frame router manager
    ///
    /// # Arguments
    /// * `draining_pool` - 排水连接池实例
    ///
    /// # Arguments  
    /// * `draining_pool` - Draining connection pool instance
    pub(crate) fn new(draining_pool: DrainingPool) -> Self {
        debug!("创建帧路由管理器 | Creating frame router manager");
        Self {
            active_connections: HashMap::new(),
            address_routing: HashMap::new(),
            draining_pool,
        }
    }

    /// 分发帧到对应的连接
    /// Dispatch frame to the corresponding connection
    ///
    /// 该方法实现智能路由逻辑：
    /// 1. 优先通过目标CID路由到已建立的连接（支持连接迁移）
    /// 2. 通过远程地址路由到握手期间的连接  
    /// 3. 对于无法路由的帧，返回相应的错误状态
    ///
    /// This method implements intelligent routing logic:
    /// 1. Route to established connections via destination CID first (supports connection migration)
    /// 2. Route to connections during handshake via remote address
    /// 3. Return appropriate error status for unroutable frames
    pub(crate) async fn dispatch_frame(
        &mut self,
        frame: Frame,
        remote_addr: SocketAddr,
    ) -> RoutingResult {
        let cid = frame.destination_cid();

        // 1. 尝试通过目标CID路由到已建立的连接
        //    这是主要的路由机制，并支持连接迁移
        //    已建立连接的CID不为零
        // 1. Try to route to an established connection via its destination CID.
        //    This is the primary routing mechanism and supports connection migration.
        //    CIDs are non-zero for established connections.
        if cid != 0 {
            if let Some(meta) = self.active_connections.get(&cid) {
                if meta.sender.send((frame, remote_addr)).await.is_err() {
                    debug!(addr = %remote_addr, cid = %cid, "端点(CID查找)已死亡，移除连接 | Endpoint (CID lookup) died, removing connection");
                    self.remove_connection_by_cid(cid);
                    return RoutingResult::ConnectionNotFound;
                }
                return RoutingResult::Dispatched;
            }
        }

        // 2. 通过查找远程地址来处理仍处于握手状态的连接的数据包（例如重传的SYN）
        //    但是要确保连接仍然存在，避免将帧发送到已关闭的连接
        // 2. Handle packets for connections that are still in handshake (e.g. retransmitted SYN)
        //    by looking up the remote address.
        //    但是要确保连接仍然存在，避免将帧发送到已关闭的连接
        if let Some(&existing_cid) = self.address_routing.get(&remote_addr) {
            // 双重检查连接是否仍然存在
            // Double-check that the connection still exists
            if let Some(meta) = self.active_connections.get(&existing_cid) {
                if meta.sender.send((frame, remote_addr)).await.is_err() {
                    debug!(addr = %remote_addr, cid = %existing_cid, "端点(地址查找)已死亡，移除连接 | Endpoint (addr lookup) died, removing connection");
                    self.remove_connection_by_cid(existing_cid);
                    return RoutingResult::ConnectionNotFound;
                }
                return RoutingResult::Dispatched;
            } else {
                // 连接不再存在，清理过时的地址映射
                // Connection no longer exists, clean up the stale address mapping
                debug!(addr = %remote_addr, cid = %existing_cid, "移除不存在连接的过时地址映射 | Removing stale address mapping for non-existent connection");
                self.address_routing.remove(&remote_addr);
            }
        }

        // 3. 如果执行到这里，说明这是一个不可路由的、非SYN的数据包
        //    我们同时会检查draining CIDs，以便为数据包为何被丢弃提供更好的日志
        // 3. If we get here, it's an unroutable, non-SYN packet.
        //    We also check the draining CIDs to provide better logging for why a packet might be dropped.
        if self.draining_pool.contains(&cid) {
            debug!(
                "忽略来自排水连接的数据包 {} CID {}: {:?} | Ignoring packet for draining connection from {} with CID {}: {:?}",
                remote_addr, cid, frame, remote_addr, cid, frame
            );
            RoutingResult::ConnectionDraining
        } else {
            debug!(
                "忽略来自未知源的非SYN数据包 {} 不可路由CID {}: {:?} | Ignoring non-SYN packet from unknown source {} with unroutable CID {}: {:?}",
                remote_addr, cid, frame, remote_addr, cid, frame
            );
            RoutingResult::ConnectionNotFound
        }
    }

    /// 注册新连接
    /// Register a new connection
    ///
    /// 将新连接添加到活跃连接映射和地址路由表中。
    /// 如果相同地址已存在旧连接，会先移除旧连接。
    ///
    /// Adds a new connection to the active connections mapping and address routing table.
    /// If an old connection exists for the same address, it will be removed first.
    pub(crate) fn register_connection(
        &mut self,
        cid: u32,
        remote_addr: SocketAddr,
        meta: ConnectionMeta,
    ) {
        debug!(cid = %cid, addr = %remote_addr, "注册新连接 | Registering new connection");
        
        // 如果相同地址已存在连接，先移除旧连接
        // If a connection already exists for the same address, remove the old one first
        if let Some(&old_cid) = self.address_routing.get(&remote_addr) {
            if old_cid != cid {
                debug!(old_cid = %old_cid, new_cid = %cid, addr = %remote_addr, 
                       "相同地址存在旧连接，移除旧连接 | Old connection exists for same address, removing old connection");
                self.remove_connection_by_cid(old_cid);
            }
        }

        // 添加到活跃连接映射
        // Add to active connections mapping
        self.active_connections.insert(cid, meta);
        
        // 添加到地址路由表
        // Add to address routing table
        self.address_routing.insert(remote_addr, cid);
    }

    /// 更新连接的地址映射（用于连接迁移）
    /// Update connection address mapping (for connection migration)
    ///
    /// 该方法用于处理连接迁移场景，更新连接的地址映射关系。
    ///
    /// This method handles connection migration scenarios by updating
    /// the connection's address mapping relationship.
    pub(crate) fn update_connection_address(&mut self, cid: u32, new_addr: SocketAddr) {
        debug!(cid = %cid, new_addr = %new_addr, "更新连接地址映射 | Updating connection address mapping");
        
        // 查找并移除旧的地址映射
        // Find and remove old address mapping
        let mut old_addr = None;
        for (addr, &mapped_cid) in self.address_routing.iter() {
            if mapped_cid == cid {
                old_addr = Some(*addr);
                break;
            }
        }
        if let Some(addr) = old_addr {
            self.address_routing.remove(&addr);
            debug!(cid = %cid, old_addr = %addr, "移除旧地址映射 | Removed old address mapping");
        }
        
        // 添加新的地址映射
        // Add new address mapping
        self.address_routing.insert(new_addr, cid);
    }

    /// 移除连接及其相关状态
    /// Remove connection and its associated state
    ///
    /// 这是连接清理的权威方法。它会从活跃连接映射和地址路由表中
    /// 移除连接，并将CID移入排水池。
    ///
    /// This is the authoritative method for connection cleanup. It removes
    /// the connection from active connections mapping and address routing table,
    /// and moves the CID to the draining pool.
    pub(crate) fn remove_connection_by_cid(&mut self, cid: u32) {
        let was_present = self.active_connections.remove(&cid).is_some();
        if !was_present {
            debug!(cid = %cid, "连接已被移除，无需操作 | Connection already removed, nothing to do");
            return;
        }

        // 查找并移除对应的地址映射
        // 只有当映射到这个特定CID时才移除，以避免移除更新的映射
        // Find and remove the corresponding address mapping.
        // Only remove if it maps to this specific CID to avoid removing newer mappings.
        let mut addr_to_remove = None;
        for (addr, &mapped_cid) in self.address_routing.iter() {
            if mapped_cid == cid {
                addr_to_remove = Some(*addr);
                break;
            }
        }
        if let Some(addr) = addr_to_remove {
            self.address_routing.remove(&addr);
            debug!(cid = %cid, addr = %addr, "移除连接的地址映射 | Removed address mapping for connection");
        }
        
        // 不直接丢弃CID，而是将其移至draining状态
        // Instead of forgetting the CID, move it to the draining state
        self.draining_pool.insert(cid);

        debug!(cid = %cid, "清理连接状态完成，CID现在处于排水状态 | Cleaned up connection state. CID is now in draining state.");
    }

    /// 检查连接是否存在
    /// Check if connection exists
    pub(crate) fn connection_exists(&self, cid: u32) -> bool {
        self.active_connections.contains_key(&cid)
    }

    /// 检查CID是否在排水池中
    /// Check if CID is in draining pool
    pub(crate) fn is_draining(&self, cid: u32) -> bool {
        self.draining_pool.contains(&cid)
    }

    /// 清理排水池
    /// Cleanup draining pool
    pub(crate) fn cleanup_draining_pool(&mut self) {
        self.draining_pool.cleanup();
    }

} 