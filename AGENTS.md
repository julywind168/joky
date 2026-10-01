# AGENTS.md

面向 AI 代理的项目说明。Joky 是一门自带编译器与虚拟机的语言：Rust 实现解析、
类型与 IR 核心，JIT（Cranelift）与 AOT 原生两路后端共用 CPS continuation 运行时。

## 日常验证

```sh
./scripts/check.sh --quick    # 日常开发：fmt + Clippy + Debug 测试 + 基准正确性
./scripts/check.sh            # 提交前：额外含 Release 测试
```

- 日常迭代只跑 `--quick` 即可，约 1 分钟（大头是测试运行，不是编译）。
- 脚本从自身路径定位仓库根目录，任意目录可调用；首个失败即停。
- 聚焦单个用例时直接用 cargo 更快：`cargo test <名字>`、`cargo test --test cli`。
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
