# Runtime 开发注意事项

面向修改 native provider、managed value、调度器和 continuation 的贡献者。
本文依据 2026-09-16 的实现及 `a4d6ceb` 并发修复整理，区分已经实现的行为、
开发时必须维护的约定，以及尚待验证的边界。测试通过次数不构成并发正确性证明。

## 1. 先确认代码边界与执行线程

`joky/env` 的 native provider 位于 [`env`](../../crates/joky-runtime/src/runtime/env.rs)。
它在 `RuntimeScope` 创建时捕获环境变量和参数快照；`get`、`vars`、`args` 不读取进程全局
缓存，callback scope 继承父 scope 的快照。所有外部字符串都必须是合法 UTF-8，无法表示
的环境值或参数会使对应调用返回 `Err`。示例见 [`examples/io/env.jk`](../../examples/io/env.jk)。

运行时实现位于 `crates/joky-runtime/src/runtime/`，宿主接口位于
`crates/joky-runtime/src/host.rs`；共享 ABI 定义位于 `crates/joky-runtime-abi/`。
编译器负责类型、布局、operation ID 和生成代码，provider 负责执行操作及资源生命周期。
不要让 runtime 重新依赖编译器的语义类型，也不要在 JIT、AOT launcher 中各实现一套 I/O。
修改 provider 接口时同时检查 host 注册入口、operation 顺序与 mask、ABI 和两种执行路径。

socket 与 file 都使用 managed `NativeHandle`，但 I/O 执行方式不同：

| 执行位置 | 当前职责 | 开发约束 |
| --- | --- | --- |
| 调用 provider start 的线程 | 校验 ABI、准备参数、保留资源、注册请求；也可能内联完成 | 不等队列空位，不等待远期 I/O；start 返回前完成也是合法时序 |
| Reactor | mio 就绪事件、非阻塞 socket read/write/accept、timer 回调 | 回调应短且不阻塞；`WouldBlock` 表示继续等待，不是操作失败 |
| Worker pool | 执行 Joky task 和通常的 continuation 恢复 | 避免阻塞 syscall；等待依赖调度队列时遵守现有帮助执行机制 |
| Blocking pool | file syscall、文件资源自动关闭 | 队列中请求可取消，已经开始的 syscall 可能无法中断 |
| 外部回调线程 | 通过 callback scope 驱动自己的恢复队列 | 不假设所有恢复都在普通 worker 上执行 |

所以不能简单把 socket 称为“worker pool I/O”。当前 socket readiness callback 在
reactor 上运行，发布结果后才把恢复工作交给 scope 的调度队列。
另一个现存边界是 socket `connect/listen` 等 start 路径使用同步 `to_socket_addrs`；
主机名解析可能阻塞，不能据此宣称整个 socket 路径都非阻塞。

实现入口：[socket](../../crates/joky-runtime/src/runtime/socket.rs)、
[reactor](../../crates/joky-runtime/src/runtime/reactor.rs)、
[blocking pool](../../crates/joky-runtime/src/runtime/blocking.rs)、
[scope](../../crates/joky-runtime/src/runtime/scope.rs)。

## 2. 把三种生命周期分开

| 对象 | 表示及所有权 | 不能做的事 |
| --- | --- | --- |
| 语言层 native handle | managed payload 的真实地址，通常唯一所有权，使用 `jk_alloc_native_handle` 和 drop callback | 不可通过复制裸指针取得第二份所有权，不可用 `jk_dup` 复制 owned handle |
| OS 资源 | file 内部 `Arc<Resource>`；socket 表内及在途请求持有的 `Arc<Mutex<...>>` | 不能因语言 handle 已消费就假定 syscall 或 reactor 回调已结束 |
| Continuation handle | 指针宽度的不透明整数 token，通过 registry 保留内部 `Arc` | 不可解引用、做地址运算，或当成 managed payload 交给 `jk_drop` |

file 的语言 handle 内含 magic 和 `Arc<Resource>`，资源 lookup 校验类型、布局与 scope。
socket 的语言 handle 内含 socket id，资源表再校验 scope 和 socket 种类。
后台请求应保留 Rust 资源引用；这让语言 handle 消费后仍能安全完成在途工作，
却不会赋予语言代码复制唯一资源的能力。

必须区分“资源表删除可以重复调用”和“释放 managed 指针可以重复调用”。
前者可以实现幂等，后者是 double free/use-after-free。
`valid_header` 及测试用 live-payload 表不提供悬空指针保护：地址一旦被 allocator 复用，
旧指针可能通过新对象的检查。把地址转成 `usize` 跨线程也没有延长对象生命周期。
Continuation 的不复用 token 协议不能直接套用到 managed 地址或 socket id 上。

