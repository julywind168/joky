# 编译器错误处理实现

> 这是编译器内部实现文档，描述错误处理架构和代码组织。

## 概述

Joky 编译器使用**分层错误类型系统**，每个编译阶段都有专门的错误类型，在边界处转换为统一的 `Diagnostic` 用于报告。

## 错误类型层次

```
LexError ────┐
ParseError ──┼──> Diagnostic ──> 用户
SemanticError┤
CodegenError ─┘
```

## 专用错误类型

### LexError (`src/diagnostic/error.rs`)

词法分析阶段的结构化错误：

```rust
pub enum LexError {
    UnexpectedCharacter { character: char, span: Span },
    UnterminatedString { span: Span },
    UnterminatedBlockComment { span: Span },
    InvalidEscapeSequence { sequence: String, span: Span },
    // ...
}
```

每个变体都携带具体的上下文信息（如非法字符、字面量值等），而不是泛化的字符串消息。

### ParseError

语法分析错误：
- `EmptyProgram` - 空程序
- `ExpectedToken` - 缺少预期的 token
- `InvalidAssignmentTarget` - 赋值左值必须是标识符
- `NotCallable` - 尝试调用非函数表达式
- `ExpectedExpression` - 期望表达式
- `ExpectedFunctionName` - 期望函数名

### SemanticError

语义分析错误（23 种），包括：

**名称解析**
- `UnknownValue` / `UnknownFunction` / `UnknownType`

**类型检查**
- `TypeMismatch` - 记录期望类型和实际类型
- `IntegerLiteralOutOfRange` / `FloatLiteralOutOfRange`

**运算符检查**
- `CannotNegateUnsigned` / `NumericOperatorRequiresNumeric`
- `DivisionByZero` - 编译期检测常量除零
- `ArithmeticModeRequiresInteger` / `CheckedCompoundAssignment`
- `OrderedComparisonRequiresNumeric` / `ComparisonTypeMismatch`

**作用域和绑定**
- `AssignToImmutable` - 给 `let` 绑定赋值
- `BreakOutsideLoop` - 循环外的 `break`

**函数**
- `MissingMainFunction` / `DuplicateMainFunction`
- `WrongArgumentCount` - 记录函数名、期望参数数量

### CodegenError

代码生成错误：
- `BackendInitialization` - Cranelift 初始化失败
- `RuntimeError` - JIT 执行时错误

## 转换机制

所有专用错误都实现 `From<SpecificError> for Diagnostic`，在模块边界自动转换：

```rust
// 内部使用专用类型
fn lex(source: &str) -> Result<Vec<Token>, LexError>

// 公共 API 转换为 Diagnostic
pub fn parse_program(source: &str) -> Result<Program, Diagnostic> {
    Parser::new(lex(source).map_err(Diagnostic::from)?)
        .parse_program()
        .map_err(Diagnostic::from)
}
```

## 设计决策

### 为什么分层？

1. **类型安全** - 编译器在编译时检查错误处理的正确性
2. **明确的上下文** - 每个错误携带结构化的上下文，而不是字符串拼接
3. **易于扩展** - 添加新错误只需添加枚举变体，不影响其他阶段
4. **测试友好** - 测试可以精确匹配错误类型和字段

### 为什么保留 Diagnostic？

公共 API 仍然使用统一的 `Diagnostic`：
- 保持 API 稳定性
- 简化错误渲染逻辑（所有错误都有 `Stage` 和 `Span`）
- 避免泛型污染（`Result<T, E>` 的 `E` 固定）

## 代码示例

### 之前（字符串错误）

```rust
if !binding.mutable {
    return Err(Diagnostic::semantic(
        format!("cannot assign to immutable binding '{name}'"),
        expression.span,
    ));
}
```

### 之后（结构化错误）

```rust
if !binding.mutable {
    return Err(SemanticError::AssignToImmutable {
        name: name.clone(),
        span: expression.span,
    });
}
```

### 测试

```rust
#[test]
fn reports_invalid_character() {
    let error = lex("2 & 3").unwrap_err();
    assert_eq!(error.span(), Span::new(2, 3));
    match error {
        LexError::UnexpectedCharacter { character, .. } => {
            assert_eq!(character, '&');
        }
        _ => panic!("expected UnexpectedCharacter"),
    }
}
```

## 未来改进

1. **错误恢复** - 在每个阶段收集多个错误，而不是遇到第一个错误就停止
2. **错误代码** - 为每个错误分配唯一代码（如 `E0001`），方便文档查询
3. **修复建议** - 某些错误可以提供 "did you mean?" 式的建议
4. **严重级别** - 区分 error、warning、hint
5. **多语言** - 将错误消息与错误类型分离，支持国际化

## 相关文件

- `src/diagnostic/error.rs` - 专用错误类型定义
- `src/diagnostic.rs` - Diagnostic 和转换实现
- `src/diagnostic/render.rs` - 错误渲染（终端输出）
