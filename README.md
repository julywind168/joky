# Joky

Joky 语言的种子实现，包含编译器、命令行工具、运行时和内置标准库。
从可运行的小语言开始，逐步完善语法、类型系统、标准库与嵌入能力。

## 项目命名与产物

| 用途 | 名称 |
|------|------|
| Cargo 包、Rust crate、命令行 | `joky` |
| Rust 库 | `libjoky.rlib` |
| 原生静态库 | `libjoky.a` |
| Joky 源文件 | `.jk` |
| 内置模块 | `joky/vm` |
| C 头文件 | `joky_vm.h`、`joky_cps.h` |
| C API | `joky_vm_*`、`joky_cps_*`，类型使用 `Joky` 前缀 |

`cargo build --release` 从本仓库生成 `target/release/joky`、`libjoky.rlib` 和 `libjoky.a`。
当前采用单个 Rust crate，静态库包含编译器、VM 和 runtime，供标准库与嵌入程序链接。

Release 默认优先执行性能（`opt-level = 3`），同时启用 Thin LTO、单个 codegen unit 和符号裁剪。
Release CLI 在 macOS/Linux 上链接程序时还会清理无用代码和符号；Debug CLI 保留符号便于排查。
Cranelift 只启用宿主架构及所需功能，关闭编译计时；保留 unwind，以支持 VM 的 panic 错误处理。
这些设置会增加 Release 构建时间；通过清理无用代码和符号控制体积。
LTO 可能增大包含中间代码的 `libjoky.a`；发布体积应以链接后的可执行文件为准。
`joky build` 当前没有独立的构建模式开关；发布程序请使用 Release CLI 和配套的 Release runtime。
导入 `joky/vm` 的程序会带上 JIT 编译器，因此仍会比普通 AOT 程序大。

## 环境要求

| 依赖 | 版本 |
|------|------|
| Rust | 1.96 及以上（当前 Cargo.lock 中 Cranelift 的要求） |
| cargo | 随 Rust 附带 |
| cc | 系统 C 编译器，用于原生可执行文件链接 |

macOS 上用 rustup 安装：

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

## 快速开始

构建命令行程序：

```bash
cargo build --release
./target/release/joky run examples/hello.jk
```

输出 `42`。安装编译器和配套标准 runtime 后，可在 PATH 中直接使用 `joky`：

```bash
./scripts/install.sh
export PATH="$HOME/.local/bin:$PATH"
```

默认安装到 `~/.local`，也可通过 `./scripts/install.sh /path/to/prefix` 指定目录。
`cargo install --path .` 只安装命令行程序；需要 `joky/vm` 的原生构建时，还须安装配套 runtime。

创建 `hello.jk`：

```joky
extern fn print_int(value: Int32)

fn main() {
    print_int(42)
}
```

运行源码，或编译成独立可执行文件：

```bash
joky run hello.jk
joky build hello.jk
./hello

joky build hello.jk -o hello-app
./hello-app
```

`run` 使用 JIT，不需要 C 编译器；`build` 使用 AOT 和系统 `cc`，默认在源码旁生成
同名可执行文件（去掉 `.jk`，Windows 使用 `.exe`）。生成的程序不需要 Joky 或源码即可运行。
`-o` / `--output` 指定输出文件，父目录须已存在。编译或链接失败会保留已有输出文件，
输出路径不能指向输入源码。

```bash
joky --help
joky --version
```

程序入口必须是 `fn main()` 或 `fn main() -> Int32`，不能接收参数。
Unit 返回值表示退出码 0；Int32 返回值的低 8 位作为退出码，`run` 和构建后的程序保持一致。
例如 [examples/exit_code.jk](examples/exit_code.jk) 退出码为 7。
CLI 目前提供 `extern fn print_int(value: Int32)` 输出整数。
String 支持不可变 UTF-8 字面量、参数和返回值；尚不支持插值或拼接。
CLI 只运行同步程序，`@suspends` 和自定义宿主使用 Rust 嵌入 API。

开发时可以直接通过 Cargo 运行 CLI：

```bash
cargo run -- run examples/hello.jk
cargo run -- build examples/hello.jk -o /tmp/hello
```

Rust 嵌入、JIT/AOT 编译和挂起调用的示例见 [examples/embedding.rs](examples/embedding.rs)：

