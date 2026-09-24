# 阶段 2：统一函数级 Pending ABI（挂起点推广）

> 历史记录：本文保留原阶段的设计、环境与验收结论，不代表当前能力。归档于 2026-09-24；当前说明见[文档入口](../../README.md)，未完成工作见[计划索引](../../plans/README.md)。

> 状态：✅ 已完成。设计文档见
> [原实施设计](phase2-suspending-calls.md)。
> 目标锚点：达到 Rust async 的等价能力——显式着色、可诊断、递归需要显式处理；
> 不做"任意函数透明挂起"。此阶段动挂起点模型，是真正的架构扩展。

## 任务清单

- [x] 设计文档先行（`docs/archive/plans/phase2-suspending-calls.md`）：挂起点从
  `Suspend` 语句推广到"调用可能挂起函数的调用点"；定义调用者协议、
  返回值与 ownership 转移、failure/handler 传播、各方法 cleanup 责任。
- [x] MIR/lowering：为 suspending 调用点生成 continuation metadata，
  复用 Suspend 的 spill/frame slot 机制；verifier 不变量同步扩展
  （每个调用点挂起 = 一个 continuation，复用 id 唯一性检查）。
- [x] codegen：调用点前后保存/恢复 frame；非 task 函数扩展为
  heap task context/capture/result storage。
- [x] runtime：非 task 函数的 Pending 返回 ABI 与调用者唤醒路径。
- [x] 实施顺序：静态直接调用 → `CallIndirect`；递归 suspending 调用
  显式要求 heap indirection 并给出诊断（对齐 Rust 的 `Box::pin` 义务）。
- [ ] `is_time_sleep_operation` 的 operation identity 特判复核：
  若 provider 选择可以走 capability 声明，则移除最后一个 identity 特判。

## 完成条件

普通（非 task）函数内挂起不再依赖同步等待；组合测试覆盖
"普通函数挂起 → 调用者脱离 → 恢复 → 与 handler/abort 组合"。
