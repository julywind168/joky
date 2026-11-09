# Chat 浮点环境调用边界优化（2026-10-30）

> 原始样本、构建元数据与一次性分析脚本已归档到 Git 历史；明细链接指向固定快照，恢复方法见[归档说明](README.md)。

在上一阶段共享分配索引优化的基础上，本次省去 18 个受审计运行时 helper 返回后的浮点环境重置。
本机三轮交替对照中，各配置广播吞吐中位数提高约 **7%～40%**，每条广播 CPU 时间下降约 **9%～24%**。
单客户端请求到回显 P50 从 **0.818 ms 降到 0.585 ms**。多 worker 的收益比单 worker 小，仍有共享路径限制。

## 实现与验证边界

[native/float_env.rs](../../src/backends/native/float_env.rs) 在用户声明加入前保存受审计 helper 的实际 FuncId，
按声明身份省去返回后的 `reset()`；用户名称、相似符号前缀和间接目标都不自动获得资格。
首批覆盖 Buffer 长度／容量、元素地址、初始化提交、取末元素／字节交换、迭代器推进／投影，
以及 CPS 槽地址、错误码查询和取消检查。别名映射到同一个实际 helper 时共享其资格。

这些路径保持控制位和状态标志，只操作整数元数据／字节；失败路径仅构造和记录 Error。
可能析构旧元素的 Store、Truncate、Replace、Release，以及资源、poll、stop、清理、浮点转换和
未知／间接调用继续重置。函数入口／出口和解释器 Guard 沿用原规则。
不保证每个内置操作清除已有浮点异常标志；完整调用仍恢复外层宿主的控制位和状态。
当前不变量见[架构说明](../../docs/architecture.md)。

实际验证：

- 生成代码执行探针：三次已审计错误码查询没有额外重置，任意宿主与间接调用各重置一次；
  宿主故意设置非标准舍入和状态后，生成代码仍得到最近偶数结果，并恢复完整的调用前环境。
- 共享浮点 fixture 扩充容器初始化、借用迭代、元素访问与失败返回，JIT／解释器／独立 AOT 均执行；
  原有重入、次正规数、Float32/64、Pending、恢复、取消和析构用例继续通过。
- **一次**完整 `./scripts/check.sh`：fmt、Clippy、Debug 库测试 1834 通过、CLI 340 通过、
  其他默认集成测试及三后端 while／Map 短输入正确性全部通过。
- 没有重复运行全量检查，没有运行全部 ignored 穷举或 Release 全量测试。
  验证机器为 macOS ARM64，本次没有 x86_64 实机复测。

## 测量方法与产物

运行器、机器和负载沿用[上一阶段](chat-storage-index-2026-10-30.md)：Apple M1 Pro（6P + 2E）、
16 GiB、macOS 26.6.2 arm64、Rust 1.99.0、Apple clang 21.0.0；工具／静态运行时为 Release，
Joky 生成代码采用默认设置。应用是 AOT host + JIT 业务脚本，Parallel 模式的 worker 数为 1／2／4，
控制、I/O 和文件线程另计。仓库 chat 业务代码没有改动。

- 旧版复用 `target/benchmarks/chat-storage-index/build`，对应上一阶段 `7f6fc74` 的实现。
  该产物的原始构建记录是 `3847d6f` 加当时的存储优化补丁，不能把运行时 HEAD 当作其构建身份。
- 新版基于 `7f6fc74` 加本次 native 后端改动，构建时工作树为 dirty。
  [旧版身份](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-float-env-2026-10-30/before-build.json)、[新版身份](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-float-env-2026-10-30/after-build.json)
  包含工具、静态运行时、应用和资源 SHA-256；[产物哈希](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-float-env-2026-10-30/artifact-hashes.json)
  另保存编译器补丁及两份实现文件的哈希。
- 所有构建和检查结束后串行计时，期间没有编译、测试或采样。
  三轮旧／新顺序是旧→新、新→旧、旧→新；worker 顺序为 `1,2,4`、`2,4,1`、`4,1,2`，
  房间顺序为 `1,4`、`4,1`、`1,4`，每个配置每版有 3 个独立进程样本。
- 并发矩阵为 16 客户端、1 房间各 16 人／4 房间各 4 人、64 字节正文；每连接预热 25 条、
  计时 500 条，共 8000 条广播。每连接等待本房间全部接收者收到后才发送下一条。
- 单客户端另测 1 房间、1 worker、每样本预热 25 条并计时 5000 条，按相同旧／新顺序交替三轮。
- 编译、启动、join、预热、余额核对及 shutdown 不计时。所有 42 个样本均通过消息内容、接收者、
  顺序、扣款与正常退出校验，没有剔除慢样本。固定并发闭环负载不代表最大容量或过载排队表现。

## 三轮对照结果

各项取样本中位数，括号为吞吐 min–max。P99 先逐样本统计，再取中位数，不是合并分布的 P99。
百分比按未舍入的中位数计算。每条广播 CPU 时间逐样本用服务端 CPU 秒数除以完成广播数，再取中位数。

