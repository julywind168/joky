# Chat 生成代码优化实验（2026-10-31）

在 `1687616` 上临时将 JIT 和 AOT 的 Cranelift `opt_level` 从默认 `none` 改为 `speed`。
两版都使用相同的 Release Rust 编译器／运行时构建方式；Rust Release 与 Joky 生成代码的优化级别是两件事。
本轮没有改业务、运行时算法、浮点语义或其他 Cranelift 选项，实际依赖版本为 0.136.1。

**结论：chat 有局部提速迹象，但发现确定的浮点正确性问题，不能直接默认开启。**
实验设置和取证改动均已还原，保留默认 `none`；首次实验只记录结果，后续依赖升级见文末。

## 性能

三轮交替测量，共 30 个样本。吞吐单位为广播/秒，CPU 为逐样本计算后取中位数的服务端 ms/广播。

| 房间 / worker | none 吞吐 | speed 吞吐 | 变化 | none CPU | speed CPU |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 / 1 | 726.0 | 751.6 | +3.5% | 2.0450 | 1.9675 |
| 1 / 4 | 1223.6 | 1251.9 | +2.3% | 2.6025 | 2.5325 |
| 4 / 1 | 1789.2 | 1872.4 | +4.6% | 0.9138 | 0.9063 |
| 4 / 4 | 2647.4 | 2934.2 | +10.8% | 1.2258 | 1.1467 |

1 房间／1 worker 三轮配对均提高，吞吐范围不重叠。其余配置范围重叠；4 房间／4 worker
各轮变化为 +10.8%、−5.4%、+22.1%，不能把中位数的 +10.8% 当成稳定收益。
单客户端吞吐中位数 1580.6→1567.8（−0.8%），P50 0.5658→0.5820 ms，CPU/请求 0.524→0.536 ms；
三轮配对变化为 +0.9%、+1.3%、−13.9%，同样有明显波动，未证实延迟改善。
矩阵客户端使用 0.26～0.56 核，未见一核客户端饱和。采样 RSS 中位数约 74.8～78.3 MiB，无内存改善结论。

负载测试结束后，另做三轮交替的完整 `joky build` 和新进程启动测量：

| 指标 | none 中位数 | speed 中位数 |
| --- | ---: | ---: |
| AOT 项目构建（含前端、代码生成、链接） | 1.063 s | 1.027 s |
| 启动至两个监听端口就绪（含加载／JIT 编译脚本） | 0.833 s | 0.798 s |
| chat 可执行文件大小 | 8,475,304 B | 8,458,792 B |

构建／启动样本范围有重叠；不是单独的优化器计时，也没有冷缓存保证。未观察到明显启动成本增加，
不能据这三个样本推断所有程序的编译速度。

## 阻碍默认启用的问题

`cargo test --lib float_` 在 `speed` 下为 19 通过、1 失败：
`aot_float_environment_isolated_at_native_and_cps_boundaries`，单独重跑也失败。
失败点是 C harness 第 68 行的 `add32`：宿主设置非默认舍入／flush-to-zero 环境后，
Joky 的加法本应在自己的标准环境中完成，再恢复宿主状态。

保留目标文件并反汇编确认，`add32` 的实际顺序变成：

```text
call __joky_float_env_enter
call dirty
call __joky_float_env_reset
call joky_runtime_error_code
... success path ...
call __joky_float_env_restore
fadd s0, s0, s1
return
```

`fadd` 已被移到恢复宿主环境之后；`add64` 和 `half32` 的反汇编中也有同类顺序。
当前插入普通函数调用的方式没有为浮点运算建立优化器可见的顺序依赖。
Cranelift 的 egraph 优化在 `none` 下不运行；其纯运算分类不能表达 Joky 这里的浮点环境契约。
这是当前后端与优化器的集成问题，不能通过删除测试或放宽舍入断言解决。
JIT 环境用例本轮通过不意味着不存在同类风险，两路共用该插桩逻辑。

后续应先让浮点运算与 enter／reset／restore 的先后关系在优化后仍成立，
覆盖直接返回、宿主回调、间接调用、挂起恢复与清理，再考虑默认启用及重新测量。
本轮不扩大到该语义改造，也不使用有已知错误的设置替换默认值。