```bash
cargo run --example embedding
```

Cranelift 依赖统一约束为 `0.136` 系列，具体版本由 `Cargo.lock` 固定。

## 内置标准库 `joky/vm`

直接在 Joky 程序中创建 VM、加载源码并调用函数：

```joky
import joky/vm

extern fn print_int(value: Int32)

fn main() -> Int32 {
    let engine: vm.Handle = vm.create()
    vm.load_source(engine, "fn add(x: Int32, y: Int32) -> Int32 { x + y }")
    print_int(vm.call_int32(engine, "add", 20, 22))
    vm.close(engine)
}
```

`vm.call_int32` 接收句柄、函数名和零到两个 Int32 参数。目标函数必须同步返回 Int32，
调用前检查完整签名。字符串中的花括号是普通文本，源码字符串不做插值。
当前模块中的同步 Int32 函数可以作为宿主函数注册：

```joky
fn host_add(a: Int32, b: Int32) -> Int32 { a + b }
let status = vm.register_int32(engine, "host_add", host_add)
```

`vm.register_int32` 支持零到两个 `Int32` 参数；被加载的源码用同名 `extern fn` 声明即可调用。
注册会保留当前 JIT 模块的代码所有权，直到目标 VM 关闭或替换该宿主注册。

| 接口 | 返回值与行为 |
|------|--------------|
| `vm.create()` | 创建 `vm.Handle` |
| `vm.close(handle)` | 释放句柄，成功返回 0，失败返回 1 |
| `vm.load_source(handle, source)` | 加载源码，成功返回 0，失败返回 1 |
| `vm.reload_source(handle, source)` | 替换模块，失败保留旧代码，状态码同上 |
| `vm.register_int32(handle, name, function)` | 注册当前模块中的同步 Int32 函数，成功返回 0 |
| `vm.call_int32(handle, name, ...)` | 返回函数结果；失败返回 0，并设置错误 |
| `vm.error_code()` | 当前线程最近一次操作的状态：0 成功，1 失败 |
| `vm.print_error()` | 将最近一次错误写到 stderr |

每次操作开始前清除上次错误，两个错误查询接口不清除错误。
调用结果为 0 时，通过 `vm.error_code()` 区分正常结果与失败；应在下一次操作前查询。
句柄属于创建它的线程，关闭后不会复用；复制句柄不会创建第二个 VM，使用完成后显式 close。
当前仅支持内置模块 `joky/vm`，尚无文件模块搜索、导入别名、宿主回调注册或标准库异步调用。

[examples/vm.jk](examples/vm.jk) 演示加载、调用、重载和关闭，依次输出 `42`、`100`：

```bash
cargo build --release
./target/release/joky run examples/vm.jk
./target/release/joky build examples/vm.jk -o /tmp/vm-demo
/tmp/vm-demo
```

`run` 内置标准 runtime。`build` 编译含 `import joky/vm` 的程序时，链接同一套 Rust runtime，
生成的程序自行创建 JIT，运行时无需 Joky、源码或外部 runtime 文件。当前原生链接支持 macOS/Linux。
构建时依次查找 `JOKY_RUNTIME` 指定的 archive、编译器旁的 `libjoky.a`、
安装目录 `lib/joky/libjoky.a`；安装脚本会同时部署匹配的编译器和 archive。

C 嵌入接口见 [include/joky_vm.h](include/joky_vm.h)，可运行示例为
[examples/vm_runtime.c](examples/vm_runtime.c)。String 使用借用的长度前缀 UTF-8 数据，
加载和调用期间读取，VM 不保留传入指针。字面量存储在编译模块的数据段中，随代码所有者一起释放。

## 运行测试

```bash
cargo test
```

测试覆盖带行号的解析错误、类型注解、普通/CPS 函数的 Unit 语义、CPS 状态分派、
多次挂起、临时值与局部变量保存、完整宿主签名检查、取消、任务表回收，
以及加载失败和模块替换后的 continuation 生命周期。
CLI 集成测试启动真实 `joky` 进程，验证 run/build 的输出与退出码、参数和入口诊断、
带空格路径、链接失败时保留旧产物，以及独立执行构建结果。标准库测试还覆盖句柄生命周期、失败重载、错误查询、
嵌套 VM 调用，以及 String 在暂停和模块替换后的保活。
AOT 测试会生成 `.o`，与 C 宿主链接并执行。另有真实 JIT → AOT → 宿主集成测试，
将生成的 AOT 动态库注册到 VM，使用同一个 Rust runtime 验证多次挂起、错误传播、
取消及重载/VM 销毁后的代码保活。动态库集成测试在 Unix 平台运行。