参考：[资源生命周期与句柄协议](resource-lifetimes.md)、
[managed](../../crates/joky-runtime/src/runtime/managed.rs)、
[socket resource 表](../../crates/joky-runtime/src/runtime/socket/resources.rs)、
[file handle](../../crates/joky-runtime/src/runtime/file/handles.rs)。

## 3. 每个 operation 先写所有权表

增加或修改 operation 时，在实现注释或设计说明中列明以下路径的责任，不能仅写“失败时清理”：

| 路径 | 需要回答的问题 |
| --- | --- |
| ABI 校验失败，start 返回 0 | 是否尚未消费参数？局部资源是否全部回收？是否仍有回调可能到达？ |
| start 返回 1，发布业务 `Err` | 谁负责输入 handle？业务失败是否已发生不可逆副作用？ |
| 等待准入或就绪 | 哪个对象保活参数、OS 资源、continuation 和 busy reservation？ |
| 取消先于注册 | 之后到达的注册如何观察持久取消标记并撤销？ |
| 排队取消 / 执行中取消 | 谁移除请求，谁释放 captures，谁处理晚到结果？ |
| 完成被接受 / 被拒绝 | payload 中的 String、Bytes、native handle 分别由谁释放？ |
| 显式 close / 自动 drop / scope 关闭 | 谁撤销 pending I/O，谁最终关闭 OS 资源，何时可释放生成代码？ |

start 返回值描述请求是否被接纳，不等同于业务 `Result` 成功，也不等同于仍在挂起。
已经发布 `Err` 的请求可以返回 1。不要用一个 bool 同时推断资源所有权和业务结果。

### Close 的两层契约

语言层 [`File.close(self)`](../../std/joky/file.jk) 消费绑定，无论返回 Ok 还是 Err，
原绑定都不能再次使用。socket 的消费语义同样应以标准库声明和生成代码为准。
底层 provider 什么时候释放 managed wrapper 是另一项契约，不能从方法名推断。

`a4d6ceb` 后，file provider 先取得 busy reservation，成功后才 `jk_drop(pointer)`；
busy 分支保留 wrapper，底层 Rust 测试随后负责 drop。该行为修复了测试中的悬空指针，
**不代表 Joky 源码可以在 close 返回 Err 后重新使用 File**。

当前需要继续核对的边界：语言 close 已消费绑定而底层 busy 拒绝保留 wrapper 时，
生成代码/参数 cleanup 是否确保恰好一次回收。现有底层测试的显式 drop 不能证明这条
端到端路径没有泄漏。新增 consuming operation 必须把这种拒绝路径一起设计和验证。
socket close 当前移除资源表项并终止 pending I/O，并不在该 hook 内直接 drop wrapper；
不能照抄 file close 的释放位置。

## 4. Provider 的参数与结果交接

1. 先检查 arguments/result 的字节数、类型、布局与 scope，再解码。flattened ABI
   不能直接依赖普通 Rust enum 布局；按既有 ABI 使用 unaligned 访问及溢出检查。
2. start 返回前，为后台工作建立独立的参数所有权。socket write 当前复制到 Rust
   buffer；file 的 SharedPath/SharedData 保留共享 managed 引用及资源计数。
   不能把 continuation 参数 slot 或临时栈内存的裸借用留给后台线程。
3. 异步期间保留 `Continuation::retain_registered` 返回的状态和所需资源。
   `Arc<ContinuationInner>` 只保活状态对象，不保证 frame/result buffer 未被取消释放。
   不缓存 `_result` 指针供后台直接写入；通过 completion 入口校验并提交结果。
4. completion 返回 true 后，payload 中的 owned 字段已经交给接收方；provider 不再 drop。
   返回 false 时，未交接的字段仍由 provider 释放。字节数组/Vec 的析构不会释放其中
   用整数表示的 managed 指针。错误 String、open/accept 的新 handle 也遵守此规则。
5. 调用 completion 本身可能释放参数存储并排队恢复用户代码。调用前必须停止借用
   参数 slot，并把 busy reservation 等下一次操作需要的状态收尾；调用后不可继续
   使用已转交的 payload。手写 ABI 测试还需自己建立 cleanup 或取走并清空结果。

每个请求要有明确的完成竞争者和唯一结果交接点。不要把 token/state 校验理解为
“任意重复 completion 都安全”；同一 continuation 可能重挂起到同一个 operation，
迟到回调仍需请求级的一次性完成/注销协议。

参考：[provider 生命周期](../../crates/joky-runtime/src/runtime/continuation/lifecycle.rs)、
[结果交接](../../crates/joky-runtime/src/runtime/continuation/mod.rs)、
[cleanup 存储](../../crates/joky-runtime/src/runtime/continuation/storage.rs)。

## 5. 取消、终态和排空是三个事件

