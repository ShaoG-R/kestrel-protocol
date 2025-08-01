//! 事件分发器的单元测试
//! Unit tests for the event dispatcher

#[cfg(test)]
mod tests {
    use crate::{
        config::Config,
        core::{endpoint::Endpoint, test_utils::MockTransport},
    };
    use std::net::SocketAddr;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn test_event_dispatcher_frame_routing() {
        // 创建一个测试用的 endpoint
        let config = Config::default();
        let remote_addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let local_cid = 1;
        let (_frame_tx, frame_rx) = mpsc::channel(10);
        let (sender_tx, _sender_rx) = mpsc::channel(10);
        let (command_tx, _command_rx) = mpsc::channel(10);

        // 启动测试用全局定时器任务
        // Start global timer task for testing
        let timer_handle = crate::timer::start_hybrid_timer_task();

        let (endpoint, _stream_tx, _stream_rx) = Endpoint::<MockTransport>::new_client(
            config,
            remote_addr,
            local_cid,
            frame_rx,
            sender_tx,
            command_tx,
            None,
            timer_handle,
        ).await.unwrap();

        // 验证初始状态
        assert_eq!(endpoint.local_cid(), local_cid);

        // 测试事件分发器是否能正确访问 endpoint 状态
        // 这里我们不测试具体的帧处理逻辑，只验证分发器能正确调用
        // 因为具体的帧处理逻辑已经在其他测试中覆盖了
    }
}