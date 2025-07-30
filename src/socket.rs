//! The socket-level API, including the main actor, commands, and handles.
pub mod event_loop;
pub mod command;
pub mod handle;
pub mod transport;

pub use command::SocketActorCommand;
pub use transport::TransportCommand;
pub use transport::{BindableTransport, Transport, UdpTransport};
pub use handle::{TransportListener, TransportReliableUdpSocket};

#[cfg(test)]
mod tests;
