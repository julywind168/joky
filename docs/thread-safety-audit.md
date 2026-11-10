# S6 线程安全边界审计记录

> 2026-11-10 更新：本文涉及自定义 `joky/drop.Drop` 的设计、限制与验收为历史记录；该能力已撤回。当前自动清理规则及迁移见[撤回说明](custom-drop-withdrawal-review.md)。

日期：2026-10-30。代码基线：`68d08e1`；路线状态修正：`c023bbf`。
本记录完成[线程安全路线 S6](thread-safety-roadmap.md#s6安全边界审计与验收)要求的系统性边界核对，
覆盖编译准入、跨线程传输、共享状态、宿主证据和取消清理。检查方法是沿创建、移交、访问、停止、
释放路径阅读实现，并对照已有行为测试；不是仅统计 `Send/Sync`，也不是对全部 unsafe Rust/C 的形式化证明。

本次未发现需要关闭已开放能力的新实现漏洞。发现外部文件服务的公开安全契约没有明确写出
“同一 context 可从任意线程收到并发回调”，已在 `fs/abi.rs` 及 `FileService::from_descriptor`
文档补明，与已有 unsafe `Send/Sync` 包装的要求一致。本次不改变执行代码或开放范围。

下列结论限定于此代码版本和第 6 节的验证范围。后续改变布局、代码保活、资源准入或停止顺序时，
应重新检查对应条目；“已审计”不能作为新入口自动获得权限的证书。

后续共享分配索引优化：`Allocation` 的登记锁序改为“地址索引桶 → 所属分片”，
索引使用稳定槽位，显式释放及 Region 清理撤销登记后在锁外执行元数据 finalizer。
该变更不新增 `unsafe impl`，也不改变下述 owning／loan 的权限、停止和代码保活协议；
普通内部地址仍检查任务身份及完整范围。索引／槽位均计费，部分登记失败由未发布的
分配记录回收。当前不变量见[共享存储](architecture.md#可转移准备与共享存储)，
回归责任在 `storage.rs` 的索引生命周期、失败退款、锁隔离和可重入清理测试。

## 1. 编译与代码身份

| 边界 | 已核对的依据与拒绝条件 |
| --- | --- |
| 类型资格与调用资格分离 | [parallel.rs](../src/core/ir/parallel.rs) 的 `qualify_with` 检查存储属性；`analyse` 的依赖闭包另查调用、清理及宿主证据。拥有包还有 [packet::classify](../src/core/ir/parallel/packet.rs) 和运行时 Shape 检查。原始指针、引用、伪装的名义字段或 phantom 参数不能靠外层容器获得传输许可。拥有型闭包、dyn、带 Drop 的值走各自内部协议，不能由宿主 `TaskValue` 任意构造。 |
| 间接调用、闭包与 dyn | [callables::Targets](../src/core/ir/parallel/callables.rs) 从当前不可变模块收集候选；拥有闭包携带编译器确定的目标，dyn 检查具体存储和所有匹配方法，包括未调用的方法。清理依赖同样参与传播。[validate_dyn_tables](../src/core/ir/dyn_traits.rs) 检查擦除描述与方法签名。未冻结目标或 Local 目标阻止并行；借用任务中的 dyn 派发仍受限。 |
| Scoped 宿主调用 | [contracts.rs](../src/core/ir/parallel/contracts.rs) 先验证完整保留签名；[scoped::rejections](../src/core/ir/parallel/scoped.rs) 传播直接调用、间接候选与清理的拒绝原因。普通宿主引用 ABI 不因函数名称或通用 Parallel 注册而获得 Scoped 许可。受检调用许可与资源能否成为任务输入是两个检查。 |
| JIT 冻结 | [CompiledModule::freeze](../src/backends/jit/mod.rs) 先 `require_packet`，保留可执行页的 Arc 及合格 Host 依赖；任务不携带原 VM 的 Rc 可变状态。旧代码页在最后持有者释放后才销毁。域计算的 `freeze_domain` 依赖域效果检查，并由控制侧 grant 保留完整本地模块。 |
| 解释器冻结 | [FrozenEntry](../src/backends/interp/mod.rs) 共享不可变指令与元数据；`instantiate` 在接收 worker 重建 Rc dispatch、HostBinding 和任务表。未获证据的宿主变为 `Unavailable`，不会复制原 VM 的本地对象。可移植解释器析构也在执行清理的 worker 实例化。 |
| AOT 与嵌入式 Native | [ParallelEntry::prepare / declare_parallel_host](../src/backends/aot/mod.rs) 是 unsafe 链接信任入口：精确生成符号、ABI、依赖保活由调用者保证。[standalone.rs](../src/runtime/parallel/standalone.rs) 校验启动描述的版本、尺寸、模式和结果类型，但不验证任意函数指针。独立进程中代码活到进程结束；Scope 保留实际代码／权限根，不能把独立 AOT 的 Files 根强转成嵌入式 VM Code。 |

## 2. 数据传输与清理责任

| 边界 | 创建与使用权限 | 失败、取消与最终释放 |
| --- | --- | --- |
| 普通拥有图 | [OwnedBuffer](../src/runtime/parallel/mod.rs) 捕获前遍历实际 Buffer 图，要求并行存储、合格资源、无 Local 清理状态；只允许同一 Storage root 安装。`take` 消耗唯一 owner 并更新任务来源。 | [Packet](../src/runtime/parallel/packet.rs) 每转移一个 owner 才清除来源对应清理项；部分安装把已安装 owner 纳入接收清理。未领取的无用户析构包可以在丢包线程释放；来源不重复释放。 |
| 带用户 Drop 的拥有图 | `portable.rs`（已随 Drop 撤回删除） 冻结 Descriptor 与 PortableCallback，保留 Storage、预算和 CleanupReceipt；不是把本地 Rc 回调直接标成 Send。 | `disposal.rs`（已随 Drop 撤回删除） 把未安装包交给所属任务 worker 执行析构。返回包的 receipt 属于消费方父任务，避免生产方等待自己已交出的结果；清理失败回到对应失败边界。receipt 归还前不得完成任务清理。 |
| 闭包、拥有 dyn | 环境中的 owner 仍走上述包协议；[Buffer::DynDispatch](../src/runtime/buffer.rs) 只含冻结 native 表或 Arc 解释器表，代码由任务根保活。前端检查所有目标、具体元素及析构依赖。 | 正常结果、未领取结果、取消和析构失败都沿统一 owning/portable 清理链；宿主根结果不开放任意 dyn、闭包或语言析构包，因此不能把需要 worker 执行的 Drop 留给任意宿主线程。 |
| 普通只读借用 | [Shape::source / Range](../src/runtime/parallel/scoped.rs) 校验当前任务的实际分配来源，或该任务收到的有效 Loan；同属一个大 Storage root 不足以授权。Scope 保留根和代码。 | 子树全部 cleaned 后来源才可销毁。接收 Loan 和 Buffer 头属于接收 worker；重借用保留原始来源 scope。取消不撤销仍在使用的数据，而是先停止所有读者。 |
| String、聚合和嵌套容器视图 | [graph.rs](../src/runtime/parallel/scoped/graph.rs) 按受检布局递归冻结中间指针表、清零 owning lanes，再安装本地视图；只共享稳定的叶子数据。检查 enum 活跃分支、长度、布局与图遍历上限。 | 安装失败只清理已建的本地视图，不取得来源的销毁权。含自定义 Drop 的来源仍由源任务在读者结束后析构；视图不复制并执行来源 Drop。 |
| Shared VM 与 Mailbox | Shared VM 只传同树控制代理身份。Mailbox 额外验证当前任务 owning 来源、私有回调和创建标记，预留返回元数据后 CAS 取得整段独占权；见 [exclusive.rs](../src/runtime/parallel/control/exclusive.rs)。 | 请求及本地 Call 同时持有临时代理；最后一个 Return 释放时才恢复来源权限，且不分配。借出的来源不可再次访问或移动；接收临时代理不拥有 close 权，也不能继续跨 worker 独占重借用。部分安装失败沿同一 Return 链归还。 |
| 域持久根与写候选 | [storage/parallel.rs](../src/vm/state_domains/storage/parallel.rs) 的 Read 用 Arc 保留不可变 Root，Write 使用独立 snapshot 和 `Arc::get_mut`。root 的整个封闭 owning graph 移交，[domain_storage](../src/core/ir/cleanup.rs) 拒绝资源、闭包和用户 Drop；不是任意 Arena 都可共享。 | [parallel.rs](../src/vm/state_domains/parallel.rs) 在控制线程授予完整访问集合；冲突写入不能与读 grant 重叠。失败回收私有候选；发布前重查 revision、code、schema 与目录有效性，容量检查通过后在同一控制回合提交。取消不能发布半个事务。 |
| 域 outbox 批次 | [PendingBatch](../src/vm/state_domains/outbox.rs) 仅由 `finish_worker` 创建，断言 `targets` 为空；移动的是拥有字节、标量与原子预算引用。 | 带 Rc 的 Target 仅在控制线程 `into_local` 后 prepare。提交与取消由同一控制线程串行，未提交批次直接回收；不能把已 prepare 的本地 Batch 发送到 worker。 |

## 3. 资源与宿主入口

运行时资源准入先检查 [OwnedResource](../src/runtime/resource.rs) 的活跃状态和 busy 位，再核对私有释放回调。
类型名只说明可能使用哪种协议，不能证明一个具体 token 来自该协议。

| 资源／入口 | 身份、并发访问与退出依据 |
| --- | --- |
| TCP listener/stream/reader/writer | [tcp::transferable](../src/runtime/tcp.rs) 先比对内置 release，再检查共享计费且没有 Local 状态；`Splitting` 和活跃操作不允许移交。请求持有 ResourceLease、拥有字节及 IoRegistration。stop/Drop 先撤销事件、注销 registration，再释放 lease。split 两个方向通过共享 IoSource 锁协调；不能由普通 `&Tcp*` 借用获得跨 worker 权限。 |
| 共享 reactor | [reactor.rs](../src/runtime/reactor.rs) 的 source、token 路由及 waiter 表有锁；IoRegistration 保留 source。stop 同步删除 waiter/ready 路由并注销或更新内核兴趣；失败会 poison source，旧 token 不再有效。通知在锁外发送，任务 generation 保留运行中收到的唤醒。池关闭同时关闭 reactor，`wait_closed` 等 I/O driver 退出。 |
| VM、迁移、admission、directory、outbox、文件代理 | [control.rs](../src/runtime/parallel/control.rs) 的控制线程独占 Local VM／资源状态。可传输类型限于 `vm.Owner/MigrationOwner`、`admission.PoolOwner/PermitOwner`、`domain.DirectoryOwner`、`outbox.QueueOwner/MessageOwner`、`fs.FileOwner/OutcomeOwner`。代理核对私有 release 和同一 Core，字节输入复制；资源请求保留 Arc 身份，不传语言地址。Call Drop 先 stop 本地 waiter，再释放临时字节与 keep；ControlReceipt 把这段清理计入子任务屏障。控制树退出统一关闭剩余本地资源。迁移未提交候选随会话取消回收。 |
| 内置文件 worker | [worker.rs](../src/runtime/fs/worker.rs) 的动作只持有服务存储、路径／字节副本及线程安全句柄。取消按 queue → ticket → file 锁顺序撤销投递；已运行 I/O 可继续到后台收尾，文件关闭使用预留清理容量。语言帧退出不等待阻塞文件调用，也不会留下指向语言帧的地址；服务 `is_quiescent` 才表示后台真正结束。 |
| 外部文件服务 | [FileService::from_descriptor](../src/runtime/fs/service.rs) 是 unsafe。描述验证只检查版本、尺寸、保留位与必需回调，不能证明指针或宿主同步。ExternalTicket/ExternalFile 的锁串行管理单个 token，ResultGuard 保证未消费结果／文件 token 被释放；宿主仍须保证同 context 的并发回调、任意线程释放、后台独立保活和不重入。已在 [ABI 文档](../src/runtime/fs/abi.rs) 明确这一责任。 |
| DNS | [dns.rs](../src/runtime/dns.rs) 的后台 Job 拥有 hostname、port、ticket；结果由 polling task 复制并写其本地错误槽。取消去掉 waiter 和投递权限，正在运行的系统解析可以继续；其 permit 直到后台完成才归还。shutdown 不承诺同步中断系统解析，quiescent 才表示 resolver 线程结束。 |
| stdout | [Host::from_binding](../src/runtime/parallel/mod.rs) 对保留 stdout 核对完整 TextViewV2 签名及实际函数地址；同步调用返回后不保留借用字节。借用调用获准不等于用户 `Show` 的其他 Local 调用也获准。重载后任务继续使用冻结绑定。 |
| 用户宿主、资源工厂与链接库 | [register_parallel_suspending_fn](../src/vm/engine.rs) 是显式 unsafe 标量 CPS 契约：入口和依赖可在任意 worker 并发调用，owner 可在任意线程销毁；只用声明的标量槽保存进度，不保留 context/waker/stop hook。普通注册、同名替换、自定义资源工厂不自动获得此证据，也不能冒充内置 token。AOT 导入最终绑定与动态库保活由链接调用者保证。 |

“任务已停止”和“后台系统调用已返回”是不同条件：文件与 DNS 可以先撤销投递并清理语言状态，
随后让仅持有服务对象的后台调用完成。宿主卸载实现代码前仍必须满足自己的 quiescent／保活契约。

## 4. unsafe Send / Sync 完整清单

本次对仓库 Rust 文件检索得到 **9 个类型、13 个显式 impl**；下表覆盖全部命中。
自动派生 Send/Sync 的包、控制代理、冻结代码、锁状态已在上面沿使用路径检查。
本地 `CpsFrame` 的 `Rc<UnsafeCell<Arena>>` 未变为 Send；只在接收 worker 新建帧，不跨线程搬运帧。

| 类型 | impl | 人工证明所依赖的不变量 |
| --- | --- | --- |
| [Allocation](../src/runtime/parallel/storage.rs) | Send | 分配记录在 shard Mutex 内注册／移除；仅审计过的元数据 finalizer 可在最后释放线程执行，不能调用语言 Drop。payload 访问权限来自 owning/loan 协议，锁住登记表本身不授权并发访问。 |
| [OwnedBuffer](../src/runtime/parallel/mod.rs) | Send | 唯一 owning graph、同根安装、无 Local 元数据／用户清理；原 owner 只能消费一次。 |
| [Range](../src/runtime/parallel/scoped.rs) | Send | 受检来源及生命周期；不跨线程访问 Buffer/Rc/frame 元数据；共享叶子或合格资源身份由 scope/return guard 保留。 |
| [Root](../src/vm/state_domains/storage/parallel.rs) | Send + Sync | Arena 及其本地计费对象随封闭图整体移动；共享访问只读，写入需独有候选。grant 与 Arc 固定活跃读根，持久预算本身线程安全。 |
| [PendingBatch](../src/vm/state_domains/outbox.rs) | Send | 私有构造入口保证 targets 为空；跨线程阶段无 Rc Target。 |
| [External](../src/runtime/fs/abi.rs) | Send + Sync | unsafe 注册者保证 context、所有回调的线程安全与生命周期；运行时不能从 C 函数指针验证它。 |
| [ResultGuard](../src/runtime/fs/abi.rs) | Send | 宿主结果存储在消费／释放前有效，释放回调可在任意线程执行；析构释放剩余 token。 |
| [ExternalTicket](../src/runtime/fs/abi.rs) | Send + Sync | opaque token 仅传给受信宿主，操作状态受 Mutex 保护，stop 后清空 token；service 与 wake 独立保活。 |
| [ExternalFile](../src/runtime/fs/abi.rs) | Send + Sync | token、closing、pending 和 result 由 Mutex 保护；关闭／释放是一次性的，宿主自行保证后台工作不依赖已释放的前台对象。 |

## 5. 任务发布、取消和退出顺序

核对入口：[pool.rs](../src/runtime/parallel/pool.rs)、[ParallelExecution](../src/vm/parallel.rs)、
[native factory](../src/runtime/cps/parallel.rs)、[解释器 worker](../src/backends/interp/execute/parallel.rs)、
[本地 concurrent drain](../src/runtime/cps/concurrent.rs)。

1. 发送方构造拥有包／scope 输入和 Send factory；接收 worker 才创建非 Send Job、帧、借用头和本地解释器表。
   队列发布与关闭共用同步状态，未启动 factory 的丢弃仍承担其输入清理。
2. 任务失败或取消先请求所有后代停止。即使根 poll 已返回错误，`ParallelExecution` 也保留内部 job，
   直到后代 cleaned；本地 race/isolate 的恢复也必须先 drain 远端读者，不能提前析构来源。
3. 后代清理后在当前 worker 清理 job，并 drain portable disposal。根任务还需关闭控制树；
   `controls_pending == 0`、`cleanups_pending == 0` 和子树屏障全部满足后才发布 Cleaned/result。
4. race 在观察到所有 cleaned 后重新读取 winner，避免沿用之前的空快照而丢掉胜者；
   丢弃败者结果引起的析构错误仍参与失败裁决。此前竞态的修复已有确定性回归。
5. `request_close` 只发起停止；`wait_closed` join 执行 worker、控制 driver、I/O driver。
   宿主结果／任务句柄仍可能持有分配与代码。释放结果之后预算并非必须立即为零；
   应在持有者释放、池清理结束后检查归零。后台文件／DNS 的独立服务计费还需等待对应后台收尾。

预算方面，[Accounts](../src/runtime/parallel/storage.rs) 按地址排序并去重后锁定多个预算，
预留／调整要么全部成功要么全部失败；Lease 释放不分配。安装失败、未领取结果、未发布 factory、
取消及 pool close 都沿原责任链归还，不能用“根还活着”掩盖具体 owner 的重复释放或丢失。

## 6. 行为证据与本次验证范围

下面是按责任选取的已有回归入口。测试名称用于定位与将来定向重验，**不是声称本次重新运行了这些测试**。
本次无执行代码修改，复用路线[第 13 节](thread-safety-roadmap.md#13-s6-本轮审查与合并验证)的本机 Debug 验收；
其完整 `check.sh` 日志末尾为 `Checks passed (quick, Debug)`，后续测试细化已单独验证。

| 责任 | 已有测试证据 |
| --- | --- |
| 存储移交、预算、三后端行为 | [task_parallel_storage.rs](../src/backends/aot/tests/task_parallel_storage.rs)：`task_parallel_aggregate_values_and_roots_on_all_backends`、Array/List/Map/FixedArray 同组；[task_parallel_probe.rs](../src/backends/aot/tests/task_parallel_probe.rs)：`task_parallel_probe_generated_children_overlap_and_drain_on_all_backends`、`task_parallel_probe_root_retention_and_competing_budget_reservations`。 |
| 旧代码保活、宿主证据、未启动清理 | [parallel/tests.rs](../src/vm/parallel/tests.rs)：`parallel_frozen_tasks_keep_code_and_owning_inputs_after_vm_drop`、`parallel_host_admission_is_explicit_and_frozen_across_replacement`、`parallel_failure_budget_and_unstarted_packets_refund_once`、`parallel_pending_cancellation_cleans_on_the_worker_and_releases_code_last`。 |
| 闭包、dyn 与 Drop | [closure_tests.rs](../src/vm/parallel/closure_tests.rs)：`parallel_owned_closures_freeze_transfer_and_cancel`；[dyn_tests.rs](../src/vm/parallel/dyn_tests.rs)：`owned_dyn_values_preserve_storage_dispatch_and_frozen_code`、`owned_dyn_rejects_unqualified_methods_destructors_and_host_packets`；[drop_tests.rs](../src/vm/parallel/drop_tests.rs)：`parallel_drop_values_transfer_return_cancel_and_retain_code`（失败、取消、无人领取、析构报错且恰好一次）。JIT／解释器覆盖，另有 CLI 独立 AOT 入口。 |
| 只读来源、恢复屏障与预算失败 | [drop_tests.rs](../src/vm/parallel/drop_tests.rs)：`scoped_borrow_failed_worker_root_defers_destructors_until_children_stop`、`scoped_borrow_readers_drain_before_source_drop_on_failure_cancel_and_close`、`scoped_borrow_local_race_and_isolate_wait_for_remote_readers`；[scoped_tests.rs](../src/vm/parallel/scoped_tests.rs)：`scoped_borrow_unpublished_dynamic_inputs_finish_cleanup_before_recovery`、`scoped_borrow_budget_boundaries`；[scoped.rs](../src/runtime/parallel/scoped.rs)：`scoped_borrow_plain_provenance_rejects_foreign_wide_and_expired_sources`。AOT storage 同组覆盖只读容器、枚举、间接调用和自定义 Drop。 |
| 域 grant、事务与 outbox | [state_domains/parallel/tests.rs](../src/vm/state_domains/parallel/tests.rs)：`parallel_domains_grant_complete_sets_and_share_read_roots_without_copying`、`parallel_domains_fifo_cancel_close_and_publication_gate`、`parallel_domains_staged_creation_is_invisible_until_commit_and_reclaims_cancelled_timers`、`parallel_domains_allocation_boundaries_preserve_live_roots`。 |
| 控制代理、迁移与 Mailbox | [control_tests.rs](../src/vm/parallel/control_tests.rs)：`parallel_control_cancel_waits_for_local_frames_and_pool_remains_usable`、`scoped_borrow_vm_graphs_share_identity_and_drain_on_cancel`、`scoped_borrow_control_allocation_failures_refund_partial_replies`、`exclusive_mailbox_loans_return_after_receives_failure_and_cancel`；[exclusive.rs](../src/runtime/parallel/control/exclusive.rs)：`exclusive_mailbox_permission_rejects_aliases_and_returns_without_budget`；[migration_tests.rs](../src/vm/parallel/migration_tests.rs)：`migration_sessions_cancel_partial_candidates_and_close_the_control_driver`。 |
| 网络与后台服务 | [tcp_tests.rs](../src/runtime/tcp_tests.rs)：`parallel_tcp_packets_require_builtin_shared_quiescent_owners`；[reactor_tests.rs](../src/runtime/reactor_tests.rs)：`shared_reactor_transfers_live_tcp_directions_without_changing_poll`、`shared_reactor_does_not_lose_an_edge_during_registration_publication`；[fs/worker_tests.rs](../src/runtime/fs/worker_tests.rs)：`running_read_cancellation_closes_without_waiting`、`ready_unclaimed_open_is_closed_and_result_storage_is_charged`；[fs_tests.rs](../src/vm/parallel/fs_tests.rs)：`scoped_borrow_file_cancel_leaves_running_io_in_service_storage`；[dns_tests.rs](../src/runtime/dns_tests.rs)：`net6_dns_cancel_reload_drop_and_shutdown_do_not_wait_for_resolver`。 |
| 调度竞态与真实独立 AOT | [pool.rs](../src/runtime/parallel/pool.rs)：`task_pool_race_completion_between_snapshot_and_drain_keeps_winner`、`task_pool_running_notification_survives_pending_and_late_wake_is_inert`、`task_pool_publication_races_close_without_duplicate_cleanup`；[CLI tasks](../tests/cli/tasks.rs)：control bridge、domain transactions、owning closure、Drop、dyn、TCP handoff 的独立链接启动测试。control bridge 的 6 个 fixture 和 dyn 都以同一产物运行 1／3 worker；并非把嵌入式 Native 测试当独立 AOT。 |

补充沿用的专项验证为 Mailbox 分配失败穷举及 chat 实网矩阵，详见路线第 13 节；
完整默认测试不包含所有 ignored 穷举。chat 的 8 组验证包含两种脚本 VM、宿主 JIT／解释器、
AOT 1／3 worker、重载与关闭。当前顶层及收发 race 为 runtime-check，部分本地节点仍符合预期。

本次只新增审计记录、导航、状态及普通文档注释：检查链接、测试符号对应关系和 `git diff --check`，
不运行 Cargo 或重复整套检查。没有新增 Release、完整 stress、TSan、Miri、其他操作系统或硬件内存模型验证；
也没有验证第三方宿主实现是否兑现 unsafe 契约。这些不应被描述成“全部通过”。

## 7. 保留限制与后续变更触发条件

- TCP／BufferedTcpReader 的跨 worker 借用继续受限。缓冲读取消可能消费部分前缀，不能套用 Mailbox 的纯身份归还协议；
  开放前需定义状态独占、读进度和取消语义。拥有型 TCP 移交已有独立协议。
- 普通可变聚合、共享 Mailbox、接收 Mailbox 的再次跨 worker 独占借用、借用 dyn／不封闭回调目标仍不开放；
  安全的本地执行继续可用。判定须先检查整组，不允许半组移交后才无协议回退。
- 通用宿主引用接口、自定义资源工厂、任意 native 指针和动态库不能靠名字或类型自动升级。
  新协议须补实例来源、同步、停止、代码保活和失败清理的证据，随后再调整编译准入。
- 本阶段目标不包含运行中动态增减 worker、任意程序都能并行或保证取消系统阻塞调用。
  后续新能力可以另立阶段；本次 S6 清单和上述明确限制完成当前路线的收敛要求。
