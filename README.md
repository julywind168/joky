# Joky

Joky 语言的种子实现，包含编译器、命令行工具、运行时和内置标准库。
从可运行的小语言开始，逐步完善语法、类型系统、标准库与嵌入能力。

## 项目命名与产物

| 用途 | 名称 |
|------|------|
| Cargo 包、Rust crate、命令行 | `joky` |
| Rust 库 | `libjoky.rlib`（通过 Cargo 依赖方式消费，安装脚本不部署） |
| 原生静态库 | `libjoky.a` |
| Joky 源文件 | `.jk` |
| 内置模块 | `joky/vm` |
| C 头文件 | `joky_vm.h`、`joky_cps.h` |
| C API | `joky_vm_*`、`joky_cps_*`，类型使用 `Joky` 前缀 |

`cargo build --release` 从本仓库生成 `target/release/joky`、`libjoky.rlib` 和 `libjoky.a`。
当前采用单个 Rust crate，静态库包含编译器、VM 和 runtime，供标准库与嵌入程序链接。
Release 构建配置与体积取舍见 [dev.md](dev.md)。

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
`-o` / `--output` 指定输出文件，父目录须已存在；编译或链接失败会保留已有输出文件，
输出路径不能指向输入源码。

需要直接验证解释器时可以运行：

```bash
joky run --interpreter hello.jk
```

`--interpreter` 只适用于 `run`，不会生成或执行机器码。

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

Rust 嵌入、JIT/AOT/Interpreter 执行和挂起调用的示例见 [examples/embedding.rs](examples/embedding.rs)：

```bash
cargo run --example embedding
```

## 可变变量与循环

`let` 声明不可重新赋值的绑定，`var` 声明可重新赋值的绑定；两者都必须初始化，
支持类型推导或显式注解。赋值不能改变绑定的类型，函数参数不可重新赋值。

```joky
fn sum(n: Int32) -> Int32 {
    var i: Int32 = 0
    var total = 0
    while i < n {
        total = total + i
        i = i + 1
    }
    total
}
```

`while` 每轮先求值 `Bool` 条件，为 false 时退出。循环体有独立块作用域，
可以修改外层 `var`，内部同名声明会遮蔽外层绑定。声明、赋值和 `while` 的结果
都是 `Unit`；循环体最后一个表达式的值会被丢弃。

Int32 支持 `==`、`!=`、`<`、`<=`、`>`、`>=`，结果为 `Bool`。比较运算优先级
低于算术，高于赋值；现有四则运算仍同优先级、左结合，改变结合方式需使用括号。
比较操作数必须都是 Int32，不支持链式比较、Bool 或 String 比较。
表达式和函数实参从左到右求值，后续赋值不会改变已经读取的值。

JIT、AOT 和解释器支持相同语义。`@suspends` 函数的循环条件及循环体都可以挂起，
恢复时继续当前调用，不重复执行此前的赋值或副作用；循环中的 `scope` 每轮创建新的分支。
循环本身不会自动挂起。目前未提供 `break`、`continue`、`+=`。

[examples/loops.jk](examples/loops.jk) 计算 0 到 9 的和，输出 `45`：

```bash
cargo run -- run examples/loops.jk
cargo run -- run --interpreter examples/loops.jk
```

### 分号与表达式序列

`;` 同时用于分隔语句和连接表达式：`a; b` 从左到右执行，丢弃 `a` 的值，
以 `b` 的值和类型作为整个序列的结果。它的优先级低于赋值。
括号用于将序列作为一个表达式，例如放进循环条件：

```joky
var checks = 0;
var bodies = 0
while (checks = checks + 1; checks < 4) {
    bodies = bodies + 1;
    999
}
```

结束时 `checks` 为 4，`bodies` 为 3。同一行可以写 `var x = 1; x = x + 1;`，
也可以继续使用原有的换行写法，分号与换行可以混用。
换行不强制结束表达式，例如 `1 +` 换行接 `2` 仍是一个表达式；`;` 显式结束前一个表达式。

尾随分号会丢弃最后一个值：`(1; 2)` 的结果是 Int32 的 2，`(1; 2;)` 和 `{ 42; }`
的结果是 Unit。因此循环条件最后不能加分号，否则会得到非 Bool 条件错误。
`let x = 1; next()` 中分号结束声明；要把序列放进初始值或赋值右侧，使用
`let x = (prepare(); value)` 或 `x = (prepare(); value)`。

序列及圆括号不创建新作用域，声明保留在当前作用域；只有 `{ ... }` 引入块作用域。
实参之间继续用逗号分隔，序列实参可写为 `f((prepare(); value), other)`。
块内允许空分号，顶层 import、extern 和函数声明后也可加分号。
`@suspends` 序列恢复时不会重放此前的表达式。

