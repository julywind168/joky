# 随机数 provider 验证记录（2026-09-25）

历史验证记录；当前 API 见[系统安全随机数](../../stdlib/random.md)。
本报告中的故障后续已定位并修复，见[独立修复与复验记录](runtime-test-fixes-2026-09-25.md)；
下文保留原始失败和当时结论。

环境：ARM64 macOS 26.6.2（25G83），Rust 1.98.1（48a229cea）。对照基线为
`9d7dcdf`；被测改动为在该提交上增加 random provider、标准库 effect、runtime ABI v27
以及本报告所在提交的测试。未运行其他操作系统的可执行文件。

## 已通过的验证

- 根目录 `cargo test --quiet`：1407 passed、11 ignored、0 failed。
- 独立 runtime 普通套件：241 passed、4 ignored、0 failed。
- `JOKY_TEST_AOT=full cargo test --test cli crypto_random`：3 项通过，覆盖实际 OS 调用、
  缓存 JIT、debug/release AOT、任务、handler 和 effect 检查。
- `cargo test --manifest-path crates/joky-runtime/Cargo.toml --quiet runtime::random`
  连续 300 轮，每轮 6 项通过；包括受控的取消先于准入及执行中取消。
- 根目录 Clippy、格式检查、文档检查与可执行示例通过。
- runtime `--all-targets` 与 `--features test-support` 检查通过；前者保留已有
  `runtime/map/tests.rs:392` 的嵌套 `unsafe` 警告。

另跑独立 runtime 的 `cargo clippy --manifest-path crates/joky-runtime/Cargo.toml -- -D warnings`
未通过：已有两个公开 unsafe provider 入口缺少 Safety 文档，以及 `runtime/debug.rs`、
`runtime/handler/payload.rs` 中两处 `chunks_exact_to_as_chunks` 提示。

这些结果不等同于完整 runtime 压力验证通过。

## 压力验证失败与基线对照

每轮使用独立 `cargo test --manifest-path crates/joky-runtime/Cargo.toml --quiet`
进程，设置 60 秒超时，遇首个失败停止并保留日志。

| 被测版本 | 结果 | 失败位置 |
| --- | --- | --- |
| 新增 random 的工作树 | 第 27 轮失败，之前 26 轮通过 | `runtime::mut_map::index_tests::indexed_growth_replacement_and_removal_release_shared_fields`，`index_tests.rs:189`，插入返回 0、预期 1 |
| 未修改的 `9d7dcdf` 独立 worktree | 第 23 轮超过 60 秒，之前 22 轮通过 | `runtime::task::tests::cancellation_cleans_up_a_pending_continuation_without_resuming_it` |
| 同一基线，仅用 `--skip` 排除上述挂起项以继续诊断 | 第 90 轮失败，之前 89 轮通过 | `runtime::map::tests::updates_share_untouched_nodes_and_release_all_arcs`，`map/tests.rs:203`，`trie::find(...).unwrap()` 得到 None |

首轮日志分别保留在本地 `target/random-checks/stress/027.log`、
`target/random-checks/baseline-stress/023.log` 和
`target/random-checks/baseline-without-hanging-task/090.log`。这些文件不随仓库提交。

对照证明原基线也有并发测试不稳定性，但没有证明三次失败具有同一根因，
也没有排除新改动对首个失败时序的影响。未修复这些问题，未重跑完整套件以覆盖失败记录；
排除挂起测试的循环仅用于诊断，不能计作完整压力验收。
