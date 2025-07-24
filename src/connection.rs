//! 定义了单个可靠连接。
//! Defines a single reliable connection.

use crate::packet::frame::Frame;
use std::net::SocketAddr;
use tokio::sync::mpsc;

/// Represents a single reliable connection.
///
/// Each connection is managed by its own asynchronous task.
///
/// 代表一个单一的可靠连接。
///
/// 每个连接都由其自己的异步任务管理。
pub struct Connection {
    /// The remote address of the connection.
    /// 连接的远程地址。
    remote_addr: SocketAddr,

    /// Receives frames from the main socket task.
    /// 从主套接字任务接收帧。
    receiver: mpsc::Receiver<Frame>,
}

impl Connection {
    /// Creates a new `Connection`.
    ///
    /// This also returns a sender for the main socket task to send frames to this connection.
    ///
    /// 创建一个新的 `Connection`。
    ///
    /// 这也会返回一个发送端，供主套接字任务向此连接发送帧。
    pub fn new(remote_addr: SocketAddr) -> (Self, mpsc::Sender<Frame>) {
        // We can tune the channel size later.
        // 我们之后可以调整通道的大小。
        let (sender, receiver) = mpsc::channel(128);
        let connection = Self {
            remote_addr,
            receiver,
        };
        (connection, sender)
    }

    /// Runs the connection's main loop to process incoming frames.
    ///
    /// 运行连接的主循环以处理传入的帧。
    pub async fn run(&mut self) {
        while let Some(frame) = self.receiver.recv().await {
            // TODO: Process the frame according to the protocol logic.
            // 根据协议逻辑处理帧。
            println!(
                "Connection to {} received frame: {:?}",
                self.remote_addr, frame
            );
        }

        // When the channel is closed, the loop exits and the connection task ends.
        // 当通道关闭时，循环退出，连接任务结束。
        println!("Connection to {} closed.", self.remote_addr);
    }
}
