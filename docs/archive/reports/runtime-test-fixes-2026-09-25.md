# Runtime 并行测试故障修复（2026-09-25）

历史排查与验收记录，接续[随机数 provider 验证记录](random-runtime-2026-09-25.md)。
当前开发契约见[Runtime 开发注意事项](../../runtime/development.md)。

环境：ARM64 macOS 26.6.2（25G83），Rust 1.98.1（48a229cea）。
基线为 b6ce2fb，被测版本为该提交加本报告所在提交的修复。
原失败日志保留，未用后续成功结果覆盖历史记录。

## 1. Map / MutMap：测试重复释放已转移的 String key

mutable_map_round_trip 将 String key 交给 jk_mut_map_insert 后，所有权已转移给 map。
测试先释放 map（同时释放 key），再释放旧 key 指针，违反消费约定。
并行执行时 allocator 可将地址分给另一测试的对象；第二次释放会破坏新对象，
因此报错可能落在无关的 Map trie 查询或 MutMap 替换断言上。

定点诊断在 live-payload 登记中记录分配线程，在异常跨测试释放时打印调用栈。
记录捕获了 mutable_map_round_trip 对另一测试对象的释放，栈指向旧代码
mut_map.rs:808 的 jk_drop(key)。证据位于本地
target/runtime-fix/instrumented-trace/002.log。临时诊断代码已移除。

修复删除重复释放，注释说明 insert 的消费语义，并检查测试线程的 managed 对象
数量回到基线。没有修改 Map / MutMap 算法，也没有串行化测试。

## 2. 取消测试：过早断言后在析构中无限等待

只修复重复释放后，完整 runtime 压力验证第 77 轮再次超时。
启用 nocapture 的日志捕获到 cancellation_cleans_up_a_pending_continuation_without_resuming_it
先在 Sleeping 断言失败：实际状态仍是 Pending。测试仅调用 1000 次 yield_now，
在完整套件竞争下不能保证 worker 已启动。

断言 panic 发生在 group.cancel 之前；TaskGroup 的 Drop 调用 close 等待任务结束，
不会自动请求取消。尚未取消的挂起任务使 panic 展开被阻塞，表现为“取消测试挂死”。
该测试的计数回调也不负责完成任务，不能靠 timer 到期让等待自然结束。
证据位于本地 target/runtime-fix/after-double-drop/077.log 和 hang-sample.txt。

修复采用以下测试同步与清理措施：

- 用条件变量和 10 秒截止时间等待明确事件，移除 100 / 1000 次 yield 的调度假设。
- 旧 continuation 测试等待整个原生 invocation 结束，再取消或外部完成；
  Sleeping 只表示挂起状态，不能证明调用栈已退出。
- 测试 guard 在 panic 路径也请求取消、排空 invocation、完成必要的外部交接并关闭 group，
  之后才释放借用的 context / captures 和 continuation handle。
- 新增受控的“invocation 尚未结束时 panic”和“挂起后 panic”回归，验证展开清理。
- 同步修复附近的 sleeping、joinable 与 external-completion race 测试。

调度器里的观察字段和通知仅在 cfg(test) 下存在；生产调度与 TaskGroup::Drop
语义未改动。捕获的挂起由上述测试缺陷解释，不将其写成已证明的生产取消死锁。

## 3. 静态检查与 provider 空数组边界

补齐两个公开 unsafe provider 入口的 Safety 契约，替换两处 chunks_exact 写法，
移除嵌套 unsafe。检查 all-targets 时另发现并修复测试模块位置与索引循环两项提示。

审阅注册入口时发现 count 为零允许 operations 为 null，但仍将其传给
slice::from_raw_parts；即使长度为零，该函数也要求非空且对齐的指针。
现在空数组直接采用空切片，增加 null / zero 与无效非零组合的回归。
函数签名及共享 ABI 版本 v27 不变。

## 4. 验证结果

- 完整 runtime 并行套件连续 300 轮全部通过：每轮 244 passed、4 ignored、0 failed。
  每轮独立 cargo test 进程，60 秒超时，首个失败立即停止；没有 skip 或串行参数。
  4 项原有 ignored 测试仍按自身隔离要求处理，不计入本次普通套件验收。
- 根目录 cargo test：1407 passed、11 ignored、0 failed，包含 209 项 CLI 回归。
- runtime 的 all-targets 与 test-support 检查通过。
- 根目录严格 Clippy、runtime all-targets 严格 Clippy 均通过。
- 根目录与独立 runtime 格式检查通过。
- 文档检查与 git diff --check 通过。

由于 provider 边界包含生产代码修复，先用 target-dir target 重建 release runtime
静态库，再运行根目录回归，避免 AOT 链接旧归档。

本地日志：target/runtime-fix/final-verified-stress/001.log 至 300.log、root-tests.log、
release-build.log、root-clippy.log；这些产物不随仓库提交。
压力命令为 cargo test --manifest-path crates/joky-runtime/Cargo.toml --quiet -- --nocapture，
每轮由 subprocess 的 60 秒期限控制，超时后保留日志并终止子进程组。

这些结果覆盖本次捕获的两条故障链及本机回归，不构成所有并发交错的形式证明，
也不代表已验证 Linux / Windows 或解决待办中其他历史 SIGSEGV。
