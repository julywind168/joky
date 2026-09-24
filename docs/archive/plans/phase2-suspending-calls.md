# 阶段 2：统一函数级 Pending ABI 设计

> 历史记录：本文保留原阶段的设计、环境与验收结论，不代表当前能力。归档于 2026-09-24；当前说明见[文档入口](../../README.md)，未完成工作见[计划索引](../../plans/README.md)。

> **状态：阶段 2.1 已完成（静态直接调用）**
>
> 本文档是阶段 2 的实施计划。现有代码已经完成函数级
> `is_suspending` 分析和 task 内嵌套挂起，普通函数 Pending 返回 ABI 已接通。
> 函数调用 metadata pass 已默认启用；task thunk 已消费隐藏 parent 参数和
> Ready/Pending 状态。ABI 规范见 `docs/runtime/function-pending-abi.md`，并由 runtime/JIT
> 竞态、失败和取消测试覆盖。

## 目标

将挂起点从显式的 `Suspend` 语句推广到"调用可能挂起函数的调用点"，达到 Rust async 的等价能力：
- 显式着色（沿用 effect 声明和 `is_suspending` 分析；暂不增加函数注解语法）
- 可诊断（编译期知道哪些调用可能挂起）
- 递归需要显式处理（类似 Rust 的 `Box::pin`）

**不做**："任意函数透明挂起"——所有挂起点在编译期已知。

## 当前架构回顾

### 当前的挂起机制

1. **显式 Suspend 语句**：只有 `Suspend` 语句能创建挂起点
   - `Suspend` 绑定一个 `Suspending` continuation
   - continuation 包含 spill values、frame slots、resume block
   - machine entry 在 resume block 处重新进入

2. **Task 内的嵌套挂起**（阶段 1b，阶段 3 已异步化）：
   - Task body 调用的函数可以再次挂起
   - 嵌套调用使用函数级 Pending ABI：callee 挂起返回 Pending，caller
     将跨调用值保存到 heap frame 并退出 machine entry，调度器在 callee
     完成后恢复 caller 的 continuation entry

3. **限制**：
   - 普通（非 task）函数调用链现已携带 Pending 状态
   - 所有挂起必须通过显式的 `Suspend` 语句
   - 嵌套调用不再阻塞 worker，也不依赖原 caller 的 native frame

### 需要扩展的地方

1. **MIR lowering**：
   - 为每个 suspending 调用点生成 continuation metadata
   - 复用现有的 spill/frame slot 机制
   - 标识哪些函数是 suspending 的

2. **Codegen**：
   - 调用 suspending 函数前保存 frame
   - 处理 Pending 返回（函数挂起）
   - 恢复时从 continuation 重新加载 frame

3. **Runtime**：
   - 非 task 函数的 Pending 返回 ABI
   - 调用者唤醒路径
   - Heap task context/capture/result storage

4. **Verifier**：
   - 每个挂起调用点 = 一个 continuation
   - continuation id 唯一性检查
   - 递归 suspending 调用的诊断

## 设计方案

### 1. 函数着色系统

#### 1.1 Suspending 函数标记

函数在以下情况下是 suspending 的：
- 包含 `Suspend` 语句
- 调用其他 suspending 函数
- 声明了 `@suspends` effect operation

```rust
// 已有字段
pub(crate) struct MirFunction {
    // ...现有字段
    pub(crate) is_suspending: bool,
}
```

#### 1.2 调用点分类

在 MIR lowering 时，对每个调用进行分类：
- **普通调用**：调用不会挂起的函数，直接编译
- **Suspending 调用**：调用可能挂起的函数，需要 continuation 支持

### 2. MIR 表示

#### 2.1 MIR 采用显式调用点 continuation

`Call` 和 `MethodCall` 保留现有语句类型，但 suspending 调用必须携带
`continuation: Some(id)`；不能只在 codegen 阶段通过 `is_suspending` 猜测。
`CallIndirect` 在函数值类型具备 effect 信息后接入同一协议。

