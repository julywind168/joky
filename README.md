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
- 默认 prelude 提供 `Option` / `Result`、`Some` / `None` / `Ok` / `Err`、借用式状态判断 / 默认值 / 组合方法、整数方法与边界常量，以及 `print` / `println` / `assert` / `panic`。
  支持后缀 `!` 强制解包与 `?` 失败提前返回（[语义](docs/language.md#optionresult-与-outcome-后缀解包)）。
  [Option / Result 示例](examples/option_result.jk)展示 `unwrap_or`、`and` / `or` 和 `ok` / `err` 转换；普通默认值急切求值，`unwrap_or_else` 可按需执行同步回调。
  `run` / `build` 均可用 `--no-prelude` 关闭；`joky/array` 和 `joky/vm` 仍需显式导入。
- CLI 入口必须无参数，返回 Unit 或 Int32；支持同步 main 和 `fn main() @suspends`。
  Unit 对应退出码 0，Int32 取低 8 位；`joky/io` 的 `print` / `println` 借用实现 Show 的值，
  可直接写 `println(value)`、`println(42)`，挂起函数中也能直接打印标量；普通字符串字面量默认插值，支持 `r` 原始字符串与三引号多行去缩进，`"value={value}"` 生成拥有型文本。
  关闭 prelude 时显式导入 `joky/io.{print, println}`；CLI 不再预注册类型专用打印宿主。
- `import joky/task` 提供 `task.sleep_ms(UInt64)` 和 `task.yield_now()`；
  在挂起 main 中可直接调用，无需 async/await。运行示例：`cargo run -- run examples/async_main.jk`，输出 42。
- `run` / `build` 的 `--task-workers N` 显式启用任务分支多线程，缺省 Local；需要受检的挂起 main。
  支持标量、String、Array、List、Map/Set、FixedArray、嵌套聚合、受检函数值及 TCP 拥有转移、任务定时器和 stdout 输出；
  VM、文件、目录、准入与邮箱通过任务树控制代理使用，业务 `when` 按完整域访问集在 worker 并行计算并统一提交，Godot 回调保持 Local；
  [模式与准入范围](docs/language.md#任务执行模式)。
- 使用 `joky --help` 查看参数。开发时可用 `cargo run -- run examples/hello.jk`。

## 源码格式化

```sh
joky fmt file.jk             # 原地格式化一个文件
joky fmt --check file.jk     # 不写入；需要格式化时退出码为 1
joky fmt --stdin < file.jk   # 从标准输入读取，向标准输出写出
```

第一版采用保守规则：花括号、圆括号和方括号内按四空格缩进，统一常见标点与
运算符周围的空格，清理代码行尾空白，并为非空结果补齐末尾换行。保留已有换行、
空行及 LF/CRLF；注释、字符与字符串原文（包括插值内部和三引号字符串的缩进）不改写。
泛型尖括号、比较/移位与 `/` 两侧保留原有的有无空格关系；暂不做长行折行、
导入排序或递归目录格式化。

`fmt` 只要求语法有效，不解析依赖、不要求 `main`、不初始化 VM。
写回前核对 token 内容和换行位置，语法错误或格式化失败时不覆盖原文件；
文件通过同目录临时文件完整写入后替换，保留权限，符号链接写入其目标。
`--check` 可与 `--stdin` 组合，此时只返回状态，不输出格式化内容。
路径以 `-` 开头时使用 `joky fmt -- -file.jk`。
开发时无需安装，可用 `cargo run -- fmt --check file.jk`。

## 安装与产物

VS Code 的 `.jk` 语法高亮与基础编辑支持见 [Joky 扩展](editors/vscode/README.md)，
可在本地打包并安装 VSIX。

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
| 语言基础 | 八种定宽整数、Float32 / Float64、Rune、Bool、Unit、表达式与控制流、名义记录和枚举、元组、模式匹配、用户文件模块 |
| 类型系统 | 泛型与常量参数、透明别名、固有方法、静态 trait、默认方法、关联类型与常量、同步借用型 dyn、同步函数值与闭包、拥有闭包的记录存储与返回、挂起函数值 |
| 所有权与存储 | String、Array、List、Set、Map、FixedArray、拥有型聚合与 Drop、受检共享／独占借用、任务 arena 与同步显式 Arena |
| 执行与任务 | 共享语义分析、Cranelift JIT / AOT、字节码解释器；`@suspends`、`join` / `race`、`scope` / `branch`、`isolate` 失败隔离、取消与资源清理、局部值和容器视图跨挂起借用；可选多线程分支执行 |
| 内置模块 | Option / Result、Iterator / IntoIterator、Array 消费迭代、Array/Slice/Map 借用遍历、for 自动转换与可选编号、Map 键值解构、泛型 range 与 take/zip/map/filter 惰性组合器、数值、文本与格式化、容器、JSON、任务定时器、文件、TCP、DNS、状态域与有界准入 |
| 嵌入与热更新 | Rust `VmEngine`、C VM / CPS API、宿主注册与 FFI、持久实例、状态域、Script 调用与兼容发布、分步迁移 |
| Godot 原型 | Node 挂载 `.jk`、解释器状态与属性、受检 Node API、帧回调、所属任务和引擎等待；[构建和示例](integrations/godot/README.md) |

具体类型准入、API 与边界以[语言参考](docs/language.md)为准。任务执行可选固定 worker 池；尚无包管理。
解释器执行限额不覆盖编译阶段或原生回调，其他执行与嵌入限制见[已知边界](docs/architecture.md#已知边界)。
文件服务由宿主提供，CLI 启用本机实现，嵌入缺省为 Unsupported；Godot 验证适配器不提供文件服务。

## 示例与文档

- [文档索引](docs/README.md)：使用参考、实现设计与开发记录。
- [语言与内置模块](docs/language.md)：当前语法、求值顺序、类型准入与 API。
- [架构与嵌入约束](docs/architecture.md)：模块职责、IR、代码所有权和宿主 ABI。
- [解释器设计](docs/interpreter.md)：字节码、预算、调度和运行时诊断。
- [基础示例](examples/hello.jk)、[for 遍历与编号](examples/for.jk)、[整数范围](examples/ranges.jk)、[范围切片](examples/range_slices.jk)、[范围模式](examples/range_patterns.jk)、[泛型](examples/generics.jk)、[拥有闭包](examples/owned_closures.jk)、[惰性组合器](examples/iterator_adapters.jk)、[模式与枚举](examples/enums.jk)、[文本与格式化](examples/show.jk)、[容器](examples/maps.jk)。
- [跨挂起借用](docs/language.md#跨挂起借用)、[文件](examples/files.jk)、[TCP 消息](examples/tcp_messages.jk)、[双向会话](examples/tcp_duplex.jk)、[JSON 派生](examples/json_derive.jk)。
- [Rust 嵌入](examples/embedding.rs)、[宿主驱动任务](examples/host_tasks.rs)、[C VM](examples/vm_runtime.c) 与 [C CPS](examples/cps_runtime.c)。
- [贡献规范](CONTRIBUTING.md)、[基准方法](benches/README.md)与[历史性能记录](docs/interpreter-performance.md)。

## TCP 状态域聊天室

[chat 项目](examples/chat/README.md) 用 `joky.toml` 声明 host 入口、源码根和运行时资源。
在仓库根目录执行：

```sh
cargo build --bin joky --lib
target/debug/joky run examples/chat
# 原生项目构建，产物可整体搬动后直接运行：
target/debug/joky build examples/chat -o /tmp/chat-app --task-workers 3
/tmp/chat-app/chat
```

聊天端口为 `127.0.0.1:7777`，管理端口为 `127.0.0.1:7778`。
修改 `examples/chat/script/main.jk` 后向管理端发送 `reload`，发送 `shutdown` 停机。
目录说明、协议、配置与验证命令见[应用说明](examples/chat/README.md)。

预留模式示例：`cargo run -- run examples/reservation.jk`，演示预留、外部查询、确认／释放、
取消后到期回收与迟到回复校验，详见[语言参考](docs/language.md#跨挂起业务的预留模式)。

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
JSON 的固定版本外部语料回归独立运行，命令、离线用法与验收记录见
[JSON 收尾验收](docs/json-codec.md#12-独立验收与可复现语料回归)。

## 参考与许可

[Cranelift 文档](https://docs.rs/cranelift/) ·
[Cranelift 源码](https://github.com/bytecodealliance/wasmtime/tree/main/cranelift) ·
[System V ABI](https://refspecs.linuxbase.org/elf/x86_64-abi-0.99.pdf)

MIT / Apache 2.0 双许可。
