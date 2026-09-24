# 有界迭代实现

用户语法、游标与并发限制见[循环与游标](../lang/iteration.md)。本文描述 lowering、任务调度与资源边界。

`for item in items { body }` 是顺序收集表达式；
`@parallel(limit: N) for item in items { body }` 使用相同的结果规则，最多让
N 次直接迭代同时进行。顺序输入支持不可变 `List(T)`、内建容器游标
（`bytes.iter()`、Map 的 `entries()` / `keys()` / `values()`、Set 的
`iter()`、可变容器的 `into_iter()`），以及实现内建 `Cursor` 的
struct/class，包括 `joky/sqlite` 的 `SqliteRows`；并发输入仍仅支持 List。
循环体有值时输出为 `List(U)`；循环体是 `Unit` 且没有 `List` 类型期望时，
`for` 本身是 `Unit`，不分配结果列表。
`(index, item)` 增加从 0 开始的 `UInt64` 索引。

正常完成收集一个尾表达式值，`continue` 收集零个；顺序 bare `break` 返回
已收集前缀。并发 `break` 不支持，避免把提前结束的边界与已经发生的并发
副作用混在一起。两种形式都按输入顺序返回。输出元素沿用 List 的限制，
不能包含唯一所有权 class、socket 等值；可返回其不可变处理结果。

## 顺序游标

`Cursor.advance(self) -> Option((Item, Self))` 每次消费当前状态，返回一个
元素及后继状态，或以 `None` 结束。循环直接逐项调用，不把输入具体化为
新的中间 List。顺序 `List(T)` 的 `advance` 内联为 head/tail；其他游标按
D5 调用。顺序结果由专用 `SeqBuilder` 在尾部链接 singleton 节点，O(1)
追加、不复制 payload、不排序；`@parallel` 仍使用带锁和最终排序的 Batch
协调器。

游标和元素分别按 Copy、Shared、Owned 管理。Shared 状态以槽位转移
（`TakeLocal`）交给 `advance`：循环侧不再持有旧引用，也不做引用计数增减；
Owned 状态移交后，循环不再持有旧状态。返回值先进入取消清理保护，再拆出
元素和后继状态；两者在进入用户代码前绑定。`continue` 保留后继状态，
`break` 释放它，`?`、abort 和取消走相同的作用域清理。元素允许是唯一
所有权 class，只有输出仍受 List 元素限制。

`bytes.iter()` 的内建游标是 Shared managed 对象，持有 Bytes 的一个引用并
记录字节偏移：`advance` 原地前进同一对象，每步 O(1)、零分配、不复制
内容；耗尽或提前退出时释放游标及其持有的引用，Bytes 本身不受影响。

Map 与 Set 的内建游标是唯一所有权 managed 对象，持有容器的一个引用和
深度不超过树高的 DFS 路径栈：`advance` 原地回退并下探，摊还 O(1)、最坏
O(log n)，遍历期间整个容器保持存活；产出的键值按需克隆共享引用
（`keys()` 不触碰值，`values()` 不触碰键），耗尽或提前退出时释放游标及
其引用，容器本身不受影响。遍历顺序是哈希序：进程内确定，但字符串键的
哈希带进程随机种子，跨运行可能不同。

`MutList` / `MutMap` / `MutSet` 不是 `Cursor`。`into_iter()` 消费容器，
产出拥有该容器加索引的唯一所有权游标，每步 `get(i)` / 打包槽位 O(1)；
耗尽、`break` 或取消时析构游标并释放容器。需要遍历后继续使用容器时，
先 `to_list()` 复制不可变快照。过滤用 `retain(&self, keep)`，不要在
遍历中增删。

SQLite 的 `statement.query()?` 返回逐行拉取的 `SqliteRows`，每项为
`Result(SqliteRow, String)`。`advance` 在 blocking pool 执行 step 并复制
当前行；循环体可用 `result?` 传播错误。循环体为 `Unit` 时不收集输出。
`break`、`?` 和取消会释放游标；正在执行的 native 工作完成后才最终
finalize，作用域退出等待清理排空。保留的行不受下一次 step 或 finalize
影响。详见 [SQLite](../stdlib/sqlite.md)。