这样可以让 verifier、dump、诊断和 codegen 共享同一挂起点身份，并保持
“一个可能挂起的调用点对应一个 continuation”的不变量。

#### 2.2 Continuation metadata 生成

对于每个 suspending 调用点，lowering 生成：
- 一个 `MirContinuation` 记录（kind = `Suspending`）
- suspend_block = 调用所在的 block
- resume_block = 调用后继续的 block
- spill_values = 调用后仍然活跃的值
- frame_slots = 需要保存的 frame 数据

```rust
// 示例：lowering suspending 调用
// 源代码：
//   let x = some_suspending_call(arg)
//   use(x)
//
// MIR：
//   b0:
//     v1 = Call(some_suspending_call, [arg]) continuation c0
//     Goto b1([v1])
//   b1(v2):  // resume block
//     Call(use, [v2])
```

### 3. Codegen 策略

#### 3.1 调用协议

Suspending 函数返回一个状态码：
- `0` = Ready（立即完成）
- `1` = Pending（已挂起，需要等待）

```rust
// 生成的代码伪代码
let status = call_suspending_function(args, continuation_ptr);
if status == 0 {
    // 立即完成，结果已写入 continuation result slot
    let result = load_result(continuation_ptr);
    continue_to_next_block(result);
} else {
    // 已挂起，返回 Pending
    return PENDING;
}
```

#### 3.2 Frame 保存

在调用 suspending 函数前：
1. 分配 continuation 对象（如果还没有）
2. 保存活跃的局部变量到 frame slots
3. 设置 resume entry 指向恢复块

#### 3.3 恢复路径

当 continuation 被唤醒时：
1. Scheduler 调用 machine entry
2. Machine entry 从 frame slots 恢复局部变量
3. 加载调用结果
4. 跳转到 resume block 继续执行

### 4. Runtime 支持

#### 4.1 非 Task 函数的 Context

普通函数调用 suspending 函数时，需要：
- 分配 TaskContext（如果调用链中还没有）
- 使用 heap 存储 continuation frame
- 通过 scheduler 唤醒

#### 4.2 调用栈管理

```
普通函数 A
  -> 调用 suspending 函数 B
    -> B 内部 Suspend
      -> Scheduler 接管
        -> 恢复到 B 的 resume block
      -> B 返回 Ready
    -> A 继续执行
```

#### 4.3 完成与退栈的握手

函数调用 activation 不拥有 task 的完成权。它不能覆盖 `TaskContext.continuation`，
不能修改 `continuation_pending`，也不能根据完成线程的 TLS 去解绑 task。
普通函数在 task 中被调用也是阶段 2.1 的目标，不能靠排除 task-reachable 函数
规避协议问题。只有最外层 task/root 边界发布整条 Pending 调用链。

runtime 的调用 activation 使用现有 continuation handle、scope lease 和 cleanup
注册机制。函数 ABI 和 parent/child 链接现已接入：

| 时机 | 原语 | 责任 |
| --- | --- | --- |
| 调用 callee 前 | `begin_function_pending(handle)` | 建立 Calling activation，不触碰 task 状态 |
| callee 原生调用返回后 | `poll_function_pending(handle)` | 完成已到达则返回 Ready，否则返回 Pending；都不调度 |
| 最外层 native 调用链已退栈 | `publish_function_pending(handle)` | 将 Pending activation 交给 scheduler，允许恢复 |
| callee 真正完成 | `complete_function_pending(handle, payload, size)` | 校验结果布局，在取消锁内移动结果；仅已交接时调度 |
| caller 领取结果 | `take_function_pending_result(handle, output, size)` | 在取消锁内复制并清空结果槽；成功后 output 唯一拥有 payload |

三个完成顺序必须都成立：

1. complete 早于 poll：poll 返回 Ready，永不入队，caller 同步消费结果。
2. poll 返回 Pending，complete 早于 publish：暂存结果，publish 恰好入队一次。
3. publish 早于 complete：complete 恰好入队一次。

