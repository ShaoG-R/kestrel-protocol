//! 帧处理器模块 - 将不同类型帧的处理逻辑模块化
//! Frame Processors Module - Modularizes the processing logic for different frame types
//!
//! 该模块将帧处理逻辑从主要的 endpoint logic 中分离出来，
//! 为每种帧类型提供专门的处理器，提高代码的可维护性和可测试性。
//!
//! This module separates frame processing logic from the main endpoint logic,
//! providing specialized processors for each frame type to improve code
//! maintainability and testability.

pub mod ack;
pub mod connection;
pub mod data;
pub mod path;

// 重新导出主要的处理器类型
// Re-export main processor types
pub use ack::AckProcessor;
pub use connection::ConnectionProcessor;
pub use data::PushProcessor;
pub use path::PathProcessor;

use crate::{
    error::Result,
    packet::frame::Frame,
    socket::AsyncUdpSocket,
};
use std::net::SocketAddr;
use tokio::time::Instant;
use crate::core::endpoint::lifecycle::ConnectionLifecycleManager;
use crate::core::endpoint::types::state::ConnectionState;
use crate::core::endpoint::Endpoint;
use async_trait::async_trait;

/// 帧类型标记模块 - 提供编译时类型安全保证
/// Frame type markers module - Provides compile-time type safety guarantees
pub mod frame_types {
    
    /// 数据帧类型标记
    /// Data frame type marker
    #[derive(Debug, Clone, Copy)]
    pub struct PushFrame;
    
    /// 确认帧类型标记
    /// Acknowledgment frame type marker
    #[derive(Debug, Clone, Copy)]
    pub struct AckFrame;
    
    /// 连接管理帧类型标记
    /// Connection management frame type marker
    #[derive(Debug, Clone, Copy)]
    pub struct ConnectionFrame;
    
    /// 路径验证帧类型标记
    /// Path validation frame type marker
    #[derive(Debug, Clone, Copy)]
    pub struct PathFrame;
    
    /// 心跳帧类型标记
    /// Heartbeat frame type marker
    #[derive(Debug, Clone, Copy)]
    pub struct PingFrame;
    
    /// 帧类型特征 - 将运行时帧与编译时类型关联
    /// Frame type trait - Associates runtime frames with compile-time types
    pub trait FrameType {
        /// 检查给定的帧是否属于此类型
        /// Check if the given frame belongs to this type
        fn matches(frame: &crate::packet::frame::Frame) -> bool;
        
        /// 获取类型名称，用于错误消息和调试
        /// Get type name for error messages and debugging
        fn type_name() -> &'static str;
    }
    
    impl FrameType for PushFrame {
        fn matches(frame: &crate::packet::frame::Frame) -> bool {
            matches!(frame, crate::packet::frame::Frame::Push { .. })
        }
        
        fn type_name() -> &'static str {
            "PushFrame"
        }
    }
    
    impl FrameType for AckFrame {
        fn matches(frame: &crate::packet::frame::Frame) -> bool {
            matches!(frame, crate::packet::frame::Frame::Ack { .. })
        }
        
        fn type_name() -> &'static str {
            "AckFrame"
        }
    }
    
    impl FrameType for ConnectionFrame {
        fn matches(frame: &crate::packet::frame::Frame) -> bool {
            matches!(frame, 
                crate::packet::frame::Frame::Syn { .. } |
                crate::packet::frame::Frame::SynAck { .. } |
                crate::packet::frame::Frame::Fin { .. }
            )
        }
        
        fn type_name() -> &'static str {
            "ConnectionFrame"
        }
    }
    
    impl FrameType for PathFrame {
        fn matches(frame: &crate::packet::frame::Frame) -> bool {
            matches!(frame, 
                crate::packet::frame::Frame::PathChallenge { .. } |
                crate::packet::frame::Frame::PathResponse { .. }
            )
        }
        
        fn type_name() -> &'static str {
            "PathFrame"
        }
    }
    
    impl FrameType for PingFrame {
        fn matches(frame: &crate::packet::frame::Frame) -> bool {
            matches!(frame, crate::packet::frame::Frame::Ping { .. })
        }
        
        fn type_name() -> &'static str {
            "PingFrame"
        }
    }
}

/// 类型安全的帧处理器特征
/// Type-safe frame processor trait
/// 
/// 这个设计使用关联类型和PhantomData来确保编译时的类型安全，
/// 防止处理器处理错误类型的帧。
/// 
/// This design uses associated types and PhantomData to ensure compile-time type safety,
/// preventing processors from handling frames of the wrong type.
#[async_trait]
pub trait TypeSafeFrameProcessor<S: AsyncUdpSocket> {
    /// 关联的帧类型标记
    /// Associated frame type marker
    type FrameTypeMarker: frame_types::FrameType;
    
    /// 处理器名称，用于错误消息和日志
    /// Processor name for error messages and logging
    fn name() -> &'static str;
    
