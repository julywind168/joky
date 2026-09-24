# MIR（Mid-level IR）

MIR 位于 `src/mir/`，是 Joky 当前唯一的 CFG-oriented 中间表示。它把类型化 HIR 的表达式树降低为基本块、语句和终止指令，之后由 verifier 检查，再交给 Cranelift codegen。

`MirProgram.types` 只持有后端类型与布局 `TypeTable`。表达式类型表、局部绑定
等源码事实留在 `CheckedTypes`，导出签名和泛型模板留在模块产物的
`ModuleInterface`；这两类数据不进入 MIR。跨模块调用由显式 import stub
携带 `SymbolId`，链接器据此生成最终调用目标。

## 函数与基本块

`MirFunction` 保存参数、局部 slot、返回类型、入口 block、所有 block，以及每个 `MirValueId` 的类型和 ownership。`MirBlock` 的语句顺序是定义顺序，末尾必须有 `MirTerminator`：

```rust
pub(crate) enum MirTerminator {
    Goto { target: MirBlockId, arguments: Vec<MirValueId> },
    Branch { condition: MirValueId,
             then_block: MirBlockId,
             else_block: MirBlockId },
    Return(Option<MirValueId>),
    Unreachable,
}
```

当前没有 `Switch` terminator；枚举匹配通过 `EnumTag` 产生标签值，再用 `Branch` 逐步判断。这样后端只需处理统一的 CFG 原语。

## 控制协议

MIR 的控制转移固定为四种协议：

- `Return`：函数正常完成，只能由 `Return` terminator 产生；
- `Abort`：当前计算不可恢复地退出，`TaskAbort`、取消返回和 `TaskFailureRethrow` 必须终止当前 block，之后只允许生成清理语句；`TaskFailureClaim` 是 Abort 传播前的 payload ownership transfer 步骤；
- `Suspend`：挂起 operation 并保存 continuation，必须绑定 `Suspending` continuation；
- `Resume`：从对应 continuation 的 resume block 继续执行，每个 continuation 只有一个 `Resume` 标记。

所有 request 语句都必须绑定 continuation：`HandlerRequest` 绑定 `Normal` continuation，`ResumableRequest` 绑定 `Resumable` continuation，`Suspend` 绑定 `Suspending` continuation；挂起 operation 不得通过普通 request 绕过 `Suspend` 协议。恢复 block 只能由对应的 suspend edge 进入，避免普通 CFG 在没有 continuation 状态时执行恢复逻辑。

协议分类有两级载体：语句级的 `MirControlProtocol`（`Return`/`Abort`/`Suspend`/`Resume`，经 `MirStatement::control_protocol()` 和 `MirTerminator::control_protocol()` 查询）供 verifier 统一归类；continuation 级的 `MirContinuationKind`（`Normal`/`Suspending`/`Resumable`）由 lowering 显式标注，连同语句变体一起构成 codegen 的控制流唯一依据（见 `docs/compiler/architecture.md`）。kind 与 operation 的合法组合只由 `MirContinuationKind::matches_operation()` 一处定义，request 校验和 continuation metadata 校验都必须经过它，不得各自手写映射。

## Machine entry 能力边界

每个 `Suspending` continuation 能否在独立 machine entry 上恢复（即挂起后脱离原 worker），由 `machine_entry_blocker`（`src/mir/continuation_capability.rs`）静态判定。这是 codegen、warning 诊断和 dump 共用的唯一判定入口，codegen 不得自行推导。当前判定覆盖：

- 函数返回类型必须具备 machine-entry ABI（标量、String、class、Duration，以及字段可恢复的聚合类型）；
- resume 尾部语句限于白名单：基础运算、`Phi`/分支/循环回边、确认不再挂起的直接/方法调用、task/race/failure 语句与 managed cleanup；owned 返回类型的多 block 合流（含 `Enum`/`Option`/`Result` 的 tag-aware cleanup）已支持；同步 Normal handler request（`HandlerRequest`）及其 Normal continuation 的 `Resume` 标记已支持；`ResumableRequest` 及 Resumable/其他 Suspending continuation 的 `Resume` 标记暂不支持；abort 语句（`TaskAbort` + cleanup + `Return` 的固定块形状）在 machine entry 内直接编译，完成回调按记录的 abort operation 发布 `Aborted` 任务状态；其中 `Store` 是结构性拒绝：字段赋值的 receiver 是借用值（方法 `self` 或 `when` 绑定），借用值不能跨挂起携带，因此它不可能出现在可验证的恢复尾部；
- 直接调用不得按调用图传递触达任何可挂起函数；间接调用要求函数类型的 effect 集确认不含挂起 operation。