## 方法与复现

- Apple M1 Pro、16 GiB、macOS 26.6.2 ARM64、Rust 1.99、Python 3.14.6。
- none 产物复用 `target/benchmarks/chat-header-registration/final`，源码／工具哈希与基线核对一致；
  两版 benchmark 源码快照逐文件哈希完全一致。相同 AOT host + JIT script，没有单独拆分两路优化收益。
- 16 客户端、64 字节正文、1／4 房间 × 1／4 worker；每客户端预热 25 条，每批 250 条，至少 3 秒。
  单客户端为 1 房间、1 worker、每批 5000 条。延迟为发送至全部房间成员收到的时间。
- 三轮版本顺序 none→speed、speed→none、none→speed，第二轮房间／worker 顺序倒置；每轮矩阵后测单客户端。
  测量期间没有构建、测试或在线 profiler。固定闭环负载，不代表最大容量；没有重测 Tokio。
- 生成优化版：JIT 使用 `JITBuilder::with_flags(&[("opt_level", "speed")], default_libcall_names())`；
  AOT 在构造 ISA 前对 `settings::builder()` 设置 `opt_level=speed`。重新构建工具及静态运行时后准备独立快照：

```sh
cargo build --release --bin joky --lib
python3 benches/chat-workers.py prepare \
  --joky target/release/joky --runtime target/release/libjoky.a --profile release \
  --output target/benchmarks/chat-codegen-opt/speed
python3 benches/chat-workers.py run \
  --build target/benchmarks/chat-codegen-opt/speed \
  --output target/benchmarks/chat-codegen-opt/matrix-1-speed \
  --workers 1,4 --rooms 1,4 --clients 16 --payload-bytes 64 \
  --messages 250 --warmup 25 --samples 1 --min-seconds 3
```

输出目录须尚不存在。对照版及单客户端按上述参数／顺序轮换。
原始数据、脚本、补丁、工具副本、浮点目标文件／反汇编和日志只保留于
`target/benchmarks/chat-codegen-opt/`，不提交 Git。实验后的 `target/release/joky` 和 `libjoky.a` 已还原为 none 版。

## 验证

`speed` 下 `cargo test --test chat -- --ignored` 的串行／并行两项通过，覆盖三后端真实 TCP、协议及清理路径；
`task_parallel_array_values_and_roots_on_all_backends` 通过；30 个性能样本均核对广播、顺序、余额和正常退出。
浮点测试为 19 通过、1 失败，单独复现相同失败；还原 `none` 后，该用例通过。
首次实验未运行全量检查或压力穷举，未验证其他架构。代码无改动，只提交文档并检查 diff。

## Cranelift 0.137.1 升级复验

随后将六个直接 Cranelift 依赖统一升级到 `0.137.1`，更新锁文件；现有代码无需 API 适配。
版本信息见 [Cranelift 0.137.1](https://docs.rs/crate/cranelift/0.137.1)。
在相同 ARM64 环境、默认 test profile 下，先验证 `none` 的 AOT 浮点环境用例通过，
再临时对 JIT／AOT 开启 `speed`：`cargo test --lib float_` 仍为 19 通过、1 失败，
失败用例与 C harness 第 68 行均相同。升级没有解除默认开启优化的阻碍。

最终保留依赖升级、还原 `none`，不保留实验开关。新版的浮点纯运算分类仍没有表达
enter／reset／restore 所要求的运算顺序；修复仍需要调整 Joky 的边界实现，并验证优化后的行为。
只在返回前强制求值不足以验证入口、宿主回调、间接调用与 CPS 路径的完整契约。
本轮不重新测量性能，上文吞吐数据仍属于 0.136.1，不能当作 0.137.1 的结果。
实验补丁和测试日志留在 `target/benchmarks/cranelift-0.137.1/`。

最终 `none` 设置下，`cargo test --lib backends::` 通过 165 项、默认忽略 4 项，
`cargo test --lib float_` 通过 20 项（与后端测试有重叠），
`cargo test --test chat -- --ignored` 通过串行／并行两项真实 TCP 测试。
`cargo fmt --check`、`cargo clippy --all-targets -- -D warnings` 和 diff 检查通过。
未运行全量检查、压力穷举或 Release 构建。
