# 4. 包的序列化与反序列化

**功能描述:**

协议能够在内存中的数据结构 (`Frame` 枚举) 与网络中传输的字节流之间进行高效、准确的双向转换。这是协议所有通信的基础。该过程遵循 `dev-principal.md` 中定义的头部结构，区分**长头部**（用于连接管理）和**短头部**（用于数据传输），以优化不同场景下的网络开销。

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

### 2. 序列化与反序列化入口

- **文件:** `src/packet/frame.rs`
- **关键方法:**
    - `Frame::encode(&self, buf: &mut B)`
    - `Frame::decode(buf: &[u8]) -> Option<Self>`

这是功能的总入口。

- **`encode`**: `encode` 方法根据 `Frame` 的不同变体，调用对应的头部（`LongHeader` 或 `ShortHeader`）的 `encode` 方法，然后将载荷（payload）写入缓冲区。
- **`decode`**: `decode` 过程更加智能。它首先会窥探缓冲区的第一个字节来解析出 `Command` 类型。根据 `Command` 指示，它会判断这是一个长头部还是短头部的包，然后调用相应的头部 `decode` 方法进行解析，最后将剩余的字节作为载荷。

```rust
// 位于 src/packet/frame.rs
impl Frame {
    // ...

    /// 从缓冲区解码一个完整的帧。
    pub fn decode(mut buf: &[u8]) -> Option<Self> {
        if buf.is_empty() {
            return None;
        }

        // 偷窥第一个字节来决定是长头还是短头
        let command = command::Command::from_u8(buf[0])?;

        if command.is_long_header() {
            // ... 解析长头 ...
        } else {
            // ... 解析短头 ...
        }
    }

    /// 将帧编码到缓冲区。
    pub fn encode<B: BufMut>(&self, buf: &mut B) {
        match self {
            // ... 匹配并编码 ...
        }
    }
    
    // ...
}
```

### 3. 头部处理

- **文件:** `src/packet/header.rs`

此文件定义了 `LongHeader` 和 `ShortHeader`，以及它们各自的序列化/反序列化逻辑。这遵循了QUIC的设计思想，为不同目的的包设计了不同开销的头部。

- **`LongHeader`**: 用于 `SYN` 和 `SYN-ACK` 包，包含协议版本等额外信息。
- **`ShortHeader`**: 用于常规数据传输，如 `PUSH`, `ACK` 等，头部更小，效率更高。

### 4. 命令定义

- **文件:** `src/packet/command.rs`

定义了 `Command` 枚举，它是每个数据包的第一个字节，明确了包的类型和意图。

### 5. SACK信息处理

- **文件:** `src/packet/sack.rs`

当一个 `Frame::Ack` 包被编码或解码时，其载荷部分的SACK范围信息由该模块中的 `encode_sack_ranges` 和 `decode_sack_ranges` 函数处理。

### 6. 测试验证

- **文件:** `src/packet/tests.rs`

提供了一系列单元测试，通过 "roundtrip"（编码后立即解码，再进行对比）的方式，验证了所有 `Frame` 变体的序列化和反序列化逻辑的正确性。

```rust
// 位于 src/packet/tests.rs

fn frame_roundtrip_test(frame: Frame) {
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let decoded_frame = Frame::decode(&buf).expect("decode should succeed");
    assert_eq!(frame, decoded_frame);
}

#[test]
fn test_push_frame_roundtrip() {
    // ...
    frame_roundtrip_test(frame);
}
``` 