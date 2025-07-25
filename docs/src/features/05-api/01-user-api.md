# 5.1: 用户接口 (Listener & Stream API)

**功能描述:**

协议为用户提供了两套高级、易于使用的API：一套是用于服务端的、类似于`TcpListener`的`bind`/`accept`模型；另一套是用于数据传输的、实现了标准`AsyncRead`和`AsyncWrite` trait的`Stream`接口。这使得用户可以像使用标准TCP一样轻松地使用本协议。

**实现位置:**

- **Listener API**: `src/socket/handle.rs` (`ReliableUdpSocket`, `ConnectionListener`)
- **Stream API**: `src/core/stream.rs` (`Stream`)

### 1. 服务端 Listener API

为了提供经典的服务器编程体验，协议封装了`SocketActor`的创建和管理过程。

- **`ReliableUdpSocket::bind(addr)`**: 这是API的入口点。调用它会：
    1.  在指定地址上绑定一个UDP Socket。
    2.  在后台`tokio::spawn`一个`SocketActor`任务和一个`SenderTask`任务。
    3.  返回两个句柄：`ReliableUdpSocket`用于发起新连接，`ConnectionListener`用于接收新连接。

- **`ConnectionListener::accept()`**:
  `ConnectionListener`持有一个MPSC通道的接收端。`SocketActor`在每次接受一个新连接（即收到一个`SYN`包）并为其创建好`Endpoint`任务和`Stream`句柄后，会将`Stream`句柄通过此通道发送过来。用户代码可以在一个循环中调用`.accept().await`来异步地、逐一地获取这些新建立的连接。

```rust,ignore
// 用户代码示例
let (socket_handle, mut listener) = ReliableUdpSocket::bind("127.0.0.1:1234").await?;

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
  `Stream`的`poll_read`实现了一个内部的`read_buffer`（一个`VecDeque<Bytes>`），用于平滑`Endpoint`批量发来的数据和用户可能的小批量读取之间的差异。
  1.  当用户调用`read`时，`poll_read`首先尝试从`read_buffer`中消耗数据。
  2.  如果`read_buffer`为空，它会`.await`从`Endpoint`接收数据的MPSC通道。
  3.  一旦收到新的一批数据块(`Vec<Bytes>`)，它会将其存入`read_buffer`，然后再次尝试从`read_buffer`中满足用户的读取请求。
  4.  当从`Endpoint`的通道关闭时（通常是因为收到了`FIN`），`poll_read`会返回`Ok(())`且不写入任何数据，这在`tokio`的`AsyncRead`中代表了EOF（文件结束符），用户的读取循环会自然终止。

这种设计使得用户可以使用标准的`tokio::io::copy`等工具函数，与`Stream`进行高效、便捷的数据交换。 