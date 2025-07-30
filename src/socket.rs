//! The socket-level API, including the main actor, commands, and handles.
pub mod actor;
pub mod command;
mod draining;
pub mod handle;
pub mod transport;
pub mod zerortt;

pub use command::{SocketActorCommand};
pub use transport::TransportCommand;
pub use transport::{BindableTransport, Transport, UdpTransport};
pub use handle::{TransportListener, TransportReliableUdpSocket};

#[cfg(test)]
mod tests;
