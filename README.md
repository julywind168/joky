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
  支持后缀 `!` 强制解包与 `?` 失败提前返回（[语义](docs/language.md#option-与-result-后缀解包)）。
  [Option / Result 示例](examples/option_result.jk)展示 `unwrap_or`、`and` / `or` 和 `ok` / `err` 转换；普通默认值急切求值，`unwrap_or_else` 可按需执行同步回调。
  `run` / `build` 均可用 `--no-prelude` 关闭；`joky/array` 和 `joky/vm` 仍需显式导入。
- CLI 入口必须无参数，返回 Unit 或 Int32；支持同步 main 和 `fn main() @suspends`。
  Unit 对应退出码 0，Int32 取低 8 位；`joky/io` 的 `print` / `println` 借用实现 Show 的值，
  可直接写 `println(value)`、`println(42)`，挂起函数中也能直接打印标量；普通字符串字面量默认插值，支持 `r` 原始字符串与三引号多行去缩进，`"value={value}"` 生成拥有型文本。
  关闭 prelude 时显式导入 `joky/io.{print, println}`；CLI 不再预注册类型专用打印宿主。
- `import joky/task` 提供 `task.sleep_ms(UInt64)` 和 `task.yield_now()`；
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
| 语言基础 | 八种定宽整数、Float32 / Float64、Rune、Bool、Unit、表达式与控制流、名义记录和枚举、元组、模式匹配、用户文件模块 |
| 类型系统 | 泛型与常量参数、透明别名、固有方法、静态 trait、默认方法、关联类型与常量、同步借用型 dyn、同步函数值与闭包、挂起函数值 |
| 所有权与存储 | String、Array、List、Set、Map、FixedArray、拥有型聚合与 Drop、受检共享／独占借用、任务 arena 与同步显式 Arena |
| 执行与任务 | 共享语义分析、Cranelift JIT / AOT、字节码解释器；`@suspends`、`parallel` / `race`、`scope` / `branch`、取消与资源清理、局部值和容器视图跨挂起借用 |
| 内置模块 | Option / Result、Iterator / IntoIterator、Array 消费迭代、Array/Slice/Map 借用遍历、for 自动转换与可选编号、Map 键值解构、Int32 range、数值、文本与格式化、容器、JSON、任务定时器、文件、TCP、DNS、状态域与有界准入 |
| 嵌入与热更新 | Rust `VmEngine`、C VM / CPS API、宿主注册与 FFI、持久实例、状态域、Script 调用与兼容发布、分步迁移 |
| Godot 原型 | Node 挂载 `.jk`、解释器状态与属性、受检 Node API、帧回调、所属任务和引擎等待；[构建和示例](integrations/godot/README.md) |

具体类型准入、API 与边界以[语言参考](docs/language.md)为准。尚无包管理或多线程任务调度；
解释器执行限额不覆盖编译阶段或原生回调，其他执行与嵌入限制见[已知边界](docs/architecture.md#已知边界)。
文件服务由宿主提供，CLI 启用本机实现，嵌入缺省为 Unsupported；Godot 验证适配器不提供文件服务。

## 示例与文档

- [文档索引](docs/README.md)：使用参考、实现设计与开发记录。
- [语言与内置模块](docs/language.md)：当前语法、求值顺序、类型准入与 API。
- [架构与嵌入约束](docs/architecture.md)：模块职责、IR、代码所有权和宿主 ABI。
- [解释器设计](docs/interpreter.md)：字节码、预算、调度和运行时诊断。
- [基础示例](examples/hello.jk)、[for 遍历与编号](examples/for.jk)、[整数范围](examples/ranges.jk)、[范围切片](examples/range_slices.jk)、[范围模式](examples/range_patterns.jk)、[泛型](examples/generics.jk)、[模式与枚举](examples/enums.jk)、[文本与格式化](examples/show.jk)、[容器](examples/maps.jk)。
- [跨挂起借用](docs/language.md#跨挂起借用)、[文件](examples/files.jk)、[TCP 消息](examples/tcp_messages.jk)、[双向会话](examples/tcp_duplex.jk)、[JSON 派生](examples/json_derive.jk)。
- [Rust 嵌入](examples/embedding.rs)、[宿主驱动任务](examples/host_tasks.rs)、[C VM](examples/vm_runtime.c) 与 [C CPS](examples/cps_runtime.c)。
- [贡献规范](CONTRIBUTING.md)、[基准方法](benches/README.md)与[历史性能记录](docs/interpreter-performance.md)。

## TCP 状态域聊天室

在仓库根目录启动服务，再分别连接聊天端口和管理端口：

```sh
cargo run -- run examples/chat.jk
# 聊天终端，可打开多个：
nc 127.0.0.1 7777
# 单独的管理终端：
nc 127.0.0.1 7778
```

两个端口均只监听本机。启动成功输出 `chat <端口>` 和 `admin <端口>`；任一端口绑定失败即退出。
聊天连接收到 `welcome <id>`，每行一条命令，支持 LF / CRLF：

```text
join 123
say hello world
balance
leave
```

房间名是非空 UTF-8 字符串，不含空格或 Tab；消息保留第一个空格之后的内容。
成员收到 `joined <room> <id>`、`left <room> <id>` 和 `message <room> <id> <message>`。
广播包括发送者，换房先离开再加入；重复加入返回错误。每个连接初始余额 5 元，成功发言扣 1 元，
不按收件人数重复扣费；其余命令不收费。扣款、成员变化和广播入队在 `when` 中一起提交。
余额不足、未加入房间或命令无效只回复错误。`leave` 保留连接；EOF、网络错误、非法 UTF-8
或超长行会断开并清理成员。无换行的末尾残片不执行。重连获得新账号；示例不包含登录和持久化。

管理连接只接受以下命令，聊天端口不接受它们：

```text
reload
shutdown
```

修改 `examples/chat_logic.jk` 后，发送 `reload`；收到 `reloaded` 表示新入口与域代码已发布。
编译、文件或契约检查失败时回复 `reload failed`，旧版本继续运行，服务端打印诊断。
连接、成员、余额和已排队消息保持；在途流程进入下一次具名操作时使用当前兼容业务实现。
管理回复直接写 admin socket，不经过聊天账户或发送队列，也不消耗聊天连接名额。
管理端串行处理一个连接，支持在该连接上连续发命令；断开管理连接不会停止服务。
`shutdown` 回复 `shutting down` 后结束服务，取消连接任务并释放状态和两个监听端口；不承诺排空聊天消息。
也可用 Ctrl-C 停止。编译仍在当前执行器上执行，独立端口不代表后台编译。

按关注点阅读示例：

- [chat_contract.jk](examples/chat_contract.jk)：共享类型、入口契约和 `defaults()` 配置。
- [chat_logic.jk](examples/chat_logic.jk)：从 `dispatch`／`handle_line` 进入；收费看 `say`，
  换房看 `join`，离开与断线看 `leave`。重试／淘汰集中在 `run_command`／`retry_command`，
  房间创建、就绪与回收集中在文件后半部分。
- [chat.jk](examples/chat.jk)：连接生命周期、房间初始化和服务启动。
- [chat_admin.jk](examples/chat_admin.jk)：独立的发布与停机协议。

修改连接策略只需调整 `defaults()`：默认每行 4096 字节（含结束符）、队列 64 条／64 KiB，
最多 512 个连接、4096 个历史房间名，每条指令最多重算 64 次、淘汰 512 个慢客户端，
房间创建最多等 5 秒。`connection.idle_ms = 0` 关闭完整行超时，`reclaim_empty = false` 保留空房；
`max_evictions = 0` 拒绝需要淘汰的指令并结束当前连接。账户由连接作用域拥有，默认回收空房域，
保留有界名字条目以供重建。状态、消息及活动帧另受运行时预算约束。

宿主可用 `cargo run -- build examples/chat.jk` 编译为 AOT，业务脚本仍由内嵌 VM 动态编译。
热更新必须保持共享入口契约、具名操作和存活状态 schema 兼容；共享契约文件在启动时登记，
改变它需同步修改宿主并重启示例，状态布局迁移不由 `reload` 自动处理。
[有状态服务设计与验收](docs/stateful-service-ergonomics.md)记录候选提交、背压、失败隔离与资源生命周期。

`cargo test --test chat -- --ignored` 执行三种宿主 × 两种脚本的实网矩阵，覆盖协议、扣费、
热更新、满队列、房间回收、管理端隔离和主动停机；JIT／AOT 宿主覆盖 260 人，解释器宿主覆盖 40 人。
这是功能示例，尚未做吞吐或长期运行验收。

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
