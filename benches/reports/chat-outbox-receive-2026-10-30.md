# Chat：outbox 单次接收优化（2026-10-30）

> 原始样本、构建元数据与一次性分析脚本已归档到 Git 历史；明细链接指向固定快照，恢复方法见[归档说明](README.md)。

将标准库 Mailbox.receive 的领取消息、查询关闭、提取字节三次控制操作，合并为一次接收。
本机三轮交替测量中，4 worker 的单房间广播吞吐提高 **37.2%**，四房间提高 **26.7%**；
1 worker 提高约 **3.7%～15.5%**。单客户端请求到回显 P50 从 **0.621 ms 降到 0.569 ms**。
本轮没有重跑 Tokio，不能把新的 Joky 数字与上一轮 Tokio 中位数拼接计算比值。

## 改动及边界

[标准库 outbox](../../src/stdlib/outbox.jk) 仍返回 `Option<Array<UInt8>>`，chat 业务与调用方式不变。
新增私有域操作 `OutboxReceiveBytes`（101），内部返回 `(Bool, Array<UInt8>)`；存在位区分空消息和关闭，
原生后端使用参数后的两个结果槽，解释器解码同样的状态与拥有字节。没有修改公开 CPS 帧布局。
旧的 OutboxReceive／OutboxMessageClosed／OutboxMessageBytes 操作保留兼容，现有编号不变。

运行时在队首出队前完成结果 Buffer 分配；这一步失败保留队首。等待、关闭、取消仍使用原 waiter
与 stop 路径。Parallel 控制回复直接携带存在位和自有字节，调用任务再安装语言 Buffer，
跨线程不传递语言指针；不再创建、查询和销毁中间 MessageOwner 控制代理。
控制回复安装失败或取消仍由原有控制回执和任务清理释放存储，不承诺失败后重投已经出队的消息。

因此每个成功送达的标准库接收从 **3 次控制操作降为 1 次**；16 人广播对应的接收部分从 48 次降为 16 次。
这是静态调用路径的数量，不是操作系统上下文切换次数或 profiler 调用计数。
本轮仍保留队列 Vec → 控制线程 Buffer → 回复 Vec → worker Buffer 的复制链，
也没有修改共享预算、域事务、浮点环境规则或 Cranelift 优化设置。

## 验证范围

遵照本阶段约定，**没有运行全量 check.sh、全量测试或压力穷举**。实际完成：

- `cargo test --lib outbox`：11 通过、1 个穷举忽略，覆盖 JIT／解释器的接收、关闭、取消、
  原子投递、慢连接处理，以及 1／3 worker 的跨任务接收；共享 fixture 增加空消息接收断言。
- `cargo test --test cli outbox_`：4 通过，覆盖 JIT、解释器与独立 AOT 的投递、溢出策略、TCP 写出和 Script 生命周期。
- 定向运行控制桥部分回复分配失败／预算归还测试，以及两个适配器契约测试：3 通过。
- `cargo test --test cli task_parallel_control_bridge_runs_in_standalone_aot`：1 通过，
  共享控制桥 fixture 在独立 AOT 的 1／3 worker 上执行，包含本次调整的 outbox fixture。
- `cargo clippy --lib -- -D warnings`、`cargo fmt --check`、`git diff --check` 通过。
- 下述 30 个正式性能样本全部完成消息内容、接收者、每发送者顺序、最终余额及正常停机校验，服务端 stderr 为空。

## 测量方法

Apple M1 Pro（6P + 2E）、16 GiB、macOS 26.6.2 arm64、Rust 1.99.0、Python 3.14.6。
工具及运行时为 Release，Joky 生成代码仍为原默认设置；AOT host 加内嵌 JIT 脚本。

- 旧产物复用 `target/benchmarks/chat-float-env/build`，实现对应 `c1dc65f`；
  [原始构建身份](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-outbox-receive-2026-10-30/before-build.json)保留 `7f6fc74` 加浮点环境补丁的记录。
- 新产物基于 `58ad67a` 加本次补丁，保存于 `target/benchmarks/chat-outbox-receive/build`；
  [构建身份](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-outbox-receive-2026-10-30/after-build.json)保留工具、运行时、二进制与应用资源哈希。
  [产物哈希](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-outbox-receive-2026-10-30/artifact-hashes.json)另记录修改的代码和补丁。
- 使用同一个 [chat-workers.py](../chat-workers.py)，16 客户端、64 字节正文，1 房间各 16 人／4 房间各 4 人。
  每连接等待自己的广播全部送达后发送下一条；每连接预热 25 条，每批计时 250 条，完整批次重复到至少 3 秒。
- 聚焦 1／4 worker，本阶段不重复 2 worker 矩阵。单客户端另测 1 房间、1 worker，每批 5000 条，至少 3 秒。
  启动、join、预热、余额核对和 shutdown 不计时。广播数记录实际批次总量，不能只用配置中的 250 作为总量。
