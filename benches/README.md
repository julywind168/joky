# 性能基准

运行同一份 [while.jk](programs/while.jk)，比较当前 AOT、JIT 和 Interpreter 的同步循环吞吐。
基准入口为 [while.rs](while.rs)，使用标准库计时和已有的 `libloading`，不引入新依赖。

## 运行

`./scripts/check.sh` 默认 quick，`--full` 为兼容别名，两者均只跑 Debug 短输入基准正确性检查。
`./scripts/check.sh --stress` 独立运行 Debug 穷举和默认长输入基准正确性。
只有显式添加 `--release` 才切换为 Release；明确要求发布验收时运行 `--full --release` 和
`--stress --release`。下面的计时命令是独立操作，不属于检查脚本，也没有 CI 耗时阈值。
**正确性通过不代表性能未回退；短计时冒烟只检查运行器和产物，不能用来宣称提速。**

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
- 保留当前后端默认代码生成配置，不为基准修改优化级别；以本轮 `metadata.txt` 中的
  `cranelift_default_opt_level` 和 `Cargo.lock` 快照为准。该字段读取 Joky 的 `DEFAULT_CODEGEN_OPT_LEVEL`
  （当前为 `speed`），不是 Cranelift 库自身的默认值。Rust bench 优化构建与 Joky 生成代码的优化级别是不同设置。

## 产物与解释

默认输出到 `target/benchmarks/while/`，同目录重跑会覆盖旧结果。需要比较多轮时使用不同的 `--output`。

- `report.md`：环境、方法和结果。
- `summary.csv`：每个内核、规模和后端的统计汇总。
- `samples.csv`：全部原始样本、调用次数、纳秒耗时和预期结果。
- `metadata.txt`：时间、Git revision/dirty、CPU、OS、Rust、cc、运行参数及默认优化级别。
- `while.jk`、`runner.rs`、`Cargo.lock`：测量源码、运行器和依赖版本快照。

测量时避免并行编译、后台重负载、低电量模式或温度变化；本运行器不固定 CPU 亲和性。
同一机器独立重跑几次后再判断回归，不直接将不同机器、调用入口或优化设置的数字相除。
比较版本时先构建并保存各自二进制，等所有构建和检查结束后再交替运行；
记录 revision/dirty、机器和工具链、输入规模、样本数、目标批次时间及各轮独立输出目录。
`metadata.txt` 中的 Git revision/dirty 和工具版本取自运行时环境；复用已保存的二进制时，
另行记录构建提交、构建工具链及二进制 SHA-256，不能把运行时工作树当作其构建身份。
耗时下降使用 `(旧耗时 - 新耗时) / 旧耗时`，加速倍数使用 `旧耗时 / 新耗时`；
`relative_to_aot` 是同一轮相同内核和规模的 `后端中位耗时 / AOT 中位耗时`，大于 1 表示更慢。
比较时从原始 `elapsed_ns / calls` 重算，展示时才舍入；不要叠加多个阶段百分比，也不要把历史数字视作当前结果。
当前基准不覆盖编译耗时、冷启动、CPS/挂起、宿主调用、内存分配或标准库性能。
后续优化应先复用此基线再追加专项负载，不能仅凭一个求和循环推断整体性能。

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

## 历史数据

三后端初始基线、Lua 对照和解释器各阶段收益集中在
[性能实验记录](../docs/interpreter-performance.md)，包含提交、环境、样本规模、
结果、复现命令与本地产物位置。当前实现见 [解释器设计](../docs/interpreter.md)。
历史耗时不自动代表当前版本；回归结论需要重新测量。

## 产物管理

Git 保存基准/诊断源码、回归测试和精简的结论与复现方法。
新生成的原始样本、完整机器元数据、字节码/反汇编转储、临时补丁和二进制写入
`target/`，不提交。每轮使用独立输出目录；有分享需要时单独归档。
`benches/reports/` 后续只增加精简结论、必要的汇总表和复现命令；逐样本 CSV、
完整 JSON 元数据、逐线程统计与压缩调用树也属于本地产物，不随优化提交。
已跟踪的初始基线作为历史记录保留，不因新增忽略规则自动移除。

## Map 专项基准

[map.jk](programs/map.jk) 与 [map.rs](map.rs) 测量 M0–M7 的源码 Map。
默认 N=0/32/128，15 个样本，目标批次 30 ms；每个样本轮换 AOT/JIT/Interpreter，
执行前验证容量边界和返回值，计时后核对校验和。运行方法及统计口径沿用上文；
此处所有调用都包含 Map 构造、增长和释放，不能把整次调用除以 N 当作孤立操作延迟。

```sh
cargo test --bench map
cargo test --release --bench map
cargo bench --bench map -- --iterations 32,128 --samples 15 --sample-ms 30 --output target/benchmarks/map-run1
```