    /// 类型安全的帧处理方法
    /// Type-safe frame processing method
    async fn process_frame(
        endpoint: &mut Endpoint<S>,
        frame: Frame,
        src_addr: SocketAddr,
        now: Instant,
    ) -> Result<()>;
}

/// 类型安全的帧验证器特征 - 不依赖于泛型参数
/// Type-safe frame validator trait - Independent of generic parameters
pub trait TypeSafeFrameValidator {
    /// 关联的帧类型标记
    /// Associated frame type marker
    type FrameTypeMarker: frame_types::FrameType;
    
    /// 编译时类型验证，确保帧类型匹配
    /// Compile-time type validation to ensure frame type matching
    fn validate_frame_type(frame: &Frame) -> Result<()> {
        if <Self::FrameTypeMarker as frame_types::FrameType>::matches(frame) {
            Ok(())
        } else {
            Err(crate::error::Error::InvalidFrame(
                format!(
                    "Frame type mismatch: expected {}, got incompatible frame",
                    <Self::FrameTypeMarker as frame_types::FrameType>::type_name()
                )
            ))
        }
    }
}

/// 统一的帧处理器特征，整合了异步处理和静态方法
/// Unified frame processor trait that integrates async processing and static methods
/// 
/// 这个新的 trait 设计解决了之前分离 trait 带来的复杂性问题，
/// 提供了更好的类型安全性和更简洁的接口。
/// 
/// This new trait design solves the complexity issues brought by separated traits,
/// providing better type safety and a cleaner interface.
#[async_trait]
pub trait UnifiedFrameProcessor<S: AsyncUdpSocket> {
    /// 关联类型：该处理器能处理的帧类型
    /// Associated type: the frame type this processor can handle
    type FrameType;
    
    /// 检查该处理器是否可以处理给定的帧类型
    /// Check if this processor can handle the given frame type
    fn can_handle(frame: &Frame) -> bool;

    /// 获取处理器的名称，用于日志记录
    /// Get the processor name for logging
    fn name() -> &'static str;
    
    /// 处理特定类型的帧
    /// Process a specific type of frame
    async fn process_frame(
        endpoint: &mut Endpoint<S>,
        frame: Frame,
        src_addr: SocketAddr,
        now: Instant,
    ) -> Result<()>;
    
    /// 可选的帧验证方法，提供编译时类型安全
    /// Optional frame validation method for compile-time type safety
    fn validate_frame(frame: &Frame) -> Result<()> {
        if Self::can_handle(frame) {
            Ok(())
        } else {
            Err(crate::error::Error::InvalidFrame(
                format!("Frame type not supported by {}", Self::name())
            ))
        }
    }
}

/// 旧版本兼容性：帧处理器特征，定义了所有帧处理器的通用接口
/// Legacy compatibility: Frame processor trait that defines the common interface for all frame processors
#[deprecated(note = "Use UnifiedFrameProcessor instead")]
pub trait FrameProcessor<S: AsyncUdpSocket> {
    /// 处理特定类型的帧
    /// Process a specific type of frame
    fn process_frame(
        endpoint: &mut Endpoint<S>,
        frame: Frame,
        src_addr: SocketAddr,
        now: Instant,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
}

/// 旧版本兼容性：帧处理器静态方法特征，用于不依赖泛型参数的方法
/// Legacy compatibility: Frame processor static methods trait for methods that don't depend on generic parameters
#[deprecated(note = "Use UnifiedFrameProcessor instead")]
pub trait FrameProcessorStatic {
    /// 检查该处理器是否可以处理给定的帧类型
    /// Check if this processor can handle the given frame type
    fn can_handle(frame: &Frame) -> bool;

    /// 获取处理器的名称，用于日志记录
    /// Get the processor name for logging
    fn name() -> &'static str;
}

/// 动态帧处理器注册表
/// Dynamic frame processor registry
pub struct FrameProcessorRegistry<S: AsyncUdpSocket> {
    processors: Vec<Box<dyn DynamicFrameProcessor<S>>>,
}

/// 动态帧处理器特征，用于 trait 对象
/// Dynamic frame processor trait for trait objects
#[async_trait]
pub trait DynamicFrameProcessor<S: AsyncUdpSocket>: Send + Sync {
    /// 检查是否可以处理给定的帧
    /// Check if this processor can handle the given frame
    fn can_handle(&self, frame: &Frame) -> bool;
    
    /// 处理器名称
    /// Processor name
    fn name(&self) -> &'static str;
    
    /// 处理帧
    /// Process frame
    async fn process_frame(
        &self,
        endpoint: &mut Endpoint<S>,
        frame: Frame,
        src_addr: SocketAddr,
        now: Instant,
    ) -> Result<()>;
}

/// 为统一处理器创建 trait 对象适配器
/// Trait object adapter for unified processors
pub struct ProcessorAdapter<P> {
    _phantom: std::marker::PhantomData<P>,
}

impl<P> ProcessorAdapter<P> {
    pub fn new() -> Self {
        Self {
            _phantom: std::marker::PhantomData,
        }
    }
}

#[async_trait]
impl<P, S> DynamicFrameProcessor<S> for ProcessorAdapter<P>
where
    P: UnifiedFrameProcessor<S> + Send + Sync,
    S: AsyncUdpSocket,
{
    fn can_handle(&self, frame: &Frame) -> bool {
        P::can_handle(frame)
    }
    
    fn name(&self) -> &'static str {
        P::name()
    }
    
