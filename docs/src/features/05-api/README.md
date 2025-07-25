# 5. 用户接口 (User-Facing API)

本章节描述了协议库暴露给最终用户的编程接口。设计的核心目标是提供一套功能强大、符合人体工程学且与标准库（如`tokio`）体验一致的API，从而降低用户的学习成本。

内容主要涵盖了用于服务端的`Listener` API和用于数据传输的`Stream` API，后者实现了标准的`AsyncRead`和`AsyncWrite` trait。