检查脚本只执行前两类正确性检查，不计时、不设置性能阈值。参数 `--iterations` 上限为 4096；
完全碰撞的大 N 可能很慢，超过目标批次时间时每样本只调用一次。

| 内核 | 一次调用的操作 |
| --- | --- |
| insert | 从空表插入 N 项并释放，含自动增长 |
| lookup | 先插入 N 项，再做 8N 次命中查询并求和 |
| churn | 先插入 N 项，再做 4N 组删除/重插，每次 value 加一 |
| iterate | 先插入 N 项，借用一次 values 切片，while 遍历 8N 项 |

key 为两个 Int32 字段（id、distribution）的 Copy 记录，value 为 Int32。
每个内核有三个后缀：uniform 使用标准整数 Hash（不表示随机输入），lowbits 使用 id << 12，
constant 返回零；Eq 比较两个字段。本轮 N≤128 时 lowbits 的初始桶均为零，
但完整缓存 hash 仍不同，constant 则同时增加相等比较次数。N=0 包含空容器生命周期开销。
遍历结果仍含构造成本，不能把碰撞组的整次 iterate 耗时归因于 Slice 遍历。

AOT 动态库链接 Cargo 同 profile 的静态运行时，每次调用检查其原生错误码；
该库及函数指针在整个测量期间保活。JIT/解释器继续走受检 VmEngine 入口。
输出包括原始样本、min/median/P95/max、机器信息、代码/依赖快照、运行器与运行时 SHA-256；
`ns_per_iteration` 表示整次调用 median/N，不扣除构造或基线。数据属于本机当前构建，未作版本收益比较。

### Map 内存口径

用测试专用 arena LIMIT 二分查找每个工作负载最小成功预算，并复核预算少一字节即失败：

```sh
JOKY_MAP_MEMORY_OUTPUT=target/benchmarks/map-memory-run1 \
  cargo test --release --lib map_memory_profile -- --ignored --nocapture
```

这是显式忽略的测量入口，日常检查不运行、不写报告；测试检查每次运行后的 LIVE 归零。
测量 JIT/解释器的**整个调用峰值 arena 收费字节数**，含初始化前缀、视图头和分配记账，
不包含宿主清理账本、容器、编译器、CPS/解释器帧、allocator 碎片或进程 RSS，也不是最终驻留 Map 的大小。
没有直接测量 AOT 内存，不能将 JIT 数字写成三后端实测。内存输出包含 CSV、代码、运行器及构建身份。
先完成所有构建和正确性检查，再串行测量；历史结论归 [性能实验记录](../docs/interpreter-performance.md)。

## 持久实例转移探针

P3 的 Map/StringBuilder 转移成本可单独运行：

```sh
cargo test --release persistent_instances_containers_transfer_measurement -- --ignored --nocapture --test-threads=1
```

