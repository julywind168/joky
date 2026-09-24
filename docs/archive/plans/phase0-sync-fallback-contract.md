# 阶段 0：把同步回退变成显式契约

> 历史记录：本文保留原阶段的设计、环境与验收结论，不代表当前能力。归档于 2026-09-24；当前说明见[文档入口](../../README.md)，未完成工作见[计划索引](../../plans/README.md)。

> 状态：✅ 已完成。任何同步回退都能在编译期静态回答"为什么回退"。

## 任务清单

- [x] 将 `has_standalone_resume`（`src/codegen/cranelift/helpers.rs`）的能力判定前移为
  MIR 层查询 API，不依赖 codegen 内部状态；codegen 与诊断共用同一判定，不再各算一遍。
- [x] 新增静态诊断：对每个无法获得 machine entry 的 `Suspend` 点输出原因——
  不支持的返回类型 / 多 resume block 且返回需 drop / resume 白名单外语句 /
  可能再次挂起的调用 / 间接调用 effect 未确认。级别先定为 warning。
- [x] MIR dump 中标注每个 continuation 是否 machine-entry-capable，并附否决原因。
- [x] 行为测试：嵌套结构化 scope 下同步 fallback 占用 worker 的场景，
  固化当前语义（可取消的阻塞），记录 worker pool 饱和时的表现。
- [x] 文档：`docs/compiler/mir.md` 更新能力矩阵；任务清单同步（该清单已审计删除，遗留项归入 phase4）。

## 完成条件

任何同步回退都能在编译期静态回答"为什么回退"；`cargo test` 全过。
