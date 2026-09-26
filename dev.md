# Joky 架构与开发路线

本仓库是 Joky 语言的种子实现，维护可运行的编译器、命令行、标准库和运行时。
从当前语言子集持续演进，每项能力通过实际程序和跨边界测试验证。
运行步骤及嵌入示例见 [README.md](README.md)，编码规范见 [CONTRIBUTING.md](CONTRIBUTING.md)。

## 当前边界

- 已实现 Int32/Bool/Unit、函数互调、块作用域、let 绑定及类型注解检查。
- `@suspends` 使用 CPS 帧和状态机，支持嵌套调用、重复 Pending、取消及错误传播。
- 普通 ABI 支持 Int32、Bool 和省略 Unit；命名宿主类型暂按 64 位不透明值传递。
- VM 的安全调用便利接口支持 Int32 参数，返回类型按具体方法限制为 Int32/Unit。
- String 支持不可变 UTF-8 字面量、参数和返回值，使用借用视图 ABI；不支持插值、拼接或动态字符串分配。
- `import joky/vm` 提供 VM 创建、关闭、加载、同步整数调用、重载、错误查询和本地同步 Int32 函数注册；`vm.register_int32` 仅限 JIT 模式的 VM，解释器模式暂不能从加载源码调用注册函数。
- `scope { | call_a() | call_b() }` 会并发轮询 `@suspends` 的 Unit 分支；不包含真实 I/O reactor、多 worker、闭包；C VM API 目前覆盖标准库同步操作。
- 四则运算同优先级、左结合；需要其他结合方式时使用括号。
- 整数越界、非法字符、未闭合字符串以及语法错误返回带行号和 UTF-8 字节范围的 `parser::Diagnostic`；类型检查、内置模块解析和编译错误通过 `ErrorPhase` 区分，带声明范围的错误由 CLI 使用 Ariadne 输出文件、行列号和源码片段。

## 编译与运行结构

```text
Source
  -> parser::parse -> AST
  -> stdlib        -> built-in imports and typed native bindings
  -> core::compiler::ir  -> typed linear IR + function/frame metadata
  -> core::compiler::lower
       -> direct SSA values
       -> CPS frame slots + resume states
  -> JITModule    -> immutable CompiledModule -> VmEngine / Continuation
  -> ObjectModule -> object file             -> native linker
  -> bytecode     -> immutable BytecodeModule -> Interpreter / Continuation
```

源码目录已经按未来 crate 边界分组，但当前仍由单个 Cargo crate 编译：`core/` 放置 AST、解析器、共享 IR 和签名，`backends/` 放置 JIT、AOT 与解释器，`runtime/` 放置稳定的 CPS/ABI 运行时，`vm/`、`stdlib/` 和 `cli/` 分别承载嵌入、内置模块和命令行。
`lib.rs` 提供兼容性 re-export，因此现有 `joky::parser`、`joky::aot` 和 `joky::vm` 路径不变。

`compiler`、`jit` 和 `interp` 是内部模块。公开入口是 `parser`、`aot::AotCompiler`、
`vm::VmEngine`、`runtime`，以及根模块的 `FunctionSignature`、`Type`、`TypeRegistry` 和结构化 `Error`。

### 构建与发布配置

Release 采用 `opt-level = 3`、Thin LTO、单个 codegen unit 和符号裁剪。
Release CLI 在 macOS/Linux 上链接程序时还会清理无用代码和符号；Debug CLI 保留符号便于排查。
Cranelift 只启用宿主架构及所需功能，关闭编译计时；保留 unwind，以支持 VM 的 panic 错误处理。
这些设置会增加 Release 构建时间；通过清理无用代码和符号控制体积。
LTO 可能增大包含中间代码的 `libjoky.a`；发布体积应以链接后的可执行文件为准。
`joky build` 当前没有独立的构建模式开关；发布程序请使用 Release CLI 和配套的 Release runtime。
导入 `joky/vm` 的程序会带上 JIT 编译器，因此仍会比普通 AOT 程序大。

### 命令行入口

