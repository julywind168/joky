# 历史归档

本目录保留设计演进和验收证据，不作为当前 API 或支持平台的依据。文中的“当前”、未勾选项、工具链版本和测试数字均属于原记录时点。归档整理日期：2026-09-24。

当前入口：[语言](../lang/README.md)、[标准库](../stdlib/README.md)、[编译器](../compiler/README.md)、[Runtime](../runtime/README.md)、[未完成计划](../plans/README.md)。

## 历史计划

原有阶段顺序、依赖关系和基线说明保留在[历史阶段总览](plans/phase-overview.md)。

| 原阶段 / 主题 | 记录 | 当前说明或接续工作 |
| --- | --- | --- |
| 阶段 0：同步回退契约 | [阶段 0](plans/phase0-sync-fallback-contract.md) | [MIR](../compiler/mir.md) |
| 阶段 1：task 覆盖 | [阶段 1](plans/phase1-task-coverage.md) | [Pending ABI](../runtime/function-pending-abi.md) |
| 阶段 2：函数 Pending ABI | [清单](plans/phase2-pending-abi.md)、[原设计](plans/phase2-suspending-calls.md) | [Pending ABI](../runtime/function-pending-abi.md)、[timer 接续](../plans/backlog.md#调度与-pending-abi) |
| 阶段 3：无栈恢复 | [研究](plans/phase3-stackless-resume.md)、[实施](plans/phase3-implementation.md) | [MIR](../compiler/mir.md) |
| 阶段 4：稳健性 | [阶段 4](plans/phase4-robustness-release.md) | [资源控制验收](../plans/resource-control.md)、[后续边界](../plans/backlog.md) |
| 阶段 6：模块与 ABI | [阶段 6](plans/phase6-module-abi.md) | [模块 ABI](../compiler/module-abi.md) |
| 阶段 7：C FFI | [阶段 7](plans/phase7-c-ffi.md) | [C FFI](../lang/c-ffi.md) |
| 阶段 8：本机 AOT | [阶段 8](plans/phase8-aot.md) | [AOT 契约](../compiler/aot.md)、[交叉编译](../plans/cross-compilation.md) |
| 局部 var 与可变捕获 | [设计与验收](plans/local-var.md) | [绑定](../lang/basics.md#绑定)、[函数](../lang/functions.md) |
| 单一 runtime 迁移 | [迁移记录](plans/runtime-unification.md) | [架构](../compiler/architecture.md)、[发布配套](../plans/cross-compilation.md#发布配套) |
| 原待办中的已完成实现 | [工作摘录](plans/completed-work.md) | [未完成事项](../plans/backlog.md) |

阶段 5、9 与迭代协议仍有未完成范围，保留在[当前计划](../plans/README.md)，不因部分任务完成而整篇归档。

## 验收与测量

| 日期 | 报告 | 内容 |
| --- | --- | --- |
| 2026-09-10 | [阶段 5.0](reports/phase5-0.md) | 本地验收、并发复验与 release 基准 |
| 2026-09-10 | [阶段 5.1](reports/phase5-1.md) | 长期资源回收 |
| 2026-09-10 | [阶段 5.2](reports/phase5-2.md) | 有界迭代与资源边界 |
| 2026-09-10 | [阶段 5.3](reports/phase5-3.md) | 文件 I/O |
| 2026-09-21 起 | [模块编译](reports/module-compilation.md) | 依赖快照与前端数据拆分测量 |
| 2026-09-25 | [随机数 provider](reports/random-runtime-2026-09-25.md) | 功能验证、取消测试，以及未通过的 runtime 压力验证与基线对照 |
| 2026-09-25 | [Runtime 并行测试修复](reports/runtime-test-fixes-2026-09-25.md) | String key 重复释放、取消测试展开挂起的根因与 300 轮完整回归 |

复跑请先看[当前测试入口](../testing/README.md)。历史命令可能依赖当时的源码路径、工具链和 fixture；追溯旧提交时使用 `git log --follow -- <当前文档路径>`。
