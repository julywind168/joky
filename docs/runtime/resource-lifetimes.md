# Runtime 资源生命周期与句柄协议

阶段 5.1（2026-09-10）。当前实现位于 `crates/joky-runtime/src/runtime/continuation/registries.rs`、
`crates/joky-runtime/src/runtime/reactor.rs` 和 `src/codegen/cranelift.rs`；验收记录见
[阶段 5.1](../archive/reports/phase5-1.md)。

## Continuation 句柄

ABI 保留 `Continuation*` 这一指针宽度的参数形式，但其值是**不透明整数
token**，不是对象地址。运行时用 `without_provenance_mut` 构造 token，
调用者只能保存、比较、原样传回，禁止解引用、做地址运算或自行释放。
frame/result/payload 指针仍是独立存储的真实地址，不能与句柄混用。

- 0 表示无句柄。进程内的分配序列从 1 递增，永不复用，跨 scope 也不重置。
- 注册表只保存活动 token → `Continuation`（内部 `Arc`）。删除活动项后
  复用 HashMap 的容量，不保留 tombstone、历史地址或占位分配。
- 递增使用 checked arithmetic；`usize` 名字空间耗尽时明确 abort，
  不回绕、不重用旧 token。本协议不拆分 slot index/generation。
- continuation 原有 generation 仍用于恢复校验；即使新 scope 使用相同
  generation，旧 token 也无法解析到新对象。
- 外部回调先在表锁下 clone `Arc`，再通过 lifecycle gate 检查 owner、
  free_requested 和 handle_released。退役先移除公开 token，再释放缓冲区；
  不存在或已请求释放的 token 被拒绝。重复 free/完成/取消不重建条目。
- 已入场回调由 callback lease 保活；取消、原生交接、子调用和内部任务等待
  继续遵守原有排空规则。只有内部父子所有权遍历允许查找等待子项排空的
  free-requested 父项；provider 不使用这个入口。

provider 接口的表示不变，但必须把参数当作不透明 token。异步 provider
应持有所需参数或 runtime `Arc`，完成时走校验入口；完成被拒绝后由 provider
清理尚未转交的结果，不能直接写入已缓存的 continuation 地址。

## Native 与 runtime 的所有权交接

挂起操作的值参数（包括 SQLite/File/Socket 等 native 句柄）交给 continuation
持有；编译器按参数所有权登记清理，在完成、启动失败或取消时释放。`&T` 借用
参数不登记清理，仍由调用者持有。provider 不自行释放传入的句柄包装对象；
后台工作需要跨挂起存活时，单独持有底层资源的 `Arc` 或共享值引用。即使
`close` 返回错误，也遵守值参数已被消费的规则。

此规则从 AOT runtime ABI **v23**、module metadata/cache ABI **40** 起统一；
旧 runtime 的 File.close 会自行释放包装对象，不能与新生成代码混用，需重新构建。

原生函数为挂起点预留的句柄由生成代码负责。Ready 返回按已有路径释放；
Pending 返回时释放未用的静态预留项，保留当前 Suspend/TaskWait 或动态调用
激活。原生调用在交接之前失败（包括 provider 缺失、启动拒绝和取消）时，
释放静态预留项和动态调用槽；动态槽可为 0 或已退役 token，free 均可重复。

最外层 native boundary 退出、发布 deferred Pending 前，将激活标为
runtime_owned；未发布而需要取消的激活也交给 runtime。之后完成或取消会
请求退役，待 callback、native、timer、子调用与 task wait lease 排空后执行。
手动调用 ABI 创建的句柄仍由调用者显式 free，不因达到终态自动失去句柄。

## 终态回收审计

| 资源 | 生命周期与保留边界 |
|---|---|
| Continuation | 公开 token 退役和最后一个内部 `Arc` 销毁分别计数；后者可被在途 provider 持有 |
| Task group / heap task storage | join/close 等待终态，释放未领取结果、failure 和 heap context，清空 task map |
| Handler frame | 分支及 continuation 的引用释放后销毁；挂起前的 frame 继承关系不改变 |
| Provider 注册 | 以 scope/operation 为键，安装 guard 退出时移除 |
| Blocking pool | ready/waiting 队列在完成或取消时移除请求；容量允许保留到峰值；等待路径及在途请求保留共享参数 |
| Socket | scope 关闭时取消 pending I/O 并移除 socket 表项；迟到事件经请求/句柄校验拒绝 |
| Reactor | reserve 在发布命令前建立活动请求；取消只标记仍活动且类型匹配的请求；完成/取消移除记录，无历史取消集合 |
| Timer deadline | BTreeSet 按 `(deadline, id)` 索引，取消时立即删除，不再等待远期 deadline 才回收 |
| JIT / 字面量 | backend 先关闭并排空关联 scope、清理未领取失败载荷、撤销 machine entry，再显式 `free_memory`；重复调用 Compiler 时替换前一次 backend |

Reactor 请求 id 也 checked 递增，耗尽会报错而不回绕。取消发生在 register
命令处理前时由活动记录保留标记；重复、错误类型和已完成请求的取消均无效。
传统没有独立 on_cancel 的 timer 仍执行其收尾 callback，保证既有排空语义。

JITModule 的普通 Drop 不释放可执行映射，因此必须显式释放；释放前要求
没有运行中的生成代码或以后会执行的 JIT drop glue。scope 的工作排空和
终态存储清理保证这一边界。后台文件 syscall 可能继续运行，但取消后只能
丢弃迟到结果，不能恢复用户代码。长跑测试同时验证 scope 最终没有残留强引用。

## 观测口径与边界

`cfg(test)` 的资源 lease 分别统计 scope 和进程的 continuation、task group、
handler frame、file request、共享文件路径字节数；计数器独立于 scope，
不会通过观测形成循环引用。注册表快照补充 token、task link、machine entry、
provider、挂起参数字节数、准入 ready/waiting、socket 和 reactor 数据。

测试进程使用包装 System 的计数分配器，记录 Rust 存活请求字节和累计分配
字节，包含注册表、闭包等非 managed 分配。它不计分配器实际 size class、
线程栈或 JIT mmap；RSS 用 `ps` 另行采样。累计分配随工作量上升是正常现象。
这些观测没有进入生产 ABI，也不是新增全局内存限额。

不可中断的文件 syscall 可以在取消后继续保留一个 continuation `Arc` 和
共享路径；公开句柄此时已经退役。受控测试阻塞 syscall，分别核对取消后
保留量、新句柄不受影响以及放行返回后最终归零。等待任务和整文件结果的
总量仍由调用者工作量决定；有界生产和分块 I/O 属于 5.2/5.3。
