# Chat 预算退款优化（2026-10-31）

> 原始样本、构建元数据与一次性分析脚本已归档到 Git 历史；明细链接指向固定快照，恢复方法见[归档说明](README.md)。

本轮针对共享任务预算的退款争锁：保留多预算收费准入锁，把独占 lease 的退款改为原子减法。
缩减额度、零额退款和额度不变不再进入准入锁。此处的预算是 `runtime::parallel::TaskBudget`，
不包含所有本地 Arena／持久域预算；没有改变 chat 业务、控制桥协议、载荷布局或代码生成优化设置。

本机三轮中位数：4 worker 吞吐提升 9.7%～11.1%、CPU ms/广播下降 5.1%～7.5%；
1 worker 并发矩阵基本不变，但单客户端吞吐下降 1.8%、CPU ms/请求增加 2.9%。
本轮保留并行争锁优化，同时明确记录单客户端代价；三个样本不能建立稳定增益或回归幅度的统计结论。

## 正确性边界

- 收费仍先对预算身份排序去重，取得全部准入锁，共同预检后才更新；任何一个预算不足或加法溢出，全部不收费。
- 持锁预检之后，其他线程只能退款、不能向这些预算收费，因而剩余额度只会增加。
  更新必须使用 `fetch_add`，不能把预检时读到的旧值写回，否则会覆盖并发退款。
- 退款由唯一 lease 执行 `fetch_sub`。缩减额度与销毁共用退款入口，不分批囤积或延迟归还额度。
- `TaskBudget::used()` 保留准入锁，不读取到未完成的多预算收费；多个预算不提供整体用量快照，退款仍逐预算发生。
- 原子计数使用 Relaxed，只承担额度记账；存储发布、资源生命周期和任务取消仍依赖原有同步协议。

新增测试覆盖持有全部准入锁期间仍可退款／缩减／完成零额操作、后一个预算拒绝时前面预算不被收费、
整数溢出，以及四个线程对重叠预算交错收费与退款后仍准确保留活 lease。原有并发准入测试验证只能有一个成功者。

## 测量方法与产物

机器为 Apple M1 Pro（6P + 2E）、16 GiB、macOS 26.6.2 ARM64，Rust 1.99、Python 3.14.6。
采用与前一轮相同的 Release Rust 工具／运行时，AOT host + JIT script，Joky 默认代码生成设置。
Release 只用于本次性能测量；行为测试使用默认 test profile。

- 原版实现为 `597025b`。复用上一轮 `chat-byte-transfer/moved` 的生产二进制，已核对工具／运行时 SHA-256 与本轮采样前的产物一致。
  旧构建记录中的 HEAD 为 `76ab329` 加当时未提交的字节传输改动，该改动随后提交为 `597025b`；不是拿 `76ab329` 实现比较。
- 新版为 `597025b` 加本次 `src/runtime/parallel/storage.rs` 改动。
- 两版使用相同 `.jk` 业务源码、同一个计时器。未重测 Tokio，不拼接历史数据计算新比值。
- 主矩阵：16 客户端、64 字节正文、1／4 房间 × 1／4 worker，预热每客户端 25 条；每批每客户端 250 条，至少 3 秒后按完整批次结束。
- 单客户端：1 房间、1 worker、64 字节正文，每批 5000 条，同样至少 3 秒。
- 三轮串行测量，版本顺序原→新、新→原、原→新；配置顺序第一／三轮为 1→4，第二轮为 4→1。
  每轮先完成两版矩阵，再完成两版单客户端，共 30 个正式样本。
- 编译、启动、join、预热、余额核对与 shutdown 不计时。正式计时期间没有构建、测试或 profiler。
  每个样本验证完整广播接收、发送者顺序、最终余额和退出状态；这是固定闭环负载，不能视为最大服务容量。

[原版构建](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-budget-2026-10-31/before-build.json)、[新版构建](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-budget-2026-10-31/after-build.json)、
[环境与命令](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-budget-2026-10-31/environments.json)、[30 个正式样本](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-budget-2026-10-31/samples.csv)、
[原始产物哈希](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-budget-2026-10-31/artifact-hashes.json)。完整二进制、逐消息延迟和日志保存在
`target/benchmarks/chat-budget/`；原版生产二进制仍在 `target/benchmarks/chat-byte-transfer/moved/`，不提交这些大文件。

