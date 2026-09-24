# Cown lease 登记、精确唤醒与 Pending

`when` 获取阶段已接入 MIR/continuation Pending ABI：无竞争直接进入 body，
竞争时保存执行状态并交还 worker，Cown 可用后重新入队。body 仍禁止挂起和嵌套
`when`。Pending 改善等待期间的 worker 可用性，不保证所有负载吞吐提高。

## 任务本地 lease

`TaskContext.wake` 指向 TaskGroup 持有的独立运行状态，其中包含活动 Cown lease
批次。初次执行、换 worker 恢复和终态清理通过 TaskContext 访问同一状态，不依赖
OS 线程本地容器，也不再查询全局 `Mutex<HashMap<TaskContext, Vec<Cown>>>`。

批次前 4 个句柄内联存储，更多句柄使用可复用 Vec。多 Cown 成功获取后一次
登记整批；正常 release 仍逐个移除。锁为每任务独立的短锁，终态清理先取走
批次、释放锁，再逐个释放 lease。取消不能在 native body 尚未退出时强行释放
payload 的独占权；沿用任务与 continuation 的执行排空协议。

单 Cown 获取有专用路径，不分配、排序多 Cown 的临时数组。句柄本身仍是
region capability，登记与移除不增加 Cown 引用计数。

## 获取与通知协议

每个 Cown 有原子状态字（LEASED、WAITERS 两个位）和受短锁保护的等待队列。
无竞争获取只更新状态字；没有等待者时，释放不取等待队列锁。

1. `jk_cown_try_acquire_many` 校验句柄，按稳定 ID 排序并拒绝重复 Cown。
   无竞争立即成功；遇到竞争最多进行 32 次短暂自旋检查。整批成功后登记到
   当前任务，直接进入 body，不转移局部变量到挂起帧。
2. 获取失败回滚全部已取得的 lease。编译器保存活跃局部变量和临时值，调用
   `jk_continuation_start_cown_acquire`。等待状态只保留 Cown capability，
   **不保留部分 lease，也不保留 payload 借用**。
3. continuation 在一个仍被占用的 Cown 上登记等待者。持队列锁，以原子 RMW
   设置 WAITERS 并检查 LEASED；若已经空闲则立即通知。登记与释放之间不会
   丢失唤醒。release 先释放队列锁，再调用通知回调。
4. 通知将 continuation 设为 Ready。native 调用栈完全退出并发布 Pending
   后才能调度恢复；早到的通知不会提前运行或释放仍被 native 栈使用的帧。
5. 恢复时安装原任务、handler 和 region 上下文，重新尝试整个 Cown 集合。
   成功后在同一机器入口中执行 body；失败则回滚并重新登记实际阻塞的 Cown。
   若阻塞点改变，还要把原来已空闲 Cown 的通知传给后续等待者。
6. 取消撤销等待登记，再清理局部变量和区域。已选中但放弃获取的等待者会在
   Cown 空闲时继续通知下一个。native handoff 前收到取消须延迟清理到交接完成。

通知只允许重试，**不转交 lease 所有权**。新的获取者可能抢先成功，当前不承诺
严格 FIFO 或无饥饿界限。等待路径没有 `park`、定时轮询或递归帮助执行；它仍
使用若干短 Mutex，因此不是 lock-free，也不能称为实现中完全没有阻塞点。

## 编译器与同步边界

MIR 用 `CownAcquire` 和独立 continuation ID 表示内部挂起点，向直接调用、
方法调用和间接调用传播 Pending。获取成功的快速路径继续遍历后续控制流；
恢复入口支持重新获取 payload 后的 Store 和 Cown 返回值。源码字段赋值仍须
通过方法进行；这里的 Store 支持也覆盖内联后产生的 MIR。

恢复后的循环使用 SSA 变量追踪重新定义的值和局部变量，避免一直读取首次保存
的循环下标。取消或失败的快速获取也先保存拥有所有权的活跃值，再执行终态清理。
verifier 拒绝跨挂起点保留 lease/payload 借用；`when` body 内不可调用可能挂起
的函数。

保留同步 `jk_cown_acquire` / `jk_cown_acquire_many` ABI，供原生兼容边界使用。
这条路径仍可能帮助执行任务并用 `thread::park` 等待，采用每 Cown 精确通知，
不再按 1ms 轮询。同步宿主需要返回最终结果时，也仍须等待 Pending 链完成。
调度器普通空闲 worker 的既有退避/超时机制未包含在本次修改中。

Region 先排空任务和 continuation，再释放 payload 与 Cown 控制块；等待者不得
活过这个边界。本次 managed ABI 为 **8**，AOT runtime ABI 为 **v25**，module
metadata/cache ABI 为 **43**；TaskContext 布局大小不变。升级需重建 runtime archive。

## 验证

