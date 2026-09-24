# Joky 设计基线

> 状态：架构设计草案。本文记录目标语言模型与编译边界；尚未实现的功能不会被描述为当前能力。

## 1. 核心结论

Joky 使用 Rust runtime 与 Cranelift 作为生产执行后端。Cranelift 编译已经完成名称解析、类型检查、Effect 检查和所有权分析的 Joky 程序；runtime 负责内存、任务调度、挂起 operation、Cown 和插件等通用机制。

Joky 不实现 Actor、mailbox 或消息行为。并发模型是多 worker 线程上的结构化任务，并通过 `Cown(T)` 与 `when` 隔离共享可变状态。

用户层不提供 `async/await`。Effect operation 可以标记为 `suspends`，编译器根据调用链和结构化运行上下文将可能挂起的计算降低为显式 continuation/state machine。

```text
source
  -> AST / resolver / type checker / effect checker
  -> Typed Core IR
  -> Ownership + Concurrency IR
  -> suspension state-machine lowering
  -> Cranelift backend + metadata
  -> Joky runtime
```

不提供生产解释器 fallback，也不允许某个函数无法编译时切换到另一条执行语义。编译失败是编译错误；调试可以使用 source map、IR dump 和 runtime tracing，但验证解释器若保留也只服务测试，不进入发行版执行路径。

## 2. 设计原则

### 必须保留

- Typed Core IR 与 Ownership/Concurrency IR 分离：前者表达计算语义，后者表达生命周期、任务和访问隔离。
- 函数、类型、Effect operation、任务入口和 ABI 使用稳定 ID/slot，而不是运行时字符串查找。
- runtime 只提供通用内存、scheduler、operation、Cown 和 native ABI，不包含具体业务协议。
- `suspends` operation 保存 continuation；恢复时继续原来的 Effect、任务和 ownership 状态。
- plugin 通过版本化 descriptor 和统一 completion/cancel/release 协议接入。
- verifier 在执行前检查类型、控制流、ownership、任务边界、lease 和 ABI。
- 不可变值、唯一对象、Cown capability、lease 和 native handle 使用不同的 ownership 类别。

### 必须避免

- 把 PostgreSQL、HTTP 或其他业务协议直接写进 VM/runtime；它们应是 plugin 或标准库实现。
- 让后端回读源码决定 ABI、ownership、Effect 或任务清理规则。
- 用一个动态 `Value` opcode 处理所有 scalar、共享对象、唯一对象、Cown lease 和 native handle。
- 在 worker 上执行不可取消的阻塞 I/O，或让 native completion thread 直接进入 Joky 代码。
- 用线性 use-count 代替 CFG liveness；分支、循环、Effect 失败、取消和挂起路径必须显式平衡 ownership。
- 让任务、闭包环境、continuation 或 Cown lease 隐式逃逸其结构化作用域。

## 3. 编译器分层

建议的工程边界如下：

```text
crates/
  joky-syntax        lexer, parser, diagnostics
  joky-sema          resolver, types, effects, traits
  joky-ir            typed Core IR
  joky-ownership     CFG, liveness, move/dup/drop/reuse
  joky-codegen       backend-independent ABI and metadata
  joky-cranelift     Cranelift lowering and code cache
  joky-runtime       heap, scheduler, cowns, operations
  joky-plugin        dynamic provider ABI and loader
  joky-cli            compiler and runner
```

依赖方向：

```text
syntax -> sema -> ir -> ownership -> suspension_lowering -> codegen -> cranelift
runtime <-----------------------------------------------+
plugin -> runtime ABI
```

`joky-cranelift` 不得依赖 parser、resolver、具体标准库或 plugin 私有实现。后端只消费已经验证的 IR 和 ABI descriptor。

## 4. Effect、Trait 与 Capability

三者描述不同维度：

```text
trait      = 值或类型支持哪些操作
Effect     = 一段计算可能请求哪些操作
Capability = 代码被允许访问哪些资源
```

Effect 使用 `effects { ... }` 声明，使用 `do { ... } with { ... }` 安装词法 Handler。operation 可以是普通、可恢复、挂起或不可恢复 operation；第一版只允许 resumable continuation 恢复一次。

```joky
eff Network {
    request(request: Request) -> Response suspends
}

fn load_user(id: String) -> User effects { Network } {
    let response = Network.request(Request(path: id))
    decode_user(response)
}

let user = do {
    load_user("alice")
} with {
    Network.request(request) => fake_network(request)
}
```

