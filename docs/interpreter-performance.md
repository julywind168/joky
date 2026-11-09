# 解释器性能实验记录

本文集中保存历史性能测量；各行只在同一次对照实验内可比较，
不是当前版本的自动性能承诺。当前实现见 [解释器设计](interpreter.md)，
运行器、Lua 语义及统计口径见 [基准方法](../benches/README.md)。

## 环境与测量边界

下述早期解释器实验均在 Apple M1 Pro / aarch64 / Darwin 25.6.0、Rust 1.97.1 上完成。
Rust 使用 opt-level 3、Thin LTO、一个 codegen unit；JIT/AOT 保持 Cranelift 默认
opt_level=none。表中为每次调用执行 100,000 轮的解释耗时，编译/链接/加载和预热不计时。

每个版本先构建并保存独立二进制，完成全部构建后才交替计时；正式测量没有同时编译、
测试或运行采样器。汇总取同一阶段两次运行中位数的中位数，百分比从未舍入数计算。
P95 是批次平均值分布的 nearest-rank 百分位，不是单次调用的 P95。
这些实验没有置信区间，不设 CI 阈值，也不覆盖 CPS、分配、深层调用或编译性能。

## 初始基线

最早三后端实测基于 `fae2f81`，每组 30 样本、目标批次 50 ms；
[完整报告](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/baselines/2026-09-27-apple-m1-pro/report.md)、
[统计汇总](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/baselines/2026-09-27-apple-m1-pro/summary.csv) 和
[810 条原始样本](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/baselines/2026-09-27-apple-m1-pro/samples.csv) 已归档到清理前的 Git 版本，
恢复方法见[历史基准归档](../benches/reports/README.md)。

| 内核 | AOT | JIT | Interpreter |
| --- | ---: | ---: | ---: |
| 累加 | 63.288 µs | 63.451 µs | 11.411 ms |
| 乘加递推 | 127.108 µs | 127.062 µs | 14.663 ms |
| 分号条件 | 63.509 µs | 63.458 µs | 12.669 ms |

此轮 AOT/JIT 差异低于 0.3%，不足以判断谁更快。N=0 的调用中位数约为
AOT 2.3 ns、JIT 21.7–25.5 ns、Interpreter 252.6–280.5 ns，反映入口成本差异。

优化前另外运行两轮 Lua 5.5.0 对照，每组 30 样本、目标 50 ms。第一轮：

| 内核 | Joky Interpreter | Lua-native | Lua-mask32 |
| --- | ---: | ---: | ---: |
| 累加 | 11.377 ms | 1.079 ms | 1.304 ms |
| 乘加递推 | 14.351 ms | 1.715 ms | 1.941 ms |
| 分号条件 | 12.658 ms | 1.252 ms | 1.467 ms |

Joky/Lua-native 倍率为 10.55/8.37/10.11，第二轮为 10.48/8.35/10.17。
大循环中位数跨轮变化低于 3%，但部分 P95 上升，表明有调度噪声。
当时的差距足以优先调查循环执行路径；不能用这些旧数字评价后续版本。
2,700 条本地样本位于 Git 忽略的 `benches/baselines/2026-09-27-apple-m1-pro-lua/`。
没有测试 LuaJIT，Lua 两种整数变体的等价范围见基准方法。

## 分阶段测量

| 阶段 | 基线 → 实现提交 | 顺序 | 样本/组；目标批次 | 样本总数 |
| --- | --- | --- | --- | ---: |
| 借用取指、消除 Label | `1312ec4` → `3c23542` | 旧/新/新/旧 | 30；50 ms | 3,600 |
| 局部 Copy 优化 | `fd9a803` → `e02cdc7` | 旧/新/新/旧 | 30；50 ms | 2,160 |
| 立即数与比较分支融合 | `e02cdc7` → `7035a3d` | 旧/立即数/融合/融合/立即数/旧 | 30；50 ms | 3,240 |
| 迭代执行、静态绑定、槽位压缩 | 融合阶段 → `0fd9c09` | 旧/新/新/旧 | 7；15 ms | 504 |

