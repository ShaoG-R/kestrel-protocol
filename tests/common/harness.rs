//! tests/common/harness.rs
use kestrel_protocol::socket::{Listener, ReliableUdpSocket};
use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicU16, Ordering},
    Arc, Once,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UdpSocket;
use tracing_subscriber::fmt::format::FmtSpan;

/// Initializes tracing for tests, ensuring it's only done once.
pub fn init_tracing() {
    static TRACING_INIT: Once = Once::new();
    TRACING_INIT.call_once(|| {
        let filter = std::env::var("RUST_LOG")
            .unwrap_or_else(|_| "kestrel_protocol=debug,lifecycle=info".to_string());
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_span_events(FmtSpan::FULL)
            .with_test_writer()
            .init();
    });
}

// Use a global atomic to assign a new port for each server harness.
// This avoids port conflicts when running tests in parallel.
static NEXT_SERVER_PORT: AtomicU16 = AtomicU16::new(40000);

/// A test harness to simplify setting up client-server integration tests.
pub struct TestHarness {
    pub server_addr: SocketAddr,
    pub server_listener: Listener,
    _server_socket: Arc<ReliableUdpSocket<UdpSocket>>, // Keep the socket alive
}

impl TestHarness {
    /// Creates a new server that listens on a unique, non-ephemeral port.
    pub async fn new() -> Self {
        init_tracing();
        let port = NEXT_SERVER_PORT.fetch_add(1, Ordering::SeqCst);
        let server_addr: SocketAddr = format!("127.0.0.1:{}", port).parse().unwrap();

        let (server_socket, server_listener) =
            ReliableUdpSocket::<UdpSocket>::bind(server_addr)
                .await
                .unwrap();

        let _server_socket = Arc::new(server_socket);
        Self {
            server_addr,
            server_listener,
            _server_socket,
        }
    }

    /// Creates a new client socket bound to a random available port.
    pub async fn create_client() -> Arc<ReliableUdpSocket<UdpSocket>> {
        // Clients can still use ephemeral ports as they don't need to be addressed directly.
        let client_addr = "127.0.0.1:0".parse().unwrap();
        let (client_socket, _) = ReliableUdpSocket::<UdpSocket>::bind(client_addr)
            .await
            .unwrap();
        Arc::new(client_socket)
    }
}

/// A simple echo server handler for testing.
pub async fn echo_server_handler(
    stream: kestrel_protocol::core::stream::Stream,
    remote_addr: SocketAddr,
    id: usize,
) {
    let (mut reader, mut writer) = tokio::io::split(stream);
    let mut buf = Vec::new();

    // Trigger SYN-ACK
    writer
        .write_all(b"ECHO_READY")
        .await
        .expect("Server handler failed to write ready message");

    // Read all data from the client until they send FIN
    reader
        .read_to_end(&mut buf)
        .await
        .expect("Failed to read client message");
    tracing::info!(
        "[Server Handler {} for {}] Read {} bytes.",
        id,
        remote_addr,
        buf.len()
    );

    // Echo the data back
    writer
        .write_all(&buf)
        .await
        .expect("Server handler failed to echo data");

    // Shutdown
    writer
        .shutdown()
        .await
        .expect("Server handler failed to shutdown");
    tracing::info!("[Server Handler {} for {}] Finished.", id, remote_addr);
}

/// A generic client handler for sending data and verifying an echo response.
pub async fn client_echo_task(
    client_socket: Arc<ReliableUdpSocket<UdpSocket>>,
    server_addr: SocketAddr,
    payload: Vec<u8>,
    id: usize,
) {
    tracing::info!("[Client {}] Connecting to server...", id);
    let stream = client_socket
        .connect(server_addr)
        .await
        .expect("Client connection failed");
    let (mut reader, mut writer) = tokio::io::split(stream);

    // Wait for the server's ready signal
    let mut ready_buf = [0; 10];
    reader
        .read_exact(&mut ready_buf)
        .await
        .expect("Client failed to read ready message");
    assert_eq!(&ready_buf, b"ECHO_READY");
    tracing::info!("[Client {}] Server is ready.", id);

    // Send the payload
    writer
        .write_all(&payload)
        .await
        .expect("Client failed to write payload");
    tracing::info!("[Client {}] Sent {} bytes.", id, payload.len());

    // Shutdown writer to send FIN
    writer
        .shutdown()
        .await
        .expect("Client failed to shutdown writer");
    tracing::info!("[Client {}] Writer shut down.", id);

    // Read the echo response from the server
    let mut response_buf = Vec::new();
    reader
        .read_to_end(&mut response_buf)
        .await
        .expect("Failed to read echo response");
    assert_eq!(response_buf, payload);
    tracing::info!(
        "[Client {}] Received correct echo response and connection closed.",
        id
    );
} 