# Joky 文档

这里按阅读目的组织文档。使用语言时从当前用法开始；修改实现时阅读内部契约；未来方案与历史验收分别放在计划和归档中。目前文档以中文为主。

## 使用 Joky

1. 按[快速开始](../README.zh.md#快速开始)构建、运行、检查程序并生成原生可执行文件。
2. 阅读[语言指南](lang/README.md)：基础语法、类型、函数、游标、结构化并发与效果。
3. 查询[标准库](stdlib/README.md)：环境、路径、文件、子进程、SQLite 与 TCP 分割。
4. 对照 [examples](../examples/) 运行完整程序。

## 参与实现

先阅读[贡献指南](../CONTRIBUTING.md)，再按修改范围选择入口：

| 修改范围 | 入口 |
| --- | --- |
| 解析、类型检查、HIR / MIR、模块和代码生成 | [编译器](compiler/README.md) → [架构](compiler/architecture.md) |
| 调度、所有权、Pending ABI、provider 与资源回收 | [Runtime](runtime/README.md) → [开发约定](runtime/development.md) |
| 回归测试、隔离资源测试与性能测量 | [测试与测量](testing/README.md) |

## 了解设计与演进

- [当前计划](plans/README.md)：正在收尾、暂缓和待验证的方案；状态不等于可用能力。
- [待办索引](plans/backlog.md)：跨领域的未完成事项与承接文档。
- [历史归档](archive/README.md)：已完成阶段、迁移过程和带日期的验收报告。

## 文档边界

| 目录 | 回答的问题 |
| --- | --- |
| `lang/` | 当前语言如何使用，有哪些语义与限制？ |
| `stdlib/` | 当前标准库 API 如何调用，错误和资源契约是什么？ |
| `compiler/` | 编译器当前如何组织，阶段之间传递什么？ |
| `runtime/` | 调度、内存、ABI 和 native 交接如何实现？ |
| `testing/` | 如何运行、复现和测量？ |
| `plans/` | 哪些工作尚未完成，设计与验收条件是什么？ |
| `archive/` | 当时为何这样设计，在哪个版本和环境验证过？ |

新增、修改或归档文档时遵循[文档维护约定](maintenance.md)。本地导航检查：

```bash
python3 scripts/check-docs.py
```