Effect 可以替代一部分跨层环境参数，但不能替代 trait 的泛型约束、类型派发和值接口。Effect 的传播、处理和挂起属性由语义分析记录，不由 Cranelift 重新推导。

## 5. Core IR 与 Ownership/Concurrency IR

Core IR 在 lowering 前完成：

- 类型、泛型实例化和 trait 静态派发；
- `?`、`!`、pattern match 和 Effect 失败的显式控制流；
- Effect 集合传播与 Handler 消除；
- 闭包转换和捕获分析；
- `scope`、`branch`、`race`、`Cown`/`when` 的类型化表示；
- 稳定的 `FunctionId`、`TypeId`、`EffectId` 和 ABI 描述。

Ownership/Concurrency IR 使用 typed SSA-like value 和 block parameter，至少包含：

```text
value:   const, construct, project, call, closure
ownership: move, dup, drop, clone, share, borrow, reuse
control: branch, switch, return, fail
handler: effect_request, handler_enter, handler_action, handler_exit
suspend: operation_start, suspend, resume, cancel
scope:   scope_enter, branch_start, join, race, task_cancel, scope_exit
cown:    acquire, lease_project, release
```

Ownership pass 在 CFG 上计算 liveness，并为正常返回、Effect 失败、取消、panic 和 suspend 边插入清理。任何后端都不能重新解释这些规则。

## 6. Cranelift 后端

所有通过 sema 和 verifier 的 Joky 函数都必须有 Cranelift lowering，包括：

- 整数/浮点 scalar、checked arithmetic 和比较；
- 分支、循环、错误传播和 ADT match；
- 普通函数、闭包、泛型单态化实例和函数值调用；
- String、ADT、List、Map、Set、Bytes 与 native handle；
- Effect request/handler dispatch 和 resumable continuation；
- `scope`、`branch`、`race`、取消和 join；
- `Cown(T)` 创建、复制、acquire、lease 访问和 release；
- `time.sleep`、TCP、plugin operation 等 `suspends` operation；
- generation-specific 的普通调用、回调和 continuation 恢复。

这不要求 Cranelift 直接理解复杂对象。代码生成可以把复杂操作降低为 runtime ABI 调用，但每个 Joky 控制流、挂起点和清理路径都必须是编译后机器码的一部分。

普通 AOT 代码不要求通过可替换的 `FunctionSlot` table 调用。2026-09-17
已将 AOT Hotfix 方向替换为 [阶段 10 的嵌入式 VM 与脚本热更新](embedded-execution.md)。
可重载脚本中的函数句柄、task、continuation 和 callback 必须持有所属模块
版本，机器码地址不能比模块的有效生命周期更长。具体表示由 VM 契约定义。

### Runtime ABI

复杂值和调度操作统一通过版本化的 C-compatible Rust ABI 进入 runtime：

```text
jk_alloc_object / jk_alloc_closure_environment / jk_alloc_native_handle
jk_drop / jk_dup
jk_string_concat
jk_list_* / jk_map_*
jk_task_group_new / jk_task_spawn_heap / jk_task_join / jk_task_race
jk_cown_new / jk_cown_acquire / jk_cown_release
jk_continuation_begin_suspend_with_payload / jk_continuation_dispatch
```

ABI 参数必须携带 value kind、ownership mode 和 layout version。`joky-cranelift` 只生成 ABI 调用，不直接读取 scheduler、Handler、Cown 或 plugin 私有结构。

## 7. 无栈挂起模型

`time.sleep`、TCP 和 plugin operation 采用无栈 continuation。它们由 `suspends` 属性描述，不需要用户写 `async/await`：

```text
operation_start
  -> 保存 live values、Handler frame 和 cleanup bitmap
  -> 当前 worker 返回 scheduler
  -> reactor/provider 完成 operation
  -> scheduler 恢复 continuation
```

Cranelift 后端不能暂停 native call stack。含有 `suspends` operation 的函数必须在 codegen 前降低为显式状态机：

```text
state 0: evaluate until operation
state 1: receive completion value/error
state 2: continue after suspension
state C: cancellation cleanup
```

runtime 持有：

```text
Continuation { generation, resume_entry, state, spill_area, operation }
```