publish 不能放在普通 caller 的返回语句前，否则另一个 worker 可能抢先恢复尚未
退栈的 caller。嵌套 Pending 传播到最外层边界前必须一直保持未交接状态。
caller frame 在 publish 后只能由恢复路径或取消路径消费。

call 返回值放入 caller 的 `CONTINUATION_SUSPEND_RESULT_STORAGE`，它与 caller
最终返回值的 `CONTINUATION_RESULT_STORAGE` 分离。callee 完成被接受后，payload
所有权移入该槽；若完成被拒绝，callee 仍负责释放。`poll` 的 Ready 不提供原始
指针的有效期保证；caller 必须通过 `take` 在同一状态锁内领取并清空结果槽，
避免取消发生在 poll 与读取之间。重复领取失败，取消胜出时 output 不变。
`Failed`/`Cancelled` 保留状态码 2/3，具体
failure payload、handler 捕获和 child cancellation 协议已在阶段 2.1 ABI 测试中冻结。

当前握手原语每次 activation 使用新 handle，避免过期回调污染后续调用。
loop 中的调用也必须创建新 activation；未来复用需要额外的 generation 校验。

`with_function_pending_boundary` 包住最外层 Pending thunk。边界内的 poll 自动
登记 Pending activation，嵌套边界不发布；只有外层 thunk 返回 Pending 后统一
publish。边界仍活跃时直接 publish 会被拒绝。native unwind 或终态与未完成
activation 不一致时，取消未发布的工作。`start_pending_timer` 和
`start_pending_provider` 使用同一个边界，既不等待，也不绑定或覆盖 task context。
provider 在 start hook 内同步完成时，结果同样等 native 返回后才派发；初版的
accepted operation 统一返回 Pending。取消遇到仍在执行的 start hook 时，必须等
hook 返回再释放参数和结果缓冲区。

当前已有真实 provider completion 驱动三层函数 continuation 的 runtime 测试，
覆盖提前完成、延后完成、timer、拒绝请求和取消；Joky 源码端到端 Pending ABI
验收已完成，新边界和 operation 入口已接入 task/root thunk 及 ordinary codegen。

普通函数 ABI 的准备代码已提交（`a277e7d`），当前包含隐藏 parent 参数和
状态码签名，调用者及返回路径已消费该协议。后续检查发现并修正了
timer/provider import 漏传 parent 的问题；新增 JIT 测试实际执行这两个入口，
验证 native boundary 退栈后结果通过隐藏 parent 到达 caller。
operation continuation 的最终结果交接现在在 cleanup metadata 被清除前完成：
父调用接受时释放子结果的字节存储，拒绝时先执行子结果的 managed cleanup。
该路径另有接受/取消拒收测试，验证结果恰好释放一次。这些测试验证 runtime
及 JIT ABI 接线，仍不替代下面的普通 Joky 函数调用链验收。

#### 普通函数 codegen 首个可执行切片

源码测试显式调用 metadata pass 后，`main → middle → leaf → time.sleep`
已能返回 Pending，随后由三个独立 machine entry 逐层恢复。另有方法 receiver、
跨挂起 String 局部变量及 String 返回值、Ready 分支不入队、缺失 provider 的
Failed 传播测试。Ready 的结果暂时采用 native 返回寄存器，caller 将其移入
call-result 槽，再使用与恢复入口相同的原子 take。Pending 的结果由 callee
最终完成时直接转移给 parent；两条路径统一了 caller 的领取方式。

该实现已覆盖普通函数调用链的 ABI、task/handler thunk、跨 parent 的取消以及
failure payload 的交接；阶段 2.1 的验收测试已纳入 JIT 与 runtime 测试套件。
root 和 machine entry 的 native 边界均已使用新协议；默认 lowering 仍保留旧路径。
不得将本组源码测试视为已覆盖 task、handler 或任意调用拓扑。