具体游标传播实现的 effect；纯泛型游标可通过内部 task wait 挂起。
每轮推进和调用结果交接都检查取消。输入侧内存由游标自身的状态与持有
资源决定；不收集时不创建输出节点或结果索引记录。

## 任务与内存上限

每个并发循环创建一个 task group、一个共享 coordinator，以及
`min(N, 输入长度)` 个逻辑任务。这些任务重复从 coordinator 领取输入；
一次迭代挂起时，该任务保存 continuation 并释放 CPU worker。不会预先
为所有输入创建 TaskContext、捕获副本或 I/O 请求。父任务通过 TaskWait
挂起等待整个组完成，检查失败后关闭 group，再取出结果。

coordinator 的短 Mutex 仅保护输入游标、索引分配和结果发布；执行用户
代码、请求 I/O、等待准入和排空子任务期间均不持锁。输入节点的临时引用
在取得元素后立即释放，避免一个慢请求额外保留完整未处理尾部。

| 占用 | 随什么增长 |
|---|---|
| 输入 List | O(输入长度)，由调用者及游标共同持有 |
| 顶层批次任务/捕获/在途迭代 | O(min(N, 输入长度)) |
| 完整输出及排序索引 | O(实际输出数量)，`continue` 不留结果元数据 |
| I/O 请求数 | 每次迭代最多一个直接等待时，不超过 N；显式子任务另计 |
| I/O 字节数 | 每项缓冲区大小之和；整文件读取仍可分配整个文件 |

并行输出暂存为带输入索引的 singleton List，排空后排序并连接已有节点，
不再复制结果 payload，成本含 O(M log M) 排序。顺序输出按推入顺序尾链接，
无排序。循环体是 `Unit` 且没有 `List` 类型期望时，`for` 本身是 `Unit`，
不收集结果；累加、计数等任务私有状态用局部 `var` 保存即可。`continue`
只用于收集循环里跳过某一项。

每次迭代的 `branch` 在本次迭代结束前排空，包括 `continue` 和顺序 `break`。
显式 `parallel`、`race`、子批次及函数内部任务有自己的结构化生命周期。
额度是批次局部的，嵌套 limit=1 不会争用父批次的同一个 semaphore；但
外层 N × 内层 K 可以产生 N*K 次叶迭代，N 不是整个程序的全局并发上限。

## Effect、错误和取消

文件、timer 和网络操作复用既有 provider 与 Pending ABI。文件池饱和时，
已领取的迭代挂起等待 FIFO 准入，仍占用一个迭代额度；不会让用户为容量
错误手动重试。CPU worker 不等待 blocking syscall，也不持有队列锁。

并发体的共享捕获各自保留引用；外部 owned/borrowed 捕获被拒绝，应在
本次迭代内创建资源或通过 Cown 共享。handler 按既有任务继承和隔离规则
工作，迭代内部安装的 handler 在对应迭代内使用。

未处理的 abort/运行时失败取消同批任务，排空后传播给父 handler 或运行
入口。普通 `Result.Err` 可作为输出收集，不是隐式 fail-fast。动态零额度
报告 `parallel limit must be greater than zero` 并结束当前失败路径。
并发体不允许通过 `?` 返回外层调用者；顺序循环可使用函数本来的 `?` 语义。

取消是协作式的：领取前及进入用户体前检查取消，已挂起的 provider 接收
取消信号；竞争中已经开始的副作用不会回滚。已启动且不可中断的 syscall
沿用文件池协议，可能继续运行，迟到完成不恢复被取消的用户代码。排队
参数、已收集输出、未处理输入和任务捕获最终由对应 scope 释放；取消
一个批次不取消无关批次。

实现验收与已修复的循环挂起问题见 [5.2 验收记录](../archive/reports/phase5-2.md)。
