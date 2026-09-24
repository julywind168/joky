# Joky

[English](README.md) | 中文

**一门实验性的静态类型语言，将结构化并发、并发所有权和代数效果融入语言设计。**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.98%2B-orange.svg)](Cargo.toml)
[![Tests](https://github.com/julywind168/joky/actions/workflows/test.yml/badge.svg)](https://github.com/julywind168/joky/actions/workflows/test.yml)

Joky 在语言层面表达任务生命周期、共享可变状态和副作用处理。编译器与运行时使用 Rust 实现，通过 Cranelift 生成原生代码，同时支持 JIT 执行和 AOT 编译。

- **结构化并发**：使用 `parallel` 在多个工作线程上执行任务，按源码顺序收集结果。
- **并发所有权**：通过 `Cown(T)` 共享可变状态，使用 `when` 临时取得对其内容的独占访问。
- **代数效果**：将操作声明与处理方式分离。可挂起操作由编译器生成 continuation，无需在用户代码中编写 `async`/`await`。

Joky 正在积极开发中，语法、API 和运行时行为都可能变化。目前适合语言探索、原型实验和编译器开发，尚不建议用于生产环境。

## 快速开始

### 环境要求

- Rust **1.98+** 和 Cargo，可通过 [rustup](https://rustup.rs/) 安装。
- Git，用于克隆仓库。
- 可通过 `cc` 调用的 C 编译器/链接器，用于原生构建和测试：macOS 使用 Xcode Command Line Tools，Linux 使用 C 构建工具链。
- SQLite 3 动态库，用于 SQLite 示例和测试。macOS 自带；Linux CI 安装 `libsqlite3-dev`。

**平台状态**：Apple Silicon 上的 macOS 是主要开发和验证平台。Linux x86_64 已纳入 [CI](.github/workflows/test.yml)，该任务目前允许失败。Windows 尚未验证。AOT 仅支持编译器自身的宿主目标，尚不支持交叉编译。

### 构建并运行

```bash
git clone https://github.com/julywind168/joky.git
cd joky

cargo build --release
cargo run --release -- run examples/basics/functions.jk
```

预期输出：

```text
Result: 30
```

后续命令均在仓库根目录运行。熟悉后，可将示例路径替换为自己的 `.jk` 文件。

### 检查程序

检查程序，但不执行：

```bash
cargo run --release -- check examples/basics/functions.jk
```

### 生成原生可执行文件

```bash
cargo run --release -- build examples/basics/functions.jk --release -o target/joky-examples/functions
./target/joky-examples/functions
```

同样输出 `Result: 30`。Cargo 的 `--release` 用于优化编译器本身，`build` 后面的 `--release` 用于优化生成的 Joky 代码。生成的可执行文件运行时不需要 Joky 编译器；使用外部库的程序仍需要相应的运行时依赖。

## 语言一览

### 结构化并发

`parallel` 的分支可以在工作线程上并发执行。整个块等待所有分支完成，再按源码顺序返回结果元组；分支的执行顺序并不固定。

```joky
fn square(value: Int32) -> Int32 {
    value * value
}

fn main() {
    let results = parallel {
        | square(2)
        | square(3)
        | square(4)
        | square(5)
    }
    let total = results.0 + results.1 + results.2 + results.3
    println(total)  // 54
}
```

完整示例见[并行计算](examples/concurrency/parallel_compute.jk)和[任务](examples/concurrency/tasks.jk)。

### 并发所有权

`Cown` 持有任务间共享的可变状态。访问其内容需要经过 `when`：进入块时取得独占访问，离开时释放。

```joky
class Counter {
    var value: Int32 = 0

    fn increment() {
        self.value = self.value + 1
    }
}

fn main() {
    let counter = Cown.new(Counter(value: 0))
    let result = when (counter) |state| {
        state.increment()
        state.value
    }
    println(result)  // 1
}
```

一个 `when` 块可以同时取得多个 Cown，参见[状态转移示例](examples/concurrency/cown_transfer.jk)。

### 代数效果

Effect 声明操作，handler 提供具体行为。下面的局部 handler 返回固定年龄，因此程序不需要交互输入。

```joky
eff console {
    fn read(prompt: String) -> Int64
}

fn read_age() -> Int64 {
    do {
        console.read(prompt: "age")
    } with {
        console.read(prompt) => 42
    }
}

fn main() {
    println(read_age())  // 42
}
```

模型说明见 [Effect 与 Handler](docs/lang/effects.md)，可挂起操作见[定时器示例](examples/effects/clock_sleep.jk)。

## 更多示例

Joky 还实现了泛型、动态 trait 与 trait 组合、模块、游标迭代，以及原生 I/O provider。[示例导航](examples/README.md)按主题列出可运行程序，并说明依赖与运行行为：

| 示例 | 展示内容 |
| --- | --- |
| [函数](examples/basics/functions.jk) | 带类型的函数定义与调用 |
| [动态 trait](examples/traits/dyn_traits.jk) | 动态分派与接收者所有权 |
| [Trait 组合](examples/traits/dyn_trait_composition.jk) | 在动态值中组合多个接口 |
| [游标](examples/collections/cursor.jk) | 自定义迭代与泛型收集 |
| [文件复制](examples/io/file_copy.jk) | 分块读写，在 `target/` 下生成文件 |
| [C FFI](examples/ffi/abs.jk) | 调用原生 C 函数 |

例如：

```bash
cargo run --release -- run examples/concurrency/cown_when.jk
```

## 测试与基准

运行测试前先构建 release runtime。AOT 专项 CLI 测试需要它，即使一致性测试矩阵使用 debug AOT 或已关闭 AOT 分支，也需要先准备。

```bash
cargo build --release --manifest-path crates/joky-runtime/Cargo.toml

# 编译器和集成测试；一致性测试默认运行 JIT + debug AOT
cargo test

# 完整的 JIT / debug AOT / release AOT 一致性测试矩阵
JOKY_TEST_AOT=full cargo test --test cli

# 关闭一致性测试中的 AOT 分支；仍会运行 AOT 专项 CLI 测试
JOKY_TEST_AOT=off cargo test --test cli
```

Runtime 使用独立的 Cargo manifest，根目录的 `cargo test` 不包含其单元测试。详见 [Runtime 开发文档](docs/runtime/development.md)和[测试范围](docs/testing/suite.md)。

测量解析器和小型程序编译、执行的开销：

```bash
cargo bench --bench compiler_pipeline

# 快速运行解析器基准
cargo bench --bench compiler_pipeline parser -- \
  --warm-up-time 0.1 --measurement-time 0.2 --sample-size 10 --noplot
```

这些基准针对特定编译器工作负载，不代表应用程序与其他语言的性能对比。

## 文档

完整入口见[文档索引](docs/README.md)。目前设计文档和贡献者文档以中文为主。

- [语言指南](docs/lang/README.md)
- [包与模块](docs/lang/modules.md)
- [Effect 与 Handler](docs/lang/effects.md)
- [标准库](docs/stdlib/README.md)
- [编译器架构](docs/compiler/architecture.md)
- [Runtime 开发](docs/runtime/development.md)
- [测试与测量](docs/testing/README.md)
- [贡献指南](CONTRIBUTING.md)

### 仓库结构

```text
src/                       编译器：syntax → sema → HIR → MIR → Cranelift
crates/joky-runtime/       运行时：任务、所有权、效果与 I/O
crates/joky-runtime-abi/   编译器与运行时共用 ABI
crates/joky-runtime-core/  运行时基础组件
std/                       标准库模块
examples/                  可运行的 Joky 程序
tests/                     集成测试与测试用例
benches/                   编译器基准
docs/                      语言、架构与设计文档
```

## 路线图

下一阶段计划探索[嵌入式执行与脚本热更新](docs/plans/embedded-execution.md)：在保留兼容状态的前提下替换模块代码，提供用于嵌入的 C API，以及由宿主驱动的调度方式。基于 Pulley 的可移植执行路径仍需通过可行性原型验证。

这些能力尚未实现。文件监视、可移植字节码和游戏引擎集成都属于设计目标，当前没有对应的可用命令或平台支持承诺。进展与待办见[当前计划](docs/plans/README.md)。

## 参与贡献

欢迎提交 bug、能复现编译器或运行时问题的小程序、文档改进和实现代码。请先阅读[贡献指南](CONTRIBUTING.md)；涉及较大语言设计或运行时改动时，请先开 issue 讨论方案。

报告问题时，请附上操作系统与架构、Rust 版本、Joky commit、完整命令、最小 `.jk` 程序和实际输出。Bug 与设计建议统一提交到 [issue tracker](https://github.com/julywind168/joky/issues)，中英文均可。

## 许可证

Joky 使用 [MIT License](LICENSE)。

感谢 [Cranelift](https://cranelift.dev/) 与 Rust 生态提供的基础设施。
