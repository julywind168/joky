# Chat Arena 共享预算优化（2026-10-31）

将域注册表 Arena 峰值预算的 `Mutex<Lease>` 替换为 `SharedLease`：原子记录未退额度，
收费复用原有多预算准入，失败仅撤销本次预留；退款不等待外层互斥锁，最终销毁归还剩余额度。
Arena 本地限额、普通 Lease 的独占语义、跨 Arena 转移及取消清理契约保持不变。
没有修改 chat 业务、语言语法或代码生成优化设置。

## 对照结果

三轮中位数（广播/秒；CPU 为服务端 ms/广播，逐样本计算后取中位数）：

| 房间 / worker | 原吞吐 | 新吞吐 | 变化 | 原 CPU | 新 CPU |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 / 1 | 754.2 | 779.7 | +3.4% | 1.9775 | 1.9175 |
| 1 / 4 | 1228.7 | 1263.1 | +2.8% | 2.6175 | 2.5575 |
| 4 / 1 | 1862.1 | 1813.0 | −2.6% | 0.8850 | 0.9238 |
| 4 / 4 | 2537.2 | 2853.3 | +12.5% | 1.3325 | 1.1717 |

四房间 4 worker 的 CPU/广播下降 12.1%，P99 10.75→8.88 ms；吞吐范围原版 2463～2571、
新版 2652～2959。四房间 1 worker 三个新样本均低于三个原样本，CPU/广播增加 4.4%，
P99 13.41→14.01 ms；保留并行收益，同时明确记录该配置的代价，不声称所有负载都改善。

单客户端吞吐 1601.2→1657.3（+3.5%），P50 0.5655→0.5533 ms，CPU/请求 0.528→0.510 ms。
其吞吐范围原版 1500～1620、新版 1511～1729，波动较大；三次样本不足以确立稳定改善幅度。
矩阵客户端约 0.27～0.55 核，未见一核客户端饱和；采样 RSS 中位数约 74～79 MiB，未见内存改善。
30 个正式样本全部完成协议和退出校验，不追加计时轮次挑选更好结果。

独立 `sample PID 5 1` 采样（4 worker、进入负载后采 5 秒）中，按最近 Joky 上层函数归组的
Arena 收费／退款互斥等待样本：单房间 8／53→0／0，四房间 410／325→0／0。
父预算准入等待仍存在；调用树计数不等于 CPU 时间或每条请求成本，不据此推算提速。
采样使用保留符号的独立 AOT 链接产物，两版 Rust 构建和业务源码分别与正式计时版本一致。

## 方法与复现

- 基线为 `bd6d20d` 的运行时实现（同 `e129369`），复用 `target/benchmarks/chat-budget/after`；
  构建元数据中较早的 HEAD 加当时源码改动对应这份实现。新版为本提交。
- Apple M1 Pro、16 GiB、macOS 26.6.2 ARM64、Rust 1.99、Python 3.14.6；Release Rust 工具／运行时，AOT host + JIT script。
- 16 客户端，64 字节正文，1／4 房间 × 1／4 worker；每客户端预热 25 条，每批 250 条，样本至少 3 秒。
  单客户端用 1 房间、1 worker、每批 5000 条。统计发送到房间全部成员收到的延迟。
- 三轮版本顺序原→新、新→原、原→新，第二轮配置顺序倒置；每轮先两版矩阵，再两版单客户端，共 30 个样本。
  构建、测试及 profiler 均在正式计时前结束；每个样本验证广播、发送者顺序、余额和正常退出。
- 固定闭环负载，不是最大容量测试；没有重测 Tokio，不使用历史数据计算新比值。

```sh
cargo build --release --bin joky --lib
python3 benches/chat-workers.py prepare \
  --joky target/release/joky --runtime target/release/libjoky.a --profile release \
  --output target/benchmarks/chat-arena-budget/after
python3 benches/chat-workers.py run \
  --build target/benchmarks/chat-arena-budget/after \
  --output target/benchmarks/chat-arena-budget/matrix-1-after \
  --workers 1,4 --rooms 1,4 --clients 16 --payload-bytes 64 \
  --messages 250 --warmup 25 --samples 1 --min-seconds 3
```

对两版按上述顺序轮换；单客户端改为 `--workers 1 --rooms 1 --clients 1 --messages 5000`。
每个输出目录须尚不存在。完整脚本、逐样本数据、构建身份、源码补丁和调用树只保存在
`target/benchmarks/chat-arena-budget/`，不提交 Git；基线生产／符号构建在 `target/benchmarks/chat-budget/`。

## 验证

默认 test profile：共享存储 13 项、Arena 5 项、并行域 5 项、独立 AOT 域事务 1 项通过，
覆盖预算拒绝回滚、收费／退款交错、最后持有者回收、跨 Arena 转移、候选失败及取消；
域测试执行 JIT、解释器和 AOT 生成代码。Clippy（`--lib -D warnings`）、格式和 diff 检查通过。
未运行全量 check.sh、全量测试或压力穷举；Release 仅用于性能测量。
