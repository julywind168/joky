# TCP 状态域聊天室

先在仓库根目录构建 Debug 编译器和运行时，再启动应用：

```sh
cargo build --bin joky --lib
examples/chat/run.sh
# 聊天终端，可打开多个：
nc 127.0.0.1 7777
# 单独的管理终端：
nc 127.0.0.1 7778
```

目录职责如下，`contract.jk` 是应用根目录中的唯一共享契约：

```text
chat/
├── main.jk
├── contract.jk
├── host/admin.jk
├── script/main.jk
├── run.sh
├── build.sh
└── README.md
```

`run.sh` 自行定位应用根目录，从其他工作目录调用它也能加载正确的脚本和契约。
源码启动支持 `./run.sh --interpreter` 和 `./run.sh --task-workers 3`。
工具选择顺序为显式 `JOKY`、PATH 中的 `joky`、本仓库的 `target/debug/joky`；
`JOKY` 也接受相对于调用位置的可执行文件路径。脚本不会自动构建编译器。
直接执行 `joky run main.jk` 时，仍须以应用目录作为工作目录。

构建可搬动的原生应用（以下命令从仓库根目录运行）：

```sh
examples/chat/build.sh /tmp/chat-app --task-workers 3
cd /tmp
./chat-app/run.sh
```

`build.sh [输出目录] [Joky 构建选项]` 默认输出到应用目录下的 `dist/`；
显式相对输出目录按调用时的工作目录解析。输出目录必须不存在，以免覆盖正在部署的脚本。
构建选项可用 `--task-workers N`，默认 Local；运行时归档沿用 `JOKY_RUNTIME` 配置或编译器的安装布局。

产物包含 `chat` 原生程序、`run.sh`、`contract.jk`、`script/` 和运行说明。
构建会先复制应用源码，再用该副本编译，使打包契约与 host 使用同一份源码。
产物不需要安装 Joky 编译器，可整体复制到相同目标平台的其他位置，并通过其中的 `run.sh` 启动。
原生启动入口不接受选项；执行模式在构建时确定。运行中的服务使用启动时选定的目录，
移动部署目录应先停机，移动后再启动。

修改部署目录中的 `script/main.jk` 可热更新业务。`contract.jk` 必须与原生程序配套，
修改契约或 host 后应重新构建到新目录并重启；`reload` 不更新契约快照。
当前脚本依赖由 host 显式登记；新增业务模块时也须登记对应模块，目录本身不启用自动依赖发现。

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
最多 512 个连接、4096 个存活房间名。`connection.idle_ms = 0` 关闭完整行超时，`reclaim_empty = false` 保留空房；
输出满或关闭时提交照常成功，慢连接通过独立控制信号断开，正常断线回调清理成员；
健康成员先收到本次业务消息，不会因慢连接而回滚或重放。账户由连接作用域拥有，默认同时回收空房域和名字条目，下次加入时在事务内重建。状态、消息及活动帧另受运行时预算约束。

宿主可用 `build.sh` 编译为 AOT 应用目录，业务脚本仍由内嵌 VM 动态编译。
热更新必须保持共享入口契约、具名操作和存活状态 schema 兼容；共享契约文件在启动时登记，
改变它需同步修改宿主并重启示例，状态布局迁移不由 `reload` 自动处理。
[有状态服务设计与验收](../../docs/stateful-service-ergonomics.md)记录候选提交、背压、失败隔离与资源生命周期。

`cargo test --test chat -- --ignored` 执行三种宿主 × 两种脚本的实网矩阵，覆盖协议、扣费、
热更新、满队列、房间回收、管理端隔离和主动停机；JIT／AOT 宿主覆盖 260 人，解释器宿主覆盖 40 人。
这是功能示例，尚未做吞吐或长期运行验收。
