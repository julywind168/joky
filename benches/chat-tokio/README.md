# Rust/Tokio chat 对照

这是 `examples/chat` 的静态 Rust 业务实现，用于同协议的性能对照。它是独立 Cargo 工程，
不改变 Joky 的依赖或 Cargo.lock。Tokio 版本由本目录 Cargo.lock 固定，当前为 1.54.1。
已完成的三轮测量见 [Joky / Tokio 对照报告](../reports/chat-tokio-2026-10-30.md)。

```sh
cargo run --manifest-path benches/chat-tokio/Cargo.toml --target-dir target/chat-tokio -- \
  --task-workers 3
```

默认监听 `127.0.0.1:7777` 和 `127.0.0.1:7778`，输出 `chat <port>`、`admin <port>`。
`--chat-port 0 --admin-port 0` 选择临时端口。聊天端口支持 `join <room>`、`say <text>`、
`balance`、`leave`；管理端口支持 `shutdown`。`reload` 明确回复不支持，不假装完成热更新。

## 对齐的行为

- 每个连接独立账户，初始 500 分，成功发言扣 100 分，广播包括发送者；未入房、重复加入、
  余额不足和命令错误使用相同回复。正文保留第一个空格后的内容，房间名支持 UTF-8。
- 房间成员用 Vec 保存并按 swap_remove 删除；发送前核对成员身份。不同房间可并行，
  每个房间用短 Mutex 临界区串行完成扣费和整批广播入队，使所有接收者看到一致的命令顺序。
  换房在目录锁及两个房间锁内完成，失败不能先移除旧成员；空房自动回收。
- 每连接队列 64 条／65536 字节，每个收件人独立保存字节副本。字节与条数限制只计排队消息，
  接收出队后即退款，活动 socket 写入另计。满队列／关闭队列通过独立 watch 信号断开慢连接，
  其余收件人仍入队，成功发言不回滚扣款、不重复广播。
- 输入缓冲区 16384 字节，完整行上限 4096 字节（含结束符）；支持 LF/CRLF，非法 UTF-8、
  超长行或网络错误断开，无换行 EOF 残片不执行。每条消息独立 write_all，不额外批量合并。
- 两版服务端均保留系统默认 Nagle 行为（TCP_NODELAY=false）。负载客户端使用同一套实现。
- 默认最多 512 个连接、4096 个房间；完整行空闲超时默认关闭。管理连接不占聊天名额，串行处理，
  接受 shutdown 后即使回复失败也停机；停止时取消并等待会话清理，不承诺排空消息。

`--help` 列出测试／测量用配置：余额、队列容量、行长、连接数、房间数和完整行超时。
`--queue-bytes 0` 表示只限制消息条数。`--task-workers N` 始终使用 Tokio multi-thread runtime，
即使 N=1 也不切换 current-thread；主线程负责 accept/admin，连接任务在 worker 上运行。

## 不能据此等同的机制

| 项目 | Joky chat | Rust/Tokio 对照 |
| --- | --- | --- |
| 业务执行 | AOT host 调用内嵌 JIT 脚本，支持热更新 | Rust Release 静态编译，直接调用 |
| 账户 | 域目录查询、受检句柄、动态操作 | 连接任务独占 Account |
| 原子操作 | 通用域 grant、候选状态与事务 outbox | 专门设计的房间锁和无挂起提交路径 |
| 分配预算 | 共享多预算收费／退款与受检资源限制 | Rust allocator；只实现业务队列／连接／房间限制 |
| 运行时线程 | worker 外还有控制、I/O、文件等线程 | Tokio worker 和驱动服务的主线程 |

因此这是**相同正常业务语义的应用实现对照**，不是只替换调度器的微基准，也不能把差距全部归因于
TCP 或语言算术性能。Rust 版不提供 Joky 的脚本错误隔离、通用事务回滚、状态迁移和热更新能力。

## 验证与测量

```sh
cargo fmt --manifest-path benches/chat-tokio/Cargo.toml -- --check
cargo clippy --manifest-path benches/chat-tokio/Cargo.toml --target-dir target/chat-tokio --all-targets -- -D warnings
cargo test --manifest-path benches/chat-tokio/Cargo.toml --target-dir target/chat-tokio
```

测试覆盖扣费、成员变更、满目录原子失败、队列条数／字节／关闭与退款、并发广播顺序、行边界，
以及 1／3 worker 的真实 TCP、名额归还、超时和管理停机。改动仅涉及独立对照工程和计时器时，
不重复运行 Joky 编译器全量检查；共享计时器须用两版产物各验证一次。

完成构建和正确性检查后再计时：

```sh
cargo build --release --locked --manifest-path benches/chat-tokio/Cargo.toml --target-dir target/chat-tokio
python3 benches/chat-workers.py prepare-tokio \
  --binary target/chat-tokio/release/chat-tokio --profile release \
  --output target/benchmarks/chat-tokio/build
python3 benches/chat-workers.py run \
  --build target/benchmarks/chat-tokio/build \
  --output target/benchmarks/chat-tokio/run1 \
  --workers 1,2,4 --rooms 1,4 --clients 16 --messages 500 --warmup 25 --samples 3 --min-seconds 5
```

prepare-tokio 只快照已有二进制、源码、Cargo.lock、哈希和启动参数，不自行编译；profile 由调用者声明。
基准启动参数将两端口设为 0、初始余额设为 10¹² 分，保持正常扣费。相同参数也用于 Joky 产物。
每个样本至少完成一批 messages；`--min-seconds` 按完整批次延长，CSV 记录真实广播数。
所有消息、顺序、余额和退出都由共用 [chat-workers.py](../chat-workers.py) 校验。

客户端仍为单进程 Python。如果客户端接近一个 CPU 核，吞吐会受发生器限制，此时只能报告这套负载下
的观测结果，不能宣称测得 Tokio 容量上限或线程扩展上限。CPU 与 RSS 口径见 [基准说明](../README.md)。

实现参考 Tokio 官方文档：[多线程运行时](https://docs.rs/tokio/latest/tokio/runtime/struct.Builder.html)、
[有界 mpsc](https://docs.rs/tokio/latest/tokio/sync/mpsc/index.html)、
[TCP split](https://docs.rs/tokio/latest/tokio/net/struct.TcpStream.html#method.into_split)。
