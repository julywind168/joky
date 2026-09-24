# 阶段 4：语义完备性、并发稳健性与发布验收

> 历史记录：本文保留原阶段的设计、环境与验收结论，不代表当前能力。归档于 2026-09-24；当前说明见[文档入口](../../README.md)，未完成工作见[计划索引](../../plans/README.md)。

> 状态：功能实现已完成，本地验收缺口由 [阶段 5.0](../../plans/resource-control.md)
> 收尾，最新证据见 [5.0 验收记录](../reports/phase5-0.md)。目前没有远端 CI，
> 不以 workflow 文件存在宣称远端通过。历史压力 SIGSEGV 的根因仍未确认；
> 大源码编译期栈溢出移入 5.4，D1–D4 的长期扩展按决策暂缓。
> 前置：阶段 0–3 已完成（见同目录各文件）。无栈恢复能力已闭环——
> 同步回退删除、machine entry 全覆盖、native-frame 独立恢复、
> Pending ABI 统一，性能基准已建立。
>
> 本计划吸收并取代 `docs/lang/4-todo.md`（已删除）。该清单约 85% 条目
> 已完成；经审计，"未勾选但实际已实现"的条目（`when` 语法/AST/检查、
> scope/branch/race 结构化并发、Cown runtime 与 lease verifier、effect
> 检查链等）均有实现和测试佐证，不再重复罗列。下方仅收录经核实的
> 真实遗留项与阶段 3 实践中新发现的问题。

## 4.1 语义缺口收尾

先审计、后实现：每一项先核实现有实现/测试覆盖，再决定补实现或
补测试证明已覆盖。

- [x] 一次性 resumable effect 的 continuation 重复恢复约束：
      确认 sema/runtime 哪一层已强制"只能恢复一次"（runtime token 已
      隔离），缺的是 sema 诊断还是测试证明。
      审计结论（2026-09-10）：约束由三层共同强制，sema 无诊断是设计使然——
      语言层面 `resume(payload)` 只是 handler arm 的终值语法
      （`legacy_resume_payload`），无法表达第二次恢复，非终位的
      `resume(...)` 由 sema 以 `unknown function 'resume'` 拒绝；MIR
      verifier 强制每个 continuation 恰好一个 suspend/resume 标记；
      runtime 以 CAS 单次标志拒绝二次恢复
      （`a_frame_accepts_exactly_one_resumption_payload`、
      `independent_resumption_handles_are_each_one_shot`）。新增
      `a_resumable_arm_can_express_at_most_one_resume` 固化语言层证据。
- [x] handler 继承与隔离：branch 内的 handler frame 继承规则说明，
      以及"每个 branch 使用独立 handler frame"的 verifier/测试证据。
      继承规则（2026-09-10 记录）：branch 降为独立任务函数，spawn 时快照
      当时的动态 handler 链（`TaskGroup::spawn` 存 `handler_frame`，worker
      执行前重新安装）；branch 内的 `do ... with` 在该快照链之上安装新
      frame，退出时恢复快照链，父链不受影响。继承已有
      `spawned_task_inherits_the_dynamic_handler_chain` 与
      `normal_effect_handler_frame_is_inherited_by_nested_parallel_body`
      证明；新增 `parallel_branches_get_isolated_handler_frames` 证明隔离：
      两个 branch 各自 frame 服务各自请求，arm 返回值互不串扰。
- [x] MIR verifier 补强（逐项核实现有覆盖后补缺）：
      - 任务值不跨越 scope 逃逸：已有 operation 级（scope 生命周期
        pass）+ borrowed 捕获拒绝；新增值级规则
        `verify_task_result_reads`——TaskJoin 目标值在对应 TaskClaimResult
        之前只能被 scope 持有，任何 CFG 路径上的提前读取（含 phi 边按
        前驱出边集判定）都被拒绝，负例
        `verifier_rejects_task_result_read_before_claim`；
      - owned value 唯一归属：已覆盖
        （`verify_ownership_flow`、`verify_local_ownership_flow`、
        `verify_aggregate_move_protocol` 的 use-after-move/重复初始化/
        部分 move 检查，以及 continuation spill/frame 的 Owned/Shared
        约束），无需新增；
      - race winner/loser 清理路径的 verifier 规则：已覆盖
        （race 任务禁止 join/claim/cancel，RaceSelect 单步完成 winner
        转移与 loser 清理，`verify_race_tasks` 校验同 scope 同类型）。
