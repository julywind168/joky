# 阶段 1：task 内覆盖面扩展（往现有白名单加能力）

> 历史记录：本文保留原阶段的设计、环境与验收结论，不代表当前能力。归档于 2026-09-24；当前说明见[文档入口](../../README.md)，未完成工作见[计划索引](../../plans/README.md)。

> 状态：✅ 已完成（1a 复杂 CFG 与 owned return、1b task body 内再次挂起的直接调用）。

## 1a 复杂 CFG 与 owned return

- [x] machine entry 支持 `needs_drop` 返回类型的多个 resume block 合流（标量多 block、Tuple/Struct、Enum/Option/Result 的 tag-aware cleanup 全部覆盖）。
- [x] 逐项评估 resume 语句白名单扩展：恢复尾部的同步 Normal handler request
  （`HandlerRequest` + Normal continuation `Resume` 标记）已纳入，verifier 形状约束不变，
  配 capability 报告与 runtime 回归测试。`Store` 经论证结构性不可达（借用 receiver 不能跨挂起），
  保留拒绝作为防御；无 continuation 的 `EffectRequest` 死变体已从 MIR 删除。
  `TaskAbort` 已纳入（固定 abort 块形状在 machine entry 内编译，完成回调发布 `Aborted`）。
  `ResumableRequest` 已纳入（与 HandlerRequest 使用相同的同步 handler frame 调度，
  Resumable continuation 的 Resume 标记同样允许）。1a 白名单扩展完成。
- [x] verifier 同步收紧：每纳入一类能力，补充对应的 continuation 形状约束，
  避免 codegen 单方面放宽导致 verifier 与能力脱节。

## 1b task body 内再次挂起的直接调用

- [x] 利用 task thunk 已有的 `Sleeping` 返回路径，让恢复尾部的静态已知
  suspending 调用脱离 worker。先限定范围：task body、无 `CallIndirect`、
  被调用函数自身可链接 machine entry。
- [x] 两层以上再挂起的 continuation 链测试：generation 递进、frame slot
  布局沿用、group/task id 沿 `TaskFrameLayout` 传递。
- [x] 组合验收：恢复 → 再挂起 → 恢复 → join/claim/cancel 全链，
  以及其中任一路径取消时的 cleanup。

## 完成条件

集成测试覆盖上述全链；不新增任何静默回退类别（阶段 0 的诊断
对阶段 1 引入的每个新场景依然给出明确答案）。