machine entry 内连续 timer/provider 操作已接入 Pending 入口，并在每次 entry
返回、恢复线程的 handler/task TLS 还原、active 标记清除后才 publish。
后续操作从当前 continuation 读取原 parent，避免覆盖函数的最终返回目的地。
源码测试验证三次 timer（整条链五次 machine resume）和 provider start hook
内立即完成两次请求（整条链四次 machine resume）。恢复后 provider 拒绝时，
终止当前调用及等待它的 parent 链，并记录 scope 内的协议终态，root drain 后
报告错误；这尚不等于语言级 handler failure payload 协议。

`Call`/`MethodCall` activation 现在在调用点执行时分配，每次执行使用新 handle，
Ready 领取结果后释放该 activation。machine entry 中的后续函数调用复用相同
ABI；Pending 时将活跃 frame/spill 保存到新 activation，旧 activation 退役，
父结果目的地不变。源码测试覆盖第二次挂起调用、恢复后 Ready 的 String
结果、方法 receiver 和 String spill、同一调用点循环三次（七次 machine
resume），以及恢复后子调用失败时等待链的终止。

#### 默认 pipeline 与 task 边界接入

lowering 现在默认物化静态调用 continuation。task thunk 传入空 parent，
只在 Ready 时写入同步结果；runtime 在整个 thunk 退栈后绑定调用链根节点并
发布 Pending。worker 帮助执行另一个 task 或恢复入口时使用独立发布边界。
绑定与取消检查共用 continuation 状态锁，已取消的 activation 不得重新绑定。

调用 activation 和 operation 都登记到父节点的子列表；父取消先停止子工作，
再释放父 frame。子恢复通过 parent 查询 task 动态上下文，但不取得 task 的
完成权。task 内取消及已交给 task group 的 abort 不再写入 root failure 状态。
恢复入口 ScopeExit 同时移除环境中的 group，避免后续调用把已释放 group
重新保存到新 frame。

验证包括 819 个库测试和 58 个 CLI 测试全部通过，以及发布前取消回归。
自定义 Duration provider 测试注册实际 provider 并检查请求与 machine resume；
普通函数调用链的 Ready/Pending、managed ownership、failure、handler/abort、
多层恢复和取消竞争均有定向覆盖。

### 5. 实施顺序

#### 阶段 2.1：静态直接调用

- [x] 识别 suspending 函数（`MirFunction.is_suspending`）
- [x] 固定点传播调用链中的 suspending 标记
- [x] 定义并冻结普通函数 Pending ABI（状态、结果、failure、cleanup；见 `docs/runtime/function-pending-abi.md`）
- [x] runtime 调用完成/原生退栈交接握手，覆盖三种完成顺序及并发竞争
- [x] timer/provider 的 Pending 入口与 native handoff，包含 start hook 取消保护
- [x] call result 与 function final result 分槽，覆盖取消和 ownership 转移竞争
- [x] 独立 MIR pass 为 `Call`/`MethodCall` 拆出 resume block 和 spill/frame metadata
- [x] verifier 对调用点执行唯一性、目标、独立恢复边和 spill/frame 校验
- [x] 在 lowering pipeline 启用调用点 pass
- [x] codegen 保存普通函数 frame，并传播 Ready/Pending
- [x] runtime 支持普通函数 context 的唤醒、取消和结果转移（含取消竞争）
- [x] 添加普通函数挂起、恢复、handler、abort 的组合测试

#### 阶段 2.2：间接调用

- [x] 函数类型携带 suspending 标记
- [x] `CallIndirect` 检查函数类型的 effect 集并写入 MIR 调用点 metadata
- [x] 动态分派 suspending 调用（closure glue + Pending ABI）
- [x] 对无法证明安全的间接调用给出稳定诊断，禁止静默退回同步等待

#### 阶段 2.3：递归处理

- [x] 检测递归 suspending 调用并物化调用点 continuation
- [x] 自动使用每次调用独立的 heap-backed continuation/frame
- [x] 文档说明该机制与 Rust `Box::pin` 的对应关系；不增加用户语法

### 6. 诊断与错误处理

#### 6.1 编译期检查

