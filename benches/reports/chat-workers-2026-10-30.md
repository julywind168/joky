# Chat worker 扩展基线（2026-10-30）

本记录测量 `a9bfcde` 的后续工作树。
准确构建提交、二进制身份以随附的测量元数据为准；本次仅增加运行器和文档，未改变编译器、运行时及仓库中的 chat 源码。

本机固定负载下，1 → 2 worker 的中位吞吐基本持平，4 worker 未带来吞吐收益，且每条广播的 CPU 成本上升。
独立复测趋势一致。带符号采样将主要互斥等待定位到共享分配表的查询／释放路径；下一步优先优化这一处。

## 环境与工作负载

- Apple M1 Pro，8 核（6 性能核、2 能效核），16 GiB；macOS 26.6.2 arm64。
- Rust 1.99.0，Apple clang 21.0.0；工具及静态运行时为 Release，Joky 生成代码仍采用默认设置。
- 同一个 AOT host 产物，内嵌业务脚本使用 JIT；Parallel 模式分别启动 1／2／4 task worker。
  控制、I/O 与文件服务线程另计，1 worker 不是单线程 Local 模式。
- 16 个客户端、64 字节正文；单房间 16 人和四房间各 4 人。每连接先预热 25 条，再完成 500 条广播。
  一条广播收到本房间全部接收者的回显后，该发送连接才发下一条；每样本共 8000 条广播。
- 独立副本仅修改临时监听端口与初始余额，正常扣费、队列、事务和消息内容保持原有行为。
  编译、启动、连接、join、预热、最终余额核对及 shutdown 都在计时外。

