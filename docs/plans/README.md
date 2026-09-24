# 当前计划与设计草案

本目录只作为未完成工作的入口，不作为当前语言或 API 的使用手册。实现现状以[语言](../lang/README.md)、[标准库](../stdlib/README.md)、[编译器](../compiler/README.md)和[Runtime](../runtime/README.md)说明为准。

## 按主题查找

| 主题 | 状态与范围 | 计划 |
| --- | --- | --- |
| 资源控制与工作流验收 | 5.0–5.3 有历史验收；5.4 总验收与 Linux 长跑待收尾 | [资源控制](resource-control.md) |
| 动态 trait | 分派、组合、所有权向上转型已实现；其余转换和类型查询待扩展 | [动态 trait 扩展](dynamic-traits.md) |
| 迭代协议 | Cursor、容器适配已实施；并发泛化、Dyn 与字符迭代按需推进 | [迭代协议](iteration-protocol.md) |
| 嵌入与热更新 | 未开始；Pulley 先做可行性验证，API 为草案 | [嵌入式执行](embedded-execution.md) |
| 交互式子进程 | 暂缓；Child、管道端点和 communicate 尚未实现 | [Child 扩展](process-child.md) |
| 交叉编译与发布 | 暂缓；当前仅本机目标，待接通目标布局与工具链 | [交叉编译](cross-compilation.md) |
| Region、调度与 FFI 等后续工作 | 只列未完成事项及其前置条件 | [待办索引](backlog.md) |

“部分已实现”的计划保留原设计脉络与实施记录，页首指向当前说明。勾选历史任务不意味着整篇候选 API 已全部可用。

## 方向文档

- [设计基线](design-baseline.md)：语言模型与编译边界的设计约束，含目标能力。
- [愿景](vision.md)：尚未承诺实施的方向，需拆成有范围和验收条件的具体计划。

## 完成后的去向

已完成的阶段 0–4、6–8、局部 var 和 runtime 迁移记录见[历史计划](../archive/README.md#历史计划)。阶段中的遗留事项由当前计划承接；历史状态、ABI 版本和测试数字仅描述当时。

新增计划写明状态、目标、非目标、前置依赖和验收；完成后先更新当前文档，再迁入归档并保留链接。完整约定见[文档维护](../maintenance.md)。
