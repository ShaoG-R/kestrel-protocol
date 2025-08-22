//! The socket-level API, including the main actor, commands, and handles.
pub mod command;
pub mod event_loop;
pub mod handle;
pub mod transport;

pub use command::SocketActorCommand;
pub use handle::{TransportListener, TransportReliableUdpSocket};
pub use transport::TransportCommand;
pub use transport::{BindableTransport, Transport, UdpTransport};

#[cfg(test)]
mod tests;