## 性能结果

以下为每配置三个样本的中位数，吞吐单位为广播/秒，延迟为发送到全部房间成员收到的完成时间。

| 房间 / worker | 原吞吐 | 新吞吐 | 变化 | 原 CPU ms/广播 | 新 CPU ms/广播 | 原 P99 ms | 新 P99 ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 / 1 | 785.5 | 788.4 | +0.4% | 1.9050 | 1.8950 | 22.15 | 22.11 |
| 1 / 4 | 1147.0 | 1274.1 | +11.1% | 2.7400 | 2.5350 | 15.99 | 14.34 |
| 4 / 1 | 2108.8 | 2091.8 | −0.8% | 0.8163 | 0.8200 | 11.35 | 11.21 |
| 4 / 4 | 2357.7 | 2587.2 | +9.7% | 1.3888 | 1.3175 | 10.13 | 9.94 |

CPU ms/广播先按每样本的服务端 CPU 秒数／实际广播数计算，再取中位数。
矩阵服务端约 1.49～3.44 核，客户端约 0.28～0.52 核，没有一核客户端饱和迹象。
4 worker 的服务端核数上升，但每条广播 CPU 成本下降，不能只看总核数判断效率。

| 房间 / worker | 原吞吐范围 | 新吞吐范围 |
| --- | ---: | ---: |
| 1 / 1 | 777.3～792.6 | 785.1～788.6 |
| 1 / 4 | 1138.0～1152.5 | 1154.2～1291.2 |
| 4 / 1 | 2103.0～2112.2 | 2085.3～2127.0 |
| 4 / 4 | 2299.3～2369.0 | 2555.9～2680.5 |

单房间 4 worker 的第三轮新版本只有 1154.2 广播/秒，几乎与原版相同，增益并非每轮一致。
1 worker 的范围重叠，不声称改善。保留全部样本，不用最好的一轮代表本次收益。

| 单客户端指标 | 原版 | 新版 |
| --- | ---: | ---: |
| 请求/秒 | 1759.9 | 1728.2 |
| P50 ms | 0.5375 | 0.5463 |
| P99 ms | 0.6385 | 0.6401 |
| 服务端 CPU ms/请求 | 0.478 | 0.492 |

单客户端吞吐中位数下降 1.8%，三个新样本均低于三个原样本；P50 增加约 0.009 ms，
CPU 成本增加 2.9%。本轮没有改善单客户端延迟，不能把多 worker 的收益推广到所有负载。
仅靠这些样本无法将小幅退步进一步归因到具体指令；不追加其他优化来混合本轮归因。

采样 RSS 最大值中位数：矩阵原版约 73.3～76.5 MiB、新版约 74.1～77.6 MiB；
单客户端 73.3→73.9 MiB。没有内存改善结论。RSS 每 250 ms 采样，包含内嵌编译器和 allocator 缓存，
不等同于精确峰值或预算用量。

## 独立热点采样

macOS `sample PID 5 1`，4 worker、16 客户端、64 字节正文，分别采 1／4 房间。
每版用只去掉链接时符号剥离的 wrapper 重新链接 AOT host，不重新编译 Rust、不改变生产链接选项。
每个进程的 `latencies.csv` 超过 32 KiB 后才启动采样；测量最短时长 12 秒，采样不包含启动／预热。
采样运行与正式计时分开，采样时吞吐不用于性能对照。

按 `__psynch_mutexwait` 的最近 Joky 上层函数归组：

| 房间 | 版本 | Lease::resize | Lease::drop | 其他互斥等待 | 合计 |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 | 原 | 343 | 326 | 922 | 1591 |
| 1 | 新 | 397 | 0 | 827 | 1224 |
| 4 | 原 | 924 | 690 | 1946 | 3560 |
| 4 | 新 | 639 | 0 | 1643 | 2282 |

原版收费／退款占互斥等待样本约 42%／45%，其中退款约 20%／19%。
新版退款互斥等待消失，收费仍有争锁；样本计数不是调用次数、CPU 时间或每条请求开销，
不能把等待样本下降比例当作吞吐收益。JIT 业务仍有未符号化地址，采样无法完整归因所有工作。