运行器及完整口径见 [chat-workers.py](../chat-workers.py) 和 [基准说明](../README.md#chat-worker-扩展基线)。
这是闭环固定并发下的端到端测量，不是固定到达率、过载排队或最大容量测量。
`say` 只读 Room、修改自己的 Account；单房间和多房间的广播 fan-out 不同，不能据此隔离房间锁竞争。

## 测量结果

主矩阵每组 5 个独立进程样本，表中指标取样本中位数，吞吐范围为 min–max。

| 房间 | worker | 广播/秒（范围） | 投递/秒 | P95 ms | P99 ms | 服务端 CPU 核 | 客户端 CPU 核 | 采样 RSS MiB |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 1 | 239.4（232.3–251.1） | 3830.5 | 85.08 | 93.18 | 1.36 | 0.15 | 77.2 |
| 1 | 2 | 241.0（235.4–243.7） | 3855.3 | 84.03 | 96.24 | 1.55 | 0.16 | 77.0 |
| 1 | 4 | 231.1（220.4–245.9） | 3697.7 | 92.81 | 114.67 | 1.99 | 0.15 | 77.4 |
| 4 | 1 | 506.6（490.4–535.3） | 2026.6 | 42.12 | 45.77 | 1.39 | 0.10 | 76.6 |
| 4 | 2 | 508.1（481.6–530.8） | 2032.3 | 43.04 | 49.00 | 1.67 | 0.11 | 77.2 |
| 4 | 4 | 467.6（462.9–488.0） | 1870.4 | 46.49 | 51.47 | 1.86 | 0.10 | 76.9 |

另开一轮独立复测，工作负载和二进制相同，每组 2 个样本，只比较 1 与 4 worker：

| 房间 | worker | 广播/秒 | P95 ms | P99 ms | 服务端 CPU 核 |
| --- | --- | --- | --- | --- | --- |
| 1 | 1 | 249.5 | 80.79 | 93.28 | 1.37 |
| 1 | 4 | 226.4 | 90.94 | 98.59 | 2.01 |
| 4 | 1 | 492.9 | 42.28 | 46.89 | 1.39 |
| 4 | 4 | 463.2 | 47.17 | 51.59 | 1.85 |

主矩阵单房间每条广播的服务端 CPU 时间中位数为 5.66／6.44／8.69 ms，多房间为 2.74／3.24／3.93 ms。
这些值逐样本用 CPU 秒数除以完成广播数再取中位数；4 worker 相比 1 worker 分别增加约 54%／43%。
客户端平均仅占约 0.10～0.16 个核，没有 CPU 饱和证据；这仍不能将闭环负载当作容量上限。

紧凑原始数据：[run1 每样本](chat-workers-2026-10-30/run1-samples.csv)、[run2 每样本](chat-workers-2026-10-30/run2-samples.csv)、
[构建身份](chat-workers-2026-10-30/build.json)、[原始产物哈希](chat-workers-2026-10-30/artifact-hashes.json)。
完整逐消息原始数据保留在下述本机输出目录，未舍弃慢样本或失败样本。
提交的 CSV 副本仅将换行规范为 LF；产物哈希对应 target 目录中的原始输出。

## 复现

先完成所有构建和检查，再串行测量，运行期间没有并行编译或其他压力检查：

```sh
cargo build --release --bin joky --lib
python3 benches/chat-workers.py prepare \
  --joky target/release/joky --runtime target/release/libjoky.a --profile release \
  --output target/benchmarks/chat-workers/build-20261030
python3 benches/chat-workers.py run \
  --build target/benchmarks/chat-workers/build-20261030 \
  --output target/benchmarks/chat-workers/run1 \
  --clients 16 --rooms 1,4 --workers 1,2,4 --messages 500 --warmup 25 --samples 5
```

复测仅改变输出目录、worker 列表和样本数：

```sh
python3 benches/chat-workers.py run \
  --build target/benchmarks/chat-workers/build-20261030 \
  --output target/benchmarks/chat-workers/run2 \
  --clients 16 --rooms 1,4 --workers 1,4 --messages 500 --warmup 25 --samples 2
```

每个 run 目录保留逐消息 `latencies.csv`、每样本 `samples.csv`、汇总、环境、运行器快照及服务日志。
build 目录保留原始构建日志、修改后的源码、打包应用、Cargo.lock 及工具／运行时／应用哈希。
这些大体积本机原始产物位于 `target/benchmarks/chat-workers/`，不加入默认测试或提交二进制。

## 线程采样与优化顺序

正式两轮测量结束后，另开 4 worker 的单房间／多房间负载，用 macOS `sample` 各采 5 秒、间隔 1 ms。
最初直接采样正式二进制只能看到系统函数与匿名地址，因为 AOT Release 链接会剥离符号。
因此另建仅供分析的符号版：复用相同 Rust 工具、静态运行时和所有 `.jk` 文件，
仅从链接参数中去掉 `-S,-x`，保留 `-dead_strip,-no_exported_symbols`。
未重编译 Rust、未更改生产链接选项；符号版具有自己的[构建身份](chat-workers-2026-10-30/profile-build.json)。
**全部受采样干扰的运行均不进入上面的吞吐表；也不将两个不同产物按地址直接对应。**

完整符号版调用树：[单房间](chat-workers-2026-10-30/profile-room-1.txt.gz)、
[多房间](chat-workers-2026-10-30/profile-room-4.txt.gz)。以下是调用树中
`__psynch_mutexwait` 按上层函数归组的结果；[每线程计数](chat-workers-2026-10-30/profile-mutex.csv)保留明细。

| 房间 | 全部互斥等待样本 | 位于 Arena::contains / release 或 Storage::finalize_with 调用链 | 占互斥等待样本 |
| --- | --- | --- | --- |
| 1 | 5411 | 4859 | 89.8% |
| 4 | 5711 | 5341 | 93.5% |

这是所有线程的**栈采样计数**，不是 CPU 时间或锁持有时间占比；条件变量休眠也不能解释为有效工作。
四个 task worker 和 control 线程都出现上述等待，不能据此把全部问题归结为单个控制线程跑满。

优先级建议：

1. **共享分配定位与释放路径。** [Storage](../../src/runtime/parallel/storage.rs) 的 contains、finalize_with、release
   持有根分片表锁，随后锁分片并线性搜索 allocation；[Arena](../../src/runtime/arena.rs) 的入口会转到这些路径。
   采样为这处竞争提供直接线索。下一阶段先区分根表、分片和计费锁的等待，再评估稳定分配标识／索引或受检本地快路径，
   缩短共享锁持有时间。必须保留跨 worker owner 来源、并行预算原子性和清理恰好一次，不直接删除验证或同步。
2. **浮点环境边界。** `__joky_float_env_reset` 在两次采样分别有 1079／965 个栈顶样本；
   Arena::release 为 634／670，Arena::contains 为 494／575，Storage::finalize_with 为 326／322。
   [float_env.rs](../../src/runtime/float_env.rs) 在 AArch64 上写 FPCR/FPSR；chat 的运行时调用密集，
   应评估受检内部调用能否省去冗余重置，同时保留真实宿主边界的舍入、异常状态与恢复保证。
   这里是热点候选，不承诺优化幅度；JIT 业务代码仍有未符号化的地址。
3. **控制桥批次与派发频率。** [control::run](../../src/runtime/parallel/control.rs) 在同一 driver 中处理调用和回复，
   是需要后续观察的路径；但现有采样对分配路径的证据更直接。先处理并复测前项，再决定是否调整控制桥。

上述分析足以选择下一阶段的优化目标，尚未通过实现前后对照证明每个热点的因果收益；不在本次基线提交中混入运行时优化。

符号版采样可在 macOS 上复现。以下 wrapper 只用于这次分析构建：

```sh
mkdir -p target/benchmarks/chat-workers/symbol-tools
cp benches/reports/chat-workers-2026-10-30/profile-cc.py target/benchmarks/chat-workers/symbol-tools/cc
chmod +x target/benchmarks/chat-workers/symbol-tools/cc
PATH="$PWD/target/benchmarks/chat-workers/symbol-tools:$PATH" \
  python3 benches/chat-workers.py prepare \
  --joky target/release/joky --runtime target/release/libjoky.a --profile release \
  --output target/benchmarks/chat-workers/build-symbols
python3 benches/chat-workers.py run \
  --build target/benchmarks/chat-workers/build-symbols \
  --output target/benchmarks/chat-workers/profile-symbols \
  --clients 16 --rooms 1,4 --workers 4 --messages 500 --warmup 25 --samples 1
```

在另一个终端对本次 chat 子进程执行 `sample PID 5 1 -file OUTPUT`。
本次自动采集在每个新进程的 `latencies.csv` 新增长超过 32 KiB 后启动，确保已经进入正式发送阶段，
不采启动编译或预热。单房间和多房间是不同进程，需要分别选取 PID。

## 验证与未覆盖项

Python 语法、帮助与非法参数拒绝检查通过；4 客户端、两种布局 × 1／2／4 worker 的 6 个 AOT 冒烟场景通过。
同一修改后的项目还通过 JIT／解释器各一次小规模协议冒烟，包括广播、顺序、余额与正常停机。
正式 38 个样本共完成 304,000 条广播、3,040,000 次投递（不含预热），全部内容及扣费校验通过、退出码为零且无服务诊断。
另有 4 个仅用于采样的场景，也完成协议校验；它们的计时不纳入基线。

本次无 Rust 或 Joky 实现修改，没有重复执行 `check.sh`、Clippy、全量测试或 stress。
执行了显式 Release 构建、上述针对性运行和 `git diff --check`。本机原始日志及数据保留在 target 输出目录；
报告中提交了紧凑样本、环境、构建身份和压缩调用树，避免把测量通过写成全量验收。

## 解读边界

延迟为客户端写入前到所有接收者读到完整消息的时间，包含回环网络、Python 调度与校验。
表中 P95/P99 为每样本分位数的中位数；RSS 是约 250 ms 采样的最大值，并非精确峰值或 arena 预算。
server CPU 包括整个服务进程，client CPU 不含独立 ps 采样进程；未绑定 CPU，客户端与服务器共用机器。
单台机器、单一并发度和两种广播布局只能建立当前基线，不代表所有 Joky 程序的并行扩展能力。
当前采样也不是长稳、泄漏、慢消费者或连接上限验收。
