# 阶段 3：实施计划与验收清单

> 历史记录：本文保留原阶段的设计、环境与验收结论，不代表当前能力。归档于 2026-09-24；当前说明见[文档入口](../../README.md)，未完成工作见[计划索引](../../plans/README.md)。

> 状态：✅ 已完成（2026-09）。全部 7 个步骤落地，
> 关键提交：`2cbac3a`（步骤 1-2）、`6f05684`（派发/取消竞争修复）、
> `0ed0684`（stackless 值纪律测试）、`cdc0936`(删除同步 fallback)、
> `ef4ad36`（wait_for_idle 饿死调度修复）、`3dbd981` + `597192e`
> （性能基准与交接修复）。验收清单见文末，均已达成。

# 阶段 3 实施计划：无同步回退与无栈恢复

## 背景与目标

这是 [phase3-stackless-resume.md](phase3-stackless-resume.md) 的实现计划。阶段 3 的目标是消除所有合法 suspending continuation 的同步等待回退，确保恢复路径完全独立于原调用的 native frame。

**当前问题**：
- 嵌套 suspending 调用使用 `wait_while_sleeping()` 同步阻塞调用者 worker
- 部分返回类型（List, Map, Set, Function）被 `supports_machine_return` 拒绝
- `UnreachableResume` 和 `UnsupportedTerminator` 是结构性错误，应在 verifier 阶段拒绝，而非运行时降级

**关键文件**：
- `src/mir/continuation_capability.rs` (339 行) - capability 分析核心
- `src/mir/verifier/continuation_shapes.rs` (157 行) - verifier 形状约束
- `src/codegen/functions/continuations/entry.rs` (1540 行) - machine entry codegen
- `src/runtime/continuation/abi.rs` - continuation ABI 实现，包含同步等待逻辑

## 实施步骤

### 步骤 1: 将结构性 blocker 前移到 verifier (3.1 的一部分)

**修改文件**：
- `src/mir/verifier/continuation_shapes.rs`

**当前状态**：
- `verify_suspending_continuation_shapes` 已检查部分形状约束
- `machine_entry_blocker` 返回 `UnreachableResume` 和 `UnsupportedTerminator`

**实施**：
1. 在 `verify_suspending_continuation_shapes` 中添加：
   - 检查 resume block 可达性（对应 `UnreachableResume`）
   - 检查所有 resume-tail terminator 是否支持（对应 `UnsupportedTerminator`）
2. 这些检查失败时返回 `Diagnostic::codegen` 错误
3. 保留 `machine_entry_blocker` 用于 codegen 和 dump，但对于这两种情况只作为内部断言

**验证**：
- 现有 verifier 测试应继续通过
- 添加测试用例验证结构性错误在 verifier 阶段被捕获

### 步骤 2: 支持更多返回类型的 machine-entry ABI (3.2)

**修改文件**：
- `src/mir/continuation_capability.rs` - `supports_machine_return` 函数
- `src/codegen/functions/continuations/entry.rs` - result storage 和 complete 路径
- `src/runtime/continuation/abi_storage.rs` - continuation result storage

**当前限制** (continuation_capability.rs:277-286):
```rust
Type::List(_)
| Type::MutList(_)
| Type::Map(_)
| Type::MutMap(_)
| Type::MutSet(_)
| Type::Function(_)
| Type::Cown(_)
| Type::Param(_)
| Type::SelfType
| Type::Associated(_) => false,
```

**实施顺序**（从低到高风险）：
1. **Bytes** - 已支持（275-276行已返回 true）
2. **List, Map, Set** - managed pointer，ABI 表示为单个指针
3. **Function** - code pointer + environment pointer
4. **Cown, Param, SelfType, Associated** - 暂不支持（类型系统限制）

**对每种类型需同步修改**：
1. `supports_machine_return` 添加到 true 分支
2. `abi_types` (src/codegen/abi.rs) 确保正确的 flattened ABI
3. machine entry 的 `take_result` 路径 (continuation/entry.rs)
4. completion callback 的 result transfer
5. 添加端到端测试（Ready/Pending/Cancel/Failure 路径）

### 步骤 3: 消除嵌套 suspending 调用的同步等待 (3.3.1 核心)