- [x] runtime 所有权与竞争测试：完成、失败、取消及重复完成竞争中，
      continuation、payload 和 race winner/loser 的值只转移或清理一次；
      已转移的值由接收方负责最终清理。MIR verifier 证明静态路径约束，
      runtime 测试证明动态竞争下 cleanup 责任唯一，两类证据分别记录。
      文件池饱和测试以全线程对象登记数验证资源回收，并发现/修复普通
      suspend 入口对 shared 参数的重复 retain 泄漏；MIR 已准备的参数
      份额直接交给 payload cleanup，调用方保留的值仍可在恢复后使用。
      新增证据（2026-09-10）：
      `cancellation_racing_completion_cleans_a_written_result_exactly_once`
      （取消与完成竞争中已写入结果恰被清理一次）、
      `external_completion_racing_cancellation_reaches_one_terminal_state`
      （外部完成与取消竞争只产生一个终态）、
      `shared_suspension_argument_survives_resume_without_leaking`
      （共享参数挂起后调用方与被调方双端可用、无泄漏）。
      审计中另发现并修复两个相关缺陷：(a) HIR task 捕获收集缺少
      Match 分支，match 模式绑定的变量传入嵌套 `race`/`parallel` arm
      时被静默丢弃（`collect_tasks_in_expr` 补 Match 用例）；
      (b) continuation cleanup 组件表遗漏 `Bytes`/`MutBytes`，挂起
      provider 的 `Bytes` 参数份额在完成后无人释放、每次挂起泄漏一个
      managed object（回归测试
      `tcp_write_argument_share_is_released_after_the_suspend`）。
- [x] 禁止普通用户 handler 接管内部操作（task cancel、Cown release
      等会破坏结构化并发不变量的 operation）。
      审计结论（2026-09-10）：内部操作不是 Effect operation——它们没有
      operation id，不进入 operation registry，编译为直接 runtime 调用
      或 continuation cleanup，永远不经过 Handler frame 分发，因此用户
      handler 结构上无法拦截。新增
      `internal_operations_never_reach_a_user_handler_frame`：race 输家
      取消与 Cown release 都发生在用户 `do ... with` frame 内，程序正常
      完成；`docs/lang/effects.md` 已补充该规则说明。
- [x] `when` 与 branch/scope 的语法章节补全：`docs/lang/syntax.md`
      增补闭包、`when`、`scope`、`branch`、`race` 的现行语法（实现已在，
      文档滞后）。
      完成（2026-09-10）：审计发现闭包、`when`、`race`、`parallel` 已有
      章节且与实现一致；`scope` 不是用户关键字而是 runtime 边界，已在
      1-syntax.md 的结构化并发章节明确说明 scope 语义约束并补充
      `branch` 示例。同批修正 1-syntax.md 中 effect 声明示例（缺少
      `@suspends`/`fn`，解析器会拒绝）。

## 4.2 挂起语义边界决策

阶段 3 实践中固化的边界，需要产品决策而非直接实现：

- [x] **D1 · 挂起类方法跨 suspend 使用借用 self**：当前编译期拒绝
      （`cannot carry borrowed local`，借用 local 跨挂起是阶段 1 的
      设计决定）。托管指针表示本身不能证明借用跨挂起安全；决策点：
      (a) 在明确 retain/所有权提升规则后，将 receiver 保存到 frame，
      并验证原 owner 结束后恢复及完成/失败/取消时的生命周期与清理，
      或 (b) 保持拒绝但把
      诊断改为指向方法的可操作建议（把 self 读取移到挂起前）。
      参考：`looped_borrowed_method_receivers_are_not_spilled_across_suspends`
      测试仅证明挂起前读取 self 的用法可行；另补跨挂起使用 self 的
      最小用例，按决策验证支持行为或拒绝诊断。
      当前阶段决策：保持拒绝；verifier 诊断要求在 suspend 前读取或复制
      所需字段，避免未经验证的借用生命周期扩展。
      已补最小用例（2026-09-10）：
      `borrowed_method_receiver_used_across_a_suspend_is_rejected` 断言
      `self.base` 跨 `time.sleep` 使用时以
      `cannot carry borrowed local ... read or copy the value before
      suspend` 拒绝。
