//! 包含协议的顶层Socket接口。
//! Contains the top-level Socket interface for the protocol.

mod actor;
mod command;
mod handle;
mod sender;
mod traits;
mod draining;

pub use command::{SocketActorCommand, SenderTaskCommand, SendCommand};
pub use handle::{Listener, ReliableUdpSocket};
pub use traits::{BindableUdpSocket, AsyncUdpSocket};

#[cfg(test)]
mod tests;