## 项目结构

```
src/
├── lib.rs               # Public embedding API and staticlib runtime
├── main.rs              # Thin joky executable entry
├── cli/
│   ├── mod.rs           # Arguments, program entry checks and JIT execution
│   └── build.rs         # AOT compilation and native executable linking
├── signature.rs         # Public function signatures
├── parser/
│   ├── mod.rs           # parse(source) -> Result<Module, Diagnostic>
│   ├── lexer.rs         # Fallible tokenization with line numbers
│   └── ast.rs           # Syntax and type annotations
├── compiler/
│   ├── mod.rs           # Shared compilation pipeline
│   ├── ir.rs            # Typed linear IR and frame planning
│   └── lower.rs         # Direct SSA and CPS state-machine emission
├── stdlib/
│   ├── mod.rs           # Built-in module resolution and typed bindings
│   └── vm.rs            # Managed VM handles and native standard functions
├── jit/mod.rs           # Immutable compiled exports and code ownership
├── aot/mod.rs           # Object emission through the shared compiler
├── runtime/
│   ├── scheduler.rs     # Cooperative tasks with monotonic IDs
│   ├── suspension.rs    # Continuation lifecycle and module identity
│   ├── cps.rs           # Shared frame ABI and C runtime exports
│   └── string.rs        # Borrowed immutable UTF-8 ABI
└── vm/
    ├── mod.rs           # Public VmEngine export
    └── engine.rs        # Host registration, checked calls and module loading
```

## 当前能力

| 组成 | 已支持 |
|------|--------|
| 语言 | Int32、Bool、Unit、不可变 String、函数、let、块作用域、类型注解 |
| 编译器 | 共享类型检查与 IR、Cranelift JIT、原生 AOT 目标文件 |
| 命令行 | `joky run`、`joky build`、独立可执行文件 |
| 标准库 | `joky/vm`：创建、加载、调用、重载、关闭与错误查询 |
| 嵌入接口 | Rust API、C VM API、宿主函数注册 |
| 运行时 | CPS 挂起/恢复、协作调度、取消、代码版本保活 |

CLI 当前执行同步入口。文件模块、动态字符串、`scope`、真实异步事件源和多线程调度尚待实现。
架构和后续方向见 [dev.md](dev.md)。

## 编译流程与嵌入接口

JIT/AOT 共用 `AST → typed IR → Cranelift` 流程，语义检查只执行一套：
函数签名、调用参数、返回值及 `let` 类型注解都在生成机器码前验证。
普通函数将 IR 值映射到 SSA；可挂起函数将值保存到 CPS 帧。
Unit 参数在直接调用 ABI 中省略，在 CPS 参数槽位中保留位置。

VM 使用不可变编译产物保存导出入口、签名、帧大小及代码所有者，编译上下文在编译完成后释放。
`load_source` 成功后才替换当前模块；失败保留旧代码、签名和版本。
`function_signature` 返回独立副本，不能修改已编译函数的 ABI。

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

三个宿主注册接口都要求显式 `FunctionSignature`，并标记为 `unsafe`：
调用方保证入口地址、真实 ABI、代码生命周期和不跨边界展开栈；加载时验证声明与注册信息一致。
同名注册会替换后续编译使用的绑定，已编译模块继续使用原绑定。
JIT 编译器内部结构不再是公共 API，嵌入方使用 `vm::VmEngine`。

## 挂起调用与运行时边界

源码中的 `@suspends` 函数采用统一入口：
`unsafe extern "C" fn(*mut CpsFrame) -> i32`。
返回 `0` 表示完成，`1` 表示仍在等待；其他值表示执行失败。
普通函数仍采用原来的直接传参 ABI，普通函数调用可挂起函数会报编译错误。