[原版单房间调用树](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-budget-2026-10-31/profile-before-1.txt.gz)、
[原版四房间调用树](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-budget-2026-10-31/profile-before-4.txt.gz)、
[新版单房间调用树](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-budget-2026-10-31/profile-after-1.txt.gz)、
[新版四房间调用树](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-budget-2026-10-31/profile-after-4.txt.gz)、
[逐线程归组明细](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-budget-2026-10-31/profile-mutex.csv)。
新版四房间另有 `Arena::Budget::charge`／`refund` 的互斥等待样本 474／371，
适合作为下一阶段候选；本轮没有修改它们，也不据此预估未来收益。

两版符号构建有独立身份：[原版](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-budget-2026-10-31/before-profile-build.json)、
[新版](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-budget-2026-10-31/after-profile-build.json)。

## 后续研究：Arena 共享预算

在 `e129369` 上检查调用路径后，确认 `arena::Budget` 的 `bytes` 本身已是原子计数；
剩余锁来自 `shared: Option<Mutex<parallel::Lease>>`。`Domains::new` 创建这一总记账凭据，
不同域的根、候选和 worker 调用共享同一个 `Arc<Budget>`，所以不同房间仍争同一把锁。
当前收费顺序是本地额度 CAS → shared mutex → 任务多预算准入锁；退款也先取得 shared mutex，
因此上一轮底层退款原子化尚未消除这个外层串行点。

复用上面的新版采样（运行时代码与 `e129369` 一致）：四房间中 Arena 预算收费／退款的
474 + 371 个互斥等待样本占总数 2282 的约 37%，分布在四个 worker 和 control 线程。
这支持继续优化，但不是 CPU 时间占比，也不能预估能提速 37%。本次研究没有新跑计时或改运行时。

建议用专门的共享记账凭据替代 `Mutex<Lease>`：不可变的预算集合加原子未退额度，
收费复用现有多预算准入规则，失败撤销本次预留；退款直接原子扣减，最后销毁时仅归还剩余额度。
普通 `Lease` 保留独占语义，不能直接删除它外面的锁后并发修改其普通 `usize` 字段。
Arena 自身限额和失败回滚继续保留；不引入按 worker 囤积额度或延迟退款。

实现前必须明确并验证：收费尚未完成时与旧额度退款交错不丢计数，父预算拒绝后本地额度还原，
多个重叠父预算失败不部分收费，最后持有者销毁不重复退款。还需覆盖跨 Arena 转移、
worker 取消／候选失败后回收与三后端域调用。改动集中在预算实现和域注册表接线，不涉及语言语法。
完成后用相同 chat 矩阵和单客户端对照验收，特别检查上一轮单客户端小幅退步；原始产物只留 `target/`。

## 验证与复现

按本轮要求没有运行全量 check.sh、全量测试或压力穷举。实际执行：

- `cargo test --lib runtime::parallel::storage::tests`：10 通过。
- `cargo test --lib task_parallel_aggregate_values_and_roots_on_all_backends`：1 通过，覆盖 JIT、解释器、AOT。
- `cargo test --lib scoped_borrow_control_allocation_failures_refund_partial_replies`：1 通过。
- `cargo test --test cli task_parallel_control_bridge_runs_in_standalone_aot`：1 通过，覆盖独立 AOT 1／3 worker 的控制桥调用。
- `cargo clippy --lib -- -D warnings`、`cargo fmt --check`、`git diff --check`。

新版生产构建：

```sh
cargo build --release --bin joky --lib
python3 benches/chat-workers.py prepare \
  --joky target/release/joky --runtime target/release/libjoky.a --profile release \
  --output target/benchmarks/chat-budget/after
```

输出目录须尚不存在。符号构建沿用[符号 wrapper](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-workers-2026-10-30/profile-cc.py)，将其放到 PATH 首位的目录下并命名为 `cc`。
将历史归档的 [compare.py](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-budget-2026-10-31/compare.py)、
[profile.py](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-budget-2026-10-31/profile.py)、[summarize-profile.py](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-budget-2026-10-31/summarize-profile.py)
复制到 `target/benchmarks/chat-budget/` 后，从仓库根目录执行；脚本所需两版生产／符号构建路径见其源码。
采样命令为 `python3 target/benchmarks/chat-budget/profile.py before`／`after`，
正式对照为 `python3 target/benchmarks/chat-budget/compare.py`。必须先结束所有构建、测试及采样，再执行正式计时。
