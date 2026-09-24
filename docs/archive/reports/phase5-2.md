# 阶段 5.2 有界迭代验收

> 历史记录：本文保留原阶段的设计、环境与验收结论，不代表当前能力。归档于 2026-09-24；当前说明见[文档入口](../../README.md)，未完成工作见[计划索引](../../plans/README.md)。

日期：2026-09-10。基于 `dfde53a` 加本次修改；全部为本地验收，没有远端 CI。
环境：M1 Pro / 8 cores / 16 GiB，macOS 26.6.2；普通 rustc 1.97.1
（8bab26f4f）。TSan 沿用 5.1 的 1.100.0-nightly（4aa1fbcf4），标准库
一并插桩，`halt_on_error=1:exitcode=66`，无抑制。

## 实现和回归依据

- 顺序 `for`、可选 `(index, item)` 和 `@parallel(limit: N)` 已贯通
  parser、sema、HIR、MIR、JIT 与 runtime。首版输入为 List，支持动态
  UInt64 上限、顺序输出、continue、顺序前缀 break 和空/Unit 结果。
- 固定数量任务复用共享 coordinator；输入节点在执行用户体前释放。
  子任务等待使用 TaskWait/Pending，每次迭代中的 branch 在结束前排空。
- 泛型实例补齐内部 `(UInt64, List(T))` 与 Option 布局；闭包/任务捕获
  按词法绑定收集，迭代变量不误捕获同名外层 owned 值。重复 owned 捕获
  和跨任务/闭包的循环控制被拒绝。Unit 集合使用一个无指针的占位存储字。

验证过程中发现并修复三个取消/恢复问题：

1. 旧 spill 分析把恢复块可达区域的所有旧 SSA 操作数视为存活；循环回边
   会把已经释放的输入节点保存进 continuation。若其地址被快速 race
   获胜结果复用，取消慢批次会误释放获胜结果。改用考虑定义和 phi 边的
   逆向存活分析；单 worker CLI 竞争用例及隔离进程资源检查覆盖这一路径。
2. 从函数/Effect 调用取得 managed 结果后，立即执行的取消检查可能在
   结果绑定局部变量前返回，丢失所有权。现在在检查前暂存为受清理保护的
   局部变量；输入构造与 timer 竞争的测试核对最终释放。
3. 根函数的取消分支原先只返回，没有关闭新建的 task scope。动态零
   上限会触发这条路径并导致入口排空等待不结束。取消检查现在关闭所有
   活动 scope；CLI 限时断言退出失败、显示明确诊断，且不执行后续用户代码。

此外，循环索引在挂起前绑定局部变量、恢复后重新读取，避免在机器入口中
引用不支配当前路径的旧索引临时值。

## 可复现命令

```bash
cargo fmt --check
cargo check --locked
python3 scripts/check-runtime.py
cargo test --locked --test cli
python3 scripts/check-runtime.py --tsan
cargo run --locked -- run examples/concurrency/bounded_for.jk

for workers in 1 2 4; do
  JOKY_WORKER_COUNT=$workers cargo test --locked --lib \
    compiler::tests::resources::bounded_for_task_storage_and_cancellation_cleanup \
    -- --ignored --exact --nocapture --test-threads=1
done
```

正常与 TSan 脚本都运行库套件及每个隔离测试，保留每步退出状态和超时。
日志在 `target/runtime-checks/{normal,tsan}/`。本次新增的批次资源测试已
加入脚本，文件池饱和测试扩展为同时包含批次请求。

## 结果

| 检查 | 结果 |
|---|---|
| fmt / cargo check | 通过 |
| 普通与 TSan 库套件 | 各 874 passed / 8 ignored |
| CLI | 68 passed；包含 1/2/4 worker 的嵌套 limit=1 与 race 取消 |
| 普通与 TSan 隔离入口 | 6 个资源/背压/socket 测试和 1 个 60 秒压力测试均通过 |
| 新批次资源测试，1/2/4 CPU workers | 分别通过，最终资源归零 |
| 示例 | 输出 `1`、`9`、`0`、`25`，各占一行 |

8 个忽略项中，7 个由隔离入口分别运行；另一个为有意忽略的手动性能测试，
不把它计入通过数量。TSan 无竞争报告。最后补充“取消不损坏调用者仍持有
的输入”和“输入构造期间取消”断言后，重新运行了普通入口，并单独重跑
增强后的 TSan 批次测试（`bounded_for_final.log`，12.88 秒）；生产代码未变。

压力测试是既有取消/挂起组合回归，不是 batch 吞吐量基准：

| 模式 | 迭代 / 耗时 | 最低阈值 | p50 | p95 | max |
|---|---|---:|---|---|---|
| 普通 | 969 / 60.008 s | 300 | 60.789 ms | 67.502 ms | 218.639 ms |
| TSan | 70 / 60.287 s | 50 | 846.819 ms | 924.721 ms | 1125.203 ms |

## 有界与释放证据

`bounded_for_task_storage_and_cancellation_cleanup` 在运行时构造 1024 个
String 输入，每次迭代经过 timer 挂起后 continue，不保留输出。直接统计
heap TaskContext 的创建量及存储峰值，而不是从源码注解推测并发数：

| limit | 输入量 | 实际创建任务 | task 存储峰值 | 排空后保留 |
|---:|---:|---:|---:|---:|
| 1 | 1024 | 1 | 1 | 0 |
| 2 | 1024 | 2 | 2 | 0 |
| 5 | 1024 | 5 | 5 | 0 |

普通、TSan 及 1/2/4 CPU worker 配置均得到上述计数。另执行 8 次预构造
输入的 race 取消，检查获胜结果和调用者输入仍有效；4 次在递归构造输入
期间竞争取消；最后测试动态零额度。每次排空核对 managed 对象、scope 资源、continuation 注册项、blocking
队列、reactor deadline 以及 socket 请求归零；各限额测试另核对 task
存储归零。

文件背压隔离测试占住所有 blocking workers 和 runnable 队列位置，同时
启动一个根读取、两个 parallel 读取和一个 12 项/limit=5 的批次。放行前
精确观察到 `1 + 2 + 5 = 8` 个等待准入项，用户文件操作执行数为 0；批次
不会为剩下 7 项创建请求。放行后全部读取成功，无手动重试，最终 managed
计数回到基线。另有 limit=3 的批次在饱和等待准入期间被 timer race 取消，
CPU 任务仍可推进，等待项与参数最终释放。

结果顺序、筛选/前缀、嵌套 limit=1、共享/嵌套 List、Unit、泛型、闭包、
文件 provider、迭代内 handler 和 abort 排空分别由 `compiler::tests::iteration`
的 11 项测试覆盖。CLI 的死锁回归设置 15 秒超时并回收失败子进程。

## 验收边界

这是 macOS 本地证据；Linux 与远端 CI 仍待 5.4/基础设施验收。
并发额度约束直接迭代数，不约束单项字节数或显式嵌套子任务的总数。
输入和保留输出仍可占 O(N) 内存；当前顺序/并发输出均通过排序连接，
丢弃结果也先收集，纯副作用遍历可尾部 continue。未声称流式输入、分块
文件读写、并发 break 或网络 HTTP 标准库已经实现。详细语义见
[有界迭代协议](../../runtime/bounded-iteration.md)。