恢复尾部的 Normal request 依赖挂起时保存的动态 handler 链：suspend 转换会把当前 handler 链钉到 continuation 上，machine entry 派发时在 task context 作用域内重装并在退出时还原；task 路径同时保留 TaskContext 上的链条供 scheduler 使用。arm body 若再次挂起，则按同步 fallback 语义阻塞当前 entry 线程。

判定不通过的挂起点走可取消的同步 fallback。`Compiler::run_program` 会收集每个回退的静态原因并通过 CLI 以 `warning:` 前缀输出，MIR dump 以 `; continuation capabilities` 段标注每个 continuation 的能力状态。扩展 machine entry 能力时必须同步收紧该判定与 verifier 形状约束，不允许 codegen 单方面放宽。

## 语句与值

常见语句包括：

- `Const`、`Unit`、`Read`：产生基础值；
- `Unary`、`Binary`、`Call`、`MethodCall`：计算或调用；
- `Project`、`EnumProject`、`Tuple`、`Construct`、`EnumConstruct`：访问和构造聚合值；
- `Phi`：在合流 block 中选择来自前驱的值；
- `Move`、`Dup`、`Drop`、`Deinit`、`DropLocal`、`Bind`：显式表达 ownership 变化；
- `RuntimeCall`：调用 `Println`、字符串/List 等 runtime intrinsic；
- Effect request/resume、continuation spill/resume 和 cancel cleanup；
- `ScopeEnter`、`ScopeExit`、`TaskCreate`、`TaskJoin`、`TaskClaimResult`、`TaskCancel`、`RaceStart`、`RaceSelect`：显式表达结构化任务边界、结果所有权领取和 race 选择；task body 是带捕获参数的私有 MIR function；
- `TaskAbort`、`TaskFailureOperation`、`TaskFailurePayload`、`TaskFailureClaim`、`TaskFailureRethrow`：传递不可恢复 Effect 的 operation ID 和 typed payload，完成父 handler 分派与 nested scope 传播；

失败 payload 在 task group 中遵循 `Pending -> Claimed -> Cleaned` 的单向 ownership 状态机：读取 operation/payload 只允许发生在 `Pending`，`TaskFailureClaim` 或 `TaskFailureRethrow` 只能转移一次，scope 关闭时统一清理未转移 payload。取消返回同样使用 `Abort` 的终止 block 形状，先释放 task scope，再写入类型化 placeholder 并 `Return`。

Runtime 暴露版本化的 task/continuation ABI descriptor，提供 context/header 的版本、大小、关键字段 offset 和状态值；Cranelift adapter 应通过 descriptor 建立布局假设，不直接复制 runtime 私有结构体布局。
- `CownAcquire`、lease projection 和 `CownRelease`。

一个 `if` 表达式的形状大致如下：

```text
entry:   Branch(cond, then, else)
then:    ...; Goto(join, [then_value])
else:    ...; Goto(join, [else_value])
join:    Phi([(then, then_value), (else, else_value)]); ...
```

循环使用回边 `Goto`，`break` 跳到循环合流 block，`continue` 跳到条件或回边 block。`match` 的每个 arm 有独立 block；标签测试、字段投影和模式绑定都以 MIR 语句表示。

## Ownership

`MirOwnership` 当前有 `Copy`、`Owned`、`Shared`、`Borrowed`。它由语义类型表决定：值类型通常是 `Copy`，class/资源等唯一所有权对象是 `Owned`，String、普通 ADT、List 等共享值是 `Shared`；包含 class 的 struct、tuple、enum、Option 或 Result 会传播 `Owned`。MIR lowering 在读取、移动、投影和离开作用域时发出相应操作；跨分支的状态由 Phi 和边参数保持一致。

普通共享对象仍使用引用计数，唯一所有权对象执行显式 drop；Cown 是例外，它的句柄不做引用计数，payload 和 control block 由所属区域在退出时批量销毁，同一区域内的环因此无需查环回收。句柄不能逃出所属区域由 typed MIR 上的区域分析检查（见 [统一 Region 生命周期](../runtime/regions.md)）；MIR 不负责判断对象图可达性，只负责让 capability 复制、lease acquire/release 和任务边界可验证、可 codegen。Cown payload 不能直接拥有 native handle 等需要确定性释放的外部资源。

## Verifier

`MirProgram::verify`（`src/mir/verifier.rs`）对每个函数执行：

