//! Endpoint 操作实现模块 - 实现抽象的端点操作接口
//! Endpoint Operations Implementation Module - Implements abstract endpoint operation interfaces
//!
//! 该模块为 Endpoint<S> 实现了 EndpointOperations 和 ProcessorOperations 特征，
//! 提供了处理器与端点之间的抽象接口，实现了解耦设计。
//!
//! This module implements EndpointOperations and ProcessorOperations traits for Endpoint<S>,
//! providing abstract interfaces between processors and endpoints to achieve decoupled design.

use crate::core::endpoint::Endpoint;
use crate::{
    core::{endpoint::{core::frame::create_path_response_frame, lifecycle::ConnectionLifecycleManager, processing::traits::{EndpointOperations, ProcessorOperations}, types::state::ConnectionState}, reliability::UnifiedReliabilityLayer},
    error::Result,
    packet::{frame::Frame, sack::SackRange},
    socket::{SocketActorCommand, Transport},
};
use async_trait::async_trait;
use bytes::Bytes;
use std::net::SocketAddr;
use tokio::{sync::mpsc, time::Instant};

/// 为 Endpoint<S> 实现 EndpointOperations trait
/// Implementation of EndpointOperations trait for Endpoint<S>
/// 
/// 这个实现将处理器需要的端点操作抽象出来，使处理器能够与 Endpoint 解耦。
/// This implementation abstracts the endpoint operations needed by processors,
/// allowing processors to decouple from the Endpoint.
#[async_trait]
impl<T: Transport> EndpointOperations for Endpoint<T> {
    // ========== 基础信息获取 (Basic Information Access) ==========
    
    fn local_cid(&self) -> u32 {
        self.identity.local_cid()
    }
    
    fn peer_cid(&self) -> u32 {
        self.identity.peer_cid()
    }
    
    fn remote_addr(&self) -> SocketAddr {
        self.identity.remote_addr()
    }
    
    fn start_time(&self) -> Instant {
        self.timing.start_time()
    }
    
    fn peer_recv_window(&self) -> u32 {
        self.transport.peer_recv_window()
    }
    
    fn set_peer_recv_window(&mut self, window_size: u32) {
        self.transport.set_peer_recv_window(window_size);
    }

    // ========== 生命周期管理 (Lifecycle Management) ==========
    
    fn current_state(&self) -> &ConnectionState {
        self.lifecycle_manager().current_state()
    }
    
    fn transition_state(&mut self, new_state: ConnectionState) -> Result<()> {
        self.lifecycle_manager_mut().transition_to(new_state)?;
        Ok(())
    }
    
    fn set_peer_cid(&mut self, peer_cid: u32) {
        self.identity.set_peer_cid(peer_cid);
    }
    
    fn set_remote_addr(&mut self, addr: SocketAddr) {
        self.identity.set_remote_addr(addr);
    }
    
    fn complete_path_validation(&mut self, success: bool) -> Result<()> {
        self.lifecycle_manager_mut().complete_path_validation(success)
    }

    // ========== 可靠性层操作 (Reliability Layer Operations) ==========
    
    fn unified_reliability(&self) -> &UnifiedReliabilityLayer {
        self.transport.unified_reliability()
    }
    
    fn unified_reliability_mut(&mut self) -> &mut UnifiedReliabilityLayer {
        self.transport.unified_reliability_mut()
    }
    
    fn is_send_buffer_empty(&self) -> bool {
        self.unified_reliability().is_send_buffer_empty()
    }

    // ========== 帧发送操作 (Frame Sending Operations) ==========
    
    async fn send_syn_ack_frame(&mut self) -> Result<()> {
        self.send_syn_ack().await
    }
    
    async fn send_standalone_ack_frame(&mut self) -> Result<()> {
        self.send_standalone_ack().await
    }
    
    async fn packetize_and_send_data(&mut self) -> Result<()> {
        self.packetize_and_send().await
    }
    
    async fn send_frame_list(&mut self, frames: Vec<Frame>) -> Result<()> {
        self.send_frames(frames).await
    }
    
    async fn send_frame_to_addr(&mut self, frame: Frame, addr: SocketAddr) -> Result<()> {
        self.send_frame_to(frame, addr).await
    }

    // ========== 通信管道操作 (Communication Channel Operations) ==========
    
    fn command_tx(&self) -> &mpsc::Sender<SocketActorCommand> {
        self.channels.command_tx()
    }

    // ========== 时间管理 (Time Management) ==========
    
    fn update_last_recv_time(&mut self, now: Instant) {
        self.timing.update_last_recv_time(now);
    }
    
    async fn cancel_connection_timeout(&mut self) {
        let cancelled = self.timing.cancel_timer(&crate::core::endpoint::timing::TimeoutEvent::ConnectionTimeout).await;
        tracing::debug!("Connection timeout timer cancelled: {}", cancelled);
    }

    // ========== 路径迁移操作 (Path Migration Operations) ==========
    
    async fn check_for_path_migration(&mut self, src_addr: SocketAddr) -> Result<()> {
        self.check_path_migration(src_addr).await
    }
}