**问题根源**：
- `src/runtime/continuation/abi.rs:341-349` - nested suspend path 条件
- `src/runtime/continuation/abi.rs:492,534` - `wait_while_sleeping()` 调用

**当前行为**：
```rust
// abi.rs:341-349
if crate::runtime::task::has_current_task_context()
    && !crate::runtime::task::suspend_publish_allowed()
{
    return unsafe {
        jk_continuation_start_suspend_nested(continuation, operation, milliseconds)
    };
}
```

**目标行为**：
- machine entry 内的 suspending callee 返回 Pending 时，caller 应：
  1. 保存跨调用值到 heap frame
  2. 退出当前 machine entry
  3. 等待 callee 完成后被 scheduler 重新调度
  4. 恢复后从 result storage 读取结果

**修改文件**：
- `src/codegen/functions/continuations/entry/calls.rs` - 已有 `compile_resumed_call`
- `src/runtime/continuation/abi.rs` - 修改 nested suspend 逻辑
- `src/runtime/continuation/function_calls.rs` - function pending boundary

**实施**：
1. **Codegen 修改** (entry/calls.rs:compile_resumed_call):
   - 当前已为 callee 调用生成 continuation metadata
   - Poll 路径处理 Pending 状态（195-209行）
   - 需要确认 Pending 时正确退出 machine entry（而非阻塞）

2. **Runtime 修改** (abi.rs):
   - 检查当前 `jk_continuation_start_suspend_nested` 的使用
   - 在 machine entry 内允许发布 Pending（移除 `dispatch_on_completion.store(false, ...)`）
   - 确保 caller continuation 在 callee Pending 时正确保存状态

3. **验证点**：
   - callee 返回 Pending 时，caller worker 可立即执行其他任务
   - callee 完成后，caller 通过 scheduler 恢复
   - 取消传播到 nested call chain
   - 失败沿 parent/child handle 链传播

**测试覆盖**：
- 直接调用、方法调用、间接调用（closure）
- Ready 路径（callee 立即完成）
- Pending 路径（callee 延迟完成）
- 取消路径
- callee 失败路径
- 循环中的重复挂起

### 步骤 4: 引入 stackless 编译验证模式 (3.4)

**修改文件**：
- `src/codegen/environment.rs` - Environment 结构
- `src/codegen/functions/values.rs` - `compile_mir_read_stackless` (已存在，50-66行)

**当前状态**：
- `compile_mir_read_stackless` 已实现基本检查
- 调用 `environment.lookup_local_stackless(local)`
- 失败时报错 "stackless machine entry cannot resolve native-only local"

**需要完善**：
1. **Environment 值来源标记**：
   ```rust
   pub(super) enum ValueSource {
       Native,      // 原调用 native SSA 值
       Stackless,   // continuation frame/spill/result/参数
   }
   ```

2. **machine entry 编译时约束**：
   - 不预置原调用 native environment
   - 只提供 frame/spill/result/参数的显式入口环境
   - 每个 Read/Move/Phi 检查值来源

3. **错误诊断**：
   - 发现 native-only 值时编译期报错
   - 错误消息指明哪个 local 不可用以及为什么

**验证**：
- 恢复入口在没有原调用 native 值的情况下成功编译
- 尝试访问 native-only 值时编译失败并给出清晰错误

### 步骤 5: Native-frame independence 测试 (3.5)

**新增测试文件**：
- `src/compiler/tests/continuations.rs` - 扩展现有测试

**测试用例**：
1. **Worker 复用测试**：
   ```rust
   // 挂起后，原 worker 立即执行其他任务
   // 恢复时可能在不同 worker 上
   ```

2. **跨挂起值测试**：
   - 标量值（Int32, Bool, Duration）
   - managed 值（String, Class）
   - 嵌套聚合（Tuple, Struct, Enum 包含 managed 值）

3. **控制流测试**：
   - handler enter/exit 跨挂起
   - task abort 跨挂起
   - failure 跨挂起
   - race 取消路径
   - 多层嵌套调用

4. **Completion 竞争测试**：
   - parent 完成 vs child 完成
   - cancel vs complete
   - failure vs complete
   - 确保只有一个 cleanup 责任方

**断言策略**：
- Debug 构建中加入值来源断言
- 检查恢复时没有读取 native frame
- 使用 scope.machine_resumptions counter 验证 machine entry 调用次数