- [x] **D2 · machine-entry 返回类型缺口**：`supports_machine_return`
      仍拒绝 `Cown/Param/SelfType/Associated`。决策：支持（Cown 是
      capability 复制）或保持拒绝并给出用户友好诊断。当前阶段决策：
      保持拒绝；verifier 明确提示改用受支持返回类型，或移除挂起语义，
      避免在缺少完整 ownership ABI 前放行这些类型。
      已补测试（2026-09-10）：
      `verifier_rejects_machine_entry_unrepresentable_return_types`
      对 `Cown/Param/SelfType` 三类返回逐一断言 machine-entry ABI 拒绝
      诊断（端到端层面 Cown 返回由 sema 的 ownership 检查先行拒绝，
      Param 在实例化后变为具体类型，故在 MIR 层验证）。
- [x] **D3 · 移除 `is_time_sleep_operation` 特判**：timer fast path
      改为 provider capability 声明，删除最后一处 operation identity
      特判（三处调用点：statements/continuations.rs、entry.rs、
      statements/mod.rs 测试）。当前阶段决策：暂不移除；timer fast path
      仍需要把 `Duration` 参数直接映射为 reactor timer，通用 provider
      ABI 尚未声明该 capability。后续改造必须先增加 provider capability
      字段与注册/查询 API，再删除 identity 检查，并保留 custom-duration
      的 payload 回归测试。
      决策记录完整（含不做理由与前置条件）；custom-duration payload
      回归测试已存在（`starts_suspend_with_a_custom_duration_payload`，
      `src/compiler/tests/resumable.rs`），identity 单测位于
      `codegen/functions/statements/mod.rs`。
- [x] **D4 · 挂起递归的 parent 链内存**：每层未完成激活保留一条
      parent 链接（fail 传播语义），深递归程序占 O(深度)。评估：链上
      已完成激活能否提前解除，或链接改为可压缩结构。已实现终态解绑：
      子 activation 完成/取消时从父 `function_children` 移除；新增
      `completed_function_child_is_removed_from_parent_activation_list`
      回归测试。parent raw link 仍用于 failure 传播，未完成链的 O(深度)
      保留符合当前语义；长期压缩结构仍待性能评估。

## 4.3 性能与并发稳健性

- [x] 审核问题 1：少量 worker 下嵌套任务等待死锁（2026-09-10 修正）。
      `parallel`、`race` 和有子任务的 `branch` scope 退出使用内部
      `TaskWait` continuation，返回 Pending 释放 worker；不通过扩大
      scheduler 帮助递归深度解决。`race` 选择完成分支后取消并排空其他
      分支，再转移结果；内部等待不经用户 effect handler。
      取消期间保留父帧直到子任务排空，并在原生调用交接后清理；外部
      完成回调持有任务组强引用直到终态通知结束，避免父任务提前释放
      任务组导致释放后访问。间接闭包调用统一 Pending ABI，恢复路径
      保存/退出 handler frame，未处理错误按 runtime scope 传回主线程。
      回归覆盖：1/2/4 worker 的三层 parallel、2000 层任务嵌套、嵌套
      取消、branch scope 排空、失败传播、间接闭包以及 handler 恢复。
      CLI 子进程均设 15 秒超时并在失败时终止/回收。运行时另有父帧
      延迟清理、原生交接前取消和旧激活脱链的确定性测试。
      最终验证：`cargo fmt --check`、855 个 lib 测试（6 个有意忽略）、
      66 个 CLI 测试通过；文件背压与 3 个套接字取消/泄漏测试分别在
      独立进程通过；嵌套恢复与嵌套取消用例分别额外重复 100 次通过。
      该次提交的取消压力测试 60 秒完成 1,136 轮，通过；当时未重跑 TSan，
      最新完整插桩复验见 5.0 记录。

- [x] release 基准基线：早期记录与口径已在 5.0 复核。`8d90b93`
      的 provider inline 为 7.45µs/suspend，四路为 18.35µs/suspend；
      `n` 是分摊到各分支的总挂起数，四路总吞吐约为单路的 **0.41 倍**。
      原来的“1.6 倍”重复乘以 4，应撤回；“每次迭代一次 join”及把差额
      归因于 join/同步开销也没有测量依据。旧 debug 扩展率不再作为基线。
      5.0 增加操作数校验、交替顺序的成对采样、有符号差分和中位数/范围
      报告；最新 release 数字、环境与复现命令见验收记录。
- [x] 本地压力测试入口：`python3 scripts/check-runtime.py` 运行串行库
      套件，随后分别在独立进程运行文件池、三个 socket 测试及 60 秒
      取消压力测试。每个隔离测试使用完整名称和 `--exact`，设置 180 秒
      进程超时，并要求恰好一个测试通过。`.github/workflows/stress.yml`
      复用同一入口；**当前尚未建立远端 CI，远端运行不在本次验收范围**。
      历史证据保留：2026-09-09 一次普通压力运行约 30 秒后 `SIGSEGV`；
      随后 lldb/普通运行分别完成 1265/1276 轮。2026-09-10 修复真实
      数据竞争后普通运行 1384 轮，任务等待修复后 1136 轮，均为 60 秒。
      这些通过记录不能证明历史崩溃与某次修复存在因果关系。
