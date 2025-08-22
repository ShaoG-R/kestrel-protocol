//! 传输管理器 - 负责底层传输操作的统一管理
//! Transport Manager - Unified management of low-level transport operations

use super::{BindableTransport, TransportCommand};
use crate::error::{Error, Result};
use std::{net::SocketAddr, sync::Arc};
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// 传输管理器 - 负责底层传输操作的统一管理
/// Transport Manager - Unified management of low-level transport operations
///
/// 该组件封装了所有与底层传输相关的操作，包括帧发送、重绑定等。
/// 它使用消息传递模式来确保线程安全，避免使用锁。
///
/// This component encapsulates all operations related to the underlying transport,
/// including frame sending, rebinding, etc. It uses message passing to ensure
/// thread safety without using locks.
#[derive(Debug)]
pub(crate) struct TransportManager<T: BindableTransport> {
    /// 传输实例的原子引用，支持运行时替换
    /// Atomic reference to transport instance, supports runtime replacement
    transport: Arc<T>,
    /// 向传输发送Actor的命令通道
    /// Command channel to transport send actor
    send_tx: mpsc::Sender<TransportCommand<T>>,
}

impl<T: BindableTransport> TransportManager<T> {
    /// 创建新的传输管理器
    /// Creates a new transport manager
    ///
    /// # Arguments
    /// * `transport` - 底层传输实例
    /// * `send_tx` - 向传输发送Actor的命令通道
    ///
    /// # Arguments
    /// * `transport` - Underlying transport instance
    /// * `send_tx` - Command channel to transport send actor
    pub(crate) fn new(transport: Arc<T>, send_tx: mpsc::Sender<TransportCommand<T>>) -> Self {
        debug!("创建传输管理器 | Creating transport manager");
        Self { transport, send_tx }
    }

    /// 重新绑定传输到新地址
    /// Rebind transport to new address
    ///
    /// 该操作会原子性地替换底层传输实例，确保在重绑定过程中
    /// 不会丢失正在处理的请求。
    ///
    /// This operation atomically replaces the underlying transport instance,
    /// ensuring that no ongoing requests are lost during the rebinding process.
    pub(crate) async fn rebind(&mut self, new_addr: SocketAddr) -> Result<SocketAddr> {
        debug!(
            new_addr = %new_addr,
            "传输管理器重绑定地址 | Transport manager rebinding address"
        );

        // 创建新的传输实例
        // Create new transport instance
        let new_transport = Arc::new(T::bind(new_addr).await?);
        let actual_addr = new_transport.local_addr()?;

        // 发送替换命令到传输Actor
        // Send swap command to transport actor
        let swap_command = TransportCommand::SwapTransport(new_transport.clone());
        if self.send_tx.send(swap_command).await.is_err() {
            warn!(
                "传输Actor通道已关闭，无法完成重绑定 | Transport actor channel closed, cannot complete rebind"
            );
            return Err(Error::ChannelClosed);
        }

        // 更新本地传输引用
        // Update local transport reference
        self.transport = new_transport;

        debug!(
            actual_addr = %actual_addr,
            "传输管理器重绑定完成 | Transport manager rebind completed"
        );

        Ok(actual_addr)
    }

    /// 获取当前传输的本地地址
    /// Get the local address of the current transport
    pub(crate) fn local_addr(&self) -> Result<SocketAddr> {
        self.transport.local_addr()
    }

    /// 获取传输实例的克隆引用
    /// Get a cloned reference to the transport instance
    ///
    /// 返回底层传输实例的Arc克隆，可用于直接访问传输功能。
    /// 注意：这主要用于向后兼容，新代码应优先使用管理器提供的方法。
    ///
    /// Returns an Arc clone of the underlying transport instance for direct access.
    /// Note: This is mainly for backward compatibility, new code should prefer
    /// using the methods provided by the manager.
    pub(crate) fn transport(&self) -> Arc<T> {
        self.transport.clone()
    }

    /// 获取传输命令发送通道的克隆
    /// Get a clone of the transport command sender channel
    ///
    /// 返回传输命令发送通道的克隆，用于向传输Actor发送命令。
    /// 这主要用于向后兼容，允许Endpoint等组件直接发送传输命令。
    ///
    /// Returns a clone of the transport command sender channel for sending commands to the transport actor.
    /// This is mainly for backward compatibility, allowing components like Endpoint to send transport commands directly.
    pub(crate) fn send_tx(&self) -> mpsc::Sender<TransportCommand<T>> {
        self.send_tx.clone()
    }
}
