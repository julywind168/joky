# 阶段 3：无同步回退与无栈恢复（研究计划）

> 历史记录：本文保留原阶段的设计、环境与验收结论，不代表当前能力。归档于 2026-09-24；当前说明见[文档入口](../../README.md)，未完成工作见[计划索引](../../plans/README.md)。

> 状态：✅ 已完成（2026-09）。实施记录见
> [phase3-implementation.md](phase3-implementation.md)；
> 关键提交：`6f05684`、`0ed0684`、`cdc0936`、`ef4ad36`、`3dbd981`、`597192e`。
> 本阶段为研究性目标，不作为发布门槛。建立在阶段 2 已具备
> continuation frame、spill、result storage、Pending ABI、machine entry 和
> 递归 continuation 的基础上。

## 目标与边界

阶段 3 最终需要达到：

1. 所有合法的 suspending continuation 都能进入独立 machine entry；
2. 不再以同步等待作为合法程序的运行时回退；
3. 恢复路径不读取原调用的 native frame 槽位；
4. 无法生成安全 continuation 的 MIR 在 verifier 阶段拒绝，而不是运行时降级。

不新增用户可见的 `Box`、`pin` 或 heap indirection 语法。heap frame 和每次调用
独立的 continuation handle 由编译器和 runtime 自动管理。

## 实施阶段

### 3.1 建立能力矩阵

- 为每个 `MachineEntryBlocker` 增加最小 MIR 和端到端测试；
- 统计各 blocker 的真实触发数量和对应源代码模式；
- 将 `UnreachableResume`、`UnsupportedTerminator` 转为 verifier 错误；
- 保留 capability 查询作为 codegen、dump 和诊断的唯一来源；
- 在文档中记录每个仍被拒绝的语句、类型和 terminator。

验收：合法程序不会因为结构性原因同步回退；非法 MIR 在 verifier 阶段失败。

### 3.2 扩展 machine-entry 返回 ABI

按风险从低到高支持当前 `supports_machine_return` 排除的类型：

1. `Bytes`；
2. `List`、`Map`、`Set`；
3. `Function`；
4. 其他带 managed leaf 的聚合类型。

每种类型都需要同步修改：

- continuation result storage 的 flattened ABI；
- `take_result`、complete、cancel 和 failure 路径；
- typed cleanup 和 ownership transfer；
- machine-entry return codegen；
- verifier 的返回类型检查。

验收：Ready、Pending、取消、失败和重复完成都只有一个 cleanup 责任方。

### 3.3 扩展 resume-tail 白名单

每接入一种语句，必须同时修改 capability、codegen 和 verifier：

- 复杂 `Resume` 链；
- `HandlerRequest` 与 `ResumableRequest` 的恢复路径；
- 仍可能再次挂起的直接、方法和间接调用；
- task、scope、race、failure 的组合路径；
- 其余合法的 managed cleanup 语句。

`Store` 若仍然要求 borrowed receiver，继续拒绝，并保留结构性诊断；不能只在
codegen 中放宽白名单。

#### 3.3.1 消除嵌套 suspending 调用的同步等待

当前 MIR 已为每个 suspending 调用点生成 continuation metadata，codegen 也已声明
独立 machine entry，并为每次调用分配新的 continuation handle。这里不重复生成
entry，而是修改 machine entry 内再次调用 suspending callee 时的控制流：

- callee 返回 Pending 时，caller 将跨调用值保存到 heap frame，并退出当前
  native/machine entry；
- 最外层 function-pending resume boundary 在 entry 完全退出后发布 Pending
  activation，避免 caller native frame 尚未退出时被其他 worker 抢先恢复；
- callee 完成后通过调用点 handle 写入结果或失败，并调度 caller 的 continuation
  entry；caller 恢复后从 result storage 领取结果并继续 resume tail；
- 取消沿 parent/child handle 链传播，完成、取消和失败竞争只能产生一个终态和一个
  cleanup 责任方；
- 合法嵌套调用不再进入 nested suspend ABI，也不再调用 `wait_while_sleeping()`。

测试必须覆盖 direct、method 和 indirect/closure 调用的 Ready 与延迟 Pending 路径，
以及交接前后取消、callee 失败、同一 resume tail 中连续调用和循环中的重复挂起。
除结果与 cleanup 外，还要断言挂起 worker 可以立即执行其他任务，证明 caller 的
native frame 已退出且恢复只依赖 continuation-owned storage。

验收：machine entry 内的 suspending callee 可以异步返回调度器并恢复 caller；合法
嵌套调用路径不阻塞 worker，不依赖原 caller native frame。

验收：每个新增语句都有 verifier 形状测试、Ready/Pending 测试和取消测试。

### 3.4 引入 stackless 编译验证模式

machine entry 编译时只允许以下值来源：

- continuation frame；
- spill storage；
- result storage；
- 参数和 receiver 的显式入口环境；
- 当前 machine entry 自己产生的 SSA 值。

实现建议：

- 为恢复编译引入 `Stackless`/`Native` 值来源标记；
- machine entry 不预置原调用 native environment 中的跨挂起值；
- 每个 `Read`、`Move`、`Phi` 和恢复后的 local 绑定都检查来源；
- 发现 native-only 值时在编译期报错，而不是隐式读取栈槽；
- 保留现有同步路径作为测试对照，但不让 machine entry 依赖它。

验收：恢复入口在没有原调用 native SSA/local 值的情况下仍可编译并执行。

### 3.5 Native-frame independence 测试

新增测试覆盖：

- 原 worker 在挂起后立即执行其他任务；
- 标量、managed 值和嵌套聚合值跨挂起恢复；
- handler、abort、failure、取消和 race；
- 多层直接调用、间接调用、suspending closure 和递归；
- provider 在任意完成顺序下完成或取消。

测试除了检查结果，还应检查恢复入口没有读取 native frame。可在 debug 构建中
加入值来源断言，避免测试只验证"碰巧还能运行"。

验收：恢复路径只依赖 heap continuation state，原调用栈是否仍存在不影响结果。

### 3.6 删除同步 fallback

当 3.1 至 3.5 完成后：

- `machine_entry_blocker` 只保留内部一致性断言；
- `Compiler::run_program` 删除同步 fallback warning 收集；
- 合法程序不再调用 `wait_while_sleeping` 作为恢复策略；
- 旧 fallback 测试改为 machine-entry 测试；
- 对无法生成安全 continuation 的输入保留 verifier/编译期错误。

验收：程序运行期间不存在合法 continuation 的同步等待回退；错误输入在编译期
失败并给出稳定原因。

## 依赖与风险

- 3.2 依赖现有 result storage 和 typed cleanup ABI，不能先删除 fallback；
- 3.3 必须与 verifier 同步推进，否则会出现 codegen 和 MIR 形状不一致；
- 3.4 是证明无栈恢复的关键，不能用"已有 heap frame"替代值来源验证；
- 3.5 需要覆盖取消和重复完成，否则可能遗漏 ownership double-free；
- 删除 fallback 应是最后一步，不能与能力扩展混在同一个未验证的改动中。

## 最终验收清单

- [x] 所有合法 continuation 都 machine-entry capable；
- [x] 所有剩余 blocker 都是 verifier/编译期错误；
- [x] `cargo test --lib -- --test-threads=1` 全部通过；
- [x] Ready/Pending/取消/失败/重复完成路径 cleanup 唯一；
- [x] native-frame independence 断言测试通过；
- [x] 文档、MIR dump 和诊断不再描述合法同步 fallback。
