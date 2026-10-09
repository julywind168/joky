# Joky

Joky 是一个 Rust 实现的小型语言项目，包含共享类型检查、Cranelift JIT/AOT、
寄存器字节码解释器、CPS 挂起运行时和内置 `joky/vm` 模块。
目前适合语言实现和嵌入实验，已实现范围与限制见下文。

## 快速开始

需要 Rust 1.97+；当前开发验证使用 Rust 1.99.0，macOS arm64 已通过 full、stress 与 Godot 集成检查。
原生链接及集成测试需要系统 `cc`（目前验证 macOS/Linux）。

```sh
cargo build --release
./target/release/joky run examples/hello.jk
./target/release/joky run --interpreter examples/loops.jk
./target/release/joky build examples/hello.jk -o /tmp/hello
/tmp/hello
```

两次源码运行依次输出 `42`、`45`，生成的 `/tmp/hello` 输出 `42`。最小程序：

```joky
fn main() {
    println(42)
}
```

- `run` 默认使用 JIT，无需 C 编译器；`run --interpreter` 不生成机器码。
- `build` 使用 AOT 与系统 `cc`，生成可独立执行的程序。默认输出位于源码旁，去掉 `.jk`；
  Windows 的文件名后缀为 `.exe`，但其原生链接尚未验证。
- `-o` / `--output` 可指定产物，父目录须已存在。失败构建保留已有产物，不能覆盖源码。
- 默认 prelude 提供 `Option` / `Result`、`Some` / `None` / `Ok` / `Err`、借用式状态判断 / 默认值 / 组合方法、整数方法与边界常量，以及 `print` / `println`。
  [Option / Result 示例](examples/option_result.jk)展示 `unwrap_or`、`and` / `or` 和 `ok` / `err` 转换；普通默认值急切求值，`unwrap_or_else` 可按需执行同步回调。
  `run` / `build` 均可用 `--no-prelude` 关闭；`joky/array` 和 `joky/vm` 仍需显式导入。
- CLI 入口必须无参数，返回 Unit 或 Int32；支持同步 main 和 `fn main() @suspends`。
  Unit 对应退出码 0，Int32 取低 8 位；`joky/io` 的 `print` / `println` 借用实现 Show 的值，
  可直接写 `println(value)`、`println(42)`，挂起函数中也能直接打印标量；字符串字面量默认插值，`"value={value}"` 生成拥有型文本。
  关闭 prelude 时显式导入 `joky/io.{print, println}`；CLI 不再预注册类型专用打印宿主。
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
`cargo install --path .` 只安装 CLI；包含数组、arena 分配、除法、外部调用（包括通用文本输出）、CPS 或 `joky/vm`
的原生构建还需要匹配的 runtime archive，用于错误传播及运行时支持。
构建时依次查找 `JOKY_RUNTIME`、CLI 旁的 `libjoky.a`、安装目录 `lib/joky/libjoky.a`。
生成的程序运行时不再需要源码、CLI 或 archive。

