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
use super::traits::ProcessorOperations;
use std::net::SocketAddr;
use tokio::time::Instant;
use crate::core::endpoint::types::state::ConnectionState;
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
/// 防止处理器处理错误类型的帧。同时通过 trait bounds 实现解耦。
/// 
/// This design uses associated types and PhantomData to ensure compile-time type safety,
/// preventing processors from handling frames of the wrong type. It also achieves
/// decoupling through trait bounds.
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
    /// 
    /// 使用 EndpointOperations trait 对象实现解耦
    /// Uses EndpointOperations trait object to achieve decoupling
    async fn process_frame(
        endpoint: &mut dyn ProcessorOperations,
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
/// 提供了更好的类型安全性和更简洁的接口，同时通过 trait bounds 实现解耦。
/// 
/// This new trait design solves the complexity issues brought by separated traits,
/// providing better type safety and a cleaner interface, while achieving decoupling
/// through trait bounds.
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
    /// 
    /// 使用 EndpointOperations trait 对象实现解耦
    /// Uses EndpointOperations trait object to achieve decoupling
    async fn process_frame(
        endpoint: &mut dyn ProcessorOperations,
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



/// 动态帧处理器注册表
/// Dynamic frame processor registry
pub struct FrameProcessorRegistry<S: AsyncUdpSocket> {
    processors: Vec<Box<dyn DynamicFrameProcessor<S>>>,
}

/// 动态帧处理器特征，用于 trait 对象
/// Dynamic frame processor trait for trait objects
/// 
/// 支持运行时多态，使用 EndpointOperations trait 对象实现解耦
/// Supports runtime polymorphism and achieves decoupling using EndpointOperations trait objects
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
    /// 
    /// 使用 EndpointOperations trait 对象实现解耦
    /// Uses EndpointOperations trait object to achieve decoupling
    async fn process_frame(
        &self,
        endpoint: &mut dyn ProcessorOperations,
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
        endpoint: &mut dyn ProcessorOperations,
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
    /// 
    /// 使用 EndpointOperations trait 对象实现解耦
    /// Uses EndpointOperations trait object to achieve decoupling
    pub async fn route_frame(
        &self,
        endpoint: &mut dyn ProcessorOperations,
        frame: Frame,
        src_addr: SocketAddr,
        now: Instant,
    ) -> Result<()> {
        // 更新最后接收时间
        // Update last receive time
        endpoint.update_last_recv_time(now);

        // 检查路径迁移
        // Check for path migration
        endpoint.check_for_path_migration(src_addr).await?;

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
        let error_context = crate::error::ProcessorErrorContext::new(
            "FrameProcessorRegistry",
            endpoint.local_cid(),
            src_addr,
            format!("{:?}", endpoint.current_state()),
            now,
        );
        Err(crate::error::Error::FrameTypeMismatch {
            expected: "supported frame type".to_string(),
            actual: format!("{:?}", std::mem::discriminant(&frame)),
            context: error_context,
        })
    }
}

impl<S: AsyncUdpSocket> Default for FrameProcessorRegistry<S> {
    fn default() -> Self {
        Self::default_registry()
    }
}

/// 🚀 高性能静态分发帧处理器 - 零开销抽象
/// High-performance static dispatch frame processor - Zero-cost abstraction
/// 
/// 这个设计使用枚举+match模式替代dyn trait对象，实现：
/// 1. 零虚函数调用开销 - 编译器可以内联所有调用
/// 2. 零堆分配 - 所有处理器都是栈上的零大小类型
/// 3. 缓存友好 - 消除间接调用，改善分支预测
/// 4. 编译时优化 - 编译器可以进行更激进的优化
///
/// This design uses enum+match pattern instead of dyn trait objects to achieve:
/// 1. Zero virtual function call overhead - compiler can inline all calls
/// 2. Zero heap allocation - all processors are zero-sized stack types  
/// 3. Cache-friendly - eliminates indirect calls, improves branch prediction
/// 4. Compile-time optimization - enables more aggressive compiler optimizations
#[derive(Debug, Clone, Copy)]
pub enum ProcessorType {
    Push,
    Ack, 
    Connection,
    Path,
    Ping,
}

impl ProcessorType {
    /// 快速帧类型识别 - O(1)常量时间
    /// Fast frame type recognition - O(1) constant time
    #[inline]
    pub fn from_frame(frame: &Frame) -> Option<Self> {
        match frame {
            Frame::Push { .. } => Some(ProcessorType::Push),
            Frame::Ack { .. } => Some(ProcessorType::Ack),
            Frame::Syn { .. } | Frame::SynAck { .. } | Frame::Fin { .. } => Some(ProcessorType::Connection),
            Frame::PathChallenge { .. } | Frame::PathResponse { .. } => Some(ProcessorType::Path),
            Frame::Ping { .. } => Some(ProcessorType::Ping),
        }
    }
    
    /// 获取处理器名称
    /// Get processor name
    #[inline]
    pub fn name(self) -> &'static str {
        match self {
            ProcessorType::Push => "PushProcessor",
            ProcessorType::Ack => "AckProcessor", 
            ProcessorType::Connection => "ConnectionProcessor",
            ProcessorType::Path => "PathProcessor",
            ProcessorType::Ping => "PingProcessor",
        }
    }
}

/// 🚀 零开销帧处理器注册表
/// Zero-cost frame processor registry
pub struct StaticFrameProcessorRegistry;

impl StaticFrameProcessorRegistry {
    /// 高性能帧路由 - 静态分发，零虚函数调用开销
    /// High-performance frame routing - static dispatch, zero virtual call overhead
    #[inline]
    pub async fn route_frame<S: AsyncUdpSocket>(
        endpoint: &mut dyn ProcessorOperations,
        frame: Frame,
        src_addr: SocketAddr,
        now: Instant,
    ) -> Result<()> {
        // 更新最后接收时间 - 在所有处理器之前
        // Update last receive time - before all processors
        endpoint.update_last_recv_time(now);

        // 检查路径迁移 - 在所有处理器之前
        // Check for path migration - before all processors  
        endpoint.check_for_path_migration(src_addr).await?;

        // 🚀 静态分发 - 编译器可以内联所有调用
        // Static dispatch - compiler can inline all calls
        match ProcessorType::from_frame(&frame) {
            Some(ProcessorType::Push) => {
                tracing::trace!(
                    processor_name = "PushProcessor",
                    frame_type = ?frame,
                    "Routing frame to processor (static dispatch)"
                );
                <PushProcessor as UnifiedFrameProcessor<S>>::process_frame(endpoint, frame, src_addr, now).await
            }
            Some(ProcessorType::Ack) => {
                tracing::trace!(
                    processor_name = "AckProcessor", 
                    frame_type = ?frame,
                    "Routing frame to processor (static dispatch)"
                );
                <AckProcessor as UnifiedFrameProcessor<S>>::process_frame(endpoint, frame, src_addr, now).await
            }
            Some(ProcessorType::Connection) => {
                tracing::trace!(
                    processor_name = "ConnectionProcessor",
                    frame_type = ?frame, 
                    "Routing frame to processor (static dispatch)"
                );
                <ConnectionProcessor as UnifiedFrameProcessor<S>>::process_frame(endpoint, frame, src_addr, now).await
            }
            Some(ProcessorType::Path) => {
                tracing::trace!(
                    processor_name = "PathProcessor",
                    frame_type = ?frame,
                    "Routing frame to processor (static dispatch)"
                );
                <PathProcessor as UnifiedFrameProcessor<S>>::process_frame(endpoint, frame, src_addr, now).await
            }
            Some(ProcessorType::Ping) => {
                tracing::trace!(
                    processor_name = "PingProcessor", 
                    frame_type = ?frame,
                    "Routing frame to processor (static dispatch)"
                );
                // PING 帧处理逻辑 - 保持原有行为
                // PING frame handling logic - maintain original behavior
                tracing::trace!(
                    cid = endpoint.local_cid(),
                    "Received PING frame, no action needed"
                );
                Ok(())
            }
            None => {
                // 未知帧类型
                // Unknown frame type
                let error_context = crate::error::ProcessorErrorContext::new(
                    "StaticFrameProcessorRegistry",
                    endpoint.local_cid(),
                    src_addr,
                    format!("{:?}", endpoint.current_state()),
                    now,
                );
                Err(crate::error::Error::FrameTypeMismatch {
                    expected: "supported frame type".to_string(),
                    actual: format!("{:?}", std::mem::discriminant(&frame)),
                    context: error_context,
                })
            }
        }
    }
    
    /// 性能基准测试辅助方法
    /// Performance benchmarking helper method
    #[inline]
    pub fn processor_type_for_frame(frame: &Frame) -> Option<ProcessorType> {
        ProcessorType::from_frame(frame)
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
    pub fn new(
        endpoint: &dyn ProcessorOperations,
        src_addr: SocketAddr,
        now: Instant,
    ) -> Self {
        Self {
            now,
            src_addr,
            connection_state: endpoint.current_state().clone(),
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
        
        // 注意：这里使用一个占位符类型来满足泛型约束
        // Note: Using a placeholder type to satisfy generic constraints
        use crate::core::test_utils::MockUdpSocket;
        assert!(<PushProcessor as UnifiedFrameProcessor<MockUdpSocket>>::can_handle(&push_frame));
        assert!(!<AckProcessor as UnifiedFrameProcessor<MockUdpSocket>>::can_handle(&push_frame));
        assert!(!<ConnectionProcessor as UnifiedFrameProcessor<MockUdpSocket>>::can_handle(&push_frame));
        assert!(!<PathProcessor as UnifiedFrameProcessor<MockUdpSocket>>::can_handle(&push_frame));
    }
}