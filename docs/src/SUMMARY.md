# Summary

- [项目介绍](./Introduction.md)

# 项目指南

- [项目现状分析报告](./project-guidance/01-status-report.md)
- [AI开发指南](./project-guidance/02-ai-dev-guide.md)

# Features

- [基础与工具](./features/01-foundations/README.md)
    - [标准化错误处理](./features/01-foundations/01-error-handling.md)
    - [统一的可配置化](./features/01-foundations/02-configuration.md)
    - [结构化日志记录](./features/01-foundations/03-logging.md)
    - [包的序列化与反序列化](./features/01-foundations/04-packet-serialization-deserialization.md)
- [核心协议逻辑](./features/02-core-protocol/README.md)
    - [清晰的协议分层](./features/02-core-protocol/01-protocol-layering.md)
    - [协议版本协商](./features/02-core-protocol/02-version-negotiation.md)
    - [双向连接ID握手](./features/02-core-protocol/03-cid-handshake.md)
    - [0-RTT连接与四次挥手](./features/02-core-protocol/04-connection-lifecycle.md)
    - [动态RTO与重传机制](./features/02-core-protocol/05-retransmission.md)
    - [基于SACK的高效确认](./features/02-core-protocol/06-sack.md)
    - [滑动窗口流量控制](./features/02-core-protocol/07-flow-control.md)
    - [基于延迟的拥塞控制 (Vegas)](./features/02-core-protocol/08-congestion-control-vegas.md)
    - [拥塞控制之慢启动](./features/02-core-protocol/09-slow-start.md)
- [并发与连接管理](./features/03-concurrency/README.md)
    - [基于MPSC的无锁并发模型](./features/03-concurrency/01-mpsc-concurrency-model.md)
    - [流迁移与NAT穿透](./features/03-concurrency/02-connection-migration.md)
- [性能优化](./features/04-performance/README.md)
    - [包聚合与快速应答](./features/04-performance/01-packet-optimization.md)
    - [批处理与内存优化](./features/04-performance/02-batching-and-memory.md)
- [用户接口](./features/05-api/README.md)
    - [Listener & Stream API](./features/05-api/01-user-api.md)


# 开发文档

- [开发文档](./dev-docs/README.md)
    - [数据流到数据包转换](./dev-docs/01-stream-to-packet.md)
    - [连接关闭机制](./dev-docs/02-connection-shutdown.md)
    - [组件文档](./dev-docs/components/README.md)
        - [传输层架构设计](./dev-docs/components/transport-layer.md)
        - [Socket层架构设计](./dev-docs/components/socket-layer.md)
        - [Endpoint层架构设计](./dev-docs/components/endpoint/README.md)
            - [核心 (`core`)](./dev-docs/components/endpoint/core.md)
            - [生命周期 (`lifecycle`)](./dev-docs/components/endpoint/lifecycle.md)
            - [事件处理 (`processing`)](./dev-docs/components/endpoint/processing.md)
            - [时间管理 (`timing`)](./dev-docs/components/endpoint/timing.md)
            - [类型定义 (`types`)](./dev-docs/components/endpoint/types.md)
        - [可靠性层 (`reliability`)](./dev-docs/components/reliability.md)
        - [用户接口 (`Stream`)](./dev-docs/components/stream.md)
