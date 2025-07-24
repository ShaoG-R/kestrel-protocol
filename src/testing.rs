//! 测试辅助工具模块
//! Test utilities module

#![cfg(test)]

use crate::connection::{Connection, SendCommand, State};
use crate::packet::frame::Frame;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::time::{self, Duration, Instant, pause};

pub const TEST_CLIENT_ADDR: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345);
pub const TEST_SERVER_ADDR: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 54321);

/// A harness for testing a single `Connection`.
///
/// This sets up a connection and a mock "peer" that it talks to.
/// It provides methods to drive the connection's event loop and to
/// interact with it from the outside.
pub struct TestHarness {
    pub connection: Connection,
    // Channel to send frames to the connection (from the peer)
    tx_to_conn: Sender<Frame>,
    // Channel to receive commands from the connection (to the peer)
    rx_from_conn: Receiver<SendCommand>,
}

impl TestHarness {
    /// Creates a new harness for a connection in a given state.
    pub fn new_with_state(initial_state: State) -> Self {
        pause();

        let (tx_to_peer, rx_from_conn) = mpsc::channel::<SendCommand>(128);
        let (tx_to_conn, mut rx_from_peer) = mpsc::channel::<Frame>(128);

        let (connection, tx_for_main_loop) =
            Connection::new(TEST_SERVER_ADDR, 1, initial_state, tx_to_peer);

        // This task simulates the main socket demux loop, feeding frames
        // from the test directly to the connection's main loop via its channel.
        tokio::spawn(async move {
            while let Some(frame) = rx_from_peer.recv().await {
                if tx_for_main_loop.send(frame).await.is_err() {
                    break;
                }
            }
        });

        Self {
            connection,
            tx_to_conn,
            rx_from_conn,
        }
    }

    /// Ticks the connection's event loop forward.
    pub async fn tick(&mut self) {
        // We run the connection's run method in a timeout to prevent infinite loops
        // in tests if something goes wrong. A short tick should be enough for
        // the connection to process one event.
        let _ = time::timeout(Duration::from_millis(100), self.connection.run()).await;
    }

    /// Ticks the connection until a certain amount of virtual time has passed.
    pub async fn advance_time(&mut self, duration: Duration) {
        let start = Instant::now();
        while Instant::now() - start < duration {
            self.tick().await;
            // Yield to allow other tasks to run
            tokio::task::yield_now().await;
        }
    }

    /// Receives the next set of frames that the connection tried to send.
    pub async fn recv_from_connection(&mut self) -> Option<Vec<Frame>> {
        self.rx_from_conn.recv().await.map(|cmd| cmd.frames)
    }

    /// Sends a frame to the connection, as if it came from the peer.
    pub async fn send_to_connection(&mut self, frame: Frame) {
        self.tx_to_conn.send(frame).await.unwrap();
    }
} 