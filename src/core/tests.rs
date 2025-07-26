//! Integration-style tests for the `Endpoint` worker, using a simulated network.

// The test utils are used by all test modules.

// Individual test modules
mod concurrency;
mod handshake;
mod lifecycle;
pub mod reliability;
pub mod shutdown;
pub mod state;
pub mod retransmission;