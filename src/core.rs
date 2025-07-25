//! The new layered protocol core.
//! 新的分层协议核心。

pub mod endpoint;
pub mod reliability;
pub mod stream;

#[cfg(test)]
mod tests;
#[cfg(test)]
pub mod test_utils; 