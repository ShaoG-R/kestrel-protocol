//! A pool for managing connection IDs that are in a "draining" state.
//!
//! This is equivalent to TCP's TIME_WAIT state. CIDs in this pool cannot be
//! immediately reused for new connections, preventing delayed packets from an
//! old connection from being misinterpreted by a new one.

use std::collections::HashMap;
use tokio::time::{Duration, Instant};
use tracing::trace;

/// Manages a collection of CIDs that are in a temporary "draining" state.
///
/// 管理处于临时“冷却”状态的CID集合。
#[derive(Debug)]
pub(crate) struct DrainingPool {
    cids: HashMap<u32, Instant>,
    timeout: Duration,
}

impl DrainingPool {
    /// Creates a new `DrainingPool` with a specified timeout for each CID.
    ///
    /// 创建一个新的 `DrainingPool`，并为每个CID指定一个超时时间。
    pub(crate) fn new(timeout: Duration) -> Self {
        Self {
            cids: HashMap::new(),
            timeout,
        }
    }

    /// Adds a CID to the draining pool. It will be automatically removed after the timeout.
    ///
    /// 将一个CID添加到冷却池中。它将在超时后被自动移除。
    pub(crate) fn insert(&mut self, cid: u32) {
        self.cids.insert(cid, Instant::now());
    }

    /// Checks if a CID is currently in the draining pool.
    ///
    // 检查一个CID当前是否在冷却池中。
    pub(crate) fn contains(&self, cid: &u32) -> bool {
        self.cids.contains_key(cid)
    }

    /// Removes all CIDs from the pool that have exceeded their timeout.
    ///
    /// 从池中移除所有已超时的CID。
    pub(crate) fn cleanup(&mut self) {
        let now = Instant::now();
        let timeout = self.timeout;

        let before_count = self.cids.len();
        if before_count == 0 {
            return;
        }

        self.cids
            .retain(|_cid, start_time| now.duration_since(*start_time) < timeout);

        let after_count = self.cids.len();
        if after_count < before_count {
            trace!(
                cleaned_count = before_count - after_count,
                "Cleaned up expired draining CIDs."
            );
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.cids.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn test_draining_pool_insert_and_contains() {
        let mut pool = DrainingPool::new(Duration::from_secs(5));
        assert!(!pool.contains(&123));
        pool.insert(123);
        assert!(pool.contains(&123));
    }

    #[tokio::test(start_paused = true)]
    async fn test_draining_pool_cleanup() {
        let timeout = Duration::from_secs(2);
        let mut pool = DrainingPool::new(timeout);

        pool.insert(1);
        pool.insert(2);
        assert_eq!(pool.len(), 2);

        // Advance time by 1 second, nothing should be cleaned up
        tokio::time::sleep(Duration::from_secs(1)).await;
        pool.cleanup();
        assert_eq!(pool.len(), 2);
        assert!(pool.contains(&1));
        assert!(pool.contains(&2));

        // Insert a new CID
        pool.insert(3);
        assert_eq!(pool.len(), 3);

        // Advance time past the timeout for the first two CIDs
        tokio::time::sleep(Duration::from_secs(1)).await;
        pool.cleanup();

        // CIDs 1 and 2 should be gone, but 3 should remain
        assert_eq!(pool.len(), 1);
        assert!(!pool.contains(&1));
        assert!(!pool.contains(&2));
        assert!(pool.contains(&3));

        // Advance time again to clean up the last one
        tokio::time::sleep(Duration::from_secs(1)).await;
        pool.cleanup();
        assert_eq!(pool.len(), 0);
    }
} 