# 架构与嵌入约束

Joky 保持单个 Rust crate。公开入口为 `parser`、`aot::AotCompiler`、`vm::VmEngine`、
`runtime`，以及根模块的类型、签名和结构化错误。源码目录表达当前职责，不预设拆包计划。
使用方式见 [README](../README.md)，语法见 [语言与内置模块](language.md)。

## 模块与依赖

```text
source → core::parser → AST → core::types → core::resolver → core::ir::ModulePlan
                                                               ├─ backends::native → JIT / AOT
                                                               └─ backends::interp → bytecode

stdlib → modules::ModuleRegistry → core::resolver::BindingRegistry
vm     → registry + backends + runtime::Continuation
cli    → public parser / VmEngine / AotCompiler
```

| 目录 | 职责 |
| --- | --- |
| `src/core/` | 词法、AST、导入解析、类型/签名、诊断及后端无关 IR |
| `src/backends/native/` | JIT/AOT 共用的 Cranelift 符号声明、数据对象及 direct/CPS lowering |
| `src/backends/jit/` | JIT 初始化、入口导出和可执行代码所有权 |
| `src/backends/aot/` | AOT 状态管理、导入布局和对象文件输出 |
| `src/backends/interp/` | 字节码生成/优化、静态绑定、迭代执行和诊断 |
| `src/modules/` | 将纯声明元数据与可选 native entry、owner 组合成不可变注册快照 |
| `src/runtime/` | 原生 ABI、CPS 帧、continuation、协作调度及借用字符串 |
| `src/stdlib/` | `joky/vm` 实现、标准绑定及 JIT 地址的弱所有权索引 |
| `src/vm/` | checked embedding API、加载发布、宿主注册和执行限额 |
| `src/cli/` | 参数、入口检查、诊断渲染、原生链接及产物发布 |

`core` 的生产代码不依赖 Cranelift、运行时或具体执行后端；跨后端行为测试可以使用它们。
JIT 和 AOT 共享 `backends::native`，解释器直接消费 `core::ir`。内部代码使用实际模块路径，
`lib.rs` 只保留面向嵌入方的公开 re-export。

`BindingRegistry` 保存声明、模块路径和符号索引；`ModuleRegistry` 增加 native 实现与生命周期。
两者的区别是数据边界，不是两套解析器。resolver 直接接收同一份声明快照；没有仅作转发的
provider trait。标准库每线程初始化一次 registry，以 `Rc` 共享，裸指针和 owner 不要求 Send/Sync。

## 语义与代码生成

词法器借用原始 UTF-8 源码，以字节位置为唯一游标；关键词和数字直接从切片解析，
标识符及解码后的字符串由 AST 所有。`Span` 是半开字节范围，不保存文件 ID 或源码副本。
`parser::Diagnostic` 转为 `Error` 时统一保留 Parse 阶段、行号和范围，CLI 在最后一步渲染。

`TypeRegistry::resolve_aliases` 收集模块全部别名，迭代解析链并检测环，统一改写函数签名和
所有局部注解，保留原始源码范围；返回的 AST 不再携带别名声明。别名只使用临时映射，
不会修改宿主注册器；参数/返回值等编译后元数据只保存实际类型。所有别名都检查，即使没有使用。
VM/AOT 在内置调用重写及 ABI 检查前执行该步骤，CLI 也在检查 main 和 print_int 前执行。
直接处理 AST 的嵌入方可以调用同一个公开 API，输入 AST 保持不变。
parser 的 `Module.type_aliases` 保存独立声明及名称/目标范围，手工构造 Module 的 Rust 调用方
需要初始化新增字段。类型与值名称分开解析，类型别名不会出现在函数导出列表或对象符号中。

resolver 展开导入，并只在导入 `joky/vm` 时重写其可变参数调用形式。
`ModulePlan` 统一检查声明、调用、返回类型、赋值权限、作用域和挂起约定。
`FunctionPlan` 保存 typed slots、操作、对应源码范围、控制流标签及挂起状态。
三个后端消费相同结果，不在 lowering 中重新推导语言类型。

必须保留的语义约束：

- 可变绑定读取生成快照，后续赋值不能改变已求值的操作数或实参；求值顺序从左到右。
- Unit 不产生机器值；普通 ABI 省略 Unit 参数，CPS 参数槽仍保留原始位置。
- 普通 lowering 通过 Cranelift Variable 构造 SSA，补齐循环回边后封闭块；
  CPS 使用槽位和独立恢复状态。表达式序列只影响规划顺序，不增加执行指令。
- 恢复继续当前子调用，不重算参数或重放副作用；每轮循环重新创建其 scope 子帧。
- String 字面量通过可重定位数据对象输出，不能把编译进程地址写入 AOT 对象。
- `joky_cps_`、`__joky_cps_`、`joky_vm_`、`joky_runtime_`、`__joky_runtime_` 保留给 runtime。