| 房间 | worker | 旧广播/秒（范围） | 新广播/秒（范围） | 吞吐增加 | 旧 P99 ms | 新 P99 ms | 旧 CPU ms/广播 | 新 CPU ms/广播 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 1 | 449.6（413.5–456.6） | 610.0（574.2–641.5） | 35.7% | 44.31 | 28.52 | 3.140 | 2.525 |
| 1 | 2 | 504.0（473.0–508.1） | 656.1（650.9–683.7） | 30.2% | 39.39 | 27.20 | 3.380 | 2.791 |
| 1 | 4 | 730.0（650.2–732.1） | 849.4（796.2–908.5） | 16.4% | 24.91 | 21.31 | 4.240 | 3.712 |
| 4 | 1 | 1299.8（1146.0–1306.4） | 1814.3（1787.2–1830.3） | 39.6% | 14.87 | 11.41 | 1.233 | 0.939 |
| 4 | 2 | 1532.1（1425.2–1554.6） | 1835.6（1754.6–1845.1） | 19.8% | 15.02 | 11.87 | 1.502 | 1.275 |
| 4 | 4 | 1776.3（1772.1–1785.3） | 1900.0（1846.1–1919.5） | 7.0% | 13.34 | 12.04 | 1.802 | 1.633 |

新版单房间 1→4 worker 吞吐增加约 39%，四房间只增加约 5%；多 worker 的单位 CPU 成本仍更高。
吞吐与尾延迟存在波动，例如第一轮单房间 4 worker 的新版 P99 为 33.2 ms，高于同轮旧版 24.6 ms，
不能将中位数收益解释为每次运行、每条请求都改善。只有三个独立样本，不给置信区间或跨机器保证。
旧版本轮比上次测量慢，全部收益使用本轮旧版对照，不与历史中位数拼接。

各配置采样 RSS 最大值的中位数，旧版约 75.0～77.0 MiB、新版约 75.4～76.9 MiB。
RSS 包含编译器、运行时与 allocator 缓存，仅每 250 ms 采样，不等同于预算峰值或泄漏证明。

| 单客户端指标 | 旧版 | 新版 |
| --- | --- | --- |
| 请求/秒 | 1146.9 | 1582.5 |
| P50 ms | 0.818 | 0.585 |
| P95 ms | 0.971 | 0.710 |
| P99 ms | 1.184 | 0.887 |
| 服务端 CPU ms/请求 | 0.746 | 0.520 |

单客户端吞吐提高约 38%，P50 下降约 28%，CPU 时间下降约 30%。这是包含解析、脚本、域访问、
扣费、队列、回环网络和客户端调度的请求到回显路径，不能当作 TCP 单程发送耗时。

## 采样复核与后续候选

正式计时结束后另用保留符号的产物采样，4 worker、16 客户端、1／4 房间，每连接 2000 条，
预热 25 条；进入测量流量后以 1 ms 间隔采 5 秒。链接只去掉 `-S,-x`，与上一阶段 wrapper 一致；
符号版的[独立构建身份](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-float-env-2026-10-30/profile-build.json)已保存。
这两个受采样干扰的运行不进入吞吐表。旧采样复用上一阶段同参数产物。

| 房间 | 旧 reset 栈顶样本 | 新 reset 栈顶样本 | 旧全部线程栈样本 | 新全部线程栈样本 |
| --- | --- | --- | --- | --- |
| 1 | 3416 | 871 | 44421 | 45604 |
| 4 | 3134 | 771 | 43069 | 45175 |

完整调用树：[单房间](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-float-env-2026-10-30/profile-room-1.txt.gz)、
[四房间](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-float-env-2026-10-30/profile-room-4.txt.gz)；
[栈顶计数](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-float-env-2026-10-30/profile-top.csv)另含 enter、restore 与互斥等待。
这说明 reset 热点减少；样本数不是调用次数、CPU 时间或每条请求的开销，不能把它的下降比例当作吞吐收益。

新采样中，`__psynch_mutexwait` 共 1925／2855 个栈顶样本。
按最近的 Joky 上层函数归组，`Lease::resize` 为 578／900，`Lease::drop` 为 444／842，
合计占互斥等待约 53%／61%；另外还有本地 Arena Budget、调度及 I/O 等等待。
[分组明细](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-float-env-2026-10-30/profile-mutex.csv)保留原始符号和计数，不把条件变量休眠当作有效工作。
下一阶段优先评估共享预算的收费／退款锁，同时保留多预算原子准入和唯一退款契约。
入口／出口 Guard 仍有成本，但本次不据此删除宿主隔离，也没有继续修改预算协议。

## 复现与原始记录

[42 个样本](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-float-env-2026-10-30/samples.csv)、[环境和每次 argv](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-float-env-2026-10-30/environments.json)
保留紧凑数据。完整逐消息延迟、服务日志、源码副本、构建日志和二进制位于
`target/benchmarks/chat-float-env/`，未提交大体积逐消息文件和二进制。

```sh
cargo build --release --bin joky --lib
python3 benches/chat-workers.py prepare \
  --joky target/release/joky --runtime target/release/libjoky.a --profile release \
  --output target/benchmarks/chat-float-env/build
python3 benches/chat-workers.py run \
  --build target/benchmarks/chat-float-env/build \
  --output target/benchmarks/chat-float-env/matrix-1-after \
  --clients 16 --rooms 1,4 --workers 1,2,4 --messages 500 --warmup 25 --samples 1
```

按方法中的顺序轮换配置和新旧 build，输出目录每次须不存在；单客户端使用
`--clients 1 --rooms 1 --workers 1 --messages 5000`。旧版须复用旧产物或从旧代码重新构建。
