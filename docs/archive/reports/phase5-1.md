# 阶段 5.1 长期资源回收验收

> 历史记录：本文保留原阶段的设计、环境与验收结论，不代表当前能力。归档于 2026-09-24；当前说明见[文档入口](../../README.md)，未完成工作见[计划索引](../../plans/README.md)。

日期：2026-09-10。基于 `132723d` 加本次 5.1 修改；以下均为本地验收，
没有远端 CI。环境与 [5.0](phase5-0.md) 相同：M1 Pro / 8 cores / 16 GiB，
macOS 26.6.2，普通 rustc 1.97.1；TSan 使用 1.100.0-nightly
（4aa1fbcf4，2026-09-08），标准库一并插桩，无抑制。

## 修复范围与回归依据

1. 删除 continuation 永久 tombstone 和地址碰撞占位分配。改成进程内
   永不复用的不透明 token，活动 HashMap 允许复用容量。局部表测试用
   256 × 32 个 token 验证历史增长与容量分离，并注入名字空间临界值，
   确认耗尽不会回绕。协议见 [资源生命周期](../../runtime/resource-lifetimes.md)。
2. 删除 reactor 永久取消集合，保留活动请求上的取消标记；用 BTreeSet
   立即移除被取消的远期 deadline。覆盖 register 发布前取消、重复取消、
   已完成取消和错误请求类型取消。
3. 长跑暴露的 native 预留句柄和 runtime Pending 终态句柄未释放：修复
   未用预留项、失败返回与交接后终态的所有权。缺失 provider 的原生失败
   回归测试原先留下 2 个 canceled handle，修复后 scope 资源归零。
4. Rust 存活字节稳定时，RSS 原先仍约每 4 个 scope 增加 64 KiB；定位到
   Cranelift JITModule 普通 Drop 有意保留可执行映射。backend 现在排空 scope、
   清理未领取 failure payload 和撤销入口后显式释放映射。重复使用 Compiler
   也不积累历次 module/字面量。另测 backend 释放时 failure drop 执行一次。
5. 审计 task group、handler frame、provider、socket 的退出路径，增加
   scope/进程计数和队列/注册表快照。资源判据不再局限于 managed object。

取消与完成竞争、父子原生交接、机器入口恢复、frame 排空沿用并复跑现有
并发回归。新增跨 scope 同 generation 的迟到回调测试；运行中 blocking
syscall 测试在公开 token 退役后仍观察到 1 个 continuation，建立新 token，
再放行旧请求，确认新对象不受影响且最终保留计数归零。文件池饱和测试
直接核对等待准入项、共享路径字节及其取消释放。

## 复现命令

```bash
cargo fmt --check
cargo check --locked
python3 scripts/check-runtime.py
cargo test --locked --test cli
python3 scripts/check-runtime.py --tsan

for workers in 1 2 4; do
  JOKY_WORKER_COUNT=$workers cargo test --locked --lib \
    compiler::tests::resources::bounded_runtime_metadata_across_batches_and_scopes \
    -- --ignored --exact --nocapture --test-threads=1
done
```

TSan 前提、插桩参数和独立压力阈值沿用 5.0。本次把资源长跑加入
`scripts/check-runtime.py` 的隔离清单；普通与 TSan 都会执行，且每个测试
必须恰好运行 1 项。日志位于 `target/runtime-checks/{normal,tsan}/`。
进程全局计数的长跑必须独立执行，不能与同一进程的其他测试并发。

## 固定负载与测量方法

`bounded_runtime_metadata_across_batches_and_scopes` 预先固定 4 个热身批次
加 20 个测量批次。每批在同一持久 scope 内执行 32 波 × 32 个 public token
和一小时 timer 的创建/取消，排空每波；随后独立创建、关闭 4 个 JIT scope。
每个程序内部并发执行文件读取成功、文件不存在、跨 timer 的 normal handler，
以及快速 timer 赢过 60 秒 timer 的 race。每批另用同一 Compiler 跑一次程序。
合计 24,576 次直接句柄/定时器操作，120 次程序运行（另有程序内部激活）。

每个 JIT scope 检查关闭后资源归零和 Weak scope 不可升级；每批按固定采样点
检查进程计数、managed object、continuation/task/provider 注册项、blocking
ready/waiting/pending、socket pending、reactor request/timer/deadline 归零。

