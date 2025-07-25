# 2.2: 协议版本协商

**功能描述:**

为了确保通信双方能够理解彼此，协议在连接握手的第一个包中内置了简单的版本协商机制。这可以防止因协议迭代导致的不兼容客户端与服务器进行通信，避免了后续可能出现的未知错误。

**实现位置:**

- **版本号定义**:
    - `src/packet/header.rs`: 在 `LongHeader` 中定义了 `protocol_version: u8` 字段。
    - `src/config.rs`: 在 `Config` 结构体中定义了本地期望的 `protocol_version: u8`。
- **协商逻辑**:
    - `src/socket/actor.rs`: 在 `SocketActor::dispatch_frame` 方法中。

### 协商流程

1.  **客户端发起**: 客户端在构建第一个 `SYN` 包时，会将自己实现的协议版本号（从 `Config` 中获取）填入 `LongHeader` 的 `protocol_version` 字段。

2.  **服务器验证**: 服务器端的 `SocketActor` 在收到一个 `SYN` 包时（这是唯一使用 `LongHeader` 的客户端初始包），会执行以下检查：

    ```rust
    // 位于 src/socket/actor.rs - dispatch_frame 方法
    if let Frame::Syn { header, .. } = &frame {
        let config = Config::default(); // 获取服务器配置
        if header.protocol_version != config.protocol_version {
            warn!(
                addr = %remote_addr,
                client_version = header.protocol_version,
                server_version = config.protocol_version,
                "Dropping SYN with incompatible protocol version."
            );
            return; // 版本不匹配，直接丢弃包，不进行任何回复
        }
        // ... 版本匹配，继续创建连接 ...
    }
    ```

3.  **结果**:
    - **成功**: 如果版本号匹配，服务器会继续处理 `SYN` 包，开始创建新连接。
    - **失败**: 如果版本号不匹配，服务器会记录一条警告日志并**静默地丢弃**该数据包。客户端因为收不到任何回复（`SYN-ACK`），会在尝试几次后因超时而失败。这种静默丢弃的方式可以有效防止潜在的放大攻击。 