`src/main.rs` 只负责调用 CLI 和返回退出码。`src/cli/mod.rs` 处理 `run/build` 参数、
读取源码、验证程序入口与可用宿主；`src/cli/build.rs` 负责对象输出和原生链接。
不额外引入命令行框架依赖。

- `joky run file.jk` 使用 VM 的签名检查和 JIT 执行入口；`joky run --interpreter file.jk` 使用寄存器式字节码解释器。
- `VmEngine::new_interpreter()` 和 `vm.create_interpreter()` 使用寄存器式字节码解释器，不分配可执行内存。
- `joky build file.jk [-o output]` 生成与 JIT 结果一致的独立可执行文件；`-o` 指定的输出父目录须已存在，输出路径不能指向输入源码，编译或链接失败会保留已有输出文件。
- CLI 接受同步的无参数 main，返回 Unit 或 Int32，并提供显式声明的 print_int 宿主。
- AOT 使用 `with_symbol_prefix` 隔离源码符号和原生 main/libc 符号；源语言名字、诊断和元数据不变。
- C 入口负责调用 main 和整数输出；导入 joky/vm 时链接同一 Rust runtime archive，挂起程序继续通过嵌入 API 执行。
- 临时构建文件位于输出目录中，链接成功后才 rename 发布；失败清理临时目录并保留旧产物。
- Rust 嵌入和编译器 API 示例位于 `examples/embedding.rs`，通过 `cargo run --example embedding` 运行。

### 语义检查只保留一套

`core::compiler::ir` 统一检查函数名、参数、返回值、调用类型和 let 注解，生成线性的已定型操作序列。
JIT、AOT、Interpreter 以及普通与可挂起函数都消费相同 IR，避免不同路径产生不同语言语义。解释器的执行模型见 [docs/interpreter.md](docs/interpreter.md)。

Unit 不需要机器值：普通 ABI 省略 Unit 参数和返回值；CPS 帧保留参数的原始位置。
普通函数采用直接传参 ABI；普通函数调用可挂起函数在编译期拒绝。
块作用域和变量遮蔽在 IR 规划时处理，lowering 不再遍历 AST 或重新推导类型。
String 常量通过 Cranelift 数据对象和重定位生成，禁止把编译进程中的地址嵌入目标文件。
`joky_cps_`、`__joky_cps_` 和 `joky_vm_` 前缀保留给 runtime，用户不能定义或导入同名函数。

Cranelift 的编译上下文是一次编译的临时资源，不由 continuation 保留。
三个后端仅负责平台初始化、符号绑定、产物封装和输出；Interpreter 额外负责字节码调度和解释帧。
一个 `AotCompiler` 接受一次编译尝试；失败后不能输出对象，也不能继续追加模块。

### 标准库与字符串 ABI

解析器保留导入路径并识别 `vm.name(...)` 和 `vm.Handle`。内置 resolver 只接受 `joky/vm`，
注入完整类型签名，将 `vm.call_int32` 的零/一/二参数形式绑定到对应固定 ABI。
共享编译器和 VM 使用同一 native binding registry；AOT 为标准库保留固定 `joky_vm_*` 符号，
不应用用户模块的符号前缀。未知导入、重复导入和错误调用类型在执行前拒绝。

标准库 VM 通过线程本地表保存，句柄来自不复用的全局递增编号，不暴露裸地址。
执行前克隆 Rc 并释放表借用，允许被调用代码继续创建和调用其他 VM。
关闭删除表记录，活动调用仍持有所有者；同一 VM 执行期间重载会返回忙碌错误。
每次操作开始清除线程错误状态；load/reload/close 返回 0/1，调用失败返回 0 并设置错误。
error_code/print_error 不清除错误。返回 Int32 的同步调用支持零到两个参数，暂不引入 Result、数组和任务句柄。

