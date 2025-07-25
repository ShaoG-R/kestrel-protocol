# Summary

- [Introduction](./chapter_1.md)

# Features

- [1. 基础与工具](./features/01-foundations/README.md)
  - [1.1 标准化错误处理](./features/01-foundations/01-error-handling.md)
  - [1.2 统一的可配置化](./features/01-foundations/02-configuration.md)
  - [1.3 结构化日志记录](./features/01-foundations/03-logging.md)
  - [1.4 包的序列化与反序列化](./features/01-foundations/04-packet-serialization-deserialization.md)
- [2. 核心协议逻辑](./features/02-core-protocol/README.md)
  - [2.1 清晰的协议分层](./features/02-core-protocol/01-protocol-layering.md)
  - [2.2 协议版本协商](./features/02-core-protocol/02-version-negotiation.md)
  - [2.3 双向连接ID握手](./features/02-core-protocol/03-cid-handshake.md)
  - [2.4 0-RTT连接与四次挥手](./features/02-core-protocol/04-connection-lifecycle.md)
  - [2.5 动态RTO与重传机制](./features/02-core-protocol/05-retransmission.md)
  - [2.6 基于SACK的高效确认](./features/02-core-protocol/06-sack.md)
  - [2.7 滑动窗口流量控制](./features/02-core-protocol/07-flow-control.md)
  - [2.8 基于延迟的拥塞控制 (Vegas)](./features/02-core-protocol/08-congestion-control-vegas.md)
- [3. 并发与连接管理](./features/03-concurrency/README.md)
  - [3.1 基于MPSC的无锁并发模型](./features/03-concurrency/01-mpsc-concurrency-model.md)
  - [3.2 流迁移与NAT穿透](./features/03-concurrency/02-connection-migration.md)
- [4. 性能优化](./features/04-performance/README.md)
- [5. 用户接口](./features/05-api/README.md)
