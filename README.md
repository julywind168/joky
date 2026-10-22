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
| 语言基础 | 八种定宽整数、Float32 / Float64、Rune、Bool、Unit、表达式与控制流、名义记录和枚举、元组、模式匹配、用户文件模块 |
| 类型系统 | 泛型与常量参数、透明别名、固有方法、静态 trait、默认方法、关联类型与常量、同步借用型 dyn、同步函数值与闭包、挂起函数值 |
| 所有权与存储 | String、Array、List、Set、Map、FixedArray、拥有型聚合与 Drop、受检共享／独占借用、任务 arena 与同步显式 Arena |
| 执行与任务 | 共享语义分析、Cranelift JIT / AOT、字节码解释器；`@suspends`、`parallel` / `race`、`scope` / `branch`、取消与资源清理、局部值和容器视图跨挂起借用 |
| 内置模块 | Option / Result、数值、文本与格式化、容器、JSON、任务定时器、文件、TCP、DNS、状态域与有界准入 |
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
- [基础示例](examples/hello.jk)、[泛型](examples/generics.jk)、[模式与枚举](examples/enums.jk)、[文本与格式化](examples/show.jk)、[容器](examples/maps.jk)。
- [跨挂起借用](docs/language.md#跨挂起借用)、[文件](examples/files.jk)、[TCP 消息](examples/tcp_messages.jk)、[双向会话](examples/tcp_duplex.jk)、[JSON 派生](examples/json_derive.jk)。
- [Rust 嵌入](examples/embedding.rs)、[宿主驱动任务](examples/host_tasks.rs)、[C VM](examples/vm_runtime.c) 与 [C CPS](examples/cps_runtime.c)。
- [贡献规范](CONTRIBUTING.md)、[基准方法](benches/README.md)与[历史性能记录](docs/interpreter-performance.md)。

## TCP 状态域聊天室

[chat.jk](examples/chat.jk) 是纯 Joky 热更新服务端，负责连接、邮箱和发布；
[chat_logic.jk](examples/chat_logic.jk) 保存状态类型与业务处理。服务端监听 `127.0.0.1:7777`，
启动时输出实际端口。启动后，在多个终端分别连接：

```sh
# 在仓库根目录执行
cargo run -- run examples/chat.jk
# 其他终端：
nc 127.0.0.1 7777
```

每行一条指令，支持 LF / CRLF：

```text
join 123
say hello world
balance
leave
```

`join <room>` 的房间名是非空 UTF-8 字符串，不含空格或 Tab；`say <message>` 在第一个空格
处分割，保留后续空格。连接收到 `welcome <id>`；房间成员收到
`joined <room> <id>`、`left <room> <id>` 或 `message <room> <id> <message>`。
广播包括发送者；换房先离开旧房间，再进入新房间。重复加入返回错误；`leave` 保留连接，
EOF、读写失败、非法 UTF-8 或超长行会断开并广播离开；错误指令只返回错误，保留连接。
无换行的末尾残片不执行。Ctrl-C 停止服务端。

每个连接创建一个独立 `#state Account`，初始余额 5 元，用整数分保存；每次成功 `say` 扣 1 元，
无论房间人数多少只扣一次。`balance` 返回 `balance <元>`；余额不足返回 `error insufficient balance`。
未加入房间、非法指令、加入／离开和余额查询不收费。这是按连接模拟的账号，断线后销毁，重连获得新账号，
不包含登录、持久化或充值接口。

每个房间是独立 `#state Room`，保存房间名和成员邮箱；`#state Catalog` 只管理房间创建和名称查询。
`say` 的读取集合只有发送者 Account 和所在 Room，写集合只有 Account；换房同时修改 Account、旧 Room 和新 Room。
从同一联合快照生成消息、收件人和状态变更，再通过候选加 `outbox.Batch` 原子提交。
扣款、成员变更、所有广播入队一起成功或失败，冲突重算不会重复收费。
其他房间或目录的变化不会使 `say` 重试；同一房间的成员变动参与校验，成功提交顺序就是广播顺序。
拆域减少无关业务冲突与复制范围，当前单线程执行器中的并发仍是任务交错，不代表多核并行。

修改 `examples/chat_logic.jk` 后，任一客户端发送 `reload`，收到 `reloaded` 后新指令使用新入口，
连接、成员、余额和待发送消息保持。没有全局业务许可，发布不等待指令完成，也不排空邮箱。
旧任务保留已开始的控制流；发布使旧候选冲突，重试可访问兼容新版快照，不能理解为一条在途指令整体切换版本。
编译仍占用当前执行器；示例没有做后台编译或发布停顿时延验收。
编译错误、文件读取失败或已有状态布局不兼容时返回 `reload failed`，旧版本继续运行，服务端输出具体诊断。

热更新必须保留宿主调用的入口契约以及 Catalog、Room、Account 布局；发布 API 校验域 schema，
不会预先检查宿主约定的所有入口，调用时才检查完整入口契约。布局变化应走显式迁移。
连接作用域的拥有型目录持有 Account，服务作用域的目录持有 Catalog 和 Room；初始化脚本移交
Owner 后返回，无需永久挂起任务。业务 `domain.request` 随 Account 生命周期取消，不用许可串行化。
新建域在对业务可见前仍由宿主发布到当前模块，覆盖“旧流程在发布后才创建域”的窗口。
空房间保留到服务停止，但不再占用持有任务的活动帧；兼容发布后，已结束初始化流程不再额外
保留旧模块。在途请求、快照等其他持有者仍可能延迟旧代码退役。
宿主可用 `cargo run -- build examples/chat.jk` 编译为 AOT，业务脚本仍由内嵌 VM 动态编译。

发送任务挂起在 `outbox.receive()`，出队立即由信箱释放容量，然后直接写 TCP，无需回访状态域。
`scope` / `branch` 接受并发连接，连接内 `race` 管理读、写和控制邮箱等待。
邮箱所有者保留到成员移除完成；断开的控制通知也随候选原子入队，即使提交后任务被取消仍能唤醒连接；关闭邮箱同样会唤醒等待，全程无需定时轮询。

收件人使用动态数组，批次按实际消息数与字节数分配，没有 64 位位图或 256 条批次上限，
仍受可用存储及执行器预算约束。每行最多 4096 字节（含结束符），每个出站邮箱最多
64 条消息、64 KiB，出队后另有最多一条消息正在写 socket。

提交若报告某个邮箱满或关闭，通过 `receipt.failed_outbox()` 取得其身份；先提交该客户端的
移除和离开广播、关闭邮箱，再重新准备原指令。多个慢客户端同时超限也会逐一清理，
向仍可接收的房间成员通知离开。清理链中即将断开的连接不再互相接收离开消息。
失败的候选不会发送部分广播；网络发送不保证对端已接收，也不提供断线重放。

房间名直接以拥有型 `String` 传入域操作、从状态查询返回，并随候选编辑一起提交；无需分块中转。
这是功能验证 demo，尚未做吞吐或长期运行验收。聊天实网测试默认忽略，仅手动运行：
`cargo test --test chat -- --ignored` 覆盖 JIT、解释器、
独立 AOT 宿主 × JIT／解释器脚本的六种组合，以握手屏障验证指令在途、邮箱未排空时直接发布，
以及失败保留旧版、晚创建域追上新版、已结束任务的旧模块回收，
包括 260 个成员邮箱的广播、JIT／AOT 宿主 260 人及解释器宿主 40 人的 TCP 广播，
以及并发加入／换房／发送／断开的一致顺序、六条并发发送只扣五次、余额不足不广播，
并检查无关房间修改不冲突、候选冲突不发送、
消息／字节额度及多个邮箱同时满的清理。解释器并发 TCP 会话还受默认 1024 个活动帧预算约束；
连接的接收、发送与控制等待仍占用活动帧；状态所有权本身不再需要常驻帧。
状态域数量及状态存储预算仍独立生效，尚未实现空房间自动回收或服务级局部失败恢复。
后续降低事务与服务接线负担的方向见[有状态服务的编程模型与实施计划](docs/stateful-service-ergonomics.md)。

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
