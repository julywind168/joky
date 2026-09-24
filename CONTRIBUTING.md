# 开发指南

> 本文档描述如何为 Joky 编译器贡献代码。

修改 runtime、native provider 或句柄生命周期前，请先阅读
[Runtime 开发注意事项](docs/runtime/development.md)。

文档入口见 [docs/README.md](docs/README.md)。修改、拆分或归档文档前，请阅读
[文档维护约定](docs/maintenance.md)，同步更新相关目录的索引和链接。

可运行示例按主题放在 `examples/`，入口和维护约定见
[示例导航](examples/README.md)。移动示例时同步更新测试中的引用路径。

## 目录

- [开发环境设置](#开发环境设置)
- [项目结构](#项目结构)
- [开发工作流](#开发工作流)
- [代码规范](#代码规范)
- [测试指南](#测试指南)
- [基准测试](#基准测试)
- [文档检查](#文档检查)
- [提交代码](#提交代码)

## 开发环境设置

### 前置要求

- **Rust**: 1.98+（推荐使用 rustup；以 `Cargo.toml` 的 `rust-version` 为准）
- **Cargo**: 随 Rust 安装
- **Git**: 版本控制
- **C 工具链**：可通过 `cc` 调用，用于原生构建和 AOT 测试
- **SQLite 3 动态库**：用于 SQLite 示例和测试；平台状态与依赖说明见 [README](README.zh.md#环境要求)

### 克隆仓库

```bash
git clone https://github.com/julywind168/joky.git
cd joky
```

### 构建项目

```bash
# 开发构建
cargo build

# 发布构建
cargo build --release
```

### 运行测试

```bash
# 准备 AOT 专项 CLI 测试所需的 release runtime
cargo build --release --manifest-path crates/joky-runtime/Cargo.toml

# 编译器和集成测试；一致性测试默认运行 JIT + debug AOT
cargo test

# 完整 JIT/debug/release AOT 一致性测试矩阵
JOKY_TEST_AOT=full cargo test --test cli

# 关闭一致性测试中的 AOT 分支；仍会运行 AOT 专项 CLI 测试
JOKY_TEST_AOT=off cargo test --test cli

# 运行特定测试
cargo test lexer
cargo test parser
cargo test sema

# 查看测试输出
cargo test -- --nocapture
```

Runtime 使用独立的 Cargo manifest，根目录的 `cargo test` 不运行其单元测试。
具体命令见 [Runtime 开发注意事项](docs/runtime/development.md)。

### 安装工具

```bash
# Snapshot 测试工具
cargo install cargo-insta

# 代码格式化与静态检查（Rust 工具链组件）
rustup component add rustfmt clippy
```

## 项目结构

```
joky/
├── src/
│   ├── syntax/           # 词法和语法分析
│   ├── sema/             # 语义分析
│   ├── hir/              # 带类型的 Core IR
│   ├── mir/              # CFG、verifier、pass
│   ├── codegen/          # Cranelift JIT / AOT
│   ├── frontend.rs       # 源码到已验证 MIR（check / 模块缓存）
│   ├── compiler.rs       # JIT / AOT 与 runtime 接入
│   ├── module.rs         # 模块图
│   ├── diagnostic/       # 错误处理
│   ├── lib.rs            # 库入口
│   └── main.rs           # CLI 入口
├── crates/
│   ├── joky-runtime/     # 独立 runtime
│   ├── joky-runtime-abi/ # 生成代码与 runtime 共用 ABI
│   └── joky-runtime-core/
├── std/                  # 标准库模块
├── tests/                # 集成测试
├── benches/              # Criterion 基准测试
├── examples/             # 示例程序
├── docs/                 # 文档（含 docs/plans/ 阶段计划）
```

## 开发工作流

### 1. 创建分支

```bash
git checkout -b feature/your-feature-name
```

### 2. 进行更改

编辑代码，遵循[代码规范](#代码规范)。

### 3. 运行测试

```bash
# 按上文准备 release runtime 后运行；完整一致性矩阵用 JOKY_TEST_AOT=full
cargo test

# 如果有 snapshot 变化
cargo insta review
```

### 4. 格式化代码

```bash
cargo fmt
```

### 5. 检查代码

```bash
cargo clippy -- -D warnings
```

### 6. 提交更改

```bash
git add .
git commit -m "feat: add new feature"
```

### 7. 推送并创建 PR

```bash
git push origin feature/your-feature-name
```

## 文档检查

从仓库根目录运行：

```bash
python3 scripts/check-docs.py
git diff --check
```

检查覆盖根目录、`docs/` 和 `examples/` 中的 Markdown，以及本地文件链接、章节锚点、
代码围栏、文档导航和示例源码的可达性；修改可执行示例或命令时，
还需要实跑相应内容。历史计划和验收报告保留原版本语境，当前用法写入主要契约文档。

## 代码规范

### Rust 风格

遵循 [Rust 官方风格指南](https://doc.rust-lang.org/1.0.0/style/):

- 使用 4 空格缩进
- 使用 `snake_case` 命名函数和变量
- 使用 `PascalCase` 命名类型
- 使用 `SCREAMING_SNAKE_CASE` 命名常量

### 文档注释

为公共 API 添加文档注释：

```rust
/// 解析源代码为 AST
///
/// # 参数
/// - `source`: 源代码字符串
///
/// # 返回
/// 解析成功返回 `Program`，失败返回 `Diagnostic`
///
/// # 示例
/// ```
/// let program = parse_program("fn main() { 42 }").unwrap();
/// ```
pub fn parse_program(source: &str) -> Result<Program, Diagnostic> {
    // ...
}
```

### 错误处理

使用 `Result` 显式处理错误：

```rust
// 好
pub fn lex(source: &str) -> Result<Vec<Token>, LexError> {
    // ...
}

// 不好
pub fn lex(source: &str) -> Vec<Token> {
    // 使用 panic! 或 unwrap()
}
```

### 模块组织

- 每个模块在单独的文件中
- 使用 `mod.rs` 作为模块入口
- 公共 API 在 `mod.rs` 中导出

```rust
// src/sema/mod.rs
mod checker;
mod types;
mod type_table;

pub use types::Type;
pub use type_table::TypeTable;
pub use checker::check_program;
```

## 测试指南

### 基准测试

使用 Criterion 基准编译器前端和完整编译流水线：

```bash
# 运行全部基准（完整编译基准包含 Cranelift JIT，耗时较长）
cargo bench --bench compiler_pipeline

# 快速验证解析器基准
cargo bench --bench compiler_pipeline parser \
  -- --warm-up-time 0.1 --measurement-time 0.2 --sample-size 10 --noplot
```

基准代码位于 `benches/compiler_pipeline.rs`。编译器基准每次使用新的 `Compiler`，避免 JIT 模块中的函数定义互相污染；`iter_batched` 的 setup 阶段不计入测量，因此结果主要反映一次源码编译和执行的成本。

### 单元测试

在模块内添加测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_function_name() {
        let input = /* 测试输入 */;
        let expected = /* 期望输出 */;
        let actual = function_under_test(input);
        assert_eq!(actual, expected);
    }
}
```

### Snapshot 测试

使用 `insta` 进行 snapshot 测试：

```rust
#[test]
fn snapshot_feature() {
    let source = "fn main() { ... }";
    insta::assert_snapshot!(format_ast(source));
}
```

运行和审查 snapshot：

```bash
# 运行 snapshot 测试
cargo insta test

# 交互式审查
cargo insta review

# 接受所有变化
cargo insta accept

# 拒绝所有变化
cargo insta reject
```

### 集成测试

在 `tests/` 目录添加端到端测试：

```rust
// tests/my_feature.rs
use joky::compile;

#[test]
fn test_my_feature() {
    let source = "fn main() { ... }";
    let result = compile(source);
    assert!(result.is_ok());
}
```

### 测试命名

- 使用描述性的测试名称
- 格式：`test_<what>_<scenario>` 或 `snapshot_<what>`

```rust
// 好
#[test]
fn test_parser_handles_nested_blocks() { }

#[test]
fn snapshot_if_expression() { }

// 不好
#[test]
fn test1() { }
```

### 测试覆盖

目标：
- 核心逻辑 100% 覆盖
- 边界情况测试
- 错误情况测试

## 添加新功能

### 示例：添加数组类型

#### 1. 更新 AST

```rust
// src/syntax/ast.rs
pub enum ExprKind {
    // ... 现有变体
    Array(Vec<Expr>),
}
```

#### 2. 更新 Parser

```rust
// src/syntax/parser.rs
fn parse_primary(&mut self) -> Result<Expr, ParseError> {
    match self.current_token().kind {
        // ... 现有分支
        TokenKind::LeftBracket => self.parse_array(),
        // ...
    }
}

fn parse_array(&mut self) -> Result<Expr, ParseError> {
    // 解析 [1, 2, 3]
}
```

#### 3. 更新类型系统

```rust
// src/sema/types.rs
pub enum Type {
    // ... 现有类型
    Array(Box<Type>),
}
```

#### 4. 更新类型检查器

```rust
// src/sema/checker.rs
fn check_expression(&mut self, expr: &Expr, expectation: TypeExpectation) -> Result<Type, SemanticError> {
    match &expr.kind {
        // ... 现有分支
        ExprKind::Array(elements) => self.check_array(elements, expectation),
        // ...
    }
}

fn check_array(&mut self, elements: &[Expr], expectation: TypeExpectation) -> Result<Type, SemanticError> {
    // 检查所有元素类型一致
}
```

#### 5. 更新代码生成

```rust
// src/codegen/cranelift.rs
fn generate_expression(&mut self, expr: &Expr, builder: &mut FunctionBuilder) -> Value {
    match &expr.kind {
        // ... 现有分支
        ExprKind::Array(elements) => self.generate_array(elements, builder),
        // ...
    }
}
```

#### 6. 更新 Visitor

```rust
// src/syntax/visitor.rs
pub trait ExprVisitor {
    // ... 现有方法
    fn visit_array(&mut self, elements: &[Expr], expr: &Expr) -> Self::Output {
        self.default_output()
    }
}

pub fn walk_expr<V: ExprVisitor + ?Sized>(visitor: &mut V, expr: &Expr) -> V::Output {
    match &expr.kind {
        // ... 现有分支
        ExprKind::Array(elements) => {
            for element in elements {
                visitor.visit_expr(element);
            }
            visitor.visit_array(elements, expr)
        }
        // ...
    }
}
```

#### 7. 添加测试

```rust
// 单元测试
#[test]
fn test_parser_handles_arrays() {
    let program = parse_program("fn main() { [1, 2, 3] }").unwrap();
    // ...
}

// Snapshot 测试
#[test]
fn snapshot_array_expression() {
    let source = "fn main() { [1, 2, 3] }";
    insta::assert_snapshot!(format_ast(source));
}

// 类型检查测试
#[test]
fn test_array_type_checking() {
    let source = "fn main() { let x: [Int32; 3] = [1, 2, 3]; x }";
    let program = parse_program(source).unwrap();
    let types = check_program(&program).unwrap();
    // ...
}
```

## 提交代码

### 提交消息格式

使用约定式提交（Conventional Commits）：

```
<type>(<scope>): <subject>

<body>

<footer>
```

**类型**:
- `feat`: 新功能
- `fix`: 修复 bug
- `docs`: 文档更新
- `style`: 代码格式（不影响功能）
- `refactor`: 重构
- `test`: 添加测试
- `chore`: 构建/工具更新

**示例**:

```
feat(parser): add array syntax support

Add support for parsing array literals:
- Parse [1, 2, 3] syntax
- Handle nested arrays
- Add error handling for malformed arrays

Closes #123
```

### Pull Request

PR 标题应该清晰描述变更：

```
feat: Add array type support
fix: Fix parser crash on empty input
docs: Update architecture documentation
```

PR 描述应包含：
- **概述**: 变更的目的
- **变更**: 具体修改内容
- **测试**: 如何测试
- **相关 Issue**: 如果有

### 代码审查

- 响应审查意见
- 根据反馈更新代码
- 保持礼貌和专业

## 调试技巧

### 打印调试

```rust
println!("{:#?}", ast);  // 美化打印
dbg!(&expression);       // 调试宏
```

### 运行单个测试

```bash
cargo test test_name -- --nocapture
```

### 使用 RUST_BACKTRACE

```bash
RUST_BACKTRACE=1 cargo test
```

### 查看生成的 IR

```rust
// 在 codegen 中添加
println!("{}", context.func.display());
```

## 常见问题

### Q: 如何添加新的 token？

1. 在 `token.rs` 添加 token 类型
2. 在 `lexer.rs` 添加识别逻辑
3. 添加测试

### Q: 如何添加新的表达式类型？

参见[添加新功能](#添加新功能)章节。

### Q: Snapshot 测试失败怎么办？

```bash
# 1. 审查变化
cargo insta review

# 2. 如果变化正确，接受
# 3. 如果变化错误，修复代码
```

### Q: 如何调试类型检查问题？

在 `checker.rs` 添加打印：

```rust
pub fn check_expression(&mut self, expr: &Expr, expectation: TypeExpectation) -> Result<Type, SemanticError> {
    eprintln!("Checking: {:?} with {:?}", expr.kind, expectation);
    // ...
}
```

## 相关资源

- [架构文档](docs/compiler/architecture.md)
- [类型检查文档](docs/compiler/type-checking.md)
- [错误处理文档](docs/compiler/error-handling.md)
- [Rust 官方文档](https://doc.rust-lang.org/)
- [Cranelift 文档](https://cranelift.dev/)

## 获取帮助

- 提交 Issue
- 在 PR 中提问
- 查看现有代码示例

感谢你的贡献！🎉
