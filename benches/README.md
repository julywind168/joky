# While 性能基准

运行同一份 [while.jk](programs/while.jk)，比较当前 AOT、JIT 和 Interpreter 的同步循环吞吐。
基准入口为 [while.rs](while.rs)，使用标准库计时和已有的 `libloading`，不引入新依赖。

## 运行

需要 macOS/Linux、Rust 工具链和系统 `cc`：

```sh
cargo bench --bench while
```

默认测试 `N = 0 / 1,000 / 100,000`，每组 15 个样本，目标每样本 30 ms。
基准使用 Cargo bench 优化构建。自定义规模和输出目录：

```sh
cargo bench --bench while -- --iterations 1000,10000,100000 --samples 30 --sample-ms 50 --output target/benchmarks/while-run2
```

`N=0` 总会包含。`CC` 可指定链接器驱动程序的可执行文件路径，不支持在变量内夹带命令行参数。
打印参数帮助用 `cargo bench --bench while -- --help`。

只检查正确性，不计时、不写报告：

```sh
cargo test --bench while
cargo test --release --bench while
cargo bench --bench while -- --check
```

`cargo test --all-targets` 也只执行正确性检查。直接运行基准二进制时，需要 `--bench` 才会计时。

## 工作负载

| 内核 | 循环体 | 用途 |
|---|---|---|
| `while_sum` | 累加循环变量 | 比较、回边、计数器和累加 |
| `while_recurrence` | 乘加递推，依赖上一轮结果 | 更重的算术，减少简单求和闭式化带来的误判 |
| `while_sequence` | 条件中执行 `checks = checks + 1; checks <= n` | 分号条件与可变状态读取 |

所有内核都有相同 `(n: Int32, seed: Int32) -> Int32` 签名；定时输入 seed 为 7。
运算按 Int32 环绕，Rust 参考实现显式使用 `wrapping_*`。
执行前比较三个后端在负数、零、小规模、配置规模及多种 seed 下的结果；
每个计时批次结束后还验证返回值校验和。

## 测量口径

- **只测预热后的执行**。解析、编译、对象输出、链接、动态库加载及正确性检查均不计时。
- AOT 生成对象文件，通过系统 `cc` 链接成动态库，在同一进程内调用真实 AOT 导出。
  动态库所有者在整个测量期间保活，避免把进程启动成本计入循环。
- JIT 与解释器使用已有的 `VmEngine::call_int32_2`。AOT 使用已解析的原生函数指针，
  因此调用入口不同：JIT/Interpreter 包含名称查询和签名校验，AOT 没有。
  `N=0` 是各自的调用/分派基线；大 N 摊薄入口开销，不将短调用比值解释成循环速度比。
- 运行参数和返回值通过 `black_box`，结果在计时区外验证，避免宿主侧消除无用调用。
- 各后端单独校准批次调用数，再预热一个批次；每个样本轮换三后端的顺序。
  调用数最多 10,000,000；单次调用已超过目标时，每样本仅调用一次。
- 报告 min、median、P95、max、每轮耗时和相对 AOT 的比值。
  每轮耗时为 `median / N`，不扣除基线；P95 使用 nearest-rank，15 样本时等于最大值。
  这是批次平均值的分布，不是单次调用延迟的 P95，也不提供置信区间。
- 保留当前后端默认代码生成配置，不为基准修改优化级别。初始版本 Cranelift 0.136.1
  的 `opt_level` 为 `none`；Rust bench 优化构建与 Joky 生成代码的优化级别是不同设置。

## 产物与解释

默认输出到 `target/benchmarks/while/`，同目录重跑会覆盖旧结果。需要比较多轮时使用不同的 `--output`。

- `report.md`：环境、方法和结果。
- `summary.csv`：每个内核、规模和后端的统计汇总。
- `samples.csv`：全部原始样本、调用次数、纳秒耗时和预期结果。
- `metadata.txt`：时间、Git revision/dirty、CPU、OS、Rust、cc、运行参数及默认优化级别。
- `while.jk`、`runner.rs`、`Cargo.lock`：测量源码、运行器和依赖版本快照。

测量时避免并行编译、后台重负载、低电量模式或温度变化；本运行器不固定 CPU 亲和性。
同一机器独立重跑几次后再判断回归，不直接将不同机器、调用入口或优化设置的数字相除。
当前基准不覆盖编译耗时、冷启动、CPS/挂起、宿主调用、内存分配或标准库性能。
后续优化应先复用此基线再追加专项负载，不能仅凭一个求和循环推断整体性能。

## 初始实测基线

2026-09-27，Apple M1 Pro / aarch64 / Darwin 25.6.0，Rust 1.97.1，
Cranelift 0.136.1 默认 `opt_level=none`。代码基于 `fae2f81`，只增加本基准及文档，
没有修改生产编译器或解释器。运行命令：

```sh
cargo bench --bench while -- --samples 30 --sample-ms 50
```

每次调用执行 100,000 轮循环的中位耗时：

| 内核 | AOT | JIT | Interpreter | Interpreter / AOT |
|---|---:|---:|---:|---:|
| 累加 | 63.288 µs | 63.451 µs | 11.411 ms | 180.30x |
| 乘加递推 | 127.108 µs | 127.062 µs | 14.663 ms | 115.36x |
| 分号条件 | 63.509 µs | 63.458 µs | 12.669 ms | 199.49x |

本轮大循环中 AOT 与 JIT 的差异低于 0.3%，不据此判断谁更快。
N=0 时调用中位数约为 AOT 2.3 ns、JIT 21.7–25.5 ns、Interpreter 252.6–280.5 ns，
反映上述调用入口差异，不能当作 while 迭代性能。

[完整报告](baselines/2026-09-27-apple-m1-pro/report.md)、
[统计汇总](baselines/2026-09-27-apple-m1-pro/summary.csv) 和
[810 条原始样本](baselines/2026-09-27-apple-m1-pro/samples.csv) 随代码保存。
这是单机初始基线，不是 CI 性能门槛；未来回归比较应复测环境和样本波动。
