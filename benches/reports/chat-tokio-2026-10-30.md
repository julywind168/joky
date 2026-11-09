# Chat：Joky 与 Rust/Tokio 应用对照（2026-10-30）

> 原始样本、构建元数据与一次性分析脚本已归档到 Git 历史；明细链接指向固定快照，恢复方法见[归档说明](README.md)。

新增独立的 [Rust/Tokio chat](../chat-tokio/README.md)，用相同客户端执行同协议、同扣费和广播业务。
本机三轮交替测量中，Tokio 的观测吞吐为 Joky 的 **14.2～23.5 倍**；单客户端请求到回显的
P50 为 **0.0505 ms 对 0.653 ms**。这说明当前 Joky chat 还有明显的执行成本优化空间，
不能把之前毫秒级耗时理解成 TCP 本身的单程发送时间。

**Tokio 并发矩阵已经受到 Python 客户端的限制。** 客户端使用约 0.94～0.98 个 CPU 核，
Tokio 服务端仅使用 0.31～0.78 核；本报告给出这套负载下的结果，不声称测得 Tokio 的容量上限，
也不能从 worker 增多没有提速推断 Tokio 不支持并行扩展。

## 实现对齐与差异

Rust 版保留账户初始余额、每次成功发言扣费、成员身份检查、加入／换房／离开、空房回收、
包含发送者的广播、房间内一致的广播顺序，以及每连接有界队列和慢连接断开策略。
房间成员用 Vec 与 swap_remove；每位收件人各持有一份消息字节，不使用共享 Arc 字节省去副本。
每房间 Mutex 临界区完成检查、扣费和入队，网络读写在连接任务内由并发 future 执行，锁内没有 await。
不同房间的发言可并行，只有成员变更需要目录锁。

两版使用同样的 16 KiB 读缓冲区、4096 字节完整行上限、64 条／65536 字节发送队列、默认无空闲超时，
并保留服务端默认 Nagle 设置。逐条 write_all，不额外批量合并。非法 UTF-8、超长行、EOF 残片、
连接准入及管理停机行为也有独立测试。完整行为与边界见 [实现说明](../chat-tokio/README.md)。

这是应用实现对照，两个系统提供的机制不完全相同：

| 项目 | Joky | Rust/Tokio |
| --- | --- | --- |
| 业务执行 | AOT host + 内嵌 JIT 脚本 | Rust Release 静态编译 |
| 账户访问 | 域目录、受检句柄和动态操作 | 连接任务独占 Account |
| 原子业务 | 通用域 grant、候选状态、事务 outbox | 专用房间锁、无挂起的提交路径 |
| 内存限制 | 共享多预算收费／退款、受检资源 | Rust allocator，保留业务队列／连接／房间限制 |
| 热更新等 | 脚本更新、错误隔离、通用回滚与状态迁移 | 未实现；reload 明确返回不支持 |

Rust 的专用实现避免了通用机制的开销。差距不能全部归因于语言、TCP、Tokio 调度器或某一个锁；
也不能据此认为 Joky 的安全和热更新能力必然要付出当前这么大的成本，需要继续按热点拆解。

## 测量方法

- Apple M1 Pro（6P + 2E）、16 GiB、macOS 26.6.2 arm64、Rust 1.99.0、Python 3.14.6。
  Tokio **1.54.1** 由独立 Cargo.lock 固定，不改变编译器的依赖。
- Joky 复用上一阶段 `target/benchmarks/chat-float-env/build`，实现对应 `c1dc65f`。
  原构建身份为 `7f6fc74` 加浮点环境优化补丁；[构建记录](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-tokio-2026-10-30/joky-build.json)
  保留原始身份，不能将本次计时的 Git HEAD 当成它的构建提交。
  工具／静态运行时为 Release，Joky 生成代码采用默认设置。
- Tokio 用 Release、opt-level=3、thin LTO、codegen-units=1 构建后快照，
  [构建记录](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-tokio-2026-10-30/tokio-build.json)包含源码、Cargo.lock 和二进制 SHA-256。
  worker=1 仍使用 multi-thread runtime；主线程运行 accept/admin，连接任务在 worker 上执行。
  两版的 worker 数相同，但控制／驱动等额外线程拓扑不同。
- 共用 [chat-workers.py](../chat-workers.py)。16 客户端、1 房间各 16 人／4 房间各 4 人、
  64 字节正文、每连接预热 25 条，每连接等待该广播的所有接收者收到后再发送下一条。
  “广播/秒”是发言命令数；送达/秒还需乘房间人数，不能把 1 房间和 4 房间当作相同扇出。
- 每批每连接 500 条，按完整批次重复到至少 **5 秒**。两版使用相同参数；CSV 记录实际广播数，
  不是固定 8000 条。这样避免更快的 Tokio 样本过短导致 CPU 计时粒度失真。
  单客户端另测 1 房间、1 worker，每批 5000 条，同样至少 5 秒。
- 三轮实现顺序为 Joky→Tokio、Tokio→Joky、Joky→Tokio；每轮先完成双方并发矩阵，再测双方单客户端。
  worker 顺序依次为 `1,2,4`、`2,4,1`、`4,1,2`，房间顺序为 `1,4`、`4,1`、`1,4`。
  每个配置每版 3 个独立服务端进程，总计 **42 个正式样本**，没有剔除慢样本。
- 构建、启动、join、预热、余额核对和 shutdown 不计时。正式计时串行执行，期间没有编译、测试或 profiler。
  所有样本通过消息内容、收件人、每发送者顺序、扣费和正常退出校验；Joky 完成 230000 条、
  Tokio 完成 1971000 条计时广播。

## 三轮结果

