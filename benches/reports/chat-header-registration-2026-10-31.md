# Chat Buffer 头部登记优化（2026-10-31）

将并行存储中的 Buffer 头部分配和元数据 finalizer 登记合并：先在登记锁外初始化空头部，再一次性登记分配与
finalizer。省去原来 `finalize_with` 的第二次索引查询、索引锁和分片锁。
索引／分片元数据准入失败时，在解锁后清理未发布的头部；载荷分配失败留下的空头部仍由根回收。
Local Arena 保持载荷分配成功后再写入完整头部的流程。本轮没有修改来源校验、递归释放、载荷布局、预算准入或 chat 业务。

## 结果

初版也提前初始化 Local 头部，30 个样本中观察到单客户端中位数下降 6.6%。因此最终版
恢复 Local 原流程，只在并行存储中合并登记；下面只统计最终版重新对照的 30 个样本，不混用初版数据。

最终版收益有限，chat 实测基本持平，未证实稳定提速。三轮中位数如下；CPU 为逐样本计算后取中位数的服务端 ms/广播。

| 房间 / worker | 原吞吐 | 新吞吐 | 变化 | 原 CPU | 新 CPU |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 / 1 | 777.6 | 760.9 | −2.1% | 1.9225 | 1.9625 |
| 1 / 4 | 1276.4 | 1266.8 | −0.7% | 2.5350 | 2.5225 |
| 4 / 1 | 1967.4 | 1990.0 | +1.1% | 0.8625 | 0.8588 |
| 4 / 4 | 2910.1 | 2948.2 | +1.3% | 1.1708 | 1.1742 |

各配置吞吐范围均重叠；4 房间／1 worker 的小幅提高不足以证明已稳定抵消前一轮的退步。
单客户端吞吐 1672.8→1698.8（+1.6%），P50 0.5482→0.5453 ms，CPU/请求 0.504→0.497 ms；
三个配对样本均未再出现初版的下降，但仍不能据三个样本声称稳定改善。
矩阵客户端约 0.27～0.54 核，未见一核客户端饱和；采样 RSS 中位数约 74.5～78.4 MiB，无内存改善结论。

独立 macOS `sample PID 5 1` 采样使用保留符号的链接产物，在正式负载开始后采集。
4 worker 的单／四房间中，`finalize_with` 互斥等待样本 18／114→0／0；
总互斥等待样本 1092／2117→1184／2313，并未下降。计数不是 CPU 时间或每请求成本，不能推算收益。
保留本次局部登记去重，但后续应关注占比更大的来源查询／释放及预算准入，而非继续扩大 finalizer 改造。

## 方法与复现

- 原版为 `c175ab5`，复用 `target/benchmarks/chat-arena-budget/after`；新版为本提交。
  两版相同 Joky 业务源码、Release Rust 工具／运行时、AOT host + JIT script、默认代码生成设置。
- Apple M1 Pro、16 GiB、macOS 26.6.2 ARM64、Rust 1.99、Python 3.14.6。
- 16 客户端、64 字节正文、1／4 房间 × 1／4 worker；每客户端预热 25 条，每批 250 条，每样本至少 3 秒。
  单客户端使用 1 房间、1 worker、每批 5000 条。延迟为发送至全部房间成员收到的时间。
- 三轮版本顺序原→新、新→原、原→新，第二轮配置顺序倒置；每轮先矩阵再单客户端，共 30 个正式样本。
  计时期间没有构建、测试或 profiler；每个样本核对广播、发送者顺序、余额与正常退出。
- 固定闭环负载，不代表最大服务容量；三次样本不足以建立稳定增益的统计结论，没有重测 Tokio。

```sh
cargo build --release --bin joky --lib
python3 benches/chat-workers.py prepare \
  --joky target/release/joky --runtime target/release/libjoky.a --profile release \
  --output target/benchmarks/chat-header-registration/final
python3 benches/chat-workers.py run \
  --build target/benchmarks/chat-header-registration/final \
  --output target/benchmarks/chat-header-registration/final-matrix-1-final \
  --workers 1,4 --rooms 1,4 --clients 16 --payload-bytes 64 \
  --messages 250 --warmup 25 --samples 1 --min-seconds 3
```

两版按上述顺序轮换；单客户端改为 `--workers 1 --rooms 1 --clients 1 --messages 5000`。
输出目录须尚不存在。脚本、原始样本、环境、构建身份、补丁和独立采样调用树只留在
`target/benchmarks/chat-header-registration/`，不提交 Git。原版生产／符号构建仍在 `target/benchmarks/chat-arena-budget/`。

## 验证

默认 test profile 的 24 个不同测试通过：共享存储 14 项、Buffer 8 项、三后端并行数组 1 项、
控制桥部分回复分配失败 1 项。新增回归覆盖初始化后登记失败、锁外 finalizer 和载荷失败后的空头部回收；
现有生成代码测试覆盖 JIT、解释器、AOT 的拥有数组及任务根清理。
Clippy（`--lib -- -D warnings`）、格式和 diff 检查通过。未运行全量检查或压力穷举；Release 仅用于性能测量。