1. 从入口计算可达 block 和支配关系；
2. 检查 value 定义先于使用，且使用点被定义点支配；
3. 检查语句、调用签名、terminator 和返回值的类型；
4. 检查 Phi 只引用真实前驱，并且每个前驱提供正确类型的 incoming value；
5. 检查局部变量的 move/borrow/drop 流和聚合字段不能重复消费；按签名检查
   receiver 与参数的 ownership，并通过借用来源和活跃性拒绝重叠调用、
   借用尚待使用时消费 owner，以及修改尚被借用的 class 字段；
6. 检查任务创建、join、取消和 scope cleanup 的结构化边界；
7. 检查 continuation spill/resume 的 ownership 平衡；
8. 检查 Cown lease 不跨 suspend、任务边界或函数返回，并且每个 acquire 都有 release；
9. 检查多 Cown 的 acquire 顺序稳定，并在 CFG 合流处保持 lease 状态一致；lease 不能跨 suspend、任务创建、函数返回或闭包捕获。任务取消/abort 后由 runtime cleanup 兜底释放；
10. 集中校验控制协议：所有 request（`HandlerRequest`/`ResumableRequest`/`Suspend`）经 `verify_request_protocol`（`src/mir/verifier/effects.rs`）统一检查 operation 模式与 continuation metadata 的一致性；所有 abortive 语句（`TaskAbort`/`TaskFailurePayload`）经 `verify_abort_operation`（`src/mir/verifier/statements.rs`）确认 operation 声明为 `Aborts` 模式，错误信息统一为 `MIR {label} ...` 风格。

发现错误时返回 `Diagnostic`，不会尝试修复 MIR。只有验证通过的 MIR 才能进入 `src/codegen/`。

数据流分析按控制流顺序迭代：所有权状态使用逆后序，借用活跃性反向传播。
支配关系、可用值、已消费值和局部所有权使用按函数编号范围分配的位集合；
每个块从最近支配块继承可用定义，再加入自身定义，避免重复扫描所有支配块的语句。
异常编号仍由验证器拒绝，不会用于扩展位集合。

`local_ssa` 的初始化分析将到达定义压缩为内联状态：没有定义、单一定义、多个定义，
并保留是否包含未初始化路径。单一定义保留其身份，以区分重复到达与不同定义合流；
多个定义的具体身份不再影响初始化检查、旧值清理和 Phi 插入判断。
这样无需为每个基本块中的每个局部变量分配和复制一个哈希集合。

## 从 HIR 到 MIR

`src/mir/lower/` 负责 lowering：`functions.rs` 创建函数和局部 slot，`expressions.rs` 递归发射表达式值，`control_flow.rs` 和 `patterns.rs` 创建 block、连接 terminator 并处理模式。最后在合流点发出 Phi。lowering 过程中不会生成 Cranelift 指令，也不直接调用 runtime。MIR 另以 `ResumableRequest` 明确表示携带 continuation 的可恢复请求；abortive operation 经 `TaskAbort` 走独立的 Abort 协议校验。continuation metadata 现在显式标记 `Suspending`/`Resumable` 协议和 resume destination，防止后续 handler trampoline 与 `time.sleep` machine entry 混用。

`src/runtime/task.rs` 定义 task thunk 的稳定边界：生成的 thunk 通过 `TaskContext` 访问 capture/result storage，runtime 的固定 worker pool 管理开始、取消、完成通知、join 与 race。包含 Suspend 的函数和 machine entry 使用 heap task context/capture/result storage，其他 native 路径可使用栈存储。Cranelift 将 `Scope*` / `Task*` / `Race*` 指令连接到 `jk_task_*` ABI。每个带析构结果都有类型化 drop thunk：`parallel` 先 join 全部 arm，确认 scope 没有 failure 后才用 `TaskClaimResult` 统一领取结果；`race` 转移赢家并清理输家；scope 关闭时清理未领取的结果。`jk_task_result_pointer(group, task)` 只借用结果地址，不转移所有权，地址有效期到 group close 为止。

普通 `do ... with` 的 Normal operation 现在优先使用 runtime `HandlerFrame` request token：直接请求、跨函数请求和复杂 body 都通过 continuation request 回到最近 handler，body 与 handler 结果在显式 join `Phi` 合流，不再为普通 handler 本身物化私有 task。若 body 显式创建 `parallel`/`race` task，子 task 会从 `TaskContext` 继承当前 frame；其中的 Normal request 仍走同一 canonical continuation ABI，而不是 `TaskAbort`。未能进入该路径的非 Normal/兼容场景仍使用 `TaskAbort` 和 task failure transport。Runtime `HandlerFrame` 的每次请求拥有独立 one-shot 状态和 payload storage，多个请求可以顺序或嵌套存在；token 还可以携带请求参数、绑定 continuation，并通过 operation callback 完成 request -> resume -> dispatch 链路。token 现在通过引用计数 pin 住 frame state，即使词法 frame wrapper 已退出并释放，也不会出现悬空 frame 访问；恢复仍受 token 的一次性状态约束。