### 步骤 6: 扩展 resume-tail 白名单其余部分 (3.3 其余)

**当前白名单** (continuation_capability.rs:70-140):
- Resume markers (76-86)
- Call/MethodCall/CallIndirect (80-85)
- Task operations (86-136)
- ResumableRequest (140)

**待接入语句** (从计划 3.3):
- 复杂 Resume 链（已部分支持，156-170行）
- HandlerRequest 与 ResumableRequest 的恢复路径（已支持）
- 再次挂起的调用（步骤 3 覆盖）
- task/scope/race/failure 组合路径（已大部分支持）

**验证每个新增语句**：
1. capability 白名单添加
2. codegen entry.rs 编译支持
3. verifier continuation_shapes.rs 验证支持
4. Ready/Pending 测试
5. Cancel 测试

**Store 语句保持拒绝**：
- 原因：borrowed receiver（continuation_capability.rs:145-149）
- 这是防御性约束，继续保留诊断

### 步骤 7: 删除同步 fallback (3.6)

**前提条件**：步骤 1-6 完成且测试全部通过

**修改文件**：
- `src/mir/continuation_capability.rs`
- `src/compiler.rs` 或调用 `machine_entry_fallbacks` 的位置
- `src/runtime/continuation/abi.rs` - 移除 nested suspend 同步路径

**实施**：
1. `machine_entry_blocker` 保留为内部一致性检查
2. 移除 `machine_entry_fallbacks` 的 warning 收集和报告
3. 移除 `jk_continuation_start_suspend_nested` 的同步等待逻辑
4. 更新 `dump_continuation_capabilities` 输出格式

**测试迁移**：
- 旧 fallback 测试改为 machine-entry 测试
- 确认无合法程序调用 `wait_while_sleeping` 作为恢复策略

## 验证策略

### 单元测试
- `src/mir/tests/continuations.rs` - MIR capability 测试
- `src/mir/verifier/continuation_shapes.rs` - verifier 形状测试

### 集成测试
- `src/compiler/tests/continuations.rs` - 端到端 continuation 测试
- 每种新支持的返回类型都需要 Ready/Pending/Cancel/Failure 测试
- 嵌套调用的深度测试（3层以上）

### 回归测试
```bash
cargo test --lib -- --test-threads=1
```

### 性能验证
- 使用 `scope.machine_resumptions` counter 确认 machine entry 被调用
- Debug 模式下加入 native frame 访问断言
- 确认 worker 在挂起后可立即执行其他任务

## 风险与依赖

1. **步骤 3 依赖 steps 1-2**：
   - 需要确保 result storage 和 typed cleanup 稳定
   - 不能先删除 fallback

2. **步骤 4 与 codegen 同步**：
   - stackless 验证必须与 codegen 一致
   - 否则可能出现编译通过但运行时读取错误值

3. **步骤 5 的覆盖范围**：
   - 必须覆盖 cancel 和重复 completion
   - 否则可能遗漏 ownership double-free

4. **步骤 7 是最后一步**：
   - 不能与能力扩展混在同一个未验证的改动中
   - 需要所有测试通过后才执行

## 最终验收清单

- [x] 所有合法 continuation 都 machine-entry capable
- [x] 所有剩余 blocker 都是 verifier/编译期错误
- [x] `cargo test --lib -- --test-threads=1` 全部通过
- [x] Ready/Pending/Cancel/Failure/重复完成路径 cleanup 唯一
- [x] native-frame independence 断言测试通过
- [x] 文档、MIR dump 和诊断不再描述合法同步 fallback
- [x] 嵌套 suspending 调用不阻塞 worker

## 实施建议

**迭代顺序**：
1. 先做 step 1（结构性错误前移），确保 verifier 稳定
2. 逐个类型完成 step 2（返回类型扩展），每个类型都充分测试
3. Step 3（消除同步等待）是最复杂的，需要仔细实现和测试
4. Step 4-5 为验证步骤，确保无栈恢复正确性
5. Step 6 扩展剩余白名单（如需要）
6. 最后执行 step 7 清理同步 fallback

**每个步骤完成标准**：
- 相关测试全部通过
- 没有引入新的 warning 或 fallback
- MIR dump 输出符合预期
- 文档更新完成