完成后按 `resume_entry` 重新进入机器码；runtime 不保存 VM instruction pointer 或寄存器。spill area 由 lowering 根据 suspension liveness 生成固定 layout，包含跨挂起存活的 owned values、Handler frame 和 cleanup bitmap。

`when` body 不得包含 `suspends` operation，因此 Cown lease 不会跨挂起点存活。取消直接进入编译生成的 cleanup entry，必须释放 operation、Handler、task、lease 和 owned values。

## 8. 多线程结构化并发

`scope` 建立结构化任务区域，`branch` 创建可以由多 worker 线程并行执行的子任务：

```joky
let (left, right) = parallel {
    | heavy_compute_left()
    | heavy_compute_right()
}
```

runtime 负责：

- 父子 task 生命周期；
- branch 的调度、结果收集和 join；
- 一个 branch 失败时对同级任务的取消传播；
- 取消后的清理等待；
- `race` winner 选择和 loser cleanup；
- 禁止 task 句柄和 branch 结果逃出 scope。

`scope` 不保证每个 branch 永远独占一个 OS thread；它保证 branch 具有并行执行资格，由多 worker scheduler 根据资源和负载调度。纯 CPU 计算和使用 `suspends` operation 的计算都可以成为 branch。

`scope`、`branch`、`race`、join 和取消是结构化任务原语，不作为可以被普通用户 Handler 替换的 Effect operation。它们拥有独立的 ownership 和清理不变量。

## 9. Cown 与共享状态

Joky 的 `class` 仍然是唯一所有权的可变堆对象，不能隐式复制或共享。需要跨 branch 访问可变状态时，显式将对象 move 到 `Cown(T)`：

```joky
let counter = Cown.new(Counter(value: 0))
```

`Cown(T)` 是可复制 capability，不是直接的共享引用。所有 Cown 由创建时的当前
区域拥有，函数和任务继承区域；入口有隐式根区域。`region { ... }` 显式建立
更短的内存区域及任务边界，退出时先排空工作，再逐个析构 payload、释放全部 Cown。
句柄复制/丢弃不做原子引用计数，最后一个句柄丢弃也不提前释放对象。

区域内允许循环引用，编译器拒绝向外带出本区域的句柄，包括经容器、闭包或辅助
函数逃逸；跨区域只能指向祖先。普通共享值继续使用原有 RC，通用 arena 分配尚未
实现。长期分配应使用较短的显式区域，详细规则和首版保守限制见
[统一 Region 生命周期与 Cown 不逃逸检查](../runtime/regions.md)。
其他堆对象的通用 arena 分配留待后续完善。

设计上 payload 不得直接拥有 file、socket、native handle、task、continuation 或 lease 等外部资源，这些资源必须由唯一所有者持有；需要时可显式关闭或取消，离开所有权图时由 runtime 自动释放。只有 `when` 可以取得 payload 的临时独占 lease，payload 用尾随闭包参数命名：

```joky
when (counter) |state| {
    state.value = state.value + 1
}
```

多个 Cown 一次显式列出：

```joky
when (source, target) |source_state, target_state| {
    transfer(source_state, target_state)
}
```

全部 Cown 都是简单名称时可以省略参数列表，payload 隐式绑定为同名局部变量。

编译器和 runtime 必须保证：

- acquire 按稳定 Cown ID 排序；
- 所有 lease 都有对应 release；
- lease 不跨挂起点、任务边界、堆存储或函数返回；
- `when` body 不启动捕获 lease 的 branch；
- 重复或嵌套 Cown 获取按语言规则拒绝或诊断；
- 正常、失败、取消和 panic 路径都释放 lease。

`when` 内直接调用 `@aborts` operation，或通过普通函数间接触发 abort 时，
离开当前 `when` 的路径先释放全部 lease，再传播失败；外层 handler 可以重新获取这些 Cown。
若直接 operation 由同一 `when` 内的局部 handler 处理，控制流仍在该 `when` 内，
外围 lease 保持有效，直到正常离开或其他退出路径释放。这里不允许通过 handler 创建捕获 payload lease 的任务。

`CownAcquire`/`CownRelease` 可以是内部 scheduler operation，但 `when` 保留为语言级结构，以便 sema 和 verifier 检查 lease 逃逸。

## 10. Plugin 与脚本热更新

Plugin ABI 只描述通用资源和 operation：

