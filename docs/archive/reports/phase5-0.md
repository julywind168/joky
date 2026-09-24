# 阶段 5.0 本地验收记录

> 历史记录：本文保留原阶段的设计、环境与验收结论，不代表当前能力。归档于 2026-09-24；当前说明见[文档入口](../../README.md)，未完成工作见[计划索引](../../plans/README.md)。

日期：2026-09-10。代码基于 `50fa95d` 加本次 5.0 修改；本记录与修改
一同提交，可用 `git log --follow -- docs/archive/reports/phase5-0.md` 追溯原验收提交。
目前没有远端 CI，以下均为本地结果。workflow 仅复用已实跑的本地入口。

## 环境与复现

- Apple M1 Pro，8 CPU cores，16 GiB RAM，MacBookPro18,3。
- macOS 26.6.2（25G83），aarch64-apple-darwin。
- 普通/release：rustc 1.97.1（8bab26f4f，LLVM 22.1.6）。
- TSan：rustc 1.100.0-nightly（4aa1fbcf4，2026-09-08，LLVM 23.1.1）。
- 库套件单测试线程；运行时仍使用多 worker，默认本机 16 个。

在仓库根目录运行：

```bash
cargo fmt --check
python3 scripts/check-runtime.py
cargo test --locked --test cli

rustup component add rust-src --toolchain nightly
python3 scripts/check-runtime.py --tsan

JOKY_WORKER_COUNT=16 cargo test --locked --lib --release \
  compiler::tests::benches::bench_suspend_resume_paths \
  -- --ignored --exact --nocapture --test-threads=1
```

`check-runtime.py` 构建并定位本次产生的 lib 测试二进制，先核对 ignored
测试完整名称，再跑串行库套件，最后逐一独立运行文件池测试、三个 socket
测试及取消压力测试。每个隔离进程有 180 秒超时；超时终止并回收进程组；
返回码非零、测试缺失、0 个或多个测试通过都不能被当作成功。详细日志在
`target/runtime-checks/{normal,tsan}/`，workflow 配置为上传该日志目录。

TSan 模式使用独立 `target/tsan` 构建目录，实际编译参数为：

```text
RUSTFLAGS=-Zsanitizer=thread
cargo +nightly test -Zbuild-std --target aarch64-apple-darwin --locked --lib
TSAN_OPTIONS=halt_on_error=1:exitcode=66
```

标准库也插桩，保留 sanitizer ABI 检查；没有 suppressions，不使用
`-Cunsafe-allow-abi-mismatch=sanitizer`。脚本将任何 TSan 报告的非零
退出状态视为失败，不能以 Rust 测试断言通过替代 sanitizer 通过。

## 修复与回归证据

- MIR：task result 检查访问 phi 状态前，用安全索引确认其数据流状态
  存在，并验证它确实是当前块的 CFG 前驱。新增
  `verifier_rejects_invalid_phi_predecessors_with_task_results`，覆盖越界、
  不可达和可达但非前驱三种输入；合法输入先验证通过。原有提前读取任务
  结果的拒绝测试继续通过，诊断优先级保持不变。
- 压力入口：删除一个 cargo 命令中的多个位置过滤参数，全部使用完整
  测试名与 `--exact`，并验证恰好执行一个测试。
- 压力阈值：普通运行仍为 60 秒至少 300 轮。TSan 模式单独设为至少
  50 轮（`JOKY_STRESS_MIN_ITERATIONS`）。完整标准库插桩的首次校准为
  83 轮/60.55 秒，原定 100 轮门槛失败（退出 101，无 TSan 报告）；
  此记录保留，不把它记为通过。以该实测吞吐约 60% 设下限，并复验
  p50/p95/max，区分整体插桩开销与偶发长停顿。历史 174 轮来自混用
  预编译标准库的配置，不能直接套用。该值是本机活跃性检测的校准值，不是
  跨平台性能承诺；换机器需记录校准依据。超时、对象清理断言和 TSan
  报告检查不随阈值降低而放宽。

## 结果

| 检查 | 本轮结果 |
| --- | --- |
| `cargo fmt --check` | 通过 |
| 普通串行 lib 套件 | 858 passed，6 ignored |
| CLI 套件 | 66 passed |
| 普通文件池与三个 socket 隔离测试 | 各自恰好 1 passed |
| 普通取消压力测试 | 1091 轮，60.05 秒，门槛 300；通过 |
| TSan 串行 lib 套件 | 858 passed，6 ignored；无报告，退出 0 |
| TSan 文件池与三个 socket 隔离测试 | 各自恰好 1 passed；无报告，退出 0 |
| TSan 取消压力测试 | 82 轮，60.72 秒，门槛 50；无报告，退出 0 |
| release 基准 | 1 passed，四个场景均完成计数及清理校验 |
| 文档链接与 `git diff --check` | 通过 |

