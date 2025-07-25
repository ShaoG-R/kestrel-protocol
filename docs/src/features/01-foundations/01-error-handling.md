# 1.1: 标准化错误处理

**功能描述:**

项目采用 `thiserror` crate 构建了一个统一、全面且符合人体工程学的错误处理系统。所有库可能产生的错误都被归纳到一个顶层的 `enum Error` 中，为库的用户提供了清晰、一致的错误处理体验。

**实现位置:**

- **文件:** `src/error.rs`

### 1. 统一的 `Error` 枚举

`src/error.rs` 文件中定义了 `pub enum Error`，这是整个库唯一的错误类型。通过使用 `thiserror::Error` 派生宏，我们能够轻松地为每种错误变体附加详细的描述信息，并实现标准的 `std::error::Error` trait。

```rust
// 位于 src/error.rs
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    /// 发生了底层的I/O错误。
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// 接收到的包无效，无法解码。
    #[error("Invalid packet received")]
    InvalidPacket,

    /// 连接被对端关闭。
    #[error("Connection closed by peer")]
    ConnectionClosed,

    /// 尝试使用一个已经关闭或正在关闭的连接。
    #[error("Connection is closed or closing")]
    ConnectionAborted,
    
    // ... 其他错误变体
}
```

这种设计的好处是：
- **清晰性**: 用户只需匹配一个 `enum` 即可处理所有可能的失败情况。
- **一致性**: 所有公共API都返回 `Result<T, crate::Error>`，提供了统一的函数签名。
- **可扩展性**: 添加新的错误类型就像在 `enum` 中添加一个新的变体一样简单。

### 2. 与标准 `std::io::Error` 的无缝集成

为了让我们的错误类型能够更好地融入到 `tokio` 的生态和标准 I/O 操作中，我们为它实现了 `From<Error> for std::io::Error` 的转换。

```rust
// 位于 src/error.rs
impl From<Error> for std::io::Error {
    fn from(err: Error) -> Self {
        use std::io::ErrorKind;
        match err {
            Error::Io(e) => e,
            Error::ConnectionClosed => ErrorKind::ConnectionReset.into(),
            Error::ConnectionAborted => ErrorKind::ConnectionAborted.into(),
            Error::ConnectionTimeout => ErrorKind::TimedOut.into(),
            // ... 其他转换
        }
    }
}
```

这个实现至关重要，它允许我们的函数在需要返回 `std::io::Result<T>` 的地方（例如在实现 `AsyncRead` 或 `AsyncWrite` trait 时），能够通过 `?` 操作符自动将我们的 `crate::Error` 转换为 `std::io::Error`，极大地简化了代码。

### 3. 便捷的 `Result` 类型别名

为了方便起见，库还定义了一个类型别名 `pub type Result<T> = std::result::Result<T, Error>`。这使得在整个库中可以统一使用 `Result<T>`，而无需重复写出完整的错误类型。 