每轮包括三个内核、N=0/100,000 及三个 Joky 后端。只有第一阶段还包含两个 Lua 变体。
每阶段统计分布分别为 120/72/108/72 组，均从原始 elapsed_ns/calls 重算核对。

| 阶段 | 累加：前 → 后 | 递推：前 → 后 | 分号条件：前 → 后 | 耗时下降 |
| --- | ---: | ---: | ---: | --- |
| 借用取指、消除 Label | 11.563 → 5.052 ms | 14.569 → 6.318 ms | 12.916 → 5.945 ms | 56.3% / 56.6% / 54.0% |
| Copy | 5.052 → 3.022 ms | 6.358 → 4.286 ms | 5.926 → 3.905 ms | 40.2% / 32.6% / 34.1% |
| 立即数与分支融合 | 2.993 → 2.478 ms | 4.270 → 3.323 ms | 3.860 → 3.055 ms | 17.2% / 22.2% / 20.9% |
| 迭代执行等 | 2.555 → 1.625 ms | 3.352 → 2.469 ms | 3.142 → 2.097 ms | 36.4% / 26.3% / 33.3% |

仅立即数阶段为 2.835/3.638/3.438 ms，比 Copy 基线降低 5.3%/14.8%/10.9%；
其上再加分支融合降低 12.6%/8.7%/11.1%。只停用融合 pass 不能严格重建该实验，
因为融合指令 enum 和分派代码本身也影响布局；原实验保存了加入融合实现前的二进制。

前三阶段优化版的两次中位数差异分别小于 0.5%、为 0.64/0.76/2.32%、为 0.41/1.18/0.23%。
最后一阶段只是默认预算关闭下的短回归检查，噪声明显，不等同于前三轮长测量。
它核对了旧融合二进制的 SHA-256，但不测试宿主调用或深层调用吞吐。
不要把跨轮数字直接相除或把多个百分比相加。

项目精简轮次另用 `0fd9c09` 的已核对哈希二进制作短对照（同日，旧/新/新/旧，
每组 7 样本、15 ms，计时无并行构建）。累加/递推/分号条件为
1.571/2.467/2.112 ms → 1.604/2.478/2.104 ms，变化 +2.1%/+0.4%/−0.4%，
不据此宣称循环提速。504 个样本、72 组汇总均重算核对；新旧二进制、哈希和报告
保存在本地 `target/project-cleanup/`。精简主要消除分层与重复分配，不改变循环字节码。

## 实验解释

借用取指实验单独去掉整条指令 clone，得到约 2.02–2.13x 加速；随后只消除 Label，
进一步减少约 8–9% 耗时。当时 Instruction 是 72 字节并包含 String/Vec/Rc 变体，
但这不等于每条整数指令都有堆分配。收益也包括 Rust 优化器对寄存器和清理路径的改变。
`sample` 的 3,777 个主线程样本中 3,776 个在 poll_frame；内联/重排使行号采样不能作为精确归因。
早期一轮与编译重叠的计时已弃用。

正常进入循环体一轮的分派数量（不含初始化、末次失败检查及返回）：

| 内核 | 最初 | 去 Label | Copy | 立即数 | 分支融合 | Lua-native |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 累加 | 14 | 12 | 6 | 5 | 4 | 4 |
| 乘加递推 | 18 | 16 | 10 | 7 | 6 | 6 |
| 分号条件 | 16 | 14 | 9 | 7 | 6 | 5 |

Lua 数字结合当时 luac 输出与 Lua 5.5.0 lvm.c 路径核对，算术快路径跳过 MMBIN，
比较直接消费/跳过 JMP；不能简单统计反汇编行数。该阶段 Joky 仍用 14/18/16 槽位、
72 字节 Instruction、24 字节 Value。迭代执行阶段变为 5/7/7 槽位和 48 字节 Instruction，
Value 仍为 24 字节（本机测量）。这是存储变化，不是各优化独立收益的证明。

