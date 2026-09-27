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

## Lua 解释器对照

显式启用系统安装的 Lua；默认构建和测试不需要 Lua 或 `pkg-config`：

```sh
cargo bench --bench while -- --lua --samples 30 --sample-ms 50
cargo test --release --bench while -- --lua --iterations 1000,65536,65537,100000
```

需要 `pkg-config`、`cc` 和 Lua 5.3 及之后的 5.x 开发库，且 `lua_Integer` 为 64 位。
默认读取 `pkg-config lua`；其他包名可用 `LUA_PKG_CONFIG=lua5.4` 等指定，
以本机实际安装的包为准。适配层会检查头文件/运行库兼容性。
目前不支持 `pkg-config` 输出带引号或转义的路径参数；请使用不含空格的安装前缀。
本次实际使用 Homebrew Lua **5.5.0**，动态库为 `liblua.5.5.dylib`。

默认输出目录变为 `target/benchmarks/while-lua/`，也可用 `--output` 覆盖。
额外保存 Lua 源码、Rust/C 适配层、Lua 版本、整数宽度、编译参数和动态库链接信息。
运行器在原有三个后端之外轮换以下两个 Lua 变体：

- **Lua-native**：循环内使用 Lua 原生 64 位整数，只在返回时归一化为有符号 Int32。
- **Lua-mask32**：每轮额外对累加器执行 `& 0xffffffff`，返回时转为有符号 Int32。

这三个负载的累加器只参与加法、乘法，不影响循环条件；因此保留 64 位再取低 32 位，
与每步 wrapping Int32 的最终返回值一致。mask32 变体额外保留每轮低 32 位，
不在循环内转成有符号数；这里没有观察累加器符号的分支或除法，所以结果仍等价。
**这不是任意 Int32 程序与 Lua 整数语义等价的声明**。正确性检查包含负数、
Int32 最小/最大 seed、配置规模及溢出边界；每个计时批次继续校验结果。

Lua 全部使用 `while`，不使用 numeric `for`。分号条件通过“递增、判断、break”展开，
保留最后一次失败检查的递增，但两种语言的源语法和字节码指令数量不同。
Lua 在同一 Rust 进程通过 C 适配层运行，预先保存函数、预留栈空间；
`lua_pcall`、参数传入、返回类型检查和最终归一化包含在计时内。
Lua 错误在 C 层保护调用内处理，不跨越 Rust 栈帧 longjmp。
加载/解析/编译/链接都在计时外，不包含启动 `lua` 子进程的成本。

调用入口仍不同：Lua 使用预先保存的函数，Joky JIT/Interpreter 包含名称查询；
因此主要比较大循环，并保留 N=0 作入口基线。未关闭 Lua 默认 GC，
循环体不分配对象；此基准不代表分配密集、GC 或 CPS 场景。

### 2026-09-27 优化前实测与判断

同一 Apple M1 Pro 上独立运行两轮，每轮每组 30 个样本，目标批次 50 ms。
第一轮每次调用执行 100,000 轮的中位耗时：

| 负载 | Joky Interpreter | Lua-native | Lua-mask32 | Joky / Lua-native | Joky / Lua-mask32 |
|---|---:|---:|---:|---:|---:|
| 累加 | 11.377 ms | 1.079 ms | 1.304 ms | 10.55x | 8.73x |
| 乘加递推 | 14.351 ms | 1.715 ms | 1.941 ms | 8.37x | 7.39x |
| 分号条件 | 12.658 ms | 1.252 ms | 1.467 ms | 10.11x | 8.63x |

第二轮对 Lua-native 的比值分别为 **10.48x / 8.35x / 10.17x**；
两个 Lua 变体和 Joky Interpreter 的大循环中位数跨轮变化都低于 3%。
第二轮部分 P95 明显上升，说明仍有系统调度噪声，不能据此细排几个百分点的差异；
但约一个数量级的解释器吞吐差距在两轮中均存在。
大循环里 Joky 解释器约为 **114–144 ns/轮**，Lua-native 约 **11–17 ns/轮**。
Joky 的 N=0 约 248–285 ns，不足以解释每次大循环十余毫秒的差距。

**若验收目标是接近 Lua 的热循环吞吐，该版本的 Joky 解释器还不合格。**
它可以继续作为语义参考实现；冷路径是否够用应依据具体延迟预算判断，
这次实验没有给出通用的“合格倍率”。按结果看，值得先优化解释执行路径，
而不是优先优化调用入口。代码中可见每条指令的 `clone`、独立的 `Copy`/`Label`
分派和通用 `Value` 类型检查；这些是后续 profiling 候选，不是已证实的热点归因。
本次没有修改生产解释器，也没有改变 AOT/JIT 优化级别。

两轮报告、原始样本和复现信息保留在本地的
`benches/baselines/2026-09-27-apple-m1-pro-lua/`（Git 忽略，不随克隆分发），
共保存 2,700 条样本。比较的是本机安装的 Lua 解释器，没有测试 LuaJIT。

随后已完成 [解释执行路径诊断与优化](../docs/interpreter-performance.md)：
取指改为借用，移除运行时 Label，保留值快照和挂起语义。
历史数据用于对照；当前实现的复测结果见该报告的“正式复测”。

后续已完成 [局部 Copy 优化](../docs/interpreter-copy-optimization.md)。该轮仅比较
Joky 自身版本，未启用外部语言对照；包含新增语义测试、分派数量和独立 Release 复测。

## 产物管理

Git 保存基准/诊断源码、回归测试和精简的结论与复现方法。
新生成的原始样本、完整机器元数据、字节码/反汇编转储、临时补丁和二进制写入
`target/`，不提交。每轮使用独立输出目录；有分享需要时单独归档。
已跟踪的初始基线作为历史记录保留，不因新增忽略规则自动移除。
