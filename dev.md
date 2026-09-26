# Joky VM 互操作：架构与开发计划

这是独立验证项目，用最小语言集验证 AOT/JIT 调用、跨边界挂起和代码版本保活。
验证通过后将核心机制整合到 Joky 主仓库的阶段 10。
运行步骤及嵌入示例见 [README.md](README.md)，编码规范见 [CONTRIBUTING.md](CONTRIBUTING.md)。

## 当前边界

- 已实现 Int32/Bool/Unit、函数互调、块作用域、let 绑定及类型注解检查。
- `@suspends` 使用 CPS 帧和状态机，支持嵌套调用、重复 Pending、取消及错误传播。
- 普通 ABI 支持 Int32、Bool 和省略 Unit；命名宿主类型暂按 64 位不透明值传递。
- VM 的安全调用便利接口支持 Int32 参数，返回类型按具体方法限制为 Int32/Unit。
- String 字面量和函数签名在编译时拒绝，解析器保留字符串 AST；所有权和字符串 ABI 尚未定义。
- `scope` 仅能解析，尚无 lowering；不包含真实 I/O reactor、多 worker、闭包或完整 C VM API。
- 四则运算同优先级、左结合；需要其他结合方式时使用括号。
- 整数越界、非法字符、未闭合字符串以及语法错误返回带行号的 `parser::Diagnostic`。

## 编译与运行结构

```text
Source
  -> parser::parse -> AST
  -> compiler::ir  -> typed linear IR + function/frame metadata
  -> compiler::lower
       -> direct SSA values
       -> CPS frame slots + resume states
  -> JITModule    -> immutable CompiledModule -> VmEngine / Continuation
  -> ObjectModule -> object file             -> native linker
```

`compiler` 和 `jit` 是内部模块。公开入口是 `parser`、`aot::AotCompiler`、
`vm::VmEngine`、`runtime`，以及根模块的 `FunctionSignature` 和 `Type`。

### 语义检查只保留一套

`compiler::ir` 统一检查函数名、参数、返回值、调用类型和 let 注解，生成线性的已定型操作序列。
JIT 与 AOT、普通与可挂起函数都消费相同 IR，避免不同路径产生不同语言语义。

Unit 不需要机器值：普通 ABI 省略 Unit 参数和返回值；CPS 帧保留参数的原始位置。
块作用域和变量遮蔽在 IR 规划时处理，lowering 不再遍历 AST 或重新推导类型。
String 尚无有效 ABI，禁止把编译进程中的地址嵌入目标文件。
`jk_cps_` 和 `__jk_cps_` 前缀保留给 runtime，用户不能定义或导入同名函数。

Cranelift 的编译上下文是一次编译的临时资源，不由 continuation 保留。
两个后端仅负责平台初始化、符号绑定、产物封装和输出。
一个 `AotCompiler` 接受一次编译尝试；失败后不能输出对象，也不能继续追加模块。

### 编译产物与版本

内部 `CompiledModule` 保存：

- 一个 `ModuleId`；
- 每个导出的入口、完整签名、挂起标记和帧大小；
- JIT 可执行代码所有者；
- 生成代码依赖的动态库所有者。

`VmEngine` 只保存当前编译产物和宿主注册表，没有可修改 AST 或第二张版本化入口表。
只有加载成功后才发布新模块并递增版本。词法、类型、宿主绑定和编译错误均保留旧模块。
`function_signature` 返回签名副本，外部修改不会影响调用前的 ABI 检查。
`ModuleId` 标识同一 VM 的加载版本，不是跨 VM 的全局唯一 ID。

Continuation 通过 `Rc` 持有创建时的编译产物，重载和 VM 销毁不会使旧入口失效。
最后一个所有者释放后回收 JIT 可执行内存，随后释放动态库依赖。
当前使用单线程所有权，不提前加入 Send/Sync 或多 worker 抽象。

### 宿主注册边界

三个注册方法均为 `unsafe`，要求提供完整 `FunctionSignature`：

```text
register_fn(name, signature, pointer)
register_suspending_fn(name, signature, entry)
register_aot_suspending_fn(name, signature, entry, frame_slots, owner)
```

调用方保证真实入口 ABI、代码生命周期以及不向生成代码展开栈。
VM 在加载时核对 extern 声明的参数类型、数量、返回值和挂起约定，不能通过错误声明改变 ABI。
注册信息无法自动证明裸函数指针的实际类型，unsafe 契约仍由嵌入方负责。
直接函数的 Unit 参数在机器 ABI 中省略；命名类型当前使用 64 位整数表示。

