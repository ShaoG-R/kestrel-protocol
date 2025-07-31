//! 帧路由分发层 - 负责帧的智能路由和连接映射管理
//! Frame Routing Layer - Handles intelligent frame routing and connection mapping

use crate::socket::event_loop::ConnectionMeta;
use crate::packet::frame::Frame;
use std::{collections::HashMap, net::SocketAddr};
use tracing::{debug, warn};
use crate::socket::event_loop::draining::DrainingPool;
use tokio::time::Instant;

/// 帧路由管理器 - 负责帧的智能路由和连接映射管理
/// Frame Router Manager - Handles intelligent frame routing and connection mapping
///
/// 该组件管理所有连接的路由状态，包括活跃连接映射、地址路由表和 draining 连接池。
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
    /// 正在 draining 的连接ID池
    /// Pool of connection IDs being drained
    draining_pool: DrainingPool,
    /// 早到帧缓存：缓存键 -> 待处理帧列表
    /// Early arrival frame cache: Cache key -> Pending frames list
    /// - 对于CID非0的帧：使用CID作为key
    /// - 对于CID为0的帧（0-RTT场景）：使用远程地址的哈希作为key
    /// For CID non-zero frames: use CID as key
    /// For CID zero frames (0-RTT scenarios): use remote address hash as key
    pending_frames: HashMap<u64, Vec<PendingFrame>>,
}

/// 早到帧缓存项
/// Early arrival frame cache entry
#[derive(Debug, Clone)]
pub(crate) struct PendingFrame {
    frame: Frame,
    remote_addr: SocketAddr,
    arrival_time: Instant,
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
    /// 连接已在 draining 状态，已忽略
    /// Connection is in draining state, ignored
    ConnectionDraining,
    /// 帧已被缓存，等待连接建立
    /// Frame cached, waiting for connection establishment
    FrameCached,
}

impl FrameRouter {
    /// 创建新的帧路由管理器
    /// Creates a new frame router manager
    ///
    /// # Arguments
    /// * `draining_pool` -  draining 连接池实例
    ///
    /// # Arguments  
    /// * `draining_pool` - Draining connection pool instance
    pub(crate) fn new(draining_pool: DrainingPool) -> Self {
        debug!("创建帧路由管理器 | Creating frame router manager");
        Self {
            active_connections: HashMap::new(),
            address_routing: HashMap::new(),
            draining_pool,
            pending_frames: HashMap::new(),
        }
    }