Cargo 包、Rust crate 和命令行均为 `joky`，源文件后缀为 `.jk`。
`cargo build --release` 还生成 `libjoky.rlib` 和 `libjoky.a`；Rust 嵌入方通过 Cargo 依赖消费库。
宿主文本统一使用显式 `JokyText` 指针/长度描述符，旧长度前缀 ABI 已移除；
已有绑定需按[文本 ABI 迁移说明](docs/architecture.md#宿主与字符串-abi)更新并重新编译。
当前保持单 crate，静态库包含编译器、VM 和 runtime；Release 配置见
[构建与验证](docs/architecture.md#构建与验证)。

## 当前能力

| 组成 | 已实现 |
| --- | --- |
| 语言 | Int8/Int16/Int32/Int64/UInt8/UInt16/UInt32/UInt64 八种整数与上下文字面量定型、窄整数真实字节布局与 `#repr(C)`/C 引用、IEEE 754 Float32/Float64 与浮点字面量（NaN/无穷/有符号零/次正规数，常量折叠按目标精度）、显式浮点转换 `float32`/`float64`、IEEE 数学原语与 ties-to-even round、受检 trunc/round 浮点转整数、to_bits/from_bits 与 to_text 格式化、浮点 extern/`#repr(C)`/C 引用与混合签名、Unicode 标量 Rune 与字符/Unicode 转义字面量、Bool、Unit、不可变拥有型 String、显式 clone 与借用查询、StringBuilder 与消费式 finish、静态 Show 格式化与 f 字符串插值、严格 UTF-8 转换与只读字节视图、复制式子串、Rune 遍历、拼接及整数转文本、泛型透明类型别名、泛型名义记录与嵌套字段、泛型枚举、穷尽 match 与共享/独占借用匹配、Copy 元组与解构绑定、泛型函数与编译期实例化、固有方法与泛型 impl、方法独立类型参数、静态 trait 默认方法、关联类型与投影、关联能力及等式约束、同步借用型 dyn 三后端分派、关联类型绑定与显式重借用（[验收记录](docs/borrowed-dyn-trait.md)）、关联常量、模块级标量常量（`const` / `pub const`、跨模块限定与选择导入、统一依赖求值）、记录字段默认私有与显式 `pub`、记录解构（let/var 与 match 载荷的重命名、忽略、嵌套、共享借用及泛型实例模式）、let/var、赋值、四则与一元运算、位运算与受检移位、进制字面量、显式整数转换、比较、短路逻辑、if/else、while、return、break/continue、分号序列、换行表达式边界与跨行方法链、块作用域、同步隐式 arena、受检共享/独占引用、共享引用字段与只读递归对象图、`Array<T>`、数组方法与 `replace/swap/swap_remove/remove/insert/truncate/clear`、同步 `List<T>`、两端与位置增删、反转、元素借用及批量转移、同步 `Set<T>`、借用查询、只读元素与消费式集合运算、同步 `Map<K, V>`、String 拥有型 key 与借用查询、参数级借用不逃逸推导与可选 `@noescape` 契约、同步匿名函数与借用/move 闭包、Fn/FnMut/FnOnce 泛型约束、元素借用、拥有型与共享引用 value、拥有记录与 Drop、受检解引用 `*p` / `*p = value`、`joky/mem` 的 `replace/swap/drop` 与 `Option.take()`、拥有型数组元素与切片借用及受检容器所有权 |
| 执行 | JIT、AOT、Interpreter；共享语义分析，分别生成机器码或字节码 |
| 挂起 | 无引用拥有值及整个对象的共享/独占借用跨挂起、`@suspends`、`parallel` / `race` 分支轮询、顺序 `scope` 主体与 `branch` 动态子任务、取消、失败传播、旧代码保活、CLI 入口与定时器 |
| 嵌入 | Rust `VmEngine`、C VM/CPS API、显式宿主注册（含浮点与混合标量签名）、同步 C 文本/NUL/外部释放适配、结构化诊断和 opaque 类型 |
| Godot 原型 | G0–G2：Node 挂载 `.jk`，解释器状态、属性与场景保存、受检 Node API、帧回调、所属任务与引擎等待；TCP 缓冲与消息生命周期已联合验收；[构建和示例](integrations/godot/README.md) |
| 网络 | 共享 I/O reactor、IPv4/IPv6 TCP、完整写入、有界接收缓冲、读满/分隔符/UTF-8 行、四字节大端长度前缀消息；CLI/JIT/解释器/AOT 与 Godot 联合验证 |
| 标准库 | 默认 prelude、option/result/integer/float/array/list/set/map/string/show/io/eq/hash/check 源码库、模块及成员别名；`joky/vm` 创建、加载、整数与浮点调用、重载、关闭和错误查询 |

支持以入口目录为根的用户文件模块、`pub` 与显式内存源码集，见[用户文件模块](docs/language.md#用户文件模块)和
[多文件示例](examples/modules/main.jk)。尚无包管理或多线程调度。显示与插值见 [Show 示例](examples/show.jk)，默认方法与关联类型见 [Source 示例](examples/trait_sources.jk)，文本操作见 [String 示例](examples/string_queries.jk)，String/Builder 已支持同步聚合、泛型与容器 value，批量文本见 [join/split 示例](examples/string_collections.jk)，C 文本边界见 [Rust 注册示例](examples/text_ffi.rs)及 [C 适配示例](examples/text_ffi.c)。
CLI 可驱动挂起 main；`joky/task` 提供定时等待和主动让出。外部宿主事件仍由嵌入方驱动。
解释器执行限额不覆盖编译阶段或原生回调。
三个后端均支持整数除零失败、位宽环绕和无符号比较/除法；调用轨迹及资源约束的差异见
[已知边界](docs/architecture.md#已知边界)。

## 示例与文档

- [借用型 dyn 示例](examples/borrowed_dyn.jk)：两个具体实现、默认/覆盖方法、泛型擦除与共享/独占重借用；三后端输出 21、42、22、42。
- [匿名函数与闭包示例](examples/anonymous_functions.jk)：同步捕获、可变闭包、消费式调用与 Option 回调；首版环境限同步局部使用。

- [语法与类型系统设计](docs/syntax.md)：v0.1 核心语法、完整类型目标、所有权规则与分阶段实施顺序。
- [parallel、scope 与 branch 开发计划](docs/structured-concurrency-scopes.md)：现有 scope 改名 parallel、顺序主体与动态子任务的生命周期和分阶段验收；首版已完成并验收。
- [Arena、隐式生命周期与 FFI 设计](docs/arena-borrowing-ffi.md)：无 GC 的内存归属、借用、结构化并发和分阶段验收。
- [持久实例、数值类型与 Godot 脚本接入计划](docs/persistent-instances-numerics.md)：跨调用状态与解释器脚本接入的分阶段验收；P0–P4 已实现 Rust 持久实例和宿主驱动任务（JIT/解释器，含 String、Array、Map、StringBuilder 与 Drop 拥有状态），非阻塞嵌入示例见 [host_tasks.rs](examples/host_tasks.rs)；G0–G2 已接入 Node 的解释器脚本、基础属性、受检 Node API、帧回调与所属异步任务/引擎等待，示例见 [async.tscn](integrations/godot/demo/async.tscn)；数值线 N0–N4 已完成（浮点核心、数学原语、受检转换、数值嵌入与窄整数补齐）。
- [Map 分阶段开发计划](docs/map-storage-ownership.md)：基于当前 Array 的哈希容器设计、AI 目标与验收条件（M0–M7 已验收，首版完成）。
- [List / Set 开发计划](docs/list-set-storage-ownership.md)：基于 Array 的可变双向链表与基于 Map 的哈希集合；LS0、List L1–L4、Set S1–S3 与联合开发验收已完成；[List 示例](examples/lists.jk)覆盖位置增删、反转和保持顺序的批量转移；[Set 示例](examples/sets.jk)覆盖去重、借用查询、并集与导出。
- [网络运行时开发计划](docs/network-runtime.md)：NET0–NET4 已提供共享驱动、`joky/tcp` 公共模块、write_all、连接额度、有界 BufferedTcp 接收缓冲、读满/分隔符/UTF-8 行与长度前缀消息；真实背压、协议取消和 Godot 联合验收已完成。[消息回声示例](examples/tcp_messages.jk)以有限回环服务展示空消息、UTF-8 与 EOF。
- [语言与内置模块](docs/language.md)：语法、求值顺序、`parallel` 和 `joky/vm` 接口。
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
./scripts/check.sh         # 默认快速检查：Debug 测试与短输入基准正确性
./scripts/check.sh --full  # 兼容别名：与默认 Debug 检查相同
./scripts/check.sh --stress # 独立压力检查：Debug 穷举与长输入基准正确性
./scripts/check.sh --release # 按需显式使用 Release
```

非公开开发期间默认只跑 Debug；明确要求发布验收时运行 `--full --release` 和 `--stress --release`。
quick/full 检查包含格式、Clippy、测试及三后端基准正确性，不计时、不判断性能回归。
要求和范围见 [CONTRIBUTING.md](CONTRIBUTING.md)。性能测量另用
`cargo bench --bench while`，工作负载、调用边界及统计口径统一见[基准方法](benches/README.md)。

## 参考与许可

[Cranelift 文档](https://docs.rs/cranelift/) ·
[Cranelift 源码](https://github.com/bytecodealliance/wasmtime/tree/main/cranelift) ·
[System V ABI](https://refspecs.linuxbase.org/elf/x86_64-abi-0.99.pdf)

MIT / Apache 2.0 双许可。