Copy 和融合的安全规则统一维护在 [字节码设计](interpreter.md#register-bytecode)：
不跨控制流/调用边界传播，不移除可失败算术，不改变快照和求值顺序，重映射所有跳转及源位置。
相关回归覆盖差分执行、整数边界、多个 Bool 使用者、重复 Pending、不重放副作用及模块释放。
迭代执行阶段还验证 256 KiB 原生栈上的 5,000 层调用、3,000 层 parallel（现名 join），以及预算、取消和错误轨迹。
历史阶段的 Debug/Release 库测试分别为 93/103/113/135 个，CLI 为 20/20/20/21 个；
这些是当时的结果，当前覆盖以实际测试输出为准。

## 复现与产物

```sh
cargo test --lib dump_while_bytecode -- --ignored --nocapture
cargo test --release --bench while -- --iterations 1000,65536,65537,100000
cargo bench --bench while -- --iterations 100000 --samples 30 --sample-ms 50 --output target/benchmarks/recheck
```

需要 Lua 对照时追加 `--lua`，查看 Lua 字节码可运行 `luac -p -l -l benches/programs/while.lua`。
比较历史版本应在独立目录检出对应提交、使用相同构建参数，保存各自二进制和哈希，
完成全部构建后按上面的交替顺序计时。运行器的 Git 信息反映运行目录，不能替代二进制身份记录。

本地归档目录分别为 `target/interpreter-investigation/`、`target/interpreter-copy-investigation/`、
`target/interpreter-fusion-investigation/`、`target/interpreter-design-review/`。
这些被 Git 忽略的目录保存样本、完整环境、哈希、中间实验补丁与日志，新克隆不会包含它们。
历史详细过程可在上述提交中查看，不再为每个优化阶段重复维护一份当前设计文档。

CPU 采样使用可复用入口：

```sh
CARGO_PROFILE_RELEASE_DEBUG=1 CARGO_PROFILE_RELEASE_STRIP=none \
  cargo build --release --example interpreter_profile
target/release/examples/interpreter_profile while_sum 100000 15
# In another terminal, after the program prints its PID:
sample PID 5 1 -file target/interpreter-sample.txt
```

该入口完成加载和正确性检查后才打印 PID；采样构建只增加符号，不改变优化等级。

## Map 首版基线

2026-10-02，Apple M1 Pro / aarch64、Darwin 25.6.0、rustc 1.97.1、Apple clang 21。
Map/编译器/运行时实现基于 `ba4f66a`，工作树增加 M7 测试和基准；运行时元数据明确记录 dirty=true。
Rust Release 优化构建，Cranelift 默认 opt_level=none，未为测量修改代码生成设置。
完整正确性检查完成后，使用同一已构建运行器串行执行两轮，无并行构建或测试。
每轮 N=0/32/128、15 个样本、目标批次 30 ms；每轮 1,620 条样本与 108 组汇总，
已从原始 elapsed_ns/calls 复算 median/P95 并核对预期结果。P95 是批次均值分布的 nearest-rank，
15 个样本时等于最大值，不是单次操作的尾延迟。

key 是 (Int32 id, Int32 distribution) 名义记录，value 是 Int32。
uniform 使用标准整数 Hash，lowbits 使用 id << 12，constant 返回零；后两种在本次规模内
初始桶都为零，但 lowbits 的完整缓存 hash 不同。下表是 **N=128 两轮中位数的范围，单位 μs/整次调用**，
范围只表示两次运行差异，不是置信区间。所有调用均包含建表、增长和释放；
lookup/iterate 另外访问 8N 项，churn 另外做 4N 组删除/重插，insert 只建表。
AOT 是原生导出及错误码检查，JIT/解释器是受检 VmEngine 入口，不能将短调用比值当作纯循环比值。

| 负载 | Hash 分布 | AOT | JIT | Interpreter |
| --- | --- | ---: | ---: | ---: |
| 插入/构造 | uniform | 162.1–163.8 | 169.3–173.5 | 6,716.1–6,757.5 |
| 插入/构造 | lowbits | 714.0–753.1 | 791.7–842.7 | 61,493.7–66,121.5 |
| 插入/构造 | constant | 715.4–736.6 | 784.3–808.1 | 62,546.2–65,323.6 |
| 命中查找 | uniform | 269.5–269.8 | 286.6–287.0 | 17,336.5–17,497.9 |
| 命中查找 | lowbits | 3,701.7–3,736.8 | 4,170.8–4,190.6 | 362,774.0–368,575.9 |
| 命中查找 | constant | 3,718.2–3,745.2 | 4,200.1–4,257.4 | 374,136.7–377,350.1 |
| 删除重插 | uniform | 593.1–595.6 | 640.3–645.6 | 36,812.5–36,959.6 |
| 删除重插 | lowbits | 5,912.7–5,916.9 | 6,605.8–6,612.8 | 567,287.8–568,901.3 |
| 删除重插 | constant | 5,955.6–6,048.7 | 6,656.2–6,674.2 | 585,855.4–595,757.2 |
| Slice 遍历 | uniform | 174.1–179.1 | 182.6–187.4 | 8,199.6–8,344.1 |
| Slice 遍历 | lowbits | 713.9–731.4 | 799.1–808.1 | 62,050.0–63,326.2 |
| Slice 遍历 | constant | 732.4–735.8 | 804.6–827.9 | 64,769.4–65,151.2 |

完全碰撞的命中查找整次调用约为普通 Hash 的 14 倍（AOT）、22 倍（解释器）；
这组结果体现当前线性探测在碰撞下的成本，不支持“所有 key 都是 O(1)”或抗碰撞能力的结论。
碰撞组的 iterate 包含高成本建表，不能把其耗时变化归因于 Slice 遍历本身。
本轮是小规模 Copy value 的同步基线，不覆盖拥有 value 的析构成本、缺失查询、CPS、编译或冷启动，
也没有与旧版本交替对照，因此不作提速或回归判断。

### 内存测量

独立优化测试二进制扫描 26 组 JIT/解释器工作负载，通过二分得到最小成功 arena 预算，
并复核少一字节失败、预期结果正确、每次调用后 LIVE=0。下表为**整次调用峰值收费字节数**：

| 负载 | N | JIT | Interpreter |
| --- | ---: | ---: | ---: |
| insert，三种分布相同 | 0 | 640 | 640 |
| insert，三种分布相同 | 32 | 3,752 | 3,752 |
| insert，三种分布相同 | 128 | 11,816 | 11,816 |
| insert，uniform | 1024 | 87,080 | 87,080 |
| lookup，uniform | 128 | 11,816 | 11,816 |
| churn，uniform | 128 | 11,816 | 11,816 |
| iterate，uniform | 128 | 11,816 | 11,816 |

这个峰值含 arena 分配记账及增长时同时存活的新旧缓冲区；不是最终 Map 常驻大小，
不包含宿主清理账本、编译器、解释器/CPS 帧、allocator 碎片或进程 RSS，也没有测量 AOT 内存。
后续操作的峰值没有超过建表峰值，并不表示查询、切片或清理路径保证零临时分配。
内存测试运行器 SHA-256 为 `5aaaccad1545f42de3b592c2aa4cc4a3331428d4849ba9058eb26e6ab5685fa3`。

### 复现与本地产物

```sh
./scripts/check.sh --full
cargo bench --bench map -- --iterations 32,128 --samples 15 --sample-ms 30 --output target/benchmarks/map-m7-20261002-run1
cargo bench --bench map -- --iterations 32,128 --samples 15 --sample-ms 30 --output target/benchmarks/map-m7-20261002-run2
JOKY_MAP_MEMORY_OUTPUT=target/benchmarks/map-m7-20261002-memory \
  cargo test --release --lib map_memory_profile -- --ignored --nocapture
```

本轮计时直接调用已由检查脚本构建的 `target/release/deps/map-d8012776a9060d3e --bench`，
其余参数与上述命令相同。运行器 SHA-256 为
`c8f1d91d092a89eb60110cee56710ab9d002df3ff9aabb76d681b2de8592f81a`，
静态运行时 SHA-256 为 `66426f865f6540e4840719d2e28a4bf9e2a89053bd5448c513ff3e379d5cd656`。
两轮的 metadata.txt、summary.csv、samples.csv、report.md、源码及 Cargo.lock 快照保存于上述目录；
内存目录保存 memory.csv、源码和独立测试运行器身份。原始产物由 Git 忽略，新克隆需按命令重建。
具体工作负载、收费内存范围及调用边界见 [基准方法](../benches/README.md#map-专项基准)。


## 2026-10-11：JSON CLI 冷启动编译

用户报告 `joky run examples/json_derive.jk` 启动卡顿。本节测量新进程启动至退出的墙钟时间，
包含解析、类型/借用检查、代码生成和程序执行；操作系统文件缓存未清空。该口径与前述预热后的
解释器吞吐不同。[运行器](../benches/json-startup.py)不构建程序、不设耗时断言，每次核对
`Ada`、`37`、`/age` 的完整输出，方法见[基准说明](../benches/README.md#json-cli-冷启动)。

环境为 Apple M1 Pro / arm64 / macOS 26.6.2（25G83）、Rust 1.99.0。
基线为 `42f82b7`，优化版为本节随附的编译器性能改动；两版本分别保留独立二进制。
Debug 路径中的 CLI 均由 `cargo test` 构建，使用仓库 test profile（opt-level=1，保留 Debug
断言与溢出检查）；它不是普通 `cargo build` 的未优化 Dev profile。Release 均为 opt-level=3、
Thin LTO、单 codegen unit，Joky 生成代码的优化设置未改。
全部构建、正确性检查和采样结束后再串行计时，交替旧/新顺序；下表为首轮各 5 次的中位数。

| 构建 | 用例 | 优化前 | 优化后 | 耗时下降 |
| --- | --- | ---: | ---: | ---: |
| Debug/test | 派生 JIT | 2.591 s | 1.195 s | 53.9% |
| Debug/test | 手写 JIT | 2.546 s | 1.199 s | 52.9% |
| Debug/test | 派生解释器 | 2.252 s | 0.847 s | 62.4% |
| Release | 派生 JIT | 2.143 s | 0.931 s | 56.6% |
| Release | 手写 JIT | 2.066 s | 0.926 s | 55.2% |
| Release | 派生解释器 | 1.830 s | 0.655 s | 64.2% |

第二轮独立复测各 3 次：派生 JIT 的 Debug/test 为 2.591 → 1.198 s，Release 为
2.103 → 0.933 s；其余用例下降幅度同样保持在 52.9%–64.4%，方向与首轮一致。

Release 的独立阶段探针各交替运行 3 次，中位数为：源码解析/规范化 94.35 → 61.94 ms，
VM 加载（含检查与代码生成）2.044 → 0.868 s，实际 main 执行 2.559 → 2.384 ms。
探针使用与 CLI 相同的 resolve_file/load_module/call_unit_0 入口链，不含 CLI 参数解析与退出。
这些执行时间仅描述小示例，不用于断言 JSON 吞吐提升；当前仍有约 0.9–1.2 s 的 JIT 进程启动开销。

采样定位到共享编译流程的重复工作：trait 环境切换深拷贝声明和方法 AST；布局递归反复扫描
完整类型图；借用分析逐指令复制引用状态，并在合并时复制整个旧状态用于比较。
本次采用不可变声明共享、单次布局查询入口检查、引用槽表写时复制及增量变化检测。
原有作用域、dyn 禁止条件、借用权限交集、循环固定点和编译预算保留；没有跳过未使用模板检查。
手写与派生示例均受益，实际映射规则和运行时 ABI 未改。

本地原始数据在 `target/benchmarks/json-startup-before/` 和 `target/benchmarks/json-startup-after/`，
包含旧/新二进制、SHA-256、机器与工具链信息、CSV、阶段探针源码/输出和采样。
`debug-run1` / `release-run1` 保存首轮结果，后续复测另建目录；产物不提交 Git。

验证：关联类型、dyn、布局和循环/分支借用专项通过，JSON 派生专项在 JIT、解释器、AOT 执行，
Debug 逐指令取消穷举通过；默认检查通过格式、Clippy、1158 项库测试、157 项 CLI 测试、
19 项模块常量、23 项源码模块测试和三后端短输入 while/Map 正确性。
Release 用于本节计时，没有运行发布验收或其他吞吐基准。
