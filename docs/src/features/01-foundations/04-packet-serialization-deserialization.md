# 4. 包的序列化与反序列化

**功能描述:**

协议能够在内存中的数据结构 (`Frame` 枚举) 与网络中传输的字节流之间进行高效、准确的双向转换。这是协议所有通信的基础。

为实现可靠的包粘连（coalescing），协议为所有头部（包括长头和短头）都增加了一个 `payload_length` 字段。该字段明确定义了每个帧载荷的确切长度，使得解码器能够准确地从一个UDP数据报中分离出多个帧，即使其中包含了像 `PUSH` 或 `ACK` 这样载荷长度可变的帧。

**实现位置:**

整个序列化/反序列化逻辑的核心代码都位于 `src/packet/` 模块中。

### 1. 核心数据结构: `Frame` 枚举

- **文件:** `src/packet/frame.rs`

`Frame` 枚举是所有网络数据包的统一抽象。它定义了协议所支持的全部帧类型，例如：

- `Frame::Push`: 携带应用数据的帧。
- `Frame::Ack`: 携带选择性确认（SACK）信息的确认帧。
- `Frame::Syn` / `Frame::SynAck`: 用于连接建立的帧。
- `Frame::Fin`: 用于连接关闭的帧。
- `Frame::PathChallenge` / `Frame::PathResponse`: 用于连接迁移的路径验证帧。

### 2. 安全的帧构造: `Frame::new_*`

- **文件:** `src/packet/frame.rs`

为了从根本上避免因 `payload_length` 设置错误而导致的协议问题，我们为 `Frame` 实现了一系列安全的构造函数 (`new_*` 方法)。**这是创建帧的首选方式。**

这些构造函数会自动处理 `payload_length` 的计算和设置，确保了帧的内部一致性。

```rust
// 位于 src/packet/frame.rs
impl Frame {
    // ...

    /// 创建一个新的 PUSH 帧。
    pub fn new_push(
        peer_cid: u32,
        sequence_number: u32,
        recv_next_sequence: u32,
        recv_window_size: u16,
        timestamp: u32,
        payload: Bytes,
    ) -> Self {
        let header = ShortHeader {
            command: command::Command::Push,
            connection_id: peer_cid,
            payload_length: payload.len() as u16, // 自动计算和设置
            recv_window_size,
            timestamp,
            sequence_number,
            recv_next_sequence,
        };
        Frame::Push { header, payload }
    }

    // ... 其他 new_* 构造函数
}
```

### 3. 序列化与反序列化

- **文件:** `src/packet/frame.rs`
- **关键方法:**
    - `Frame::encode(&self, buf: &mut B)`
    - `Frame::decode(buf: &mut &[u8]) -> Option<Self>`

这些是帧与字节流之间转换的底层接口。

- **`encode`**: `encode` 方法根据 `Frame` 的不同变体，调用对应的头部（`LongHeader` 或 `ShortHeader`）的 `encode` 方法，然后将载荷（payload）写入缓冲区。
- **`decode`**: `decode` 过程利用了 `payload_length` 字段来精确解析。它首先窥探第一个字节来确定 `Command`，然后解码完整的头部以获得 `payload_length`。根据这个长度，它精确地读取载荷，并将缓冲区的指针前进到下一个帧的起始位置，从而能够正确地处理粘包。

### 4. 头部处理

- **文件:** `src/packet/header.rs`

此文件定义了 `LongHeader` 和 `ShortHeader`。每个头部都包含一个 `payload_length: u16` 字段，用于指明紧跟其后的载荷字节数。

- **`LongHeader`**: 用于 `SYN` 和 `SYN-ACK` 包，包含协议版本、`payload_length` 和完整的连接ID。
- **`ShortHeader`**: 用于常规数据传输，如 `PUSH`, `ACK` 等，包含 `payload_length`，头部相对更小。

### 5. SACK信息处理

- **文件:** `src/packet/sack.rs`

`ACK` 帧的载荷是SACK范围信息。`Frame::new_ack` 构造函数封装了其创建逻辑，它会调用 `sack::encode_sack_ranges` 函数来生成 `ACK` 帧的载荷，并自动设置正确的 `payload_length`。

### 6. 测试验证

- **文件:** `src/packet/tests.rs`

提供了一系列单元测试，验证所有 `Frame` 变体的序列化和反序列化逻辑。
- **往返测试 (Roundtrip Test):** 通过 "编码 -> 解码 -> 对比" 的方式确保每个帧都能被正确处理。
- **粘包测试 (Coalescing Test):** 存在名为 `test_coalesced_frames_decode` 的专门测试，用于验证将多个帧（如一个`PUSH`和一个`FIN`）编码进同一个缓冲区后，依然能够被正确地逐个解码出来。

```rust
// 位于 src/packet/tests.rs

#[test]
fn test_coalesced_frames_decode() {
    // 1. 使用安全的构造函数创建帧
    let push_frame = Frame::new_push(123, 1, 0, 1024, 1, Bytes::from_static(b"push data"));
    let fin_frame = Frame::new_fin(123, 2, 2, 0, 1024);

    // 2. Encode both frames into a single buffer.
    let mut buf = BytesMut::new();
    push_frame.encode(&mut buf);
    fin_frame.encode(&mut buf);

    // 3. Decode the frames from the buffer.
    let mut cursor = &buf[..];
    let decoded_push = Frame::decode(&mut cursor).expect("Should decode PUSH frame");
    let decoded_fin = Frame::decode(&mut cursor).expect("Should decode FIN frame");

    // 4. Assert that the buffer is fully consumed and frames are correct.
    assert!(cursor.is_empty(), "Buffer should be fully consumed");
    assert_eq!(push_frame, decoded_push);
    assert_eq!(fin_frame, decoded_fin);
}
``` 