探针先完成解析、编译和构造，再对 0/8/32/128/512 项 Map 的稳定状态各调用 200 次
无状态修改的 container_count。状态同时包含 String key/value、带 String/Drop 的记录 value、
嵌套 Map/StringBuilder 和 UTF-8 builder。输出平均每次调用纳秒、分配数和逻辑计费；
调用前后检查存储稳定，关闭归零，没有耗时阈值。
这包含 VM 校验、分派以及拥有图扫描/登记转移，并非纯转移内核；每组只有一个批次，
用于记录成本随规模增长的趋势，不用于宣称后端提速。测量时不要并行运行构建或压力检查。
当次环境与数据见[专项计划 P3 记录](../docs/persistent-instances-numerics.md#p3-落地与验收记录)。


## JSON CLI 冷启动

[json-startup.py](json-startup.py) 比较两个已构建二进制的 `joky run` 进程耗时，覆盖
自动派生、手写 struct codec 和派生的解释器入口。先分别构建并保存旧、新二进制，
完成所有构建与检查后串行测量；不要把不同 Cargo profile 的差异作为代码优化收益。

```sh
python3 benches/json-startup.py \
  --before target/startup-before/joky --after target/startup-after/joky \
  --before-revision OLD_COMMIT --after-revision NEW_COMMIT \
  --profile release --samples 5 --output target/benchmarks/json-startup/run1
```

每个样本交替旧/新顺序，进程墙钟时间包括启动、编译、执行和退出；不清空操作系统文件缓存，
因此这是新进程启动测量，不等于磁盘冷缓存测量。stdout/stderr 通过管道捕获，每次核对完整输出。
此入口不测预热后的 JSON 吞吐，不设耗时阈值，也不加入默认正确性检查。
输出目录必须不存在，保存原始 CSV、中位数/min/max、二进制和示例 SHA-256、平台与调用参数、
调用方提供的构建版本及运行器源码。独立重跑后再判断趋势，历史结果见
[性能实验记录](../docs/interpreter-performance.md#2026-10-11json-cli-冷启动编译)。

## 状态域候选存储

[R5 候选存储预算对照](reports/service-concurrency-r5-memory.md)记录全根快照与仅写域快照的
内存预算差异和复现方法。该对照不计时，不代表吞吐测量。

## Chat worker 扩展基线

[chat-workers.py](chat-workers.py) 使用 Python 3.10+ 标准库和系统 `ps`，在 macOS/Linux
测量 **AOT host + JIT 业务脚本**。先明确构建 Release 工具和运行时，再只构建一次应用：

```sh
cargo build --release --bin joky --lib
python3 benches/chat-workers.py prepare \
  --joky target/release/joky --runtime target/release/libjoky.a --profile release \
  --output target/benchmarks/chat-workers/build
python3 benches/chat-workers.py run \
  --build target/benchmarks/chat-workers/build \
  --clients 16 --rooms 1,4 --workers 1,2,4 \
  --messages 100 --warmup 10 --samples 5 \
  --output target/benchmarks/chat-workers/run1
```

输出目录必须不存在；独立重跑时复用同一 `--build`，更换 `--output`。
prepare 复制 `examples/chat` 到输出目录，只将两个监听端口改为 0，并将初始余额从 500 分提高到
10¹² 分以支持持续发送。保留正常扣费、事务、队列、解析和广播逻辑，不修改仓库示例。
生成源码、打包资源、工具／运行时／应用 SHA-256、构建提交及 dirty 状态、Cargo.lock 和运行器快照均保存。
`--profile` 是调用方对工具与运行时构建的声明，不是从二进制反推出的编译选项；
Rust Release 不代表 Joky 生成代码另开了优化选项，后者仍使用当前默认设置。

每个样本启动一个新进程，在进程启动、脚本编译、连接、加入房间及预热完成后开始计时。
所有 worker 配置使用完全相同的 AOT 文件与业务脚本；每轮旋转 worker 顺序、交替房间场景顺序。
启动 1 worker 仍是 Parallel 模式，额外控制／I/O／文件服务线程并不消失。

测量口径：

- 默认 16 个连接、1 或 4 个房间、64 字节消息正文，每连接先预热 10 条，再测量 100 条。
  房间均分连接；广播包含发送者。每个连接最多一条未完成命令，等待本房间全部接收者收到后再发下一条。
  这是有界闭环负载，不是固定到达率或最大容量测量；不代表过载时的排队延迟，不能消除 coordinated omission。
- 每条广播核对 room、sender、顺序号和完整正文，拒绝丢失、重复、乱序、错误及提前断线。
  测量后逐连接发送 balance，验证实际扣款并建立输出顺序屏障，最后通过 admin 正常关闭，要求退出成功且无诊断。
- `broadcasts_s` 是完成的 say 命令数／墙钟时间；`deliveries_s` 是实际接收条数／墙钟时间。
  延迟从客户端写入命令前到该消息所有接收者读到完整行，包含回环网络和客户端调度。
  P50/P95/P99 是每个样本内单消息延迟的 nearest-rank 分位数；summary 是这些样本指标的中位数，不是合并分布的分位数。
- 单房间 16 人与四房间各 4 人的每条广播工作量不同。只在相同房间布局内比较 worker 扩展，
  不能把两种场景的命令吞吐之比当作去除锁竞争的收益。
- server CPU 来自测量前后 `ps time` 的进程累计 CPU 差值／墙钟时间，包含该进程所有线程；
  1.0 表示平均使用一个逻辑核，短样本受 `ps` 时间精度影响。CPU 快照略宽于墙钟计时区间。
  client CPU 来自 Python `process_time`，用于发现负载发生器饱和；不含独立 `ps` 进程的 CPU。
- RSS 在测量开始／结束及期间约每 250 ms 采样，报告采样最大值；不是精确峰值、arena 预算或泄漏证明。
  后台采样也有开销。客户端和服务端共用本机，未固定亲和性，M 系列异构核及系统噪声会影响结果。
- 上限 32 个客户端、16..1024 字节正文，用有界在途量控制示例默认队列压力；这不是慢消费者、满载连接数或长稳验收。

每轮保存 `samples.csv`、逐消息原始 `latencies.csv`、`summary.csv`、`report.md`、环境和构建身份，
以及每个样本的 stdout/stderr。异常时立即失败并清理本轮服务进程，已有样本保留；不把失败样本排除后继续报告成功。
运行期间不要并行编译或执行压力测试。不设置耗时断言，也不接入默认 `check.sh`。
本机首轮结论与复现参数见 [2026-10-30 记录](reports/chat-workers-2026-10-30.md)。
后续共享分配索引与锁外释放的三轮新旧对照、单客户端延迟和 CPU 成本见
[共享分配索引优化](reports/chat-storage-index-2026-10-30.md)。
在此基础上省去受审计 helper 返回后的浮点环境重置，结果见
[浮点环境调用边界优化](reports/chat-float-env-2026-10-30.md)。
合并 outbox 领取／关闭查询／字节提取为一次控制接收，结果见
[outbox 单次接收优化](reports/chat-outbox-receive-2026-10-30.md)。
随后对控制桥整段字节复制和出站 Vec 移交分别测量，结果见
[控制桥字节传输优化](reports/chat-byte-transfer-2026-10-30.md)。
共享任务预算随后保留收费准入锁、将退款改为原子减法，结果见
[预算退款优化](reports/chat-budget-2026-10-31.md)。
域 Arena 外层共享记账锁的后续优化见
[Arena 共享预算优化](reports/chat-arena-budget-2026-10-31.md)。
Buffer 头部分配与 finalizer 登记合并见
[Buffer 头部登记优化](reports/chat-header-registration-2026-10-31.md)。
JIT／AOT `opt_level=speed` 的实验结果、浮点正确性阻碍及 0.137.1 性能对照见
[生成代码优化实验](reports/chat-codegen-opt-2026-10-31.md)。
浮点修复后的新一轮对照、每条消息的控制请求／分配／复制计数，以及三档优化的编译时间见
[speed 与消息成本](reports/chat-speed-costs-2026-10-31.md)。
随后省去受审计纯整数调用的整组浮点保护，结果见
[纯整数浮点保护优化](reports/chat-integer-fp-2026-10-31.md)。
随后减少 Script／域请求契约的重复分配，结果见
[请求契约缓存](reports/chat-request-contracts-2026-10-31.md)。
仅判断域资格时省去 schema 字符串生成的后续实验未显示稳定收益，暂未采用，见
[域资格校验实验](reports/chat-schema-check-2026-10-31.md)。
随后对 CPU、线程等待与索引增长做采样，下一步优先处理临时切片头的累计保留，见
[CPU 与等待路径采样](reports/chat-cpu-profile-2026-10-31.md)。
全范围切片复用后的预算回归、长窗口性能和存储保留量见
[全范围切片复用](reports/chat-full-slice-2026-10-31.md)。
随后让已知内部直接调用共享浮点保护，边界回归与性能对照见
[内部直接调用共享浮点保护](reports/chat-fp-boundaries-2026-10-31.md)。
随后分离任务完成通知并按 worker 定向唤醒，见
[任务完成通知与定向唤醒](reports/chat-targeted-wakes-2026-10-31.md)。
随后根据新采样减少预算准入与类型遍历的小分配，见
[预算准入与类型遍历的小分配](reports/chat-small-allocations-2026-10-31.md)。


2026-10-31 的字节批处理与保留输入的 write_all，相对 `9ff6852` 的同机对照：
M1 Pro / macOS、Release、speed，16 客户端／4 房间／4 worker，各档交替运行三轮、每样本至少 10 秒。
64 B 广播吞吐中位数 3379 → 4157/s（+23%），1024 B 为 1369 → 3920/s（2.86 倍）；
服务器 CPU 分别为 0.897 → 0.632、2.456 → 0.661 ms/广播。
同场静态 Rust/Tokio 参照为 25220、22730/s，其客户端约 0.95–0.96 核，Joky 约 0.42–0.45 核；
Tokio 已接近负载发生器单核上限，不能据此推断服务器容量上限。全部配对变化为正，保留第三轮整体变慢的样本。
本轮也包含 reserve 扩容分支分离和 chat 按命令验证房间名；数据不能拆解为各改动的独立贡献。
构建身份、原始样本与细项只留在忽略的 `target/benchmarks/chat-byte-paths/`，不新增 Git 细项报告。

### Rust/Tokio 应用对照

三轮交替测量及单客户端 RTT 见 [Joky / Tokio 对照报告](reports/chat-tokio-2026-10-30.md)。

[独立对照工程](chat-tokio/README.md) 实现同样的聊天协议、扣费、成员变化与有界发送队列，
用 `prepare-tokio` 接入同一个计时器。其业务静态编译，不包含 Joky 的内嵌 VM、热更新、
通用域事务和共享分配预算；比较的是两种应用实现的总成本。

`run --min-seconds 5` 可按完整消息批次延长每个样本，默认 0 仍只执行一批。
两版使用相同 clients、rooms、workers、payload、warmup 和最短时长；CSV 的 broadcasts 为实际完成数，
延长后的最终余额按实际发送数验证。至少运行数秒有助于降低 `ps` CPU 时间精度的影响，
但不能消除 Python 客户端饱和。对照须报告 client CPU，不能将客户端受限的吞吐当作服务端容量上限。