task function 的循环头会降低为 `TaskPoll`。它查询当前 task 的线程本地 cancellation token；命中时进入编译器生成的 cancellation return block，按正常 MIR scope-drop 路径释放 live owned local，随后结束 task。取消路径会标记 result storage 只是占位值，因此 race/scope cleanup 不会把零 class 指针等占位结果交给类型化析构 thunk。取消是协作式的：不包含循环或未来 suspend 点的长时间 native call 不能被强制中断。

`time.sleep(Duration)` 和其他 `@suspends` operation 都在 MIR 中降为 `Suspend`。Duration 由单 reactor 线程的 timer heap 管理；其他 operation 使用相同的 continuation 状态机等待 provider payload。任务通过固定数量的 runtime worker 执行；没有 machine entry 的路径仍是可取消的阻塞式 fallback，支持的 resume CFG 则由独立 machine entry 脱离原 worker 执行。worker 使用本地 deque、global injection queue 和 work stealing；worker 在等待子任务时会帮助执行其他 job，避免嵌套结构化 scope 阻塞整个 worker pool。

等待期间 task state 为 `Sleeping`，`join` 会把它视为未完成状态；timer/provider 完成或取消唤醒后才回到 `Running`，最后统一进入 `Completed` 或 `Cancelled`。

`Suspend` 的 destination 类型等于 operation 的返回类型，不再固定为 `Unit`。codegen 将参数和结果按 flattened ABI word 布局保存到独立的 suspend argument/result storage；provider 可以读取 pointer/size，并调用 `jk_continuation_complete_suspend_with_payload` 写入结果。`String`、class、Tuple/Struct、Option/Result/Enum 等 managed payload 会注册类型化 cleanup；恢复时所有权转移到 SSA 值并清零源指针，取消或 malformed completion 则保留原 payload 供 cleanup。除 canonical `time.sleep` 外的 operation 必须先通过 `jk_continuation_register_suspend_provider` 注册 provider；cancel hook 返回前必须停止后续 completion，未注册请求会被拒绝。

MIR 还定义了 `Resume` 标记，且每个 `Suspend` 都携带函数内稳定的 continuation id；lowering 为 `time.sleep` 创建独立的 resume block，并在函数的 continuation metadata 表中记录 operation、suspend/resume block、generation、按 local 编号的 frame slots，以及带稳定 slot 序号和 ownership 的保守推导 live spill values。每个 frame slot 还记录可解析的 suspend 前 SSA 初始化值；参数和 receiver 在 MIR metadata 中仍以 `None` 表示，但 Cranelift 会从入口环境把它们写入 frame，并在独立 resume entry 中直接恢复到 local。Cranelift 为这些 slot 计算 typed ABI 大小，在函数入口向 runtime continuation 分配 heap frame、result storage 和 spill 区，Suspend 前写入 frame/spill 并设置 resume program counter；Resume 入口通过 generation 校验进入 `Resuming`，从 frame 恢复缺失的 SSA value 并重新绑定 local，再读取 spill，返回时把当前 continuation 结果写入 result storage，随后标记 `Completed` 并释放 frame/result/spill。直接托管值以及 Tuple/Struct 固定布局中的托管叶子会注册 typed cleanup；`Enum`、`Option`、`Result` 则注册 tag-aware region cleanup，由生成的 drop thunk 只释放活动 payload。返回值从 frame 移入 result storage 时，所有可能的 source payload pointer 都会清零而保留 tag：正常完成时 frame cleanup 不会重复释放；在 transfer 与 complete 之间取消时 result region 仍可按 tag 回收 payload。同步 timer 下已有值通常仍在 native environment 中，因此恢复加载主要为未来无栈入口准备；当前仍不是完整的无栈恢复。`src/runtime/continuation.rs` 提供 generation、resume entry 查询、spill pointer/size、frame pointer/size、result pointer/size、program-counter、一次性状态机、scheduler callback dispatch，以及受状态保护的 machine-entry trampoline ABI：JIT 在 finalize 后可用稳定 key 注册机器地址，scheduler 以 generation 校验触发一次 `Ready -> Resuming` dispatch，再由 trampoline 调用已绑定的机器入口。当前 Cranelift 已能为返回 `Unit`、标量、`String`、class、Tuple/Struct，以及字段本身可恢复的 `Enum`/`Option`/`Result` 生成独立机器入口；其中无环、且只含已支持语句的 resume 子 CFG 可以包含 `Goto`、条件 branch 与 Phi/block 参数。入口可恢复 frame locals，并执行常量、读局部、基础一元/二元运算、投影、enum tag/project、已验证不会再次挂起的调用，以及 `Show`/`Println` 尾部。非根 task lifecycle 的 machine entry 支持见下文；再次挂起的普通调用和复杂 owned-return CFG 仍受能力限制，完整 native frame 脱离尚未完成。

