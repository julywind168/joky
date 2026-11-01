# TCP 状态域聊天室

chat 是一个 Joky 项目，配置位于根目录的 `joky.toml`：

```toml
[project]
name = "chat"
entry = "main.jk"
source-root = "."
resources = ["contract.jk", "script"]
```

```text
chat/
├── joky.toml
├── main.jk
├── contract.jk
├── host/admin.jk
├── script/main.jk
└── README.md
```

在仓库根目录构建 Debug 工具后运行：

```sh
cargo build --bin joky --lib
target/debug/joky run examples/chat
# 使用解释器，或启用并行 host：
target/debug/joky run examples/chat --interpreter
target/debug/joky run examples/chat --task-workers 3
```

安装 `joky` 后，也可进入此目录执行 `joky run`。不提供输入时，CLI 从当前目录向上
查找最近的 `joky.toml`；从其他目录传入项目路径同样有效。
源码模块按 `source-root` 解析，文件 API 与内嵌 VM 的文件加载按项目根目录解析。

构建并直接运行原生应用（以下构建命令从仓库根目录执行）：

```sh
target/debug/joky build examples/chat -o /tmp/chat-app --parallel
/tmp/chat-app/chat --task-workers 3
```

默认输出是项目下的 `dist/`。原生程序和清单声明的运行时资源一起组成应用目录，
可整体搬动到相同目标平台，直接执行其中的 `chat`；它按自身位置定位资源，不需要 shell 启动脚本
或安装 Joky 编译器。构建时选择 Local／Parallel，默认 Local。
`--parallel` 构建的程序默认启动 1 个 task worker；同一份产物可在启动时用
`--task-workers 1` 或 `--task-workers 3` 选择数量，无需重新构建。
也可使用 `JOKY_TASK_WORKERS=3 /tmp/chat-app/chat`；启动参数优先于环境变量，
环境变量优先于构建默认值。旧的 `build --task-workers 3` 保留为设置 Parallel 默认数量的入口。
数量在本次进程生命周期内固定，不影响控制线程和 I/O 服务线程的数量；1 worker 仍是 Parallel。
Local 产物收到 worker 配置会报错，不能由启动参数变成 Parallel。
完整规则见[任务执行模式](../../docs/language.md#任务执行模式)。

同名项目生成过的输出目录可重新构建；构建失败保留旧产物，成功后替换整个目录。
普通目录、符号链接和其他项目的产物不会被覆盖。重新构建会更新打包脚本，因此运行服务前
应完成构建；更新 host 或搬动部署目录时先停机，完成后再启动。

修改源码或部署目录中的 `script/main.jk` 后可发送 `reload` 热更新业务。
`contract.jk` 的打包内容使用 host 编译时读到的源码快照，必须与原生程序配套；
修改契约需重新构建并重启，`reload` 不更新契约快照。
脚本依赖仍由 host 显式登记；项目配置负责源码根、文件根与资源打包，不自动创建内嵌 VM。
完整规则见[项目模式](../../docs/language.md#joky-项目模式)。

分别连接聊天端口与管理端口：

```sh
nc 127.0.0.1 7777
nc 127.0.0.1 7778
```

两个端口均只监听本机。`JOKY_CHAT_PORT`／`JOKY_ADMIN_PORT` 可覆盖默认的 7777／7778；
值须为 0..65535 的十进制数字，0 表示由系统分配端口，无效值以状态 2 退出。
例如 `JOKY_CHAT_PORT=8787 JOKY_ADMIN_PORT=8788 joky run .`，原生产物使用相同环境变量。
启动成功输出 `chat <端口>` 和 `admin <端口>`；任一端口绑定失败即退出。
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

修改 `script/main.jk` 后，发送 `reload`；收到 `reloaded` 表示新入口与域代码已发布。
编译、文件或契约检查失败时回复 `reload failed`，旧版本继续运行，服务端打印诊断。
连接、成员、余额和已排队消息保持；在途流程进入下一次具名操作时使用当前兼容业务实现。
管理回复直接写 admin socket，不经过聊天账户或发送队列，也不消耗聊天连接名额。
管理端串行处理一个连接，支持在该连接上连续发命令；断开管理连接不会停止服务。
`shutdown` 回复 `shutting down` 后结束服务，取消连接任务并释放状态和两个监听端口；不承诺排空聊天消息。
也可用 Ctrl-C 停止。编译仍在当前执行器上执行，独立端口不代表后台编译。

按关注点阅读示例：

- [contract.jk](contract.jk)：共享类型、入口契约和 `defaults()` 配置。
- [script/main.jk](script/main.jk)：从 `handle_line` 进入，每条命令是一个具名 `when`；
  收费看 `say` 与 `SAY_PRICE_CENTS`，换房和缺失房间的事务内创建看 `join`，主动离开看 `leave`；
  开户与欢迎看 `init_account`，断线清理看 `forget`，空房条件回收看 `reclaim`。
- [main.jk](main.jk)：连接生命周期（开户、会话、清理）、失败隔离和服务启动。
- [host/admin.jk](host/admin.jk)：独立的发布与停机协议。

修改连接策略只需调整 `defaults()`：默认每行 4096 字节（含结束符）、队列 64 条／64 KiB，
最多 512 个连接、4096 个存活房间名。`connection.idle_timeout = 0ns` 关闭完整行超时，`reclaim_empty = false` 保留空房；
输出满或关闭时提交照常成功，慢连接通过独立控制信号断开，正常断线回调清理成员；
健康成员先收到本次业务消息，不会因慢连接而回滚或重放。账户由连接作用域拥有，默认同时回收空房域和名字条目，下次加入时在事务内重建。状态、消息及活动帧另受运行时预算约束。

宿主可用 `joky build` 编译为 AOT 应用目录，业务脚本仍由内嵌 VM 动态编译。
热更新必须保持共享入口契约、具名操作和存活状态 schema 兼容；共享契约文件在启动时登记，
改变它需同步修改宿主并重启示例，状态布局迁移不由 `reload` 自动处理。
[有状态服务设计与验收](../../docs/stateful-service-ergonomics.md)记录候选提交、背压、失败隔离与资源生命周期。

`cargo test --test chat -- --ignored` 执行三种宿主 × 两种脚本的实网矩阵，覆盖协议、扣费、
热更新、满队列、房间回收、管理端隔离和主动停机；JIT／AOT 宿主覆盖 260 人，解释器宿主覆盖 40 人。
这是功能示例。AOT host + JIT 脚本的 worker 吞吐基线见
[专项测量记录](../../benches/reports/chat-workers-2026-10-30.md)，尚未做长期运行或最大容量验收。
