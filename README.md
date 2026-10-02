# Joky

Joky 是一个 Rust 实现的小型语言项目，包含共享类型检查、Cranelift JIT/AOT、
寄存器字节码解释器、CPS 挂起运行时和内置 `joky/vm` 模块。
目前适合语言实现和嵌入实验，已实现范围与限制见下文。

## 快速开始

需要 Rust 1.96+；原生链接及集成测试需要系统 `cc`（目前验证 macOS/Linux）。

```sh
cargo build --release
./target/release/joky run examples/hello.jk
./target/release/joky run --interpreter examples/loops.jk
./target/release/joky build examples/hello.jk -o /tmp/hello
/tmp/hello
```

两次源码运行依次输出 `42`、`45`，生成的 `/tmp/hello` 输出 `42`。最小程序：

```joky
extern fn print_int(value: Int32)

fn main() {
    print_int(42)
}
```

- `run` 默认使用 JIT，无需 C 编译器；`run --interpreter` 不生成机器码。
- `build` 使用 AOT 与系统 `cc`，生成可独立执行的程序。默认输出位于源码旁，去掉 `.jk`；
  Windows 的文件名后缀为 `.exe`，但其原生链接尚未验证。
- `-o` / `--output` 可指定产物，父目录须已存在。失败构建保留已有产物，不能覆盖源码。
- 默认 prelude 提供 `Option` / `Result`、`Some` / `None` / `Ok` / `Err`、状态判断 / 默认值 / 组合方法，以及整数方法、边界常量。
  [Option / Result 示例](examples/option_result.jk)展示 `unwrap_or`、`and` / `or` 和 `ok` / `err` 转换；实参均为急切求值。
  `run` / `build` 均可用 `--no-prelude` 关闭；`joky/array` 和 `joky/vm` 仍需显式导入。
- CLI 入口必须无参数，返回 Unit 或 Int32；支持同步 main 和 `fn main() @suspends`。
  Unit 对应退出码 0，Int32 取低 8 位；`print_int` / `print_int64` 输出有符号整数，`print_uint32` / `print_uint64` 输出无符号整数。
- `import joky/task as task` 提供 `task.sleep_ms(UInt64)` 和 `task.yield_now()`；
  在挂起 main 中可直接调用，无需 async/await。运行示例：`cargo run -- run examples/async_main.jk`，输出 42。
- 使用 `joky --help` 查看参数。开发时可用 `cargo run -- run examples/hello.jk`。

## 安装与产物

```sh
./scripts/install.sh
export PATH="$HOME/.local/bin:$PATH"
```

默认安装到 `~/.local`，也支持 `./scripts/install.sh /path/to/prefix`。
脚本一起部署 `bin/joky`、`lib/joky/libjoky.a` 和 `include/joky/` 下的 C 头文件。
`cargo install --path .` 只安装 CLI；包含数组、arena 分配、除法、外部调用（包括 `print_int`）、CPS 或 `joky/vm`
的原生构建还需要匹配的 runtime archive，用于错误传播及运行时支持。
构建时依次查找 `JOKY_RUNTIME`、CLI 旁的 `libjoky.a`、安装目录 `lib/joky/libjoky.a`。
生成的程序运行时不再需要源码、CLI 或 archive。