- 三轮实现顺序为旧→新、新→旧、旧→新；每轮先完成双方并发矩阵，再完成双方单客户端。
  worker 顺序为 `1,4`、`4,1`、`1,4`，房间顺序为 `1,4`、`4,1`、`1,4`。
  每种配置每版 3 个独立服务进程，合计 30 个样本；没有剔除慢样本。
- 所有构建和检查先结束，再串行计时，期间没有编译、测试或 profiler。
  两版各完成 87000 条计时广播；旧样本持续 3.37～6.54 秒，新样本持续 3.11～5.67 秒。

## 三轮结果

各项为独立样本中位数，括号为吞吐 min–max；分位数先逐样本计算再取中位数，不是合并分布的 P99。
吞吐变化按未舍入中位数计算，每条广播 CPU 时间逐样本由服务端 CPU 秒数／广播数得出后取中位数。

| 房间 | worker | 旧广播/秒（范围） | 新广播/秒（范围） | 吞吐增加 | 旧 CPU ms/广播 | 新 CPU ms/广播 | 旧 P99 ms | 新 P99 ms |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 1 | 616.3（612.0–635.4） | 712.1（706.0–718.8） | 15.5% | 2.480 | 2.085 | 27.90 | 28.85 |
| 1 | 4 | 788.8（788.7–878.4） | 1082.0（1081.0–1088.3） | 37.2% | 3.885 | 2.915 | 22.99 | 17.54 |
| 4 | 1 | 1707.6（1707.0–1763.7） | 1770.9（1753.5–1873.9） | 3.7% | 0.989 | 0.930 | 11.81 | 13.63 |
| 4 | 4 | 1803.4（1803.0–1839.3） | 2285.5（2224.2–2299.3） | 26.7% | 1.680 | 1.391 | 12.23 | 10.31 |

每条广播 CPU 成本下降约 6%～25%，但不能称为延迟全面改善：两个单 worker 配置的 P99 中位数都上升了，
四房间 1 worker 第二轮的新吞吐还略低于旧版。只有三个样本，尤其 3.7% 的变化不应当作稳定保证。
所有收益都使用本轮旧版对照，不与历史中位数拼接。

| 房间 | worker | 旧服务端核数 | 新服务端核数 | 旧客户端核数 | 新客户端核数 |
| --- | --- | --- | --- | --- | --- |
| 1 | 1 | 1.53 | 1.49 | 0.31 | 0.35 |
| 1 | 4 | 3.08 | 3.15 | 0.41 | 0.51 |
| 4 | 1 | 1.70 | 1.66 | 0.26 | 0.26 |
| 4 | 4 | 3.08 | 3.18 | 0.30 | 0.33 |

客户端没有出现此前 Tokio 对照中的一核饱和。服务端核数包含控制、I/O 等所有线程，不能等同于 worker 数。
并发矩阵各配置的采样 RSS 最大值中位数，旧版约 73.8～76.1 MiB，新版约 74.7～77.1 MiB。
这是每 250 ms 采样的进程 RSS，包含内嵌编译器和 allocator 缓存，不是精确峰值或预算用量。

| 单客户端指标 | 旧版 | 新版 |
| --- | --- | --- |
| 请求/秒 | 1478.7 | 1604.0 |
| P50 ms | 0.6211 | 0.5688 |
| P95 ms | 0.8137 | 0.7316 |
| P99 ms | 0.9609 | 0.8425 |
| 服务端 CPU ms/请求 | 0.558 | 0.528 |

单客户端吞吐增加 8.5%、P50 下降 8.4%。仍然是完整业务请求到回显路径，不能当作 TCP 单程时间。
本轮确认合并控制操作有效，但尚未解释或消除与 Tokio 的大部分差距。
后续可分别测量控制回复中的字节复制／分配与共享预算锁，保留逐阶段归因，不从本轮结果外推下一阶段收益。

## 复现与记录

```sh
cargo build --release --bin joky --lib
python3 benches/chat-workers.py prepare \
  --joky target/release/joky --runtime target/release/libjoky.a --profile release \
  --output target/benchmarks/chat-outbox-receive/build
python3 benches/chat-workers.py run \
  --build target/benchmarks/chat-outbox-receive/build \
  --output target/benchmarks/chat-outbox-receive/matrix-1-after \
  --workers 1,4 --rooms 1,4 --clients 16 --messages 250 --warmup 25 --samples 1 --min-seconds 3
```

按方法中的版本和配置顺序交替运行；单客户端改为 `--workers 1 --rooms 1 --clients 1 --messages 5000`。
每个输出目录必须尚不存在。旧版须复用原产物或从旧实现构建，不能只切换计时参数充当旧版。

[30 个样本](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-outbox-receive-2026-10-30/samples.csv)、[环境与 argv](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-outbox-receive-2026-10-30/environments.json)
保存紧凑记录。完整二进制、源码快照、补丁、逐消息延迟及服务日志在 `target/benchmarks/chat-outbox-receive/`；
旧二进制仍在 `target/benchmarks/chat-float-env/build`。大体积二进制和逐消息延迟不提交。
