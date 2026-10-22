# AGENTS.md

面向 AI 代理的项目说明。Joky 是一门自带编译器与虚拟机的语言：Rust 实现解析、
类型与 IR 核心，JIT（Cranelift）与 AOT 原生两路后端共用 CPS continuation 运行时。

## 按改动范围验证

提交前完成与改动相关的检查，不因“要提交”或“完成一个阶段”自动运行整套检查。
当前项目处于非公开开发阶段，日常验证统一优先使用 Debug，不默认构建或运行 Release。
按实际影响选择范围；混合改动取较高要求。仅在用户明确要求发布验收、性能测量，
或排查优化构建特有问题时使用 Release，并说明原因；完成普通开发阶段不视为发布验收。

| 改动范围 | 默认验证 |
| --- | --- |
| 纯文档、普通注释 | 检查内容、相关链接与 `git diff --check`；不运行 Cargo 或检查脚本。可执行文档示例按代码改动验证 |
| 仅新增或调整独立测试、fixture、探针（含 `#[cfg(test)]` 模块接线） | Rust 改动运行 `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`，并运行受影响测试；涉及生成代码的用例覆盖 JIT、解释器、AOT，不默认跑无关基准和全量穷举 |
| 局部实现修改 | 先跑针对性行为测试，再运行 `./scripts/check.sh` |
| 跨模块语义、ABI、存储布局、所有权或任务生命周期修改；影响全局的构建/依赖/共享测试设施修改 | 先跑针对性测试，再运行 `./scripts/check.sh`；按影响补跑相关 Debug 穷举，不自动扩大到 Release |

明确要求发布验收时运行 `./scripts/check.sh --full --release` 和
`./scripts/check.sh --stress --release`；需要 Debug 覆盖时另跑默认检查。
所有权、取消机制、Map 实现等改动按影响补跑相关 Debug 穷举；无需自动运行全部压力检查。

需要扩大验证时说明具体原因。只改某个 Release/穷举用例时直接运行该用例；
测试或 fixture 若被多处共用，要覆盖所有受影响入口，不能只按文件名判断范围。
例如 String S0 仅增加探针时，运行 `cargo test string_storage_probe` 加 Rust 格式与
Clippy 检查即可，无需全套 Release、Map/while 基准正确性和无关穷举。

full 与 quick 运行相同的 Debug 检查，不必在同一版本上依次重复运行两者。验证通过后仅修改
文档无需重跑代码检查；新增代码修改只重跑受影响检查，必要时扩大范围。
报告实际执行的检查和未验证项，不把针对性验证写成全量通过。

```sh
./scripts/check.sh         # 默认 quick：fmt + Clippy + Debug 测试 + 短输入基准正确性
./scripts/check.sh --full  # 兼容别名：与 quick 相同，不额外构建 Release
./scripts/check.sh --stress # 独立压力检查：Debug 穷举与长输入基准正确性
./scripts/check.sh --release # 显式选择 Release 测试与短输入基准正确性
```

- 无参数默认 quick；`--quick` 和 `--full` 保留为兼容入口，`--stress` 独立运行，不重复 quick。
  `--release` 可与任一模式组合，只将测试和基准正确性切换为 Release，不重复跑两套 profile。
- Test profile 使用一级优化，保留 Debug 断言与溢出检查；依赖使用二级优化，普通 Dev
  构建中的 Joky 仍不优化。首次构建较慢，后续复用缓存；需无优化调试测试时使用
  `CARGO_PROFILE_TEST_OPT_LEVEL=0 cargo test <名字>`。
- quick/full 的短输入基准正确性仍覆盖全部后端、边界输入与种子；while 最大 1000 步、Map 最大 32 项。
  stress 默认在 Debug 下使用默认长输入；较慢时优先缩小到受影响用例，不自动切换 Release。
- 脚本从自身路径定位仓库根目录，任意目录可调用；首个失败即停。
- 聚焦单个用例时直接用 cargo 更快：`cargo test <名字>`、`cargo test --test cli`。
- 默认测试只保留独有的回归责任。新增前检查同一 fixture、诊断、边界是否已有覆盖；
  三后端覆盖按行为在整套测试中满足，不在 VM、AOT、CLI 各层重复同一矩阵。
  静态拒绝矩阵使用共享前端，入口错误一致性与失败后旧模块可用性集中验证。
  CLI 保留参数、文件、输出、诊断与路由回归；已有两种 VM 执行覆盖的 fixture 只补独立 AOT。
- 同一资源契约的类型边界与 API 行为分别验证，避免无理由的笛卡尔积；相关 fixture
  尽量批量编译。分配失败预算扫描在首次完整成功且清理断言通过后停止。
  密集指令扫描留在 `exhaustive_`，普通测试验证明确的生命周期或资源变化边界。
- 长模型压力、逐指令取消穷举与额外 CLI 类型矩阵用例以 `exhaustive_` 命名并默认
  `#[ignore]`，stress 默认在 Debug 下自动执行；默认测试保留短模型周期与资源变化边界回归。
  单独运行穷举：`cargo test exhaustive_ -- --ignored`；聚焦某组时用更具体的测试名替代 `exhaustive_`。
- TCP 的 1 MiB 完整消息传输、解释器背压恢复和额外 CLI 网络后端矩阵归入 stress，保留原消息尺寸与断言。
  日常 Debug 保留短消息协议边界、真实背压、JIT 恢复、两种 VM 模式的取消与资源清理、AOT 互操作及 CLI 路由回归。
- 检查只验证正确性，不计时、不断言性能；性能测量独立进行，方法见
  benches/README.md。

## 注意事项

- `src/stdlib/*.jk` 经 `include_str!` 在编译期内嵌，改动后需重新编译才生效。
- 编译器或运行时改动必须附带行为测试（测试要执行生成代码）；JIT、解释器、
  AOT 三个后端语义保持一致，改动时三边一起验证。
- Clippy 以 `-D warnings` 运行；不要用抑制手段掩盖未用代码或 ABI 错误。
- AOT 互操作测试需要 `cc`。
- 提交信息用 Conventional Commits 单行主题：`feat:`/`refactor:`/`fix:`/`docs:`/
  `perf:`/`test:`/`chore:` 前缀，英文祈使句，如
  `feat: support owning tuples and consuming destructuring`。
- 编码规范与命名约定见 CONTRIBUTING.md；代码内标识符、注释、诊断、测试名用
  英文，文档与讨论可用中文。

## 文档分工

入门与能力表看 README.md；实现语法与内置模块看 docs/language.md；语法设计与
阶段目标看 docs/syntax.md；架构边界与 ABI 不变量看 docs/architecture.md；
字节码设计看 docs/interpreter.md；历史性能数据归 docs/interpreter-performance.md。
新增文档内容按此归属放置，不要制造重复。
