# 历史阶段总览与依赖

> 历史记录：保留归档前的阶段索引、依赖关系和基线说明，整理于 2026-09-24。下列状态属于原索引，不代表当前验收结果；未完成工作见[当前计划](../../plans/README.md)。

语言与运行时的计划文档，按阶段（phase）拆分。阶段 0–4 围绕无栈恢复
能力（stackless resume），阶段 5 推进资源控制与实用文件 I/O。
每个文档记录该阶段的目标、任务清单与验收状态；完成后保留作为设计决策
与验收标准的记录，新计划也落在本目录。

## 阶段总览

| 阶段 | 文档 | 状态 |
|---|---|---|
| 0 · 把同步回退变成显式契约 | [phase0-sync-fallback-contract.md](phase0-sync-fallback-contract.md) | ✅ 已完成 |
| 1 · task 内覆盖面扩展（1a 复杂 CFG / 1b 再挂起调用） | [phase1-task-coverage.md](phase1-task-coverage.md) | ✅ 已完成 |
| 2 · 统一函数级 Pending ABI | [phase2-pending-abi.md](phase2-pending-abi.md) | ✅ 已完成（遗留一项复核，见文内） |
| 3 · 无同步回退与无栈恢复（研究计划） | [phase3-stackless-resume.md](phase3-stackless-resume.md) | ✅ 已完成 |
| 3 · 实施计划与验收清单 | [phase3-implementation.md](phase3-implementation.md) | ✅ 已完成 |
| 4 · 语义完备性、并发稳健性与发布验收 | [phase4-robustness-release.md](phase4-robustness-release.md) | 功能实现完成，本地验收收尾见 [5.0 记录](../reports/phase5-0.md)；无远端 CI |
| 5 · 长期运行的资源控制与实用文件 I/O | [phase5-resource-control-io.md](../../plans/resource-control.md) | 5.0–5.3 已实施并本地验收；5.4 待收尾 |
| 6 · 模块、稳定 ABI 与独立编译 | [phase6-module-abi.md](phase6-module-abi.md) | ✅ 当前阶段范围已完成 |
| 7 · C FFI 与原生资源边界 | [phase7-c-ffi.md](phase7-c-ffi.md) | ✅ 已完成 |
| 8 · AOT 与可发布程序 | [phase8-aot.md](phase8-aot.md) | 本机 AOT 已完成；交叉编译暂缓 |
| 9 · 动态 trait/interface | [phase9-dyn-trait.md](../../plans/dynamic-traits.md) | 动态分派、组合与显式所有权向上转型已实现；借用转型、向下转型与类型查询待实施 |
| 10 · 嵌入式执行、脚本热更新与宿主集成 | [phase10-hotfix.md](../../plans/embedded-execution.md) | 未开始；2026-09-24 调整为单管线（JIT + Pulley）、代码热修优先 |
| 迭代协议 · `Cursor` 与 `for` 泛化 | [iteration-protocol.md](../../plans/iteration-protocol.md) | 阶段 0 至 5a 与 5b 的 IntoCursor 容器适配已实施；`@parallel` 泛化 / `Dyn` / 字符迭代仍视需求 |
| 局部可变绑定 · `var` | [local-var.md](local-var.md) | 阶段 0、1、2 已完成；支持局部 `var` 与 `move fn` 持久可变捕获 |
| 愿景 · 语言级修正与新概念 | [vision.md](../../plans/vision.md) | 方向草案；`split` 与守卫 `when` 已采纳，其余待拆分为独立计划 |

## 阶段间依赖

- 阶段 1b 依赖阶段 0：每个新场景必须能被诊断回答是否/为何回退。
- 阶段 2 依赖 1b 的 continuation 链经验（多级 generation/frame 传递）。
- 阶段 0、1a 相互独立，可并行。
- 阶段 3 的删除回退必须是最后一步，不能与能力扩展混在同一改动中。
- 阶段 5 复用阶段 4 的任务等待和文件准入机制；按剩余验收缺口 →
  生命周期与句柄回收 → 有界批量并发 → 文件读写 → 真实工作流验收推进。
- 阶段 6 是 7、8、10 的共同前置；阶段 9 建议在稳定 ABI 和 AOT 后实施。
- 阶段 10 复用阶段 7、8 的宿主 ABI 与 AOT 基础，不以跨目标编译或动态 trait 完成为前置；可重载模块沿用 Cranelift lowering，按平台选择本机 JIT 或 Pulley 解释执行（Pulley 须先通过验证 spike），不再更新 AOT 宿主自身代码。

## 前置状态

无栈恢复能力扩展启动前的基线：MIR 控制协议迁移已完成并关闭
（`d861d75` 前后的提交序列）。计划目标不是把无栈机制推到理论完备，
而是先补齐"成熟度门槛"（回退边界显式化），再按白名单扩展 → 挂起点
推广的顺序推进。

## 相关文档

- [compiler/phase2-suspending-calls.md](phase2-suspending-calls.md) — 阶段 2 设计文档
- [runtime/function-pending-abi.md](../../runtime/function-pending-abi.md) — 函数级 Pending ABI 运行时契约
- [compiler/MIR.md](../../compiler/mir.md) — continuation 能力矩阵与 MIR dump 说明

> 历史说明：阶段 0–3 的长期任务清单 `docs/lang/4-todo.md` 已审计并删除；
> 未完成的遗留项吸收进 [phase4-robustness-release.md](phase4-robustness-release.md)，
> 已完成条目由对应阶段文档与测试覆盖佐证。