解释器的局部变换、跳转/源码映射、静态适配器和帧存储见 [解释器设计](interpreter.md)。
这些优化不改变共享 IR；死值不能成为删除有错误或副作用操作的理由。

## 加载与代码所有权

`VmEngine` 保存当前不可变模块、宿主注册和类型环境。只有解析、类型检查、宿主校验和编译
全部成功后才发布替换产物并递增版本；失败保留旧入口与签名。
`ModuleId` 是同一 VM 内的加载版本，不是全局唯一编号。
公共 `function_signature` 返回副本；内部调用检查借用元数据，避免每次解释调用复制参数列表。

JIT 模块拥有导出入口、元数据、可执行内存及外部代码依赖；最后一个 owner 释放时先回收
生成代码，再释放依赖。Continuation 持有创建时的模块，重载、替换宿主注册或销毁 VM
不会使旧调用失效。编译上下文、AST 不进入 continuation。

一个 `AotCompiler` 只接受一次编译尝试。编译失败后禁止继续追加或输出对象。
registry 的固定 symbol 不受导出前缀影响；普通源码定义和 extern 可通过 prefix 隔离原生符号。
AOT 允许只有 symbol 的绑定，由系统链接器解析；VM 加载缺失 native entry 的绑定会失败。

模块注册先验证重复路径、名称、固定 symbol、direct/CPS 一致性、入口及完整帧大小，
成功后一起发布元数据、opaque 类型和 native 实现。签名声明不能覆盖既有 registry ABI。
VM 只收集源码 extern 依赖，导入内置模块时 resolver 会注入该模块的全部声明。
解释器只为实际使用的导入要求适配器；这不等于任意 ABI 都能执行。

## 宿主与字符串 ABI

Rust 嵌入示例：[examples/embedding.rs](../examples/embedding.rs)。三个宿主注册入口均为 unsafe：

| 方法 | 生命周期与布局要求 |
| --- | --- |
| `register_fn` | 指针真实 C ABI 必须匹配完整 FunctionSignature，所有调用期间有效 |
| `register_suspending_fn` | 静态 CPS 回调，只分配参数槽；进度放在 state/host_data |
| `register_aot_suspending_fn` | 提供该入口的完整 frame_slots 和保活代码依赖的 owner |

声明校验无法证明裸指针真实类型；宿主不得向生成代码 unwind、保留借用帧指针或调整 slots 大小。
同名自定义注册只影响后续编译；内置绑定名会被拒绝，避免注册成功但永远不生效。
签名中的合法命名类型自动登记；AOT 可显式注册 opaque 类型。注册入口、类型名和 CPS 帧大小
会在修改宿主表前完成校验。
默认已有 `vm.Handle` 和 `vm.Function`，非别名且未登记的类型在 IR 阶段拒绝；
别名指向未知类型时在别名解析阶段拒绝。宿主注册仍使用实际类型，源码别名不属于注册 API。

String 原生表示为本机字节序 u64 长度，随后是 UTF-8 字节，不要求 NUL 终止。
视图归代码模块或宿主所有，调用方不得持有超过其 owner 生命周期；宿主返回视图也需覆盖
所有保留它的生成调用。解释器在 native 边界使用自有缓冲区跨 Pending 保活。
更多 Rust/C 安全约束以 API 注释及 [VM 头文件](../include/joky_vm.h)、
[CPS 头文件](../include/joky_cps.h) 为准，不复制 Rust Vec 的内存布局。

