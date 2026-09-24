# 编译器

本目录描述当前实现与编译阶段之间的契约。先读[总体架构](architecture.md)，再进入相关主题；早期实施计划已归入[历史归档](../archive/README.md)。

| 主题 | 文档 |
| --- | --- |
| 流水线、Frontend / Compiler 边界 | [总体架构](architecture.md) |
| 名称解析、类型推断与检查 | [类型检查](type-checking.md) |
| 源级诊断与分层错误 | [错误处理](error-handling.md) |
| 带类型的高层表示 | [HIR](hir.md) |
| CFG、ownership、continuation 与 verifier | [MIR](mir.md) |
| 模块图、导入与依赖 | [模块系统](modules.md) |
| 接口、缓存和链接契约 | [模块 ABI](module-abi.md) |
| Cranelift lowering 与机器码 | [代码生成](code-generation.md) |
| 原生对象、实例化和入口注册 | [AOT](aot.md) |

函数挂起的运行时协议见 [Pending ABI](../runtime/function-pending-abi.md)。测试分工与复现入口见[测试与测量](../testing/README.md)。
