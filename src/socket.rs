//! The socket-level API, including the main actor, commands, and handles.
pub mod actor;
pub mod command;
mod draining;
pub mod handle;
mod sender;
pub mod traits;
pub mod zerortt;

pub use command::{SocketActorCommand, SenderTaskCommand, SendCommand};
pub use handle::{Listener, ReliableUdpSocket};
pub use traits::{BindableUdpSocket, AsyncUdpSocket};

#[cfg(test)]
mod tests;
