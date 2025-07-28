//! Defines the command sent from the `Stream` handle to the `Endpoint` worker.
use crate::error::Result;
use bytes::Bytes;
use std::net::SocketAddr;
use tokio::sync::oneshot;

/// Commands sent from the `Stream` handle to the `Endpoint` worker.
#[derive(Debug)]
pub enum StreamCommand {
    SendData(Bytes),
    Close,
    Migrate {
        new_addr: SocketAddr,
        notifier: oneshot::Sender<Result<()>>,
    },
    #[cfg(test)]
    /// (For testing only) Manually set the peer's connection ID.
    UpdatePeerCid(u32),
} 