/// 为 Endpoint<S> 实现 ProcessorOperations trait
/// Implementation of ProcessorOperations trait for Endpoint<S>
/// 
/// 这个实现提供了更高级的抽象接口，专门为帧处理器设计，
/// 组合了多个底层操作以提供更符合处理器使用模式的接口。
/// 
/// This implementation provides higher-level abstract interfaces specifically designed
/// for frame processors, combining multiple low-level operations to provide interfaces
/// that better match processor usage patterns.
#[async_trait]
impl<T: Transport> ProcessorOperations for Endpoint<T> {
    async fn process_ack_and_get_retx_frames(
        &mut self,
        recv_next_seq: u32,
        sack_ranges: Vec<(u32, u32)>,
        now: Instant,
    ) -> Result<Vec<Frame>> {
        // 使用 reliability layer 处理 ACK 并获取完整结果
        // Use reliability layer to process ACK and get comprehensive result
        let sack_ranges_converted: Vec<SackRange> = sack_ranges
            .into_iter()
            .map(|(start, end)| SackRange { start, end })
            .collect();
        let context = self.create_retransmission_context();
        
        // 使用新的综合ACK处理方法
        // Use new comprehensive ACK processing method
        let ack_result = self.unified_reliability_mut().handle_ack_comprehensive(
            recv_next_seq,
            sack_ranges_converted,
            now,
            &context,
        ).await;
        
        Ok(ack_result.frames_to_retransmit)
    }
    
    async fn receive_data_and_maybe_ack(
        &mut self,
        seq: u32,
        payload: Bytes,
    ) -> Result<()> {
        // 接收数据并检查是否需要立即发送 ACK
        // Receive data and check if immediate ACK is needed
        if self.unified_reliability_mut().receive_push(seq, payload) {
            self.send_standalone_ack().await?;
        }
        Ok(())
    }
    
    async fn receive_fin_and_ack(&mut self, seq: u32) -> Result<bool> {
        // 接收 FIN 并处理相应逻辑，返回是否成功接收了 FIN
        // Receive FIN and handle corresponding logic, return whether FIN was successfully received
        let fin_received = self.transport.unified_reliability_mut().receive_fin(seq);
        if fin_received {
            self.send_standalone_ack().await?;
        }
        Ok(fin_received)
    }
    
    async fn send_path_response(
        &mut self,
        echo_seq: u32,
        challenge_data: u64,
        target_addr: SocketAddr,
    ) -> Result<()> {
        // 创建并发送路径响应帧
        // Create and send path response frame
        let response_frame = create_path_response_frame(
            self.peer_cid(),
            echo_seq,
            self.start_time(),
            challenge_data,
        );
        
        self.send_frame_to(response_frame, target_addr).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::Config,
        core::{
            endpoint::types::state::ConnectionState,
            test_utils::MockTransport,
        }, socket::TransportCommand,
    };
    use tokio::{sync::mpsc, time::Instant};

    async fn create_test_endpoint() -> Endpoint<MockTransport> {
        let config = Config::default();
        let remote_addr = "127.0.0.1:8081".parse().unwrap();
        let local_cid = 1;
        
        // 创建必要的通道
        let (_frame_tx, frame_rx) = mpsc::channel(10);
        let (sender_tx, mut sender_rx) = mpsc::channel::<TransportCommand<MockTransport>>(10);
        let (command_tx, mut command_rx) = mpsc::channel(10);
        
        // 启动后台任务来处理发送命令，避免通道关闭
        tokio::spawn(async move {
            while let Some(_cmd) = sender_rx.recv().await {
                // 在测试中我们只是丢弃这些命令
                // In tests we just discard these commands
            }
        });
        
        // 启动后台任务来处理Socket命令
        tokio::spawn(async move {
            while let Some(_cmd) = command_rx.recv().await {
                // 在测试中我们只是丢弃这些命令
                // In tests we just discard these commands
            }
        });
        
        // 启动测试用全局定时器任务
        // Start global timer task for testing
        let timer_handle = crate::timer::start_hybrid_timer_task();

        // 使用正确的构造函数创建 Endpoint
        let (endpoint, _stream_tx, _stream_rx) = Endpoint::new_client(
            config,
            remote_addr,
            local_cid,
            frame_rx,
            sender_tx,
            command_tx,
            None, // 无初始数据
            timer_handle,
        ).await.unwrap();
            
        endpoint
    }

    #[tokio::test]
    async fn test_endpoint_operations_basic_info() {
        let endpoint = create_test_endpoint().await;
        
        // 测试基础信息获取
        assert_eq!(endpoint.local_cid(), 1);
        assert_eq!(endpoint.remote_addr().to_string(), "127.0.0.1:8081");
    }

    #[tokio::test]
    async fn test_endpoint_operations_state_management() {
        let mut endpoint = create_test_endpoint().await;
        
        // 测试当前状态
        let _initial_state = endpoint.current_state();
        
        // 测试状态转换
        let result = endpoint.transition_state(ConnectionState::Established);
        assert!(result.is_ok());
        
        assert_eq!(endpoint.current_state(), &ConnectionState::Established);
    }

    #[tokio::test]
    async fn test_endpoint_operations_reliability_layer() {
        let endpoint = create_test_endpoint().await;
        
        // 测试可靠性层访问
        let _reliability = endpoint.unified_reliability();
        assert!(endpoint.is_send_buffer_empty());
    }

    #[tokio::test]
    async fn test_processor_operations_ack_processing() {
        let mut endpoint = create_test_endpoint().await;
        
        // 测试 ACK 处理
        let result = endpoint.process_ack_and_get_retx_frames(
            1,
            vec![(1, 5), (10, 15)],
            Instant::now(),
        ).await;
        
        assert!(result.is_ok());
        let frames = result.unwrap();
        // 由于没有实际的发送缓冲区数据，应该返回空的重传帧列表
        assert!(frames.is_empty());
    }

    #[tokio::test]
    async fn test_processor_operations_fin_handling() {
        let mut endpoint = create_test_endpoint().await;
        
        // 测试 FIN 处理
        let result = endpoint.receive_fin_and_ack(10).await;
        assert!(result.is_ok());
        
        // FIN 接收应该成功
        let fin_received = result.unwrap();
        assert!(fin_received);
    }
} 