    async fn process_frame(
        &self,
        endpoint: &mut Endpoint<S>,
        frame: Frame,
        src_addr: SocketAddr,
        now: Instant,
    ) -> Result<()> {
        P::process_frame(endpoint, frame, src_addr, now).await
    }
}

impl<S: AsyncUdpSocket> FrameProcessorRegistry<S> {
    /// 创建新的处理器注册表
    /// Create a new processor registry
    pub fn new() -> Self {
        Self {
            processors: Vec::new(),
        }
    }
    
    /// 注册一个处理器
    /// Register a processor
    pub fn register<P>(&mut self, _processor_type: std::marker::PhantomData<P>)
    where
        P: UnifiedFrameProcessor<S> + Send + Sync + 'static,
    {
        self.processors.push(Box::new(ProcessorAdapter::<P>::new()));
    }
    
    /// 创建默认的处理器注册表，包含所有内置处理器
    /// Create default processor registry with all built-in processors
    pub fn default_registry() -> Self {
        let mut registry = Self::new();
        registry.register::<PushProcessor>(std::marker::PhantomData);
        registry.register::<AckProcessor>(std::marker::PhantomData);
        registry.register::<ConnectionProcessor>(std::marker::PhantomData);
        registry.register::<PathProcessor>(std::marker::PhantomData);
        registry
    }
    
    /// 路由帧到合适的处理器
    /// Route frame to appropriate processor
    pub async fn route_frame(
        &self,
        endpoint: &mut Endpoint<S>,
        frame: Frame,
        src_addr: SocketAddr,
        now: Instant,
    ) -> Result<()> {
        // 更新最后接收时间
        // Update last receive time
        endpoint.update_last_recv_time(now);

        // 检查路径迁移
        // Check for path migration
        endpoint.check_path_migration(src_addr).await?;

        // 特殊处理 PING 帧（保持原有逻辑）
        // Special handling for PING frames (maintain original logic)
        if matches!(frame, Frame::Ping { .. }) {
            tracing::trace!(
                cid = endpoint.local_cid(),
                "Received PING frame, no action needed"
            );
            return Ok(());
        }

        // 查找能处理该帧的处理器
        // Find a processor that can handle this frame
        for processor in &self.processors {
            if processor.can_handle(&frame) {
                tracing::trace!(
                    processor_name = processor.name(),
                    frame_type = ?frame,
                    "Routing frame to processor"
                );
                return processor.process_frame(endpoint, frame, src_addr, now).await;
            }
        }

        // 没有找到合适的处理器
        // No suitable processor found
        Err(crate::error::Error::InvalidFrame(
            format!("No processor found for frame type: {:?}", frame)
        ))
    }
}

impl<S: AsyncUdpSocket> Default for FrameProcessorRegistry<S> {
    fn default() -> Self {
        Self::default_registry()
    }
}

/// 帧处理上下文，包含处理帧时需要的通用信息
/// Frame processing context containing common information needed when processing frames
pub struct FrameProcessingContext {
    /// 当前时间
    /// Current time
    pub now: Instant,
    
    /// 源地址
    /// Source address
    pub src_addr: SocketAddr,
    
    /// 连接状态
    /// Connection state
    pub connection_state: ConnectionState,
    
    /// 本地连接ID
    /// Local connection ID
    pub local_cid: u32,
}

impl FrameProcessingContext {
    /// 创建新的帧处理上下文
    /// Create a new frame processing context
    pub fn new<S: AsyncUdpSocket>(
        endpoint: &Endpoint<S>,
        src_addr: SocketAddr,
        now: Instant,
    ) -> Self {
        Self {
            now,
            src_addr,
            connection_state: endpoint.lifecycle_manager().current_state().clone(),
            local_cid: endpoint.local_cid(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::frame::Frame;
    use crate::packet::header::ShortHeader;
    use crate::packet::command::Command;
    use bytes::Bytes;

    #[test]
    fn test_frame_processor_routing() {
        // 测试不同帧类型的路由逻辑
        // Test routing logic for different frame types
        
        let push_frame = Frame::Push {
            header: ShortHeader {
                command: Command::Push,
                connection_id: 1,
                payload_length: 10,
                recv_window_size: 100,
                timestamp: 1000,
                sequence_number: 1,
                recv_next_sequence: 0,
            },
            payload: Bytes::from("test data"),
        };
        
        assert!(<PushProcessor as FrameProcessorStatic>::can_handle(&push_frame));
        assert!(!<AckProcessor as FrameProcessorStatic>::can_handle(&push_frame));
        assert!(!<ConnectionProcessor as FrameProcessorStatic>::can_handle(&push_frame));
        assert!(!<PathProcessor as FrameProcessorStatic>::can_handle(&push_frame));
    }
}