标准 VM 句柄来自不复用的全局递增编号，但只在创建线程可用。
执行前保留 Rc、释放句柄映射借用，允许嵌套创建/调用其他 VM；正在执行的 VM 不可重载。
错误状态、返回码及完整内置接口见 [语言与内置模块](language.md#内置标准库-jokyvm)。
JIT/AOT Joky 函数可注册为 native entry，目标 VM 可以是 JIT 或解释器；解释器函数本身没有原生地址。

## Continuation 与调度

原生 CPS 入口为 `unsafe extern "C" fn(*mut CpsFrame) -> i32`：0 完成、1 Pending、其他值失败。
生成入口完成时还设置 `DONE`；普通 native 子回调按返回状态完成即可。
Int32 使用槽低 32 位，Bool 使用低 8 位。嵌套调用及 scope 使用运行时管理的子帧。
AOT CPS 导入必须传入 `cps_frame_slots` 的完整布局；跨对象编译使用 `compile_with_import_layouts`。

`ContinuationStatus` 区分 Pending、Completed、Failed。终态不能再次 resume，
`cps_result` 只在成功完成后可读。原生错误保留 CPS status，解释器可保留结构化错误及调用轨迹；
`call_async` 首次轮询遇到宿主失败时统一返回 HostError。
生成代码的除零失败在两个 VM 模式均返回带源码范围的 `RuntimeError`，恢复时为 `ResumeError::Failed`。
该便利接口接受 Int32 参数，结果限于 Int32/Unit。

`Scheduler` 是手动驱动的单线程协作调度器：闭包按入队顺序执行，continuation 按单调 TaskId
顺序轮询。ID 仅属于创建它的调度器。`take_completed`、`take_failed`、`cancel` 删除记录；
终态在收取前计入 task_count，只有 Pending 计入 suspended_count。resume_all 遇首个错误返回。
真实 I/O 就绪和再次轮询的时机由嵌入方提供。解释器预算见
[Resource limits](interpreter.md#resource-limits)。

## 原生错误传播

Int32 除法先检查零，再将两个操作数符号扩展到 I64 做有符号除法，最后截断回 I32。
这样 `MIN / -1` 环绕为 MIN，不执行会触发除零或有符号溢出的机器除法；不使用 signal、
longjmp 或跨生成代码的 Rust unwind。同步函数继续使用原有 C 参数及返回值 ABI，CPS 帧布局不变。

`runtime::error` 用线程局部通道保存本次调用的首个结构化错误及半开 UTF-8 字节范围。
生成代码在可能失败的调用之后、消费结果之前检查错误，并立即向上传播；失败返回的机器值
不具备语言含义，也不能作为 String 指针解引用。IR 通过调用图固定点传播 `may_fail`，
将所有外部调用保守视为可能失败；无除法和外部依赖的纯函数无需生成调用后检查。

`VmEngine`、continuation 及解释器 native 适配器的 Rust 调用边界保存外层错误、隔离嵌套调用，
退出时恢复外层状态。嵌套 VM 中已由调用者处理的失败不会污染外层执行或下一次调用。
该通道与 `joky/vm` 的公共错误查询独立。CPS 除零返回 `JOKY_CPS_RUNTIME_ERROR`，
设置 `JOKY_CPS_FAILED` 并释放子帧；scope 不再轮询后续分支。失败任务没有成功结果，不能重试。

AOT 编译后用 `AotCompiler::requires_runtime()` 判断是否需要链接 `libjoky.a`；含除法、
外部调用或 CPS 的对象需要 runtime。C 调用方按 [joky_runtime.h](../include/joky_runtime.h)
在顶层调用或每次 CPS poll 前清除错误，返回后先查询状态，再读取结果；宿主再次调用生成函数时
必须传播尚未处理的失败。跨动态库需要共享同一个 runtime，不能分别静态链接独立错误通道。
新增 helper 依赖要求旧分发环境更新配套 archive 和头文件，无需改变现有函数签名或复制帧布局。
CLI 自动链接所需 runtime；独立程序除零时报告错误和字节范围并以状态 1 退出，无需保留源码文件。

## 已知边界

- Int32 除法语义已一致；原生错误携带出错范围但没有完整语言调用轨迹，解释器仍提供
  `runtime_trace`。Span 不带文件 ID，跨模块或重载后的嵌入方必须使用对应模块的源码。
  不捕获宿主自身的机器陷阱，不能把 catch_unwind 当作机器陷阱处理。
- 解释器执行栈是平坦存储，但解析器、AST 规划和原生执行仍有递归；解释器限额不约束
  解析/编译内存、源码嵌套深度、原生回调或回调新建的 VM。当前不是不可信代码隔离环境。
- `core` 已与机器码生成分层，但单 crate 仍编译 Cranelift；没有 interpreter-only feature，
  也没有证明可在所有禁止可执行内存的平台构建。可序列化字节码也尚未定义。
- resolver 仍认识 `joky/vm` 的内置调用形式；有第二种真实模块需求后再考虑通用重写协议。
- 保持公开 API/C 符号和显式 ABI 包装，避免为减少几行代码引入宏生成或任意函数指针调用。

## 构建与验证

Release 使用 opt-level 3、Thin LTO、一个 codegen unit 和符号裁剪，保留 unwind。
CLI 的 Release 原生链接也清理无用代码/符号。LTO 可能增大含中间代码的 archive，
分发体积应看最终可执行文件；导入 `joky/vm` 会带上 JIT 编译器。
`joky build` 没有独立模式参数，要用 Release CLI 和配套 Release runtime 构建发布程序。

原生产物先在输出目录内链接，成功后 rename 发布；失败清理临时文件并保留旧输出。
安装和 runtime 查找方式见 [README](../README.md#安装与产物)。Cranelift 依赖保持同一版本系列，
Cargo.lock 固定精确版本。C 示例链接所需系统库可用 `cargo rustc --lib -- --print native-static-libs` 查询。

提交前命令和测试要求统一维护在 [CONTRIBUTING](../CONTRIBUTING.md)。
回归覆盖三个后端的类型与控制流、重复 Pending、不重放副作用、失败/取消、重载后所有权、
真实 AOT 链接和安装布局。基准只覆盖同步循环；不能代替编译、宿主调用、内存和挂起专项测量。