String 为指向不可变数据对象的指针：本机字节序 u64 长度，随后为 UTF-8 字节，无 NUL 终止要求。
字面量归编译模块所有，同一模块内按内容复用；CPS 帧可保存视图，其 continuation 保活模块。
宿主读取时不转移所有权，不得保存超过所属模块生命周期的视图；标准 VM 加载接口同步解析并复制所需内容。
String 参数/返回值在生成函数之间使用相同 ABI。花括号是普通文本，不执行插值。
动态字符串分配、跨模块返回值所有权和通用字符串便利调用 API 留待后续设计。

`joky build` 在需要标准库时链接 Rust staticlib；安装脚本部署 bin/joky、lib/joky runtime 及 C 头文件。
未导入标准库的简单程序继续只依赖系统 C 链接器。运行生成程序时无需 compiler 或 archive。

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

最小的 Rust 嵌入流程——注册宿主函数、加载源码、检查后调用：

```rust
use joky::{vm::VmEngine, FunctionSignature, Type};

extern "C" fn double(value: i32) -> i32 { value * 2 }

let mut vm = VmEngine::new();
unsafe {
    vm.register_fn(
        "double",
        FunctionSignature::new(vec![Type::Int32], Type::Int32),
        double as *const u8,
    );
}
vm.load_source("extern fn double(x: Int32) -> Int32 fn f() -> Int32 { double(21) }")?;
assert_eq!(vm.call_int32_0("f")?, 42);
```

JIT 编译器内部结构不再是公共 API，嵌入方使用 `vm::VmEngine`。
调用方保证真实入口 ABI、代码生命周期以及不向生成代码展开栈。
VM 在加载时核对 extern 声明的参数类型、数量、返回值和挂起约定，不能通过错误声明改变 ABI。
注册信息无法自动证明裸函数指针的实际类型，unsafe 契约仍由嵌入方负责。
直接函数的 Unit 参数在机器 ABI 中省略；命名类型当前使用 64 位整数表示。

`TypeRegistry` 保存可用于源码声明的 opaque 类型。`vm.Handle` 和 `vm.Function` 默认注册；
`VmEngine::register_fn`、`register_suspending_fn` 和 `register_aot_suspending_fn` 会自动登记其签名中的命名类型，
AOT 使用方也可以在编译前调用 `AotCompiler::register_opaque_type`。未登记的命名类型在共享 IR 阶段拒绝。

`VmEngine` 的 Rust 调用返回结构化 `Error`，包含 `ErrorPhase`、原始消息和可选源码行号；
`Display` 保持原有 CLI 文本格式，便于现有嵌入代码迁移。

同名注册替换后续编译的绑定；已经编译的模块继续使用旧入口及其所有者。
只把当前源码实际声明的宿主依赖交给编译器。
静态宿主代码通过前两个方法注册，动态 AOT CPS 入口使用第三个方法保活。

### 唯一 continuation 协议

入口签名为 `unsafe extern "C" fn(*mut CpsFrame) -> i32`：

- `0`：完成；生成入口同时把自身状态设为 `DONE`。
- `1`：Pending；下次继续轮询尚未完成的子调用。
- 其他值：失败；任务保留原始错误。

帧保存参数、局部值、表达式临时值及一个活动子帧。Int32 使用槽位低 32 位，Bool 使用低 8 位。
恢复不会重算参数或重放此前副作用。
`ContinuationStatus` 为 Pending、Completed 或 Failed；结束后拒绝再次 resume。
`cps_result()` 只在成功完成后暴露返回位模式。

`call_async(name, &[i32])` 是执行到第一个挂起点或完成的便利入口，当前支持 Int32 参数和
Int32/Unit 返回值。通过 `register_suspending_fn` 注册的回调，参数保存在 `frame.slots`，
返回值写入 `frame.result`，自身恢复进度保存在 `state` 和 `host_data`。
回调返回 Pending 后，下一次恢复会再次调用同一回调，因此必须根据保存的状态继续执行。

静态原生回调只获得参数槽位，进度放入 state/host_data；不得改变 slots 大小或保留帧指针。
生成的 AOT CPS 函数需要完整帧大小：输出对象前读取 `cps_frame_slots(name)`，
注册时同时提供该值和动态库所有者。跨 AOT 模块编译时用 `compile_with_import_layouts` 传入导入布局。