- [x] 并发检查工具评估及本地复验流程：采用 ThreadSanitizer。
      历史运行发现 `TaskContext.continuation_pending` 非同步 bool
      读写，已改为 AtomicBool（Acquire/Release）。早期 851 个库测试的
      断言通过，但进程存在 TSan 报告，不能记为 sanitizer 全绿。
      早期命令以 `-Cunsafe-allow-abi-mismatch=sanitizer` 混用预编译
      标准库；crossbeam-epoch TLS 与 reactor OnceLock 相关报告的
      “第三方真实竞争”归因不足，撤回直接建议抑制的结论。
      5.0 用 nightly + `-Zbuild-std` 一起插桩标准库，保留 sanitizer
      ABI 检查，不加抑制；压力测试的吞吐阈值与 sanitizer 退出状态
      分开判定。命令、结果及工具覆盖边界见 5.0 验收记录。
- [x] Cown 高竞争下的公平性、饥饿和调度策略测试。已新增
      `contended_cown_allows_every_waiter_to_make_progress`，验证 8 个
      并发 waiter 各完成 32 轮获取/释放；仍需在不同 worker 数和调度
      配置下补充长期饥饿观测与策略结论。
      已补充（2026-09-10）：
      `contended_cown_progresses_under_different_worker_counts`
      （tests/cli.rs，同一竞争程序在 `JOKY_WORKER_COUNT=1/2/4` 三个
      进程级 worker 配置下各自完成且结果一致——为此给调度器加了
      `JOKY_WORKER_COUNT` 环境覆盖，1..=64 截断，仅用于观测/调优）；
      `contended_cown_progresses_under_branch_oversubscription`
      （16 个 branch 一次性竞争同一 Cown，8 轮获取/释放，全部推进、
      无泄漏）。策略结论：Cown 等待者队列 + 单 worker 顺序执行下
      未见饥饿；worker 数从 1 到默认池均收敛。附注：64 branch × 8 轮
      的更大规模组合在编译期栈溢出（源程序规模问题，非运行时公平性
      问题），作为已知限制记录。
- [x] blocking pool 队列饱和与背压策略：默认改为异步等待准入。
      保留固定 worker 数和有界执行队列；`file.read` 饱和时保持 Pending，
      FIFO 接纳等待请求，不再返回内部队列已满的文件错误。等待请求
      仅保留共享路径引用，执行时才分配读缓冲区；等待任务的数量与
      保留参数仍需应用并发预算约束，不宣称总内存有硬上限。
      取消会立即移除等待/排队请求并回收位置；覆盖公平顺序、取消与
      注册竞争、捕获释放，以及析构在队列锁外执行。
      `file_backpressure_reaches_jit_callers_while_blocking_workers_are_occupied`
      在真实 blocking worker 全被占用时验证排队/等待请求的 scope
      取消、timer race 继续推进，以及 root/恢复入口的文件读取在释放
      容量后正常完成且无泄漏（保留原测试名供 CI 调用）。
      验证：857 个 lib 测试、66 个 CLI 测试通过；该饱和测试在
      1/2/4 CPU worker 配置下分别重复 20 次通过，格式检查通过。
      该测试独占全局池，必须单独运行（本地入口及 workflow 已配置）：

      ```bash
      cargo test --lib runtime::blocking::tests::file_backpressure_reaches_jit_callers_while_blocking_workers_are_occupied -- --ignored --exact --nocapture
      ```