计数分配器覆盖 Rust 分配（包括测试本身），记录存活请求字节和累计分配量；
JIT 映射、线程栈及分配器缓存另看 `ps` RSS。断言可复用元数据容量不超过
热身最大值的 2 倍，存活 Rust 分配不超过热身最大值 + 512 KiB。
HashMap 的 `capacity()` 会随删除产生的控制字节状态波动，不能把每次
capacity 回升当作新分配；表格保留采样值，RSS 不设跨平台硬阈值。

## 最终结果

- `cargo fmt --check`、`cargo check --locked` 通过。
- 普通及 TSan 库套件均为 **863 passed / 7 ignored**；CLI **66 passed**。
- 普通及 TSan 的资源长跑、文件背压、3 个 socket 隔离测试全部通过。
  资源长跑分别耗时 6.81 / 81.13 秒，低于每个隔离测试 180 秒的超时。
- 普通压力测试 **1049 轮 / 60.026 秒**（下限 300），p50 **56.829 ms**、
  p95 **59.693 ms**、max **79.814 ms**。
- TSan 压力测试 **75 轮 / 60.784 秒**（下限 50），p50 **805.869 ms**、
  p95 **848.104 ms**、max **925.976 ms**；整个 TSan 入口无报告。
- 1/2/4 CPU worker 下各自完成相同的 24 批资源长跑；均通过。

下面取最后一个热身批次（3）和最终批次（23），字节数均为原始采样：

| 模式 | 批次 | 元数据 capacity 合计 | Rust 存活字节 | 累计分配字节 | RSS 字节 |
|---|---:|---:|---:|---:|---:|
| 普通 / 16 workers | 3 | 165 | 288,163 | 114,464,245 | 18,432,000 |
| 普通 / 16 workers | 23 | 155 | 288,703 | 682,084,809 | 18,448,384 |
| TSan / 16 workers | 3 | 180 | 282,990 | 116,478,837 | 126,844,928 |
| TSan / 16 workers | 23 | 143 | 283,038 | 695,227,053 | 145,129,472 |
| 普通 / 1 workers | 3 | 160 | 184,799 | 114,335,717 | 17,825,792 |
| 普通 / 1 workers | 23 | 167 | 185,267 | 682,019,817 | 17,907,712 |
| 普通 / 2 workers | 3 | 108 | 196,370 | 114,389,439 | 17,924,096 |
| 普通 / 2 workers | 23 | 247 | 196,370 | 682,109,579 | 17,924,096 |
| 普通 / 4 workers | 3 | 182 | 209,276 | 114,394,515 | 17,989,632 |
| 普通 / 4 workers | 23 | 193 | 209,744 | 682,119,735 | 17,989,632 |

普通 16 worker 的测量批次中，存活分配范围为 288,211–288,703 字节，RSS
为 18,432,000–18,448,384 字节，元数据 capacity 最大 270（热身最大 256）。
每批 scope/进程活动资源均回到零。上述近乎稳定的数值对应本次有限负载，
并非任意负载的长期内存上限或生产吞吐基准。


## 解释与限制

普通运行在固定活动峰值下的存活分配与 RSS 趋稳；累计分配量仍随工作量
上升，因为程序持续创建并释放对象。TSan 的 Rust 存活分配也应独立观察，
其 RSS 包含 sanitizer shadow/history 等额外元数据，不以 TSan RSS 的增长
单独判定应用泄漏，也不宣称插桩进程 RSS 已达到平台。

本次验证为 macOS 本地结果，不代表 Linux 或远端 CI 已通过。TSan 检查 Rust
runtime 和标准库，JIT 生成的机器码本身没有 TSan load/store 插桩。历史
SIGSEGV 尚无可确认根因，不能归因于本次发现的内存泄漏或用通过记录关闭。

固定并发回收不等于任意工作量都有内存硬上限：等待准入请求、整文件结果、
用户保留值仍可随输入增长。峰值后的 HashMap/队列容量和分配器缓存允许保留。
运行中的不可中断 syscall 允许在取消后暂时持有参数/状态，其最终释放用
可放行测试后端单独验证。控制任务生产与大文件缓冲区属于 5.2/5.3。