独立 machine entry 支持循环回边，以及经调用图确认不会再次挂起的直接函数调用和方法调用。函数值调用会按其函数类型携带的 effect 集检查：确认不含 `suspends` operation 时，可在恢复入口执行；否则保守回退同步路径。恢复入口也能创建闭包环境。其 `TaskPoll` 查询 continuation 自身的取消标记，因此 race/父 scope 能在已恢复的循环头停止它。恢复尾部的 `Drop`、`DropLocal` 和 `Deinit` 使用与普通函数相同的 class/managed drop glue。再次挂起的普通调用和复杂 owned-return CFG 仍回退同步路径。

非根结构化 task scope 已接入 machine entry，不再单独触发 fallback。`TaskFrameLayout` 在函数最大 local frame 后追加固定的 group/task id 槽；首次 Suspend 将 native CFG 环境中的 handle 写入，恢复入口读取到 `Environment`，再次 Suspend 沿用同一布局。新建 scope/task 同步写入槽位，因此挂起前创建的任务能在恢复后 join、读取真实 typed result、claim 或 cancel；race 与 failure operation/payload/claim/rethrow 使用同一 scope 的 group。group 拥有 heap result 及其 drop callback。正常 `ScopeExit` 先清空 frame 中的 owning group slot，再释放 group；取消时 cleanup 从内向外取消、等待并释放仍存活的 group，清理未领取结果。同步恢复路径清空 frame 的 group slot，将关闭责任留给 native `ScopeExit`，防止重复释放。

Runtime 另提供 `jk_continuation_begin_sleep`：它以非阻塞方式将 continuation 置为 `Sleeping`（Pending），timer 到期后自动转为 `Ready` 并调用 machine entry 或 scheduler callback。所有 suspending 函数统一使用函数级显式 Pending 返回 ABI；同步等待回退已删除（阶段 3）。

在结构化 task 中，`Suspend` 已通过该 ABI 暴露 Pending 结果：task thunk 返回时保留 `Sleeping` 状态，machine entry 完成后由 continuation 回调 task group，最终唤醒 `join`。没有可用 machine entry 的复杂 resume body 仍回退到同步路径。普通 `do ... with` 的 Normal operation 统一通过 runtime `HandlerFrame` request：请求参数按 canonical `HandlerCall` ABI 传递，body 通过 continuation 回到最近 handler，结果在调用方继续执行；不创建私有 task，也不使用 `TaskAbort`。显式 `parallel`/`race` 子 task 会继承动态 frame。`@resumable` runtime frame 使用同一 `HandlerCall` thunk ABI，`HandlerEnv` 保存捕获字节及 ownership/drop 描述，已接入无捕获、标量、shared managed capture 和 managed owning capture（`MutList`、`MutMap`、`MutSet`、闭包及其固定布局聚合）。唯一所有权捕获从词法 local 转移到环境；handler 返回该值时会清空环境 slot，避免重复释放。class 仍走兼容的 borrowed 路径。

未处理的 `@aborts` operation 通过 `TaskAbort` 把扁平 ABI payload 复制到 task group。`parallel` 和 `race` 在等待后读取首个 failure：匹配最近的词法 handler 时，先按 operation 参数类型重建 MIR value，再由 `TaskFailureClaim` 转移所有权并跳到 handler target；没有当前匹配时使用 `TaskFailureRethrow` 把 failure 继续传给外层 task scope。每种含托管值的 payload 都有类型化 drop thunk，因此并发失败的输家、未匹配 failure 和 nested rethrow 竞争都能准确清理。传播到 root 仍未处理的 operation 会成为运行时诊断。abort 同时设置当前 task 的取消 token，task 在用户函数调用返回点和循环头停止后续执行并走 cleanup return。