新增回归覆盖真实任务挂起后单 worker 处理无关任务、取消发生在交接前后、通知
先于交接、多 Cown 部分获取回滚、恢复时阻塞点改变及其通知接续。编译器和 CLI
用例覆盖挂起后的循环、反向多 Cown 获取、直接/间接调用、Cown 返回值、恢复后
payload Store，以及 native/恢复入口获取失败时拥有所有权的局部值清理。
取消回归还覆盖恢复后新调用在 Pending 发布前失败时的 continuation 句柄回收。

最终验证：编译器库 1137 项通过（10 项忽略），独立 runtime 232 项通过
（4 项忽略）；5 项 Pending runtime 回归在单 worker 的独立进程中通过。
完整 CLI 183 项通过默认 JIT/debug AOT 矩阵及资源检查；12 项 Cown 用例和
7 项取消用例另外通过完整 debug/release AOT 矩阵。

## 基准

脚本：[scripts/bench-cown.py](../../scripts/bench-cown.py)。必须使用未开启
`test-support` 的 CLI 和独立 release runtime，以免资源登记锁干扰测量：

```sh
cargo build
CARGO_TARGET_DIR="$PWD/target" cargo build --manifest-path crates/joky-runtime/Cargo.toml --release
python3 scripts/bench-cown.py --compiler target/debug/joky \
  --runtime target/release/libjoky_runtime.a --output-dir /tmp/joky-cown-bench
```

### 任务本地登记与精确唤醒（上一阶段）

本机 macOS ARM64，基线 `1900b3b`。双方均为 release AOT、相同源程序，8 个任务
各执行 100000 次 `when`；每种 worker 数先预热一次，取 3 次独立进程执行的中位数。
独立/热点场景每次获取单 Cown，重叠场景获取环状相邻的两个 Cown。每次运行都校验
最终计数，计时包含进程启动、任务创建及区域销毁。

| 场景 | workers | 修改前 ms | 修改后 ms |
| --- | ---: | ---: | ---: |
| 独立 Cown | 1 | 131.9 | 34.8 |
| 独立 Cown | 2 | 191.5 | 20.4 |
| 独立 Cown | 4 | 317.5 | 17.8 |
| 独立 Cown | 8 | 349.3 | 15.7 |
| 共享热点 | 1 | 105.5 | 34.4 |
| 共享热点 | 2 | 191.8 | 47.5 |
| 共享热点 | 4 | 285.6 | 78.8 |
| 共享热点 | 8 | 317.5 | 83.5 |
| 重叠双 Cown | 1 | 156.3 | 68.3 |
| 重叠双 Cown | 2 | 276.8 | 41.5 |
| 重叠双 Cown | 4 | 522.6 | 43.3 |
| 重叠双 Cown | 8 | 507.7 | 42.9 |

8 worker 下，三种场景的进程 CPU 时间中位数分别从 1.466/0.953/1.398 秒降至
0.051/0.240/0.133 秒。短 body 放大了登记和唤醒开销，数字不能外推为实际应用
提速；高竞争仍有等待节点分配、队列维护和获取重试，未测尾延迟或证明公平性。

### Pending（本阶段）

基线为 `5e8d216`（已含任务本地登记和精确唤醒）。复用其 AOT 可执行文件，
与本次代码在同一机器顺序复测，未并行运行编译或测试。参数仍为 8 个任务 ×
100000 次 `when`，每项预热 1 次、计时 3 次取中位数；本次保留最多 32 次短暂
自旋。计时包含进程启动，单 worker 基本没有竞争。

| 场景 | workers | 同步获取 ms | Pending ms |
| --- | ---: | ---: | ---: |
| 独立 Cown | 1 | 32.3 | 33.0 |
| 独立 Cown | 2 | 19.5 | 19.2 |
| 独立 Cown | 4 | 17.2 | 15.5 |
| 独立 Cown | 8 | 14.6 | 11.9 |
| 共享热点 | 1 | 34.6 | 31.9 |
| 共享热点 | 2 | 48.4 | 130.0 |
| 共享热点 | 4 | 78.5 | 165.5 |
| 共享热点 | 8 | 79.9 | 326.6 |
| 重叠双 Cown | 1 | 67.0 | 71.6 |
| 重叠双 Cown | 2 | 37.7 | 41.3 |
| 重叠双 Cown | 4 | 51.3 | 88.3 |
| 重叠双 Cown | 8 | 45.9 | 306.4 |

独立 Cown 场景基本持平；高竞争且 body 极短时，保存/恢复 continuation、等待
节点分配及重新入队的成本超过工作本身。短暂自旋只能有限缓解，不能消除回退。
Pending 的本次收益是等待任务交还 worker 和同步调用栈，不是热点计数器吞吐。
生产场景的尾延迟、公平性及更长 body 的性能尚未测量，后续需优化争用路径并
用真实负载验证。

## 条件等待

`when (...) |payload| until condition { body }` 复用 Pending 获取协议，增加独立的
条件等待队列。假条件在释放 lease 前完成登记，收到相关 `when` 完成通知后重新
获取并检查；没有定时轮询。协议、条件限制和取消语义见 [when until](when-until.md)。
完整的网络主动推送示例见 [push_server.jk](../../examples/networking/push_server.jk)。