各项为样本中位数，括号为吞吐 min–max；分位数先逐样本统计再取中位数，不是合并分布的分位数。
倍数按未舍入中位数计算。只有三个独立样本，不给跨机器保证或显著性结论。

| 房间 | worker | Joky 广播/秒（范围） | Tokio 广播/秒（范围） | Tokio / Joky | Joky P99 ms | Tokio P99 ms |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | 1 | 598（580–611） | 14046（13293–14215） | 23.48× | 35.70 | 1.65 |
| 1 | 2 | 627（623–634） | 13651（13644–13766） | 21.78× | 32.83 | 1.57 |
| 1 | 4 | 738（715–764） | 13318（12738–13532） | 18.03× | 37.52 | 1.58 |
| 4 | 1 | 1593（1528–1625） | 25130（23373–25430） | 15.77× | 13.72 | 0.99 |
| 4 | 2 | 1694（1523–1740） | 24054（22974–24657） | 14.20× | 16.96 | 1.14 |
| 4 | 4 | 1539（1488–1567） | 24178（23915–24531） | 15.71× | 19.72 | 1.02 |

本轮 Joky 部分结果低于上一阶段；所有比值只用本轮交替测量，不拼接历史中位数。
下表 CPU ms/广播逐样本用服务端 CPU 时间除以完成广播数后取中位数；CPU 核数为 CPU 时间／墙钟时间，
包含进程所有线程，所以 Joky 即使只有一个 worker 也可能大于一核。

| 房间 | worker | Joky 服务端核数 | Tokio 服务端核数 | Joky 客户端核数 | Tokio 客户端核数 | Joky CPU ms/广播 | Tokio CPU ms/广播 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 1 | 1.53 | 0.31 | 0.30 | 0.97 | 2.593 | 0.0221 |
| 1 | 2 | 1.87 | 0.47 | 0.34 | 0.98 | 2.961 | 0.0347 |
| 1 | 4 | 3.07 | 0.78 | 0.41 | 0.98 | 4.128 | 0.0582 |
| 4 | 1 | 1.67 | 0.37 | 0.24 | 0.94 | 1.045 | 0.0145 |
| 4 | 2 | 2.30 | 0.53 | 0.28 | 0.95 | 1.347 | 0.0230 |
| 4 | 4 | 3.02 | 0.76 | 0.26 | 0.95 | 1.959 | 0.0311 |

Tokio 的客户端解析、逐消息校验、延迟记录等都在同一个 Python 进程中，已限制继续施压。
服务端 CPU 成本仍可作为这套负载下的观测，但不能据此线性外推容量；若下一步要测 Tokio 饱和吞吐，
应使用多进程或原生客户端，并让两版一起重测，不能只更换 Tokio 一边的客户端。

并发矩阵中采样 RSS 最大值的中位数：Joky 为 74.4～78.0 MiB，Tokio 为 2.30～2.42 MiB。
Joky 包含内嵌编译器／VM；RSS 每 250 ms 采样，不等同于精确峰值、应用活跃内存或预算收费量。

| 单客户端指标 | Joky | Tokio |
| --- | --- | --- |
| 请求/秒 | 1276.5 | 12106.0 |
| P50 ms | 0.6531 | 0.0505 |
| P95 ms | 0.9679 | 0.1112 |
| P99 ms | 1.4691 | 0.1808 |
| 服务端 CPU ms/请求 | 0.6190 | 0.0146 |
| 客户端 CPU 核数 | 0.086 | 0.457 |

单客户端的客户端 CPU 没有占满一核。这是包含解析、成员检查、扣费、入队、回环网络和客户端调度的
请求到回显延迟，不能除以二当作 TCP 单程延迟；请求/秒还包括闭环客户端发起下一条的间隔，
也不等于 P50 的倒数。

## 验证与复现

本次仅修改独立基准工程和共用计时器，没有修改 Joky 编译器、运行时、标准库或 chat 业务。
已通过独立 crate 的 fmt、Clippy（all-targets，-D warnings）、5 个单元测试和 2 个真实 TCP 测试
（覆盖 1／3 worker），以及两版各 6 配置的计时入口冒烟和全部 42 个正式样本校验。
Python 语法检查与 git diff --check 通过。没有重跑编译器全量检查或无关压力测试；Release 仅用于性能测量。

构建和单次矩阵命令见 [Rust/Tokio README](../chat-tokio/README.md)。共用运行器为 Joky 准备新产物的命令：

```sh
cargo build --release --bin joky --lib
python3 benches/chat-workers.py prepare \
  --joky target/release/joky --runtime target/release/libjoky.a --profile release \
  --output target/benchmarks/chat-tokio/joky-build
python3 benches/chat-workers.py run \
  --build target/benchmarks/chat-tokio/joky-build \
  --output target/benchmarks/chat-tokio/reproduce-joky \
  --workers 1,2,4 --rooms 1,4 --clients 16 --messages 500 --warmup 25 --samples 1 --min-seconds 5
```

切换为 Tokio build 后使用完全相同的 run 参数；按上述三轮顺序交替。
单客户端改为 `--workers 1 --rooms 1 --clients 1 --messages 5000`。各输出目录必须尚不存在。
新编译的 Joky 要使用本次版本才能复现本次实现；以后 HEAD 的结果属于新的比较。

[42 个样本](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-tokio-2026-10-30/samples.csv)、[环境与实际 argv](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-tokio-2026-10-30/environments.json)、
[产物哈希](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-tokio-2026-10-30/artifact-hashes.json)保存紧凑数据。
完整源码快照、二进制、逐消息延迟和服务日志位于 `target/benchmarks/chat-tokio/`；
Joky 原产物保留在 `target/benchmarks/chat-float-env/build`。大体积原始延迟和二进制不提交。
