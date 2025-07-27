#![deny(clippy::expect_used, clippy::unwrap_used)]

//! The root of the reliable UDP protocol library.
//! 可靠UDP协议库的根。

pub mod config;
pub mod error;
pub mod packet;
pub mod socket;

pub mod congestion;
pub mod core;