    /// 计算早到帧缓存的键
    /// Calculate cache key for early arrival frames
    ///
    /// 对于CID非0的帧使用CID，对于CID为0的帧（0-RTT场景）使用地址哈希
    /// For non-zero CID frames use CID, for zero CID frames (0-RTT) use address hash
    fn get_cache_key(&self, cid: u32, remote_addr: SocketAddr) -> u64 {
        if cid != 0 {
            cid as u64
        } else {
            // 为0-RTT场景使用地址哈希作为缓存键
            // Use address hash as cache key for 0-RTT scenarios
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            
            let mut hasher = DefaultHasher::new();
            remote_addr.hash(&mut hasher);
            // 确保与CID范围不冲突（使用高位）
            // Ensure no conflict with CID range (use high bits)
            hasher.finish() | (1u64 << 63)
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
        //    检查是否应该缓存早到的PUSH帧（包括0-RTT场景）
        // 3. If we get here, it's an unroutable, non-SYN packet.
        //    Check if we should cache early arrival PUSH frames (including 0-RTT scenarios)
        if self.draining_pool.contains(&cid) {
            debug!(
                "忽略来自 draining 连接的数据包 {} CID {}: {:?} | Ignoring packet for draining connection from {} with CID {}: {:?}",
                remote_addr, cid, frame, remote_addr, cid, frame
            );
            RoutingResult::ConnectionDraining
        } else if matches!(frame, Frame::Push { .. }) {
            // 缓存早到的PUSH帧，支持0-RTT场景（CID可能为0）
            // Cache early arrival PUSH frames, supporting 0-RTT scenarios (CID may be 0)
            debug!(
                "缓存早到的PUSH帧 {} CID {} | Cached early arrival PUSH frame from {} with CID {}",
                remote_addr, cid, remote_addr, cid
            );
            self.cache_early_frame(cid, frame, remote_addr);
            RoutingResult::FrameCached
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
    /// 注册完成后会检查并转发任何缓存的早到帧。
    ///
    /// Adds a new connection to the active connections mapping and address routing table.
    /// If an old connection exists for the same address, it will be removed first.
    /// After registration, checks and forwards any cached early arrival frames.
    pub(crate) async fn register_connection(
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

        // 检查并转发此连接的任何缓存早到帧
        // Check and forward any cached early arrival frames for this connection
        self.forward_cached_frames(cid, remote_addr).await;
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
    /// 移除连接，并将CID移入 draining 池。
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

        debug!(cid = %cid, "清理连接状态完成，CID现在处于 draining 状态 | Cleaned up connection state. CID is now in draining state.");
    }

    /// 检查连接是否存在
    /// Check if connection exists
    pub(crate) fn connection_exists(&self, cid: u32) -> bool {
        self.active_connections.contains_key(&cid)
    }

    /// 检查CID是否在 draining 池中
    /// Check if CID is in draining pool
    pub(crate) fn is_draining(&self, cid: u32) -> bool {
        self.draining_pool.contains(&cid)
    }

    /// 清理 draining 池
    /// Cleanup draining pool
    pub(crate) fn cleanup_draining_pool(&mut self) {
        self.draining_pool.cleanup();
    }

    /// 缓存早到的帧
    /// Cache early arrival frame
    ///
    /// 该方法用于缓存在连接建立前到达的PUSH帧，解决网络包乱序问题。
    /// 缓存的帧会在连接注册时自动转发。支持0-RTT场景（CID为0）。
    ///
    /// This method caches PUSH frames that arrive before connection establishment,
    /// solving the network packet reordering problem. Cached frames will be
    /// automatically forwarded when the connection is registered. Supports 0-RTT scenarios (CID=0).
    fn cache_early_frame(&mut self, cid: u32, frame: Frame, remote_addr: SocketAddr) {
        let cache_key = self.get_cache_key(cid, remote_addr);
        let pending_frame = PendingFrame {
            frame,
            remote_addr,
            arrival_time: Instant::now(),
        };
        
        self.pending_frames
            .entry(cache_key)
            .or_insert_with(Vec::new)
            .push(pending_frame);
        
        debug!(
            cid = %cid,
            addr = %remote_addr,
            cache_key = %cache_key,
            "早到帧已缓存 | Early arrival frame cached"
        );
    }

    /// 检查并转发缓存的早到帧
    /// Check and forward cached early arrival frames
    ///
    /// 当新连接注册时调用此方法，检查是否有对应连接的缓存帧需要转发。
    /// 会检查两种缓存：基于CID的缓存和基于地址的缓存（0-RTT场景）。
    /// 成功转发的帧会从缓存中移除。
    ///
    /// This method is called when a new connection is registered to check if there
    /// are cached frames for the corresponding connection that need to be forwarded.
    /// Checks both CID-based cache and address-based cache (0-RTT scenarios).
    /// Successfully forwarded frames will be removed from the cache.
    pub(crate) async fn forward_cached_frames(&mut self, cid: u32, remote_addr: SocketAddr) {
        let mut keys_to_check = Vec::new();
        
        // 检查基于CID的缓存（如果CID非0）
        // Check CID-based cache (if CID is non-zero)
        if cid != 0 {
            keys_to_check.push(cid as u64);
        }
        
        // 检查基于地址的缓存（0-RTT场景）
        // Check address-based cache (0-RTT scenarios)
        let addr_cache_key = self.get_cache_key(0, remote_addr);
        keys_to_check.push(addr_cache_key);
        
        if let Some(meta) = self.active_connections.get(&cid) {
            let mut total_forwarded = 0;
            
            for cache_key in keys_to_check {
                if let Some(pending_frames) = self.pending_frames.remove(&cache_key) {
                    let mut forwarded_count = 0;
                    
                    for pending_frame in pending_frames {
                        // 确保帧来源地址匹配当前连接
                        // Ensure frame source address matches current connection
                        if pending_frame.remote_addr == remote_addr {
                            if meta.sender.send((pending_frame.frame.clone(), pending_frame.remote_addr)).await.is_ok() {
                                forwarded_count += 1;
                            } else {
                                warn!(
                                    cid = %cid,
                                    addr = %pending_frame.remote_addr,
                                    "转发缓存帧失败，连接通道已关闭 | Failed to forward cached frame, connection channel closed"
                                );
                                break;
                            }
                        } else {
                            // 地址不匹配的帧放回缓存
                            // Put back frames with mismatched addresses
                            self.pending_frames
                                .entry(cache_key)
                                .or_insert_with(Vec::new)
                                .push(pending_frame);
                        }
                    }
                    
                    total_forwarded += forwarded_count;
                    if forwarded_count > 0 {
                        debug!(
                            cid = %cid,
                            cache_key = %cache_key,
                            count = forwarded_count,
                            "成功转发缓存的早到帧 | Successfully forwarded cached early arrival frames"
                        );
                    }
                }
            }
            
            if total_forwarded > 0 {
                debug!(
                    cid = %cid,
                    addr = %remote_addr,
                    total_count = total_forwarded,
                    "总共转发缓存帧 | Total forwarded cached frames"
                );
            }
        }
    }

    /// 清理超时的早到帧缓存
    /// Cleanup expired early arrival frame cache
    ///
    /// 清理超过指定时间的缓存帧，防止内存泄漏。建议定期调用。
    ///
    /// Cleans up cached frames that exceed the specified time to prevent memory leaks.
    /// It's recommended to call this periodically.
    pub(crate) fn cleanup_expired_frames(&mut self, max_age: std::time::Duration) {
        let now = Instant::now();
        let mut expired_keys = Vec::new();
        
        for (cache_key, frames) in &mut self.pending_frames {
            frames.retain(|frame| now.duration_since(frame.arrival_time) < max_age);
            if frames.is_empty() {
                expired_keys.push(*cache_key);
            }
        }
        
        for cache_key in expired_keys {
            self.pending_frames.remove(&cache_key);
            debug!(cache_key = %cache_key, "清理超时的早到帧缓存 | Cleaned up expired early arrival frame cache");
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::socket::event_loop::draining::DrainingPool;
    use crate::packet::frame::Frame;
    use std::net::{Ipv4Addr, SocketAddr};
    use tokio::sync::mpsc;
    use std::time::Duration;

    fn create_test_router() -> FrameRouter {
        let draining_pool = DrainingPool::new(Duration::from_secs(10));
        FrameRouter::new(draining_pool)
    }

    fn create_test_address() -> SocketAddr {
        SocketAddr::new(Ipv4Addr::new(127, 0, 0, 1).into(), 8080)
    }

    #[tokio::test]
    async fn test_early_frame_caching() {
        let mut router = create_test_router();
        let remote_addr = create_test_address();
        let cid = 123u32;

        // Create a PUSH frame that would normally be unroutable
        let push_frame = Frame::new_push(cid, 0, 0, 1024, 12345, bytes::Bytes::from_static(b"test data"));

        // Dispatch frame when no connection exists - should be cached
        let result = router.dispatch_frame(push_frame.clone(), remote_addr).await;
        assert!(matches!(result, RoutingResult::FrameCached));

        // Verify frame is cached
        let cache_key = cid as u64;
        assert!(router.pending_frames.contains_key(&cache_key));
        assert_eq!(router.pending_frames[&cache_key].len(), 1);

        // Create a mock connection and register it
        let (tx, mut _rx) = mpsc::channel(10);
        let meta = ConnectionMeta { sender: tx };
        
        router.register_connection(cid, remote_addr, meta).await;

        // Verify cached frame was cleared (forwarded)
        assert!(!router.pending_frames.contains_key(&cache_key));
    }

    #[tokio::test]
    async fn test_multiple_early_frames_caching() {
        let mut router = create_test_router();
        let remote_addr = create_test_address();
        let cid = 456u32;

        // Create multiple PUSH frames
        let push_frame1 = Frame::new_push(cid, 0, 0, 1024, 12345, bytes::Bytes::from_static(b"data1"));
        let push_frame2 = Frame::new_push(cid, 1, 0, 1024, 12346, bytes::Bytes::from_static(b"data2"));

        // Dispatch frames when no connection exists - should be cached
        let result1 = router.dispatch_frame(push_frame1, remote_addr).await;
        let result2 = router.dispatch_frame(push_frame2, remote_addr).await;
        
        assert!(matches!(result1, RoutingResult::FrameCached));
        assert!(matches!(result2, RoutingResult::FrameCached));

        // Verify frames are cached
        let cache_key = cid as u64;
        assert!(router.pending_frames.contains_key(&cache_key));
        assert_eq!(router.pending_frames[&cache_key].len(), 2);
    }

    #[tokio::test]
    async fn test_non_push_frames_not_cached() {
        let mut router = create_test_router();
        let remote_addr = create_test_address();
        let cid = 789u32;

        // Create an ACK frame (non-PUSH)
        let ack_frame = Frame::new_ack(cid, 0, 1024, &[], 12345);

        // Dispatch frame when no connection exists - should NOT be cached
        let result = router.dispatch_frame(ack_frame, remote_addr).await;
        assert!(matches!(result, RoutingResult::ConnectionNotFound));

        // Verify frame is NOT cached
        let cache_key = cid as u64;
        assert!(!router.pending_frames.contains_key(&cache_key));
    }

    #[tokio::test]
    async fn test_cleanup_expired_frames() {
        let mut router = create_test_router();
        let remote_addr = create_test_address();
        let cid = 999u32;

        // Create a PUSH frame and cache it
        let push_frame = Frame::new_push(cid, 0, 0, 1024, 12345, bytes::Bytes::from_static(b"test data"));
        let result = router.dispatch_frame(push_frame, remote_addr).await;
        assert!(matches!(result, RoutingResult::FrameCached));

        // Verify frame is cached
        let cache_key = cid as u64;
        assert!(router.pending_frames.contains_key(&cache_key));

        // Cleanup with a very short max_age (should remove all frames)
        router.cleanup_expired_frames(Duration::from_nanos(1));

        // Verify cached frame was cleaned up
        assert!(!router.pending_frames.contains_key(&cache_key));
    }

    #[tokio::test]
    async fn test_forward_cached_frames_success() {
        let mut router = create_test_router();
        let remote_addr = create_test_address();
        let cid = 555u32;

        // Create and cache a PUSH frame
        let push_frame = Frame::new_push(cid, 0, 0, 1024, 12345, bytes::Bytes::from_static(b"cached data"));
        let _result = router.dispatch_frame(push_frame.clone(), remote_addr).await;

        // Create a mock connection with a receiver to verify forwarding
        let (tx, mut rx) = mpsc::channel(10);
        let meta = ConnectionMeta { sender: tx };

        // Register connection (should automatically forward cached frames)
        router.register_connection(cid, remote_addr, meta).await;

        // Verify cached frame was forwarded
        let cache_key = cid as u64;
        assert!(!router.pending_frames.contains_key(&cache_key));
        
        // Verify frame was actually sent to the connection
        let received = rx.try_recv();
        assert!(received.is_ok());
        let (forwarded_frame, forwarded_addr) = received.unwrap();
        assert_eq!(forwarded_frame, push_frame);
        assert_eq!(forwarded_addr, remote_addr);
    }

    #[tokio::test]
    async fn test_zero_rtt_early_frames_caching() {
        let mut router = create_test_router();
        let remote_addr = create_test_address();
        let cid = 0u32; // 0-RTT scenario with CID=0

        // Create a PUSH frame with CID=0 (0-RTT scenario)
        let push_frame = Frame::new_push(cid, 0, 0, 1024, 12345, bytes::Bytes::from_static(b"0-RTT data"));

        // Dispatch frame when no connection exists - should be cached using address hash
        let result = router.dispatch_frame(push_frame.clone(), remote_addr).await;
        assert!(matches!(result, RoutingResult::FrameCached));

        // Verify frame is cached using address-based key
        let addr_cache_key = router.get_cache_key(0, remote_addr);
        assert!(router.pending_frames.contains_key(&addr_cache_key));
        assert_eq!(router.pending_frames[&addr_cache_key].len(), 1);

        // Create a mock connection with non-zero CID and register it
        let actual_cid = 123u32;
        let (tx, mut _rx) = mpsc::channel(10);
        let meta = ConnectionMeta { sender: tx };
        
        router.register_connection(actual_cid, remote_addr, meta).await;

        // Verify cached frame was cleared (forwarded from address-based cache)
        assert!(!router.pending_frames.contains_key(&addr_cache_key));
    }

    #[tokio::test]
    async fn test_multiple_clients_zero_rtt_isolation() {
        let mut router = create_test_router();
        let addr1 = SocketAddr::new(Ipv4Addr::new(127, 0, 0, 1).into(), 8081);
        let addr2 = SocketAddr::new(Ipv4Addr::new(127, 0, 0, 1).into(), 8082);

        // Both clients send 0-RTT PUSH frames with CID=0
        let push_frame1 = Frame::new_push(0, 0, 0, 1024, 12345, bytes::Bytes::from_static(b"client1 data"));
        let push_frame2 = Frame::new_push(0, 1, 0, 1024, 12346, bytes::Bytes::from_static(b"client2 data"));

        // Dispatch frames - should be cached separately based on addresses
        let result1 = router.dispatch_frame(push_frame1, addr1).await;
        let result2 = router.dispatch_frame(push_frame2, addr2).await;
        
        assert!(matches!(result1, RoutingResult::FrameCached));
        assert!(matches!(result2, RoutingResult::FrameCached));

        // Verify frames are cached separately
        let cache_key1 = router.get_cache_key(0, addr1);
        let cache_key2 = router.get_cache_key(0, addr2);
        
        assert!(router.pending_frames.contains_key(&cache_key1));
        assert!(router.pending_frames.contains_key(&cache_key2));
        assert_ne!(cache_key1, cache_key2); // Keys should be different
        assert_eq!(router.pending_frames[&cache_key1].len(), 1);
        assert_eq!(router.pending_frames[&cache_key2].len(), 1);
    }
} 