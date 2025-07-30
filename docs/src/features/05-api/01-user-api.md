# 1: 用户接口 (Listener & Stream API)

**功能描述:**

协议为用户提供了两套高级、易于使用的API：一套是用于服务端的、类似于`TcpListener`的`bind`/`accept`模型；另一套是用于数据传输的、实现了标准`AsyncRead`和`AsyncWrite` trait的`Stream`接口。这使得用户可以像使用标准TCP一样轻松地使用本协议。

**实现位置:**

- **Listener API**: `src/socket/handle.rs` (`TransportReliableUdpSocket`, `TransportListener`)
- **Stream API**: `src/core/stream.rs` (`Stream`)

### 1. 服务端 Listener API

为了提供经典的服务器编程体验，协议封装了`SocketEventLoop`的创建和管理过程。

- **`TransportReliableUdpSocket::bind(addr)`**: 这是API的入口点。调用它会：
    1.  在指定地址上绑定一个UDP Socket。
    2.  在后台`tokio::spawn`一个`SocketEventLoop`任务和一个`transport_sender_task`任务。
    3.  返回两个句柄：`TransportReliableUdpSocket`用于发起新连接，`TransportListener`用于接收新连接。

- **`TransportListener::accept()`**:
  `TransportListener`持有一个MPSC通道的接收端。`SocketEventLoop`在每次接受一个新连接（即收到一个`SYN`包）并为其创建好`Endpoint`任务和`Stream`句柄后，会将`(Stream, SocketAddr)`通过此通道发送过来。用户代码可以在一个循环中调用`.accept().await`来异步地、逐一地获取这些新建立的连接。

```rust,ignore
// 用户代码示例
let (socket_handle, mut listener) = TransportReliableUdpSocket::bind("127.0.0.1:1234").await?;

loop {
    let (stream, remote_addr) = listener.accept().await?;
    tokio::spawn(async move {
        // ... 处理这个新的stream ...
    });
}
```

### 2. Stream API (`AsyncRead` / `AsyncWrite`)

`Stream`是用户与单个可靠连接交互的唯一途径。它抽象了所有底层的包、ACK、重传等复杂性，提供了一个标准的字节流接口。

- **`AsyncWrite`**:
  `Stream`的`poll_write`实现非常轻量。它只是将用户提供的数据封装成一个`StreamCommand::SendData`命令，并通过MPSC通道`try_send`给对应的`Endpoint`任务。如果通道已满，`try_send`会失败并返回`Poll::Pending`，这自然地实现了**背压（Backpressure）**，防止用户写入速度过快导致内存无限增长。

- **`AsyncRead`**:
  `Stream`的`poll_read`实现了一个内部的`read_buffer`（一个`VecDeque<Bytes>`），用于平滑`Endpoint`批量发来的数据和用户可能的小批量读取之间的差异。其核心逻辑在一个循环中运行，以确保在有数据可读时能立即提供给用户：
  1.  **优先消耗内部缓冲区**: `poll_read`首先尝试从`read_buffer`中拷贝数据到用户提供的缓冲区。根据`AsyncRead`的契约，只要有**任何**数据被成功拷贝，函数就必须立即返回`Poll::Ready(Ok(()))`，即使`read_buffer`中的数据不足以填满用户缓冲区。这可以防止在已经读取部分数据后错误地返回`Poll::Pending`。
  2.  **拉取新数据**: 仅当`read_buffer`为空，并且上一步没有拷贝任何数据时，`poll_read`才会尝试从`Endpoint`的任务通道中`poll_recv`新数据。
  3.  **处理新数据**: 如果从通道成功接收到一批新的数据块(`Vec<Bytes>`)，它们会被追加到`read_buffer`的末尾。然后，`poll_read`的循环会**继续**（`continue`），立即回到第一步，尝试从现在非空的`read_buffer`中满足用户的读取请求。
  4.  **处理悬挂/结束**:
      *   如果通道暂时没有新数据（返回`Poll::Pending`），并且`read_buffer`也为空，`poll_read`则返回`Poll::Pending`，并将`Waker`注册，以便在数据到达时唤醒任务。
      *   如果通道被关闭（`Endpoint`任务已终止，通常是因为连接正常关闭），并且`read_buffer`也已耗尽，`poll_read`会返回`Ok(())`且不写入任何数据，这在`tokio`的`AsyncRead`中代表了EOF（流结束），用户的读取循环会自然终止。

这种设计使得用户可以使用标准的`tokio::io::copy`等工具函数，与`Stream`进行高效、便捷的数据交换。