- Suspending 函数只能被 suspending 调用者调用（着色传播）
- 非 suspending 函数调用 suspending 函数 → 编译错误
- 递归 suspending 调用 → 自动使用独立 heap continuation

#### 6.2 运行时行为

- Pending 返回通过 scheduler 唤醒
- Continuation 生命周期由 refcount 管理
- 取消传播到整个调用链

## 完成条件

阶段 2 完成前必须满足：

- 普通（非 task）函数内的静态 suspending 调用不再依赖同步等待。
- 调用者可以脱离 worker，provider 完成后能恢复到调用点之后的 block。
- 返回值、managed ownership、failure、handler request、abort 和取消在
  Ready/Pending 两条路径上都只有一个 cleanup 责任方。
- `CallIndirect` 和递归场景使用 heap-backed continuation；不得静默使用同步
  fallback。
- MIR dump、能力诊断和 runtime 测试能定位每个调用点的 continuation。
- 阶段 0、1a、1b 的现有测试全部保持通过。

## 分阶段验收与回滚边界

每个实现阶段都必须独立通过现有测试，并保留旧的 `Suspend` 路径：

1. 先落地 ABI 类型和 MIR metadata，不改变运行时行为。
2. 接入一个静态直接调用样例，验证 Ready/Pending 和结果 ownership。
3. 扩展 handler、abort、取消及多层调用链。
4. 最后接入间接调用和递归诊断。

阶段 2.1 的 Pending ABI 已稳定，后续阶段不得删除旧的 `Suspend` 路径。间接调用
必须携带函数类型的挂起属性，并通过与直接调用相同的 Pending ABI；无法构造
continuation 时才报告编译期错误。

## 与现有机制的交互

#### 7.1 Machine Entry 能力

Suspending 调用点是否能获得 machine entry？
- **是**：与 Suspend 语句相同
- 条件：resume tail 符合白名单限制

#### 7.2 Nested Suspending Calls

阶段 3 已完成异步化：
- Task body / machine entry 内调用 suspending 函数 → Pending ABI 异步交接
- 普通函数调用 suspending 函数 → 返回 Pending，调用者也挂起

#### 7.3 Effect 系统

- `@suspends` 标记的 operation 调用继续生成 `Suspend` 语句
- Suspending 函数调用生成带 continuation 的 `Call` 语句
- Effect 集检查确保 suspending operation 不被遗漏

## 开放问题

1. **函数指针与闭包**：
   - 闭包捕获 suspending 上下文如何处理？
   - 函数指针类型需要编码 suspending 属性吗？

2. **优化机会**：
   - 尾调用优化如何处理 suspending 调用？
   - Inline 如何影响 continuation 生成？

3. **向后兼容**：
   - 现有的 `Suspend` 语句继续工作
   - 阶段 1 的所有能力保持不变

4. **ABI 具体形状**：
   - 初版优先采用 continuation handle + 状态码 + heap result slot，避免把
     多分量返回值编码进单一机器寄存器。
   - 实现前必须确认 failure payload、结果 slot 和 frame 的释放顺序。

## 下一步

1. 调用点 MIR 已拆出独立 resume block，删除同 block 临时形状及 task-reachable
   排除；覆盖多调用、标量 spill、managed local、分支 Phi、循环和 task 调用链。
   变换同时更新后移的 effect 请求及 Phi 前驱，使用支配顺序恢复 local 绑定。
   Codegen 的 spill 布局改为函数内稳定的 SSA 槽位，支持标量和 managed 临时值；
   native 和 machine resume 共用恢复逻辑，多次挂起不再覆盖前一个 spill。
   call result 的恢复类型来自 MIR destination，与 effect result 共用加载路径。
2. 实现 callee 的 Pending signature、caller/child 链接以及最外层 publish 边界；
   不能用同步 Call 前后的 begin/complete 代替 callee 完成通知。
3. 接入 task/root 的结果和取消所有权、动态 handler frame 生命周期。
4. 阶段 2.1 已完成。后续工作转入阶段 2.2 的间接调用和阶段 2.3 的递归诊断。
