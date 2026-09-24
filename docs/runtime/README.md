# Runtime

本目录面向修改运行时、代码生成和 native provider 的贡献者。标准库的用户 API 已移至 [`stdlib/`](../stdlib/README.md)。

## 开发入口

先阅读[Runtime 开发约定](development.md)，特别是参数与结果交接、取消、终态和排空之间的区别。

| 契约 | 文档 |
| --- | --- |
| native 句柄、token 与资源所有权 | [资源生命周期](resource-lifetimes.md) |
| 普通函数挂起、恢复与调用链 | [Pending ABI](function-pending-abi.md) |
| Cown 区域归属、逃逸与销毁 | [Region 生命周期](regions.md) |
| lease 获取、竞争和精确唤醒 | [Cown 调度](cown-scheduling.md) |
| 条件登记、quiet release 与重新获取 | [条件等待协议](when-until.md) |
| 游标 lowering、批次调度与资源上限 | [有界迭代实现](bounded-iteration.md) |
| MutMap / MutSet 存储 | [可变哈希索引](mutable-map-index.md) |
| Map / Set 存储 | [持久化集合](persistent-map.md) |

## 相关入口

- 用户语义：[结构化并发与 Cown](../lang/concurrency.md)、[条件等待](../lang/when-until.md)、[循环与游标](../lang/iteration.md)。
- [编译器架构](../compiler/architecture.md)与[AOT 入口注册](../compiler/aot.md)。
- [测试与测量](../testing/README.md)：独立 runtime、隔离资源测试和历史验收。