压力测试耗时分布（包含每轮前端/JIT 编译和执行）：

| 模式 | p50 | p95 | 最大 |
| --- | ---: | ---: | ---: |
| 普通 | 54.59 ms | 58.47 ms | 111.47 ms |
| TSan，含 std 插桩 | 727.42 ms | 791.92 ms | 939.93 ms |

两种模式总体耗时比例与稳定分布支持插桩整体开销的解释；本轮未见秒级
长停顿。首次 TSan 的 83 轮及复验 82 轮接近，校准下限 50 轮留有波动
余量。首次 100 轮门槛失败记录在 `target/runtime-checks/tsan-calibration-first.log`。
普通压力初跑 880 轮、复验 1091 轮均通过；普通流程与 TSan 构建/库测试
有部分时间重叠，因此这些轮数不能作为独占机器的性能比较。

旧配置中的 crossbeam-epoch TLS/全局 reactor OnceLock 报告在完整
标准库插桩后未重现；本次没有新增第三方报告，没有添加抑制或改动依赖。
现有证据不足以归因为上游真实竞争，故不提交未经证实的上游报告。
任何以后重现的报告都需保留完整命令、日志及最小复现后再归因。

## release 基准口径

`n` 是整个程序的总挂起次数：四路各执行 `n/4` 次，总计仍是 `n`。
生成器要求可整除，递归返回各分支完成数，程序校验总数等于请求数；
每个场景继续检查 managed object 清理。新增计数累加会改变少量工作量，
本轮数据不能直接用于宣称相对旧版本的性能回归或提升。

每个场景分别 warmup `n` 和 `2n`，再测 5 对数据，交替 n/2n 执行顺序。
每对成本为 `(seconds(2n) - seconds(n)) / n`，报告中位数与最小/最大值；
吞吐为中位成本的倒数，**四路不能再乘以 4**。差分采用有符号浮点数，
非正样本提示噪声；中位数非正时报告 inconclusive，不发生 Duration
下溢，也不产生无穷大/负吞吐。所有样本包含前端、JIT、执行及 scope
关闭；差分仅估计抵消固定开销，不能保证消除编译和调度噪声。

本轮使用默认 release profile，显式 `JOKY_WORKER_COUNT=16`；每个场景
分别测 n/2n，总 12 次运行（2 warmup + 10 measured）。未与其他本任务
测试并行运行基准。全部场景操作数与清理断言通过，结果如下：

| 场景 | n | 中位成本 µs/suspend | 最小–最大 µs/suspend | 总吞吐 suspends/s |
| --- | ---: | ---: | ---: | ---: |
| provider inline，单路 | 5,000 | 8.910 | 8.733–9.419 | 112,230 |
| timer 1ms，单路 | 500 | 1,416.299 | 1,292.941–1,687.294 | 706 |
| timer 0ms，单路 | 5,000 | 27.943 | 26.207–29.424 | 35,787 |
| provider inline，四路 | 5,000 | 21.469 | 20.793–21.964 | 46,578 |

本轮四路/单路总吞吐比约 **0.415**。这说明当前递归挂起工作负载没有
获得吞吐扩展；没有 profiling 证据确定瓶颈，不能据此判断 CPU 计算型
parallel 的收益。各样本差分均为正；timer 1ms 范围较宽，仅作趋势数据。
原始采样输出在本机 `/tmp/joky-phase5-bench.log`；上表为可提交的结果记录。

旧 `8d90b93` 的单路 7.45µs、四路 18.35µs 都是每个总挂起的成本：
四路总吞吐约为单路 0.41 倍。原文的 1.6 倍结论撤回，也不再把未分析
的性能差异归因于 join 开销或用 Cown 测试推断基准无 worker 饥饿。

## 证据边界与后续

- 历史压力 SIGSEGV 未取得能确认根因的栈/复现；后续通过不证明它与
  已修复的 bool 数据竞争或任务等待缺陷存在因果关系。
- TSan 覆盖 Rust 编译器和运行时及插桩依赖；Cranelift 动态生成的
  Joky 机器码没有 TSan 插桩，不能宣称检测了所有 JIT 内存访问。
- 当前没有远端 CI，也未在本次验证 Linux；远端启用后的首次运行和
  跨平台工作流验收分别在基础设施建立后及 5.4 处理。
- 句柄永久退役记录与长期内存增长属于 5.1。此处对象清理测试不替代
  长跑内存验收。