C 宿主通过 [include/joky_cps.h](include/joky_cps.h) 使用不透明帧；不能仿造 Rust Vec 布局。
JIT 和 AOT 共享同一 runtime，导出帧管理函数及三个编译器 helper。
[examples/cps_runtime.c](examples/cps_runtime.c) 可与 Rust staticlib 链接运行；
`cargo build --lib` 生成 `target/debug/libjoky.a`，链接时还需平台所要求的 Rust native
系统库，可通过 `cargo rustc --lib -- --print native-static-libs` 查看。

### 调度器

`Scheduler` 为单线程协作式调度器：

- `spawn(closure)` 保存 FnOnce，`run_all()` 按入队顺序执行。
- `suspend` 返回递增且不复用的 `TaskId`；ID 属于创建它的调度器。
- `BTreeMap<TaskId, Continuation>` 保存仍需管理的记录，`resume_all` 按 ID 顺序轮询。
- `take_completed`、`take_failed`、`cancel` 删除记录，不保留历史空槽。
- 完成或失败的任务在收取前仍存在；`task_count` 包含它们，`suspended_count` 只计算 Pending。
- `resume_all` 遇到第一个错误即返回；错误留在任务状态中，可按 ID 查询和收取。

挂起调用经 `call_async` 进入调度器后，由调用方在事件就绪时恢复：

```rust
let task = vm.call_async("compute", &[20])?;
let id = scheduler.suspend(task);
// Resume when ready; execution may suspend again at the next call.
scheduler.resume(id)?;
if scheduler.continuation(id)?.status() == ContinuationStatus::Completed {
    let task = scheduler.take_completed(id)?;
    let result = task.cps_result();
}
```

暂不保留未使用的 Task/Priority、分配器占位接口或单独的 FFI 占位模块。
未来功能应有实际调用方后再增加抽象。

## 后续方向

| 方向 | 当前能力 | 后续工作 |
|------|----------|----------|
| 基础数据交换 | JIT/AOT 执行、joky/vm 与 C VM 接口、借用 String ABI | 动态 String 所有权与更多返回类型 |
| VM 调用原生代码 | 显式注册、签名检查和返回值测试 | 更完整的嵌入接口 |
| 跨边界挂起 | 真实 JIT → AOT 动态库 → 原生回调，使用同一 runtime | 扩展事件源 |
| scope 并发 | 单线程手动就绪通知、分支轮询、取消和错误传播 | 分支返回值、动态分支、闭包捕获，再评估多 worker |
| 代码热替换 | 失败加载回退、旧 continuation 保活和模块回收 | 扩展重载场景；状态布局迁移需另行设计 |

语言侧逐步完善表达式、控制流、文件模块与字符串能力；运行时侧按表中边界推进。
持续补充性能基线和内存检测，性能目标和多线程安全需要实际测量。

## 验证要求

```sh
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --release
cargo run --quiet -- run examples/hello.jk
cargo run --quiet -- run examples/vm.jk
cargo run --quiet --example embedding
```

测试覆盖统一语义、诊断、完整签名、状态分派、Bool/Unit、嵌套挂起、副作用顺序、
错误传播、取消、重载及 10,000 次任务收取/取消后的记录释放。
AOT 测试链接并运行实际生成对象。动态库测试中的 C 桥只转发到当前 Rust runtime，
不实现第二套帧管理逻辑；同时验证最后一个任务释放后动态库所有者消失。
这些测试验证功能和所有权行为，尚未替代专门的内存检测或性能测试。

依赖 Rust 1.96+、系统 `cc`。Cranelift 统一约束为 `0.136` 系列，
精确依赖版本由 Cargo.lock 固定；升级时一起验证 JIT、AOT、ABI 和 release 构建。
CLI 测试还验证真实进程的命令行行为、JIT/AOT 输出和退出码一致性，以及失败构建的产物保护。
Unix 平台执行 AOT 链接/动态库与 CLI 原生构建集成测试；其他平台的链接步骤需另行验证。