同名注册替换后续编译的绑定；已经编译的模块继续使用旧入口及其所有者。
只把当前源码实际声明的宿主依赖交给编译器。
静态宿主代码通过前两个方法注册，动态 AOT CPS 入口使用第三个方法保活。

### 唯一 continuation 协议

入口签名为 `unsafe extern "C" fn(*mut CpsFrame) -> i32`：

- `0`：完成；生成入口同时把自身状态设为 `DONE`。
- `1`：Pending；下次继续轮询尚未完成的子调用。
- 其他值：失败；任务保留原始错误。

帧保存参数、局部值、表达式临时值及一个活动子帧。Int32 使用槽位低 32 位，Bool 使用低 8 位。
恢复不会重算参数或重放此前副作用。原有 Native 字节帧协议已删除。
`ContinuationStatus` 为 Pending、Completed 或 Failed；结束后拒绝再次 resume。
`cps_result()` 只在成功完成后暴露返回位模式。

静态原生回调只获得参数槽位，进度放入 state/host_data；不得改变 slots 大小或保留帧指针。
生成的 AOT CPS 函数需要完整帧大小：输出对象前读取 `cps_frame_slots(name)`，
注册时同时提供该值和动态库所有者。跨 AOT 模块编译时用 `compile_with_import_layouts` 传入导入布局。

C 宿主通过 [include/jk_cps.h](include/jk_cps.h) 使用不透明帧；不能仿造 Rust Vec 布局。
JIT 和 AOT 共享同一 runtime，导出帧管理函数及三个编译器 helper。
[examples/cps_runtime.c](examples/cps_runtime.c) 可与 Rust staticlib 链接运行。

### 调度器

`Scheduler` 为单线程协作式调度器：

- `spawn(closure)` 保存 FnOnce，`run_all()` 按入队顺序执行。
- `suspend` 返回递增且不复用的 `TaskId`；ID 属于创建它的调度器。
- `BTreeMap<TaskId, Continuation>` 保存仍需管理的记录，`resume_all` 按 ID 顺序轮询。
- `take_completed`、`take_failed`、`cancel` 删除记录，不保留历史空槽。
- 完成或失败的任务在收取前仍存在；`task_count` 包含它们，`suspended_count` 只计算 Pending。
- `resume_all` 遇到第一个错误即返回；错误留在任务状态中，可按 ID 查询和收取。

暂不保留未使用的 Task/Priority、分配器占位接口或单独的 FFI 占位模块。
未来功能应有实际调用方后再增加抽象。

## 验证场景与后续阶段

| 场景 | 当前证据 | 后续工作 |
|------|----------|----------|
| 基础数据交换 | JIT 便利接口与 AOT 对象链接执行 | 完整 AOT 宿主调用 C VM API、String 所有权 |
| VM 调用原生代码 | 显式注册、签名检查和返回值测试 | 更完整的嵌入接口 |
| 跨边界挂起 | 真实 JIT → AOT 动态库 → 原生回调，使用同一 runtime | 扩展事件源 |
| scope 并发 | 单线程手动就绪通知和任务管理 | 阶段 6：scope lowering、调度语义，再评估多 worker |
| 代码热替换 | 失败加载回退、旧 continuation 保活和模块回收 | 阶段 7：完整场景验证；不做状态布局迁移 |

阶段 1–5 的当前子集已实现。阶段 6/7 按上述边界推进；阶段 8 补充性能基线、
内存检测和最终整合报告。性能目标和多线程安全需要实际测量，不能从功能测试推断。

## 验证要求

```sh
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --release
cargo run --quiet
```

测试覆盖统一语义、诊断、完整签名、状态分派、Bool/Unit、嵌套挂起、副作用顺序、
错误传播、取消、重载及 10,000 次任务收取/取消后的记录释放。
AOT 测试链接并运行实际生成对象。动态库测试中的 C 桥只转发到当前 Rust runtime，
不实现第二套帧管理逻辑；同时验证最后一个任务释放后动态库所有者消失。
这些测试验证功能和所有权行为，尚未替代专门的内存检测或性能测试。

依赖 Rust 1.96+、系统 `cc`。Cranelift 统一约束为 `0.136` 系列，
精确依赖版本由 Cargo.lock 固定；升级时一起验证 JIT、AOT、ABI 和 release 构建。
Unix 平台执行 AOT 链接/动态库集成测试；其他平台的链接步骤需另行验证。