取消请求表达“不再继续用户计算”，不会回滚 OS 副作用，也不保证所有后台工作已退出。
`Cancelled/Completed` 状态发布、callback 退出、存储释放、最后一个资源引用释放可能
发生在不同时间。只检查终态或只等待 timer 都不足以证明 cleanup 完成。

本次修复增加 `cancellation_draining` 活跃计数：`cancel()` 进入/退出清理范围时增减，
`wait_for_idle` 与 `finish_free` 将它纳入排空条件。它不串行化所有 cancel，
也不是可以删除既有 callback/native/timer/child/task-wait 检查的通用屏障。
修改任一终态路径时，都要审计谁保活存储、谁最后触发退役，以及计数与通知的时序。

| 时序 | 必须维护的行为 |
| --- | --- |
| socket 就绪早于 pending 表插入 | `remember_pending` 插入后复核请求是否仍挂起，清除过期项 |
| socket 取消早于注册完成 | 保留取消状态，补偿撤销 reactor 请求，不残留 pending 记录 |
| socket 显式关闭时还有 read/write | 撤销就绪注册，并以 `socket closed` 完成等待者，不能只删除资源表 |
| file 排队时取消 | 从 ready/waiting 队列移除；在队列锁外释放 captures 和 reservation |
| file syscall 执行中取消 | 允许 syscall 持资源完成，拒绝晚到结果并回收；不宣称操作未发生 |
| 回调内部 free 自己 | 不能等待自己的 callback lease，延迟到退出后退役 |
| 子 activation 异步 detach | 必须保留父取消重试机会；不能用“正在取消”标记简单跳过重试 |

`blocking::submit` 在队列锁内检查持久取消标记，避免 cancel 先查空队列、submit 后插入
的丢失取消。沿用这个握手，不要拆成无同步的“先检查 flag，再入队”。
file reservation 覆盖准入、排队与 syscall，发布结果前释放；自动关闭任务不可取消。
ready queue 有容量限制，但 waiting queue 和调用方保留的数据并不因此拥有总内存上限。

## 6. 锁、析构与 scope

- 更新共享表时先取出待清理项，再释放表锁，最后执行 drop/cancel/completion。
  析构可能再次提交 blocking cleanup 或访问 registry，属于会重入 runtime 的代码。
- 保持已有锁序，例如 function-call 路径先锁 continuation state，再锁 function-call phase。
  新增锁时检查取消、回调、free、父子遍历四个方向；不能只审视单个函数。
- 不跨用户回调、provider hook、子任务等待持有粗粒度终态锁；不要在 reactor 上等待
  自己需要处理的事件，也不要在 callback 内等待自己的 lease 排空。
- 持锁期间删掉冗余的取消重试，或仅靠 flag 跳过重试，都可能产生“最后一次唤醒丢失”。
  必须能说明清理者退出与新工作到达交叉时由谁继续推进。
- provider 注册与资源 lookup 使用正确的 scope。后台线程不会自动继承创建者 TLS；
  在需要 scope 归属的工作中进入被保留的 scope，不用默认 scope 替代请求 scope。
- 使用 `ProviderScope`/host registration guard 管理注册生命期；关闭 scope、排空允许
  执行用户代码的工作，再撤销注册和释放 JIT 代码。检查 cleanup callback 是否也是 JIT
  地址，不能只统计 machine entry。不可中断 file syscall 的最终资源回收需要另外观察。

本次排查曾尝试覆盖终态路径的大锁及跳过重复取消的 bool，两者都在压力测试中出现
挂起，已撤销。它们是反例，不是建议沿用的修复模板。

## 7. 并发测试与排查流程

先证明 ownership 与时序，再用压力测试扩大覆盖。不要通过给全部测试加锁或单次重跑
通过来处理未知失败；共享静态测试计数才需要对应的隔离锁。

| 场景 | 应验证的结果 |
| --- | --- |
| inline completion 与 native Pending 交接 | start 内完成也只能在合法交接后恢复，无丢失或重复恢复 |
| 取消与注册 / 完成相撞 | 没有残留 pending，无重复 payload drop，无取消后恢复 |
| busy close 拒绝 | 按底层约定立即检查 wrapper 存活及最终回收，不等地址复用才发现错误 |
| 慢 syscall 持锁或执行中 | 取消可返回，在途资源仍活；放行后资源最终归零 |
| 显式 close 与 pending socket I/O | 所有等待者结束，请求表与 reactor 注册最终清除 |
| 重复 free、迟到 token、跨 scope 操作 | 不碰到新对象，不串入其他 scope；不向 freed managed 指针做类似测试 |
| JIT 和 AOT | 相同结果与所有权；有选择性 provider 注册的程序也能运行 |

