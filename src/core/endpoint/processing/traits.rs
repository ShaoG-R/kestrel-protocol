//! 端点操作特征 - 为帧处理器提供抽象接口
//! Endpoint Operations Trait - Provides abstract interfaces for frame processors
//!
//! 该模块定义了处理器与端点之间的抽象接口，使处理器能够与具体的
//! Endpoint<S> 实现解耦，提高代码的可测试性和可维护性。
//!
//! This module defines abstract interfaces between processors and endpoints,
//! allowing processors to decouple from concrete Endpoint<S> implementations,
//! improving code testability and maintainability.

use crate::{
    core::{
        endpoint::types::state::ConnectionState, reliability::UnifiedReliabilityLayer,
    }, error::Result, packet::frame::Frame, socket::SocketActorCommand
};
use std::net::SocketAddr;
use tokio::{sync::mpsc, time::Instant};
use async_trait::async_trait;

/// 端点操作特征 - 抽象处理器需要的所有端点操作
/// Endpoint Operations Trait - Abstracts all endpoint operations needed by processors
///
/// 该特征定义了帧处理器在处理各种帧类型时需要访问的端点操作。
/// 通过抽象这些操作，我们能够：
/// 1. 将处理器逻辑与具体的 Endpoint 实现解耦
/// 2. 简化单元测试（可以提供模拟实现）
/// 3. 提高代码的可维护性和模块化程度
///
/// This trait defines the endpoint operations that frame processors need to access
/// when handling various frame types. By abstracting these operations, we can:
/// 1. Decouple processor logic from concrete Endpoint implementations
/// 2. Simplify unit testing (can provide mock implementations)
/// 3. Improve code maintainability and modularity
#[async_trait]
pub trait EndpointOperations: Send {
    // ========== 基础信息获取 (Basic Information Access) ==========
    
    /// 获取本地连接ID
    /// Get local connection ID
    fn local_cid(&self) -> u32;
    
    /// 获取对端连接ID
    /// Get peer connection ID
    fn peer_cid(&self) -> u32;
    
    /// 获取远程地址
    /// Get remote address
    fn remote_addr(&self) -> SocketAddr;
    
    /// 获取连接开始时间
    /// Get connection start time
    fn start_time(&self) -> Instant;
    
    /// 获取对端接收窗口大小
    /// Get peer receive window size
    fn peer_recv_window(&self) -> u32;
    
    /// 设置对端接收窗口大小
    /// Set peer receive window size
    fn set_peer_recv_window(&mut self, window_size: u32);

    // ========== 生命周期管理 (Lifecycle Management) ==========
    
    /// 获取当前连接状态
    /// Get current connection state
    fn current_state(&self) -> &ConnectionState;
    
    /// 执行状态转换
    /// Perform state transition
    fn transition_state(&mut self, new_state: ConnectionState) -> Result<()>;
    
    /// 设置对端连接ID
    /// Set peer connection ID
    fn set_peer_cid(&mut self, peer_cid: u32);
    
    /// 设置远程地址
    /// Set remote address
    fn set_remote_addr(&mut self, addr: SocketAddr);
    
    /// 完成路径验证
    /// Complete path validation
    fn complete_path_validation(&mut self, success: bool) -> Result<()>;

    // ========== 可靠性层操作 (Reliability Layer Operations) ==========
    
    /// 获取可靠性层的只读引用
    /// Get read-only reference to reliability layer
    fn unified_reliability(&self) -> &UnifiedReliabilityLayer;
    
    /// 获取可靠性层的可变引用
    /// Get mutable reference to reliability layer
    fn unified_reliability_mut(&mut self) -> &mut UnifiedReliabilityLayer;
    
    /// 检查发送缓冲区是否为空
    /// Check if send buffer is empty
    fn is_send_buffer_empty(&self) -> bool;

    // ========== 帧发送操作 (Frame Sending Operations) ==========
    
    /// 发送SYN-ACK帧
    /// Send SYN-ACK frame
    async fn send_syn_ack_frame(&mut self) -> Result<()>;
    
    /// 发送独立的ACK帧
    /// Send standalone ACK frame
    async fn send_standalone_ack_frame(&mut self) -> Result<()>;
    
    /// 打包并发送数据
    /// Packetize and send data
    async fn packetize_and_send_data(&mut self) -> Result<()>;
    