- [x] file/socket provider 的取消语义：分别覆盖排队请求、执行中的
      blocking I/O 与 reactor 注册请求。明确 scope 取消后的退出时机、
      晚到结果的丢弃与资源清理责任；对无法中断的系统调用记录限制，
      不把丢弃结果等同于立即终止系统调用。
      文件路径已有取消前/执行中两类受控测试（`src/runtime/file.rs`），
      `docs/lang/effects.md` 已明确排队占位及不可中断 I/O 的退出/释放边界。
      socket 路径审计与验收完成（2026-09-10）：审计发现两个缺陷并修复
      ——(a) HIR task 捕获收集缺 Match 分支，match 绑定的 socket 无法
      传入嵌套 `race`/`parallel` arm（见 4.1）；(b) `Bytes`/`MutBytes`
      不在 continuation cleanup 组件表中，挂起 `write` 的参数份额每次
      泄漏一个 managed object（回归测试
      `tcp_write_argument_share_is_released_after_the_suspend`）。
      新增两类受控测试（`#[ignore]`，需隔离单线程进程运行，已接入
      stress.yml）：`tcp_read_cancelled_while_queued_discards_the_late_payload`
      （排队读取在数据到达前被 race 取消，晚到的 payload 被丢弃、
      不恢复用户代码、无泄漏）与
      `tcp_read_completing_racing_cancellation_keeps_one_winner`
      （两条连接的读取都在途时数据到达，输家的完成与取消竞争，恰好
      一个 winner、无泄漏）。`docs/lang/effects.md` 已补充 socket 取消
      语义：cancel hook 注销 reactor 注册、generation/状态检查保证
      结果只接收或丢弃一次、provider 在挂起开始时复制全部所需数据、
      socket 为非阻塞 I/O 无不可中断阶段、scope 退出时关闭并注销全部
      provider 资源。

## 4.4 发布验收

- [x] 示例补齐：scope + 多 branch 并行计算、race 与取消、Cown counter、
      `do ... with` 测试替身（`examples/networking/echo_server.jk` 已可作为 I/O 挂起示例）。
      完成（2026-09-10）：新增 `examples/concurrency/parallel_compute.jk`（4 个 arm
      并行计算并在 join 后聚合）与 `examples/concurrency/race_timeout.jk`（race +
      `time.sleep` 慢 arm 被取消，winner 正常返回）；Cown counter 由
      `examples/concurrency/cown_when.jk` 覆盖，`do ... with` 测试替身由
      `examples/effects/handlers.jk` 覆盖，`examples/effects/resumable.jk` 覆盖 resumable
      恢复。全部 11 个有限示例逐一实跑验证通过（echo/socket 为常驻
      网络服务，由 tcp 集成测试覆盖）。
- [x] 全仓库搜索清除过时的 Actor/mailbox/`async/await` 表述。
      完成（2026-09-10）：全仓扫描后仅剩合法命中——设计文档中的
      否定性陈述（"Joky 不实现 Actor、mailbox 或 `async/await`"）与
      phase 计划文档中的历史对比；唯一的代码级过时表述
      （`src/mir/suspending_analysis.rs` 的 "async tasks" 注释）已改为
      结构化上下文（effect handlers、task branches、race arms）。
- [x] 文档一致性终检：并发模型、Effect 模型、类型命名与实现一致。
      完成（2026-09-10）：修正 `docs/lang/syntax.md` 的 effect 声明
      语法（补 `@suspends`/`fn`）；修正 `docs/lang/effects.md` 的
      `race`/`scope` 示例为实际的 `|` arm 语法、删除"worker-pool 调度
      仍在后续阶段"的过时状态说明、补充内部操作不可被 handler 拦截与
      socket 取消语义；`docs/plans/design-baseline.md` 的 Runtime ABI 列表改为
      实际符号（`jk_task_*`、`jk_continuation_*`、`jk_cown_*` 等）。
      类型命名（task/scope/branch/race/Cown/HandlerFrame/continuation）
      与实现导出一致。
- [x] `cargo fmt --check` 与 `cargo test` 全绿；无任务逃逸 scope、
      无 lease 跨挂起，owned value 保存、转移与最终清理责任唯一
      （复验 4.1 的 verifier 与 runtime 竞争测试证据）。
      任务等待与文件准入修复后的历史基线为 857 个库测试、66 个 CLI
      测试通过；6 个 ignored 测试需要另行运行（1 个基准、4 个资源
      隔离测试、1 个压力测试）。5.0 新增 MIR 回归后的最新数量及 TSan
      结论统一记录于 [5.0 验收记录](../reports/phase5-0.md)，避免旧的
      851/59 数量被误当成当前验收结果。

## 验收清单

- [x] 4.1 各语义缺口有实现或"已覆盖"的测试证据；
- [x] 4.2 的 D1–D4 每项有明确的决策记录（含不做理由）；
- [x] release 性能基线与运行环境入档，各场景结果经人工审阅，
      bench 的 managed object 无泄漏断言通过；
- [x] blocking pool 饱和时的任务推进与取消测试通过，file/socket
      provider 的取消边界及资源清理责任有文档和测试证据；
- [x] 压力/并发检查在 CI 或文档化的手动流程中可重复执行；
- [x] 示例与文档与实现一致。