Cargo 包、Rust crate 和命令行均为 `joky`，源文件后缀为 `.jk`。
`cargo build --release` 还生成 `libjoky.rlib` 和 `libjoky.a`；Rust 嵌入方通过 Cargo 依赖消费库。
当前保持单 crate，静态库包含编译器、VM 和 runtime；Release 配置见
[构建与验证](docs/architecture.md#构建与验证)。

## 当前能力

| 组成 | 已实现 |
| --- | --- |
| 语言 | Int32/Int64/UInt32/UInt64 与上下文字面量定型、Bool、Unit、不可变 String、泛型透明类型别名、泛型名义记录与嵌套字段、泛型枚举、穷尽 match 与共享/独占借用匹配、Copy 元组与解构绑定、泛型函数与编译期实例化、固有方法与泛型 impl、方法独立类型参数、关联常量、let/var、赋值、四则与一元运算、位运算与受检移位、进制字面量、显式整数转换、比较、短路逻辑、if/else、while、return、break/continue、分号序列、块作用域、同步隐式 arena、受检共享/独占引用、共享引用字段与只读递归对象图、同步 `Array<T>`、数组方法、拥有记录与 Drop、通用 `memory.replace` 与 `Option.take()`、拥有型数组元素与切片借用及受检容器所有权 |
| 执行 | JIT、AOT、Interpreter；共享语义分析，分别生成机器码或字节码 |
| 挂起 | `@suspends`、`scope` / `race` 分支轮询、取消、失败传播、旧代码保活、CLI 入口与定时器 |
| 嵌入 | Rust `VmEngine`、C VM/CPS API、显式宿主注册、结构化诊断和 opaque 类型 |
| 标准库 | 默认 prelude、option/result/integer 源码库、模块及成员别名；`joky/vm` 创建、加载、调用、重载、关闭和错误查询 |

支持以入口目录为根的用户文件模块、`pub` 与显式内存源码集，见[用户文件模块](docs/language.md#用户文件模块)和
[多文件示例](examples/modules/main.jk)。尚无包管理、动态字符串、闭包、I/O reactor 或多线程调度。
CLI 可驱动挂起 main；`joky/task` 提供定时等待和主动让出。外部宿主事件仍由嵌入方驱动。
解释器执行限额不覆盖编译阶段或原生回调。
三个后端均支持整数除零失败、位宽环绕和无符号比较/除法；调用轨迹及资源约束的差异见
[已知边界](docs/architecture.md#已知边界)。

## 示例与文档

- [语法与类型系统设计](docs/syntax.md)：v0.1 核心语法、完整类型目标、所有权规则与分阶段实施顺序。
- [Arena、隐式生命周期与 FFI 设计](docs/arena-borrowing-ffi.md)：无 GC 的内存归属、借用、结构化并发和分阶段验收。
- [语言与内置模块](docs/language.md)：语法、求值顺序、`scope` 和 `joky/vm` 接口。
- [架构与嵌入约束](docs/architecture.md)：模块职责、IR、代码所有权、宿主 ABI 和构建取舍。
- [解释器设计](docs/interpreter.md)：字节码优化、迭代执行、预算和运行时诊断。
- [基准方法](benches/README.md)与[历史性能记录](docs/interpreter-performance.md)：复测命令与测量边界。
- [贡献规范](CONTRIBUTING.md)：命名、unsafe 契约及提交前检查。
- [拥有值替换示例](examples/memory_replace.jk)：取出 Option 内容并替换数组资源，三后端输出 42、1、42。
- [独占借用示例](examples/mutable_borrows.jk)：原地更新数组资源与枚举载荷，三后端均输出 42、42、42。
- [共享借用匹配示例](examples/borrowed_matches.jk)：递归查看拥有型载荷后仍可消费原枚举；三后端均输出 42、42、22、20。
- [拥有型数组示例](examples/owning_arrays.jk)：只读借用元素、整体移动与自动 Drop；三后端均输出 42、20、22。
- [Rust 嵌入示例](examples/embedding.rs)：`cargo run --example embedding`。
- [循环](examples/loops.jk)、[跳转](examples/jumps.jk)、[序列](examples/sequences.jk)、[类型别名](examples/aliases.jk)、[记录](examples/records.jk)、[枚举](examples/enums.jk)、[元组](examples/tuples.jk)、[Int64](examples/int64.jk)、[无符号整数](examples/unsigned.jk)、[位运算](examples/bitwise.jk)、[整数转换](examples/conversions.jk)、[泛型函数](examples/generics.jk)、[泛型枚举](examples/generic_enums.jk)、[泛型记录](examples/generic_records.jk)、[泛型别名](examples/generic_aliases.jk)、[泛型 impl](examples/generic_impls.jk)、[泛型方法](examples/generic_methods.jk)、[受检整数方法](examples/integer_methods.jk)、[VM](examples/vm.jk) 以及
  [C VM](examples/vm_runtime.c) / [C CPS](examples/cps_runtime.c) 示例。

## 验证

```sh
./scripts/check.sh          # 完整提交前检查
./scripts/check.sh --quick  # 开发时跳过 Release 检查
```

检查包含格式、Clippy、测试及三后端基准正确性，不计时、不判断性能回归。
要求和范围见 [CONTRIBUTING.md](CONTRIBUTING.md)。性能测量另用
`cargo bench --bench while`，工作负载、调用边界及统计口径统一见[基准方法](benches/README.md)。

## 参考与许可

[Cranelift 文档](https://docs.rs/cranelift/) ·
[Cranelift 源码](https://github.com/bytecodealliance/wasmtime/tree/main/cranelift) ·
[System V ABI](https://refspecs.linuxbase.org/elf/x86_64-abi-0.99.pdf)

MIT / Apache 2.0 双许可。