```text
provider lookup -> start -> wait/cancel -> resume -> release
```

PostgreSQL、HTTP 等协议实现留在独立 plugin；runtime 不包含业务 codec 或连接状态机。

脚本热更新使用不可变模块版本，不另写一套执行语义：可重载模块沿用 MIR → Cranelift IR，
允许 JIT 的平台用本机 JIT，禁止 JIT 的平台（iOS、主机）把同一份 IR 编为 Pulley 字节码解释执行。
首轮只支持持久状态布局不变的代码热修，新旧版本可并存；布局变化的状态迁移随后实现。
普通 AOT 宿主不使用可重载模块时无需承担 JIT 或解释器的体积和执行成本。

- 函数句柄、执行帧、continuation、callback 和析构入口持有模块版本；
- 首轮在实例停止接收新调用并排空旧工作后切换模块，不迁移活动执行栈；
- 持久状态由实例持有，在副本上进行显式版本迁移，成功后提交；
- 宿主接口、字节码或 runtime 契约不兼容时拒绝候选模块；
- 无活动引用后安全回收旧模块，具体回收算法在实施时确定。

AOT 宿主自身的原生函数替换不再属于目标。脚本状态回退不自动撤销外部
副作用，也不能恢复已经取消的任务；详细边界以阶段 10 计划为准。

## 11. 实施阶段

1. **语义基线**：完成 Rust parser/sema/Core IR、类型命名、Effect/trait 规则与测试。
2. **闭包与 Ownership**：实现捕获、CFG liveness、move/dup/drop/reuse、失败/取消清理与 verifier。
3. **基础 Cranelift**：实现 scalar、CFG、普通调用、函数表和 source map；CLI 只运行编译后的代码。
4. **Managed ABI**：完成 String、ADT、List/Map/Set/Bytes、闭包和 native handle 的 runtime ABI。
5. **Effect 与挂起 lowering**：实现 Effect set、`do ... with`、spill layout、resume entry、取消 cleanup，以及 timer/TCP/plugin operation 状态机。
6. **结构化并发**：接入 `scope`、`branch`、`race`、失败传播、取消和多 worker scheduler。
7. **Cown runtime**：实现 Cown payload、单/多 Cown acquire、lease verifier 和公平调度策略。
8. **嵌入式执行与脚本热更新**：按阶段 10 实现可重载模块的 JIT / Pulley 执行、模块版本保留、代码热修、实例状态迁移、宿主驱动调度和安全回收，不替换 AOT 宿主函数。
9. **性能与 AOT**：用同一 IR/codegen 支持 JIT 与 object emission，以端到端 benchmark 驱动优化。

## 12. 验收标准

- 所有有效 Joky 程序都能被完整 lowering 并由 Cranelift 执行；现有 CLI 的 JIT/AOT 路径不做隐式解释回退。阶段 10 的可重载模块可显式选择 Pulley 解释执行，它与 JIT 共用同一份 Cranelift lowering，受支持语义一致。
- Effect propagation、`do ... with`、resumable operation 和未处理 Effect 诊断一致。
- `scope` 等待所有 branch，失败能取消同级任务，取消后没有 task、continuation 或资源泄漏。
- `race` 正确保留 winner、清理 loser，并且不会重复释放值。
- `suspend/resume`、Cown acquire/release 和 plugin completion 没有 worker 泄漏或重复释放。
- Cown lease 不能跨挂起点或逃逸；多 Cown 访问不会因源码顺序产生死锁。
- 脚本重载不要求普通 AOT 调用增加函数表间接调用、parent-chain 查找或全局锁。
- 业务协议可以在不修改 runtime 的情况下作为 plugin 添加。
- 未实现的 lowering 在编译期给出对应源位置的诊断，而不是悄悄走另一条执行语义。
- benchmark 至少覆盖 scalar、ADT/List、scope branch、race、Cown contention、TCP 和 plugin 路径。

## 13. 非目标

本设计第一版不承诺：

- Actor、mailbox、消息行为或 Actor capability；
- 用户层 `async/await`；
- 让 Cranelift 替代 Rust runtime 的 scheduler、reactor 或 heap；
- 对所有挂起函数做零成本机器码优化；
- 自动 task/continuation 状态迁移；
- 把所有 Rust async 类型暴露给 Joky 用户；
- 让 plugin 绕过统一 runtime ABI 直接操作 VM 内部对象。