[examples/sequences.jk](examples/sequences.jk) 演示该循环，依次输出 `4`、`3`。

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
`vm.create_interpreter()` 创建不生成机器码的解释器 VM；它适合受限平台和验证解释执行语义。当前模块中的同步 Int32 函数可以作为宿主函数注册：

```joky
fn host_add(a: Int32, b: Int32) -> Int32 { a + b }
let status = vm.register_int32(engine, "host_add", host_add)
```

`vm.register_int32` 支持零到两个 `Int32` 参数；被加载的源码用同名 `extern fn` 声明即可调用。
该接口仅限 JIT 模式的 VM，解释器模式暂不能从加载源码调用注册函数。
注册会保留当前 JIT 模块的代码所有权，直到目标 VM 关闭或替换该宿主注册。

| 接口 | 返回值与行为 |
|------|--------------|
| `vm.create()` | 创建默认 JIT `vm.Handle` |
| `vm.create_interpreter()` | 创建解释器 `vm.Handle` |
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
当前仅支持内置模块 `joky/vm`，尚无文件模块搜索、导入别名或标准库异步调用；
同步 `Int32` 宿主函数可通过 `vm.register_int32` 注册。

`@suspends` 函数可以用 `scope` 并发等待多个 `Unit` 分支：

```joky
fn main() @suspends {
    scope {
        | fetch_left()
        | fetch_right()
    }
}
```

每个分支必须是 `@suspends` 的函数调用；scope 轮询所有未完成分支，全部完成后继续执行。
分支失败会停止后续轮询并释放其他分支。

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
[examples/vm_runtime.c](examples/vm_runtime.c)。

## 运行测试

```bash
cargo test
```

完整的提交前检查清单见 [CONTRIBUTING.md](CONTRIBUTING.md)；
测试覆盖范围与验证要求见 [dev.md](dev.md)。

## 项目结构

```
src/
├── lib.rs               # Public embedding API and staticlib runtime
├── main.rs              # Thin joky executable entry
├── cli/
│   ├── mod.rs           # Arguments, program entry checks and JIT execution
│   └── build.rs         # AOT compilation and native executable linking
├── core/                # Future joky-core crate boundary
│   ├── mod.rs
│   ├── error.rs        # Structured embedding errors
│   ├── types.rs        # Registered opaque host types
│   ├── parser/
│   │   ├── mod.rs       # parse(source) -> Result<Module, Diagnostic>
│   │   ├── lexer.rs     # Fallible tokenization with line numbers and byte spans
│   │   └── ast.rs       # Syntax and type annotations
│   ├── compiler/
│   │   ├── mod.rs       # Shared compilation pipeline
│   │   ├── ir.rs        # Typed slot IR, control flow and frame planning
│   │   └── lower.rs     # Direct SSA and CPS state-machine emission
│   ├── resolver.rs       # Built-in import and call resolution
│   └── signature.rs     # Public function signatures
├── backends/            # Future backend crate boundary
│   ├── mod.rs
│   ├── jit/mod.rs       # Immutable compiled exports and code ownership
│   ├── aot/mod.rs       # Object emission through the shared compiler
│   └── interp/mod.rs    # Register bytecode lowering and interpreter
├── stdlib/
│   ├── mod.rs           # Built-in binding registry and runtime ownership
│   └── vm.rs            # Managed VM handles and native standard functions
├── runtime/
│   ├── abi.rs           # Audited native function-pointer ABI wrappers
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
| 语言 | Int32、Bool、Unit、不可变 String、函数、let/var、赋值、Int32 比较、while、分号序列、块作用域、`@suspends`、`scope`、类型注解 |
| 编译器 | 共享类型检查与 IR、Cranelift JIT、原生 AOT 目标文件、解释器字节码 |
| 命令行 | `joky run`、`joky build`、独立可执行文件 |
| 标准库 | `joky/vm`：创建、加载、调用、重载、关闭、错误查询与本地函数注册 |
| 嵌入接口 | Rust API、C VM API、宿主函数注册、结构化错误和 opaque 类型注册 |
| 运行时 | CPS 挂起/恢复、协作调度、取消、代码版本保活 |

CLI 当前执行同步入口。文件模块、动态字符串、`scope`、真实异步事件源和多线程调度尚待实现。
架构、嵌入协议和后续方向见 [dev.md](dev.md)。
解释器设计和执行模型见 [docs/interpreter.md](docs/interpreter.md)。

## 代码规范

源码注释、标识符、诊断信息和测试名称统一使用英文；项目说明文档可使用中文。
详细规范见 [CONTRIBUTING.md](CONTRIBUTING.md)。

## 参考资料

- [Cranelift 文档](https://docs.rs/cranelift/)
- [Cranelift GitHub](https://github.com/bytecodealliance/wasmtime/tree/main/cranelift)
- [System V ABI](https://refspecs.linuxbase.org/elf/x86_64-abi-0.99.pdf)

## 许可证

MIT / Apache 2.0 双许可