    /// 发送多个帧
    /// Send multiple frames
    async fn send_frame_list(&mut self, frames: Vec<Frame>) -> Result<()>;
    
    /// 发送帧到指定地址
    /// Send frame to specific address
    async fn send_frame_to_addr(&mut self, frame: Frame, addr: SocketAddr) -> Result<()>;

    // ========== 通信管道操作 (Communication Channel Operations) ==========
    
    /// 获取与Socket的命令通道发送端
    /// Get command channel sender to Socket
    fn command_tx(&self) -> &mpsc::Sender<SocketActorCommand>;

    // ========== 时间管理 (Time Management) ==========
    
    /// 更新最后接收时间
    /// Update last receive time
    fn update_last_recv_time(&mut self, now: Instant);
    
    /// 取消连接超时定时器
    /// Cancel connection timeout timer
    async fn cancel_connection_timeout(&mut self);

    // ========== 路径迁移操作 (Path Migration Operations) ==========
    
    /// 检查路径迁移
    /// Check for path migration
    async fn check_for_path_migration(&mut self, src_addr: SocketAddr) -> Result<()>;
}

/// 为处理器提供的简化操作接口
/// Simplified operations interface for processors
///
/// 这个特征提供了一个更高级别的抽象，专门为帧处理器设计。
/// 它组合了多个底层操作，提供更符合处理器使用模式的接口。
///
/// This trait provides a higher-level abstraction specifically designed for frame processors.
/// It combines multiple low-level operations to provide interfaces that better match
/// processor usage patterns.
#[async_trait]
pub trait ProcessorOperations: EndpointOperations {
    /// 处理ACK信息并返回需要重传的帧
    /// Process ACK information and return frames that need retransmission
    async fn process_ack_and_get_retx_frames(
        &mut self,
        recv_next_seq: u32,
        sack_ranges: Vec<(u32, u32)>,
        now: Instant,
    ) -> Result<Vec<Frame>>;
    
    /// 接收数据并决定是否需要立即发送ACK
    /// Receive data and decide if immediate ACK is needed
    async fn receive_data_and_maybe_ack(
        &mut self,
        seq: u32,
        payload: bytes::Bytes,
    ) -> Result<()>;
    
    /// 接收FIN并处理相应逻辑
    /// Receive FIN and handle corresponding logic
    async fn receive_fin_and_ack(&mut self, seq: u32) -> Result<bool>;
    
    /// 发送路径响应
    /// Send path response
    async fn send_path_response(
        &mut self,
        echo_seq: u32,
        challenge_data: u64,
        target_addr: SocketAddr,
    ) -> Result<()>;
}

/// 处理器错误上下文创建器
/// Processor error context creator
pub trait ProcessorErrorContext {
    /// 创建处理器错误上下文
    /// Create processor error context
    fn create_processor_error_context(
        &self,
        processor_name: &'static str,
        src_addr: SocketAddr,
        now: Instant,
    ) -> crate::error::ProcessorErrorContext;
}