用 barrier/channel/受控锁建立竞争点，用带截止时间的等待检查结果；sleep 只可辅助调度，
不能证明另一线程进入了指定阶段。自行 spawn 的线程要 join，callback 要等待覆盖其
实际工作的排空条件。测试失败路径也要能释放 gate，避免 panic 后把公共 worker 永久堵住。
scope 的 managed/resource 计数优先于进程全局计数；使用进程全局计数或占满公共 pool
的测试应独立运行。后台 OS 资源的最终释放不能仅靠 continuation 终态推断。

TaskGroup 的 Drop 等待任务完成，不自动取消。使用栈上 context / captures 的测试，
应在断言失败时先请求取消并排空原生 invocation，再关闭 group 和释放借用存储。
固定次数的 yield 不能证明任务已启动，Sleeping 也不能证明原生调用栈已退出；
旧挂起入口测试可参考 task/tests.rs 的 TestTask guard 与 cfg(test) 完成通知。
相关故障证据见[2026-09-25 修复记录](../archive/reports/runtime-test-fixes-2026-09-25.md)。

### 本次故障的因果链

旧 file busy-close 路径提前 drop wrapper -> 测试仍持旧地址 -> allocator 将地址分给
cown 或其他 managed 对象 -> file 测试再次 drop -> 新对象引用计数受损 -> 无关测试失败。

定点记录目标 cown 最后一次 drop 的调用栈，才确认来源为 file handle 测试。
“去掉 continuation 测试后暂时不复现”只改变了调度及分配时序，不能证明根因位于
continuation。修复后增加直接检查 handle 有效性的断言，使回归不依赖地址碰巧复用。
另外两项 continuation 失败涉及断言早于 callback/取消清理排空，应分别分析。

建议排查顺序：保留第一次失败和调用栈；对照单测、完整并行、串行；跟踪谁持有与消费
每一份引用；在实际释放点做有限诊断；写确定性回归；移除诊断后再运行完整并行压力测试。
测试超过正常耗时应检查具体卡住项与线程栈，不能不断等待并把“未返回”算作通过。

### 验证命令

从仓库根目录运行；runtime 是独立 manifest，根目录 `cargo test` 不能替代其单元测试。

```sh
cargo test --manifest-path crates/joky-runtime/Cargo.toml
cargo check --manifest-path crates/joky-runtime/Cargo.toml --all-targets
cargo check --manifest-path crates/joky-runtime/Cargo.toml --features test-support
cargo test --quiet
cargo test --quiet --test cli
```

并发/所有权改动另跑完整 runtime 套件压力验证；一旦失败就保留日志并停止，不覆盖失败：

```sh
runtime_stress_log=$(mktemp /tmp/joky-runtime-stress.XXXXXX)
(
for iteration in $(seq 1 300); do
    if ! cargo test --manifest-path crates/joky-runtime/Cargo.toml --quiet >"$runtime_stress_log" 2>&1; then
        printf 'Failed at iteration %s; log: %s\n' "$iteration" "$runtime_stress_log"
        tail -n 100 "$runtime_stress_log"
        exit 1
    fi
    printf 'Passed %s/300\n' "$iteration"
done
)
```

该循环返回失败退出码并保留日志；放进 CI 时还需给每轮测试设置超时。
pool 饱和测试按现有 ignore 注释单独运行：

```sh
cargo test --manifest-path crates/joky-runtime/Cargo.toml \
  runtime::blocking::tests::file_backpressure_cancels_queued_requests_while_workers_are_occupied \
  -- --ignored --exact
```

`a4d6ceb` 当时的验证记录：完整 runtime 并行套件连续 300 轮通过，根库测试
859 passed / 9 ignored，CLI 84 passed，runtime 普通与 test-support 检查无警告。
这是该提交的验证记录，不是之后版本的测试结果。

## 8. 提交前核对

- [ ] 输入、输出及 close 的消费点明确；所有拒绝、取消和晚到结果都有唯一清理责任。
- [ ] 后台 captures 自己保活参数及资源，没有缓存可能失效的 ABI buffer 地址。
- [ ] start 内完成、取消早于注册、native 交接前取消均有合法推进路径。
- [ ] 没有锁内重入析构、自等待或丢失最后一次取消重试。
- [ ] terminal state 与 drain 分开验证，scope 资源归属和 JIT drop glue 生命周期正确。
- [ ] 确定性回归和适当的完整并行测试通过；日志中未解决的失败如实记录。
- [ ] ABI、标准库契约、JIT/AOT 注册与文档相互一致；差异明确列出，未写成已保证行为。

相关文档：[文件 I/O](../stdlib/file-io.md)、[资源生命周期](resource-lifetimes.md)、
[函数 Pending ABI](function-pending-abi.md)、[有界迭代](bounded-iteration.md)。
