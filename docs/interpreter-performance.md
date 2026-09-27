# 解释器分派优化：2026-09-27

本文记录 `3c23542` 的历史实验。[后续 Copy 优化](interpreter-copy-optimization.md)
记录在此基础上的局部复制传播、直接写入目标及内部版本对照结果。

基于 `1312ec4` 的诊断确认：当前 while 差距主要来自解释执行器的额外工作。
取指借用替代整条指令克隆、编译时消除可执行 Label 后，三种循环耗时下降 **54–57%**。
这些改动保留共享 IR、值快照、String 所有权、挂起恢复与错误处理语义，没有新增 unsafe 代码。
实现与回归测试已提交为 `3c23542`。

## 已确认的原因

优化前每次取指执行 `frame.function.code.get(frame.pc).cloned()`。
本机 `Instruction` 为 72 字节的 enum，包含 String、Vec 和 Rc 等变体。
整数指令也经过通用克隆、临时值和清理路径；这不意味着每条整数指令都分配堆内存。
只改为借用指令的隔离实验，使三种循环加速约 **2.02–2.13x**。
该收益包括编译器因借用而改善寄存器使用和清理路径的效果，不能视为纯内存复制的占比。

在此基础上，仅消除运行时 Label、将跳转重映射到实际指令，进一步减少约 **8–9%** 的耗时。
连续标签可以共享 PC；末尾标签指向指令流结束。指令、常量和分支元数据不再实现 Clone，
避免无意重新引入整条指令克隆。

macOS `sample` 对优化前的累加工作负载采样 5 秒，3,777 个主线程样本中有 3,776 个
位于 `poll_frame`。源码行映射会受内联和重排影响，不能把采样行数当作精确耗时占比；
隔离对照实验是上述收益的主要证据。N=0 的调用基线约 0.25 µs，也不能解释十余毫秒的循环耗时。

## 指令数量

共享 IR 为可变读取生成快照，解释器逐条执行这些操作。
例如 `total = total + i` 仍包含两次读取 Copy、一次 Binary 和一次写回 Copy。
这些快照保障求值顺序和跨 Pending 的正确性，必须经过数据流证明才能进一步合并。

以下是循环体正常执行一轮的实际分派次数，不含初始化、最终失败检查和返回：

| 负载 | 优化前 Joky | 当前 Joky | Lua-native |
|---|---:|---:|---:|
| 累加 | 14 | 12 | 4 |
| 乘加递推 | 18 | 16 | 6 |
| 分号条件 | 16 | 14 | 5 |

Joky 数字通过手动字节码导出核对。Lua 数字结合本机 `luac -p -l -l` 与官方 Lua 5.5.0
源码计算：`lvm.c` 的整数算术成功路径跳过 `MMBIN*`，比较指令直接消费或跳过条件 JMP。
因此不能直接把 Lua 反汇编行数当作执行次数。Lua 的分号条件负载使用递增、检查和 break 展开。

## 正式复测

环境为 Apple M1 Pro / aarch64 / Darwin 25.6.0，Rust 1.97.1，安装的 Lua 5.5.0。
Rust 使用 bench Release 配置：`opt-level=3`、thin LTO、1 codegen unit，`debug_assertions=false`。
AOT/JIT 保持原有 Cranelift `opt_level=none`。Lua 64 位整数结果在返回时归一化为 Int32；
这三个负载的最终结果等价，详见 [基准方法](../benches/README.md#lua-解释器对照)。

保留优化前后各自的二进制，按“旧版、新版、新版、旧版”顺序执行。
每组 30 个样本，目标批次 50 ms，N=0/100,000；四次运行共 3,600 条样本。
编译、加载、校准和预热均在计时外，测量期间没有并行编译、测试或采样器。
每次运行包含三种 Joky 后端和两个 Lua 变体，轮换顺序并核验返回值。

下列数字为两次运行中位数的中位数，倍率从未舍入值计算：

| 100,000 轮 | 优化前 | 优化后 | 加速 | 耗时下降 | 优化后 / 同轮 Lua-native |
|---|---:|---:|---:|---:|---:|
| 累加 | 11.563 ms | 5.052 ms | 2.29x | 56.3% | 4.61x |
| 乘加递推 | 14.569 ms | 6.318 ms | 2.31x | 56.6% | 3.63x |
| 分号条件 | 12.916 ms | 5.945 ms | 2.17x | 54.0% | 4.66x |

优化版两次运行的中位数差异均低于 0.5%。120 组 min/median/P95/max 已从原始样本重算核对。
P95 是批次平均值分布的 nearest-rank 百分位，不是单次调用的 P95；本结果没有置信区间。
初期定位中一轮与编译重叠的测量被弃用，没有纳入任何结果。
这是单机结果和三个算术负载，不构成 CI 性能阈值或通用语言性能评分。

## 验证与后续

Debug/Release 的 93 个库测试和 20 个 CLI 测试，以及五种后端/变体的结果校验通过。
新增回归覆盖末尾/相邻标签、空循环，以及循环条件多次 Pending 后，在 VM 重载和释放后
恢复到终点且不重放副作用。已有 String 快照、scope、错误和取消测试继续通过。

剩余差距约为 Lua-native 的 **3.6–4.7 倍**。优先继续研究有数据流证明的 Copy 合并、
立即数算术和比较分支融合。24 字节的 Value enum、类型检查、通用挂起分派与指令布局
也是候选方向，但尚未分别测出占比；没有证据表明缓存未命中是本次差距的主因。

## 复现与产物管理

```sh
cargo test --lib dump_while_bytecode -- --ignored --nocapture
luac -p -l -l benches/programs/while.lua
cargo bench --bench while -- --lua --iterations 100000 --samples 30 --sample-ms 50 --output target/benchmarks/interpreter-dispatch
CARGO_PROFILE_RELEASE_DEBUG=1 CARGO_PROFILE_RELEASE_STRIP=none \
  cargo build --release --example interpreter_profile
target/release/examples/interpreter_profile while_sum 100000 15
# In another terminal, use the PID printed after loading:
sample PID 5 1 -file target/interpreter-sample.txt
```

profiling 入口只在加载和校验完成后打印 PID；采样用的构建仅添加符号，不修改优化等级。
比较旧版时，在独立目录检出 `1312ec4`，分别保存两个版本的基准二进制，完成所有编译后再计时。
字节码导出仅在测试构建中提供，默认 ignored；当前代码导出的是优化后的指令流。

Git 保存实现、回归测试、可复用诊断工具和这份结论/方法文档。
原始样本、完整机器元数据、反汇编、字节码转储、临时补丁和二进制作为本地产物保存到
已忽略的 `target/`，不作为源码依赖或测试快照。
本次记录位于 `target/interpreter-investigation/`，整理前的实验资料另存于其 `review-archive/`。
这些路径仅说明本地保存位置，新克隆的仓库不包含它们，需按上述命令重新生成。