// 为任何实现了 EndpointOperations 的类型提供默认的错误上下文创建
// Provide default error context creation for any type implementing EndpointOperations
impl<T: EndpointOperations> ProcessorErrorContext for T {
    fn create_processor_error_context(
        &self,
        processor_name: &'static str,
        src_addr: SocketAddr,
        now: Instant,
    ) -> crate::error::ProcessorErrorContext {
        crate::error::ProcessorErrorContext::new(
            processor_name,
            self.local_cid(),
            src_addr,
            format!("{:?}", self.current_state()),
            now,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    /// 模拟端点操作实现，用于测试
    /// Mock endpoint operations implementation for testing
    pub struct MockEndpointOperations {
        pub local_cid: u32,
        pub peer_cid: u32,
        pub remote_addr: SocketAddr,
        pub start_time: Instant,
        pub peer_recv_window: u32,
        pub current_state: ConnectionState,
        pub sent_frames: Arc<Mutex<VecDeque<Frame>>>,
        pub unified_reliability: UnifiedReliabilityLayer,
        pub command_tx: mpsc::Sender<SocketActorCommand>,
    }

    impl MockEndpointOperations {
        pub async fn new() -> (Self, mpsc::Receiver<SocketActorCommand>) {
            let (command_tx, command_rx) = mpsc::channel(100);
            let mock = Self {
                local_cid: 1,
                peer_cid: 2,
                remote_addr: "127.0.0.1:8080".parse().unwrap(),
                start_time: Instant::now(),
                peer_recv_window: 1000,
                current_state: ConnectionState::Established,
                sent_frames: Arc::new(Mutex::new(VecDeque::new())),
                unified_reliability: {
                    let config = crate::config::Config::default();
                    let connection_id = 1; // Test connection ID
                    let timer_handle = crate::timer::start_hybrid_timer_task::<crate::core::endpoint::timing::TimeoutEvent, crate::timer::task::types::SenderCallback<crate::core::endpoint::timing::TimeoutEvent>>();
                    let timer_actor = crate::timer::start_sender_timer_actor(timer_handle, None);
                    let (tx_to_endpoint, _rx_from_stream) = tokio::sync::mpsc::channel(128);
                    UnifiedReliabilityLayer::new(connection_id, timer_actor, tx_to_endpoint, config)
                },
                command_tx,
            };
            (mock, command_rx)
        }
    }

    #[async_trait]
    impl EndpointOperations for MockEndpointOperations {
        fn local_cid(&self) -> u32 { self.local_cid }
        fn peer_cid(&self) -> u32 { self.peer_cid }
        fn remote_addr(&self) -> SocketAddr { self.remote_addr }
        fn start_time(&self) -> Instant { self.start_time }
        fn peer_recv_window(&self) -> u32 { self.peer_recv_window }
        fn set_peer_recv_window(&mut self, window_size: u32) {
            self.peer_recv_window = window_size;
        }
        
        fn current_state(&self) -> &ConnectionState { &self.current_state }
        fn transition_state(&mut self, new_state: ConnectionState) -> Result<()> {
            self.current_state = new_state;
            Ok(())
        }
        fn set_peer_cid(&mut self, peer_cid: u32) { self.peer_cid = peer_cid; }
        fn set_remote_addr(&mut self, addr: SocketAddr) { self.remote_addr = addr; }
        fn complete_path_validation(&mut self, _success: bool) -> Result<()> { Ok(()) }
        
        fn unified_reliability(&self) -> &UnifiedReliabilityLayer { &self.unified_reliability }
        fn unified_reliability_mut(&mut self) -> &mut UnifiedReliabilityLayer { &mut self.unified_reliability }
        fn is_send_buffer_empty(&self) -> bool { true }
        
        async fn send_syn_ack_frame(&mut self) -> Result<()> { Ok(()) }
        async fn send_standalone_ack_frame(&mut self) -> Result<()> { Ok(()) }
        async fn packetize_and_send_data(&mut self) -> Result<()> { Ok(()) }
        async fn send_frame_list(&mut self, frames: Vec<Frame>) -> Result<()> {
            let mut sent = self.sent_frames.lock().await;
            for frame in frames {
                sent.push_back(frame);
            }
            Ok(())
        }
        async fn send_frame_to_addr(&mut self, frame: Frame, _addr: SocketAddr) -> Result<()> {
            let mut sent = self.sent_frames.lock().await;
            sent.push_back(frame);
            Ok(())
        }
        
        fn command_tx(&self) -> &mpsc::Sender<SocketActorCommand> { &self.command_tx }
        fn update_last_recv_time(&mut self, _now: Instant) {}
        async fn check_for_path_migration(&mut self, _src_addr: SocketAddr) -> Result<()> { Ok(()) }
        async fn cancel_connection_timeout(&mut self) {}
    }

    #[tokio::test]
    async fn test_mock_endpoint_operations() {
        let (mut mock, _rx) = MockEndpointOperations::new().await;
        
        // 测试基础操作
        assert_eq!(mock.local_cid(), 1);
        assert_eq!(mock.peer_cid(), 2);
        
        // 测试状态转换
        mock.transition_state(ConnectionState::FinWait).unwrap();
        assert!(matches!(mock.current_state(), ConnectionState::FinWait));
        
        // 测试错误上下文创建
        let context = mock.create_processor_error_context(
            "TestProcessor",
            "127.0.0.1:9999".parse().unwrap(),
            Instant::now(),
        );
        assert_eq!(context.processor_name, "TestProcessor");
        assert_eq!(context.connection_id, 1);
    }
}
