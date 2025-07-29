//! The socket-level API, including the main actor, commands, and handles.
pub mod actor;
pub mod command;
mod draining;
pub mod handle;
pub mod traits;
pub mod transport;
pub mod zerortt;

pub use command::{SocketActorCommand, SenderTaskCommand, SendCommand};
pub use traits::{BindableUdpSocket, AsyncUdpSocket};
pub use transport::{BindableTransport, Transport, UdpTransport};
pub use handle::{TransportListener, TransportReliableUdpSocket};

#[cfg(test)]
mod tests;