Rust 宿主通过 `register_suspending_fn` 注册非阻塞回调。
`call_async(name, &[i32])` 执行到第一个挂起点或完成；它当前支持 Int32 参数和
Int32/Unit 返回值。回调的参数保存在 `frame.slots`，返回值写入 `frame.result`，
自身恢复进度可保存在 `state` 和 `host_data`。Int32 使用槽位低 32 位，Bool 使用低 8 位。
回调返回 Pending 后，下一次恢复会再次调用同一回调，因此必须根据保存的状态继续执行。

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

编译器将参数、局部值、尚未完成的表达式临时值保存到帧中；每个挂起调用拥有独立子帧。
恢复不会重新计算此前的参数或重复执行此前的副作用。`ContinuationStatus` 明确区分
`Pending`、`Completed`、`Failed(error)`；完成或失败后拒绝再次恢复。
`cancel` 丢弃帧，`take_completed` 只取走成功结果，失败任务通过 `take_failed` 取走。
任务表使用递增 `TaskId` 和有序映射，按 ID 顺序轮询；取走或取消会删除记录，旧 ID 不会复用。
`task_count()` 包括等待收取的完成/失败任务，`suspended_count()` 仅统计 Pending 任务。
普通任务通过 `spawn(closure)` 入队，`run_all()` 按入队顺序执行。
恢复错误会保留在任务状态中；即使使用 `resume_all`，也可按 ID 查询原始错误。任务持有对应编译模块，VM 被销毁或重新加载
后，旧任务仍可恢复；释放最后一个所有者时回收该版本的 JIT 内存。

AOT `.o` 使用相同协议，公开 C 头文件为 [`include/joky_cps.h`](include/joky_cps.h)。
`cargo build --lib` 生成 `target/debug/libjoky.a`，导出帧分配、释放、
状态/结果访问接口以及编译器使用的三个 `__joky_cps_*` 符号。
C 宿主使用不透明的 `JokyCpsFrame`，不能自行定义包含 Rust `Vec` 的帧布局。
链接时还需平台所要求的 Rust native 系统库，可通过
`cargo rustc --lib -- --print native-static-libs` 查看。
[`examples/cps_runtime.c`](examples/cps_runtime.c) 提供可直接链接该静态库的 C 示例。

注册生成的 AOT 挂起函数时，先在 `write_object` 前保存
`AotCompiler::cps_frame_slots(name)` 返回的槽位数，再调用
`VmEngine::register_aot_suspending_fn(name, signature, entry, slot_count, owner)`。
`owner` 是保持 AOT 动态库及其依赖存活的 `Rc<dyn Any>`，生成的 JIT 模块会保留它。
完整帧大小包括参数、局部变量和临时值，不能仅按参数数量分配。
AOT 模块若调用其他生成模块的 CPS 导出，使用 `compile_with_import_layouts` 传入导入帧大小。
手写的静态宿主回调使用 `register_suspending_fn`，仅获得参数槽位，其他状态放入 `host_data`。

集成测试中的 C 桥接函数只把符号转发到 Rust runtime；帧分配和子帧管理均使用
同一套 runtime。`embedding` example 保留较轻量的 JIT → Rust 宿主演示，真实生成 AOT 的链路由测试验证。

每个 `AotCompiler` 只接受一次编译尝试，成功后才允许 `write_object`；失败后应创建新实例。

当前调度器为单线程协作式调度，事件就绪由调用方通知；演示使用模拟事件，不包含真实
定时器、I/O reactor 或多 worker。`scope` 尚未实现；C VM API 当前仅覆盖标准库同步操作。
String 当前是由编译模块持有的不可变文本，不支持动态拼接或插值。
词法/语法错误返回带行号的 `Diagnostic`。
四则运算仍采用同优先级、左结合的最小语法规则，复杂表达式应显式加括号。模块重载与旧挂起任务保活已支持，状态布局迁移尚未实现。

## 代码规范

源码注释、标识符、诊断信息和测试名称统一使用英文；项目说明文档可使用中文。
详细规范见 [CONTRIBUTING.md](CONTRIBUTING.md)。提交前运行：

```bash
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --release
```

## 参考资料

- [Cranelift 文档](https://docs.rs/cranelift/)
- [Cranelift GitHub](https://github.com/bytecodealliance/wasmtime/tree/main/cranelift)
- [System V ABI](https://refspecs.linuxbase.org/elf/x86_64-abi-0.99.pdf)

## 许可证

MIT / Apache 2.0 双许可
