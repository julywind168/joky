# Joky 类型检查系统

> 本文档描述 Joky 的类型系统、类型推断算法和类型检查实现。

## 目录

- [概述](#概述)
- [类型系统](#类型系统)
- [类型推断](#类型推断)
- [类型检查器](#类型检查器)
- [错误处理](#错误处理)

## 概述

Joky 使用**静态类型系统**，在编译时进行类型检查。类型检查器负责：

1. **类型推断** - 推断表达式的类型
2. **类型检查** - 验证类型一致性
3. **作用域管理** - 管理变量绑定
4. **错误报告** - 报告类型错误

## 类型系统

### 基本类型

Joky 的用户可见类型名统一使用大写开头；Rust 内部枚举仍使用现有的变体命名：

```joky
Int8, Int16, Int32, Int64
UInt8, UInt16, UInt32, UInt64
Float32, Float64
String, Bool, Unit
```

```rust
pub enum Type {
    // 有符号整数
    I8, I16, I32, I64,
    // 无符号整数
    U8, U16, U32, U64,
    // 浮点数
    F32, F64,
    // 其他
    String, Bool, Unit,
}
```

### 类型分类

```rust
impl Type {
    pub fn is_integer(self) -> bool;        // Int8-Int64, UInt8-UInt64
    pub fn is_signed_integer(self) -> bool; // Int8-Int64
    pub fn is_float(self) -> bool;          // Float32, Float64
    pub fn is_numeric(self) -> bool;        // 整数 + 浮点
}
```

### 默认类型

- **整数字面量**: `Int32`
- **浮点字面量**: `Float64`

```joky
let x = 42;    // x: Int32
let y = 3.14;  // y: Float64
```

### 类型注解

语法层的 `TypeAnnotation` 保存 `TypeExpr` 与源码 span，不再保存需要重新解析的类型字符串。`TypeExpr` 包含 `Name`、`Member`、`Apply`、`Tuple` 和 `Function`；类型应用统一使用普通括号。

语义阶段递归解析注解，并将每个注解位置的结果记录到 `annotation_types`。类型函数应用复用 `type_values` 中的编译期求值逻辑；类型函数会在解析字段和普通函数签名前预注册，支持前向引用。别名与普通值共用词法作用域。

在生成 `CheckedTypes` 前，编译器沿泛型调用关系补齐函数体中注解的具体类型实例。HIR 通过 `checked_annotation` 读取语义结果并应用类型参数及 receiver 替换，不再依赖源码字符串解析来决定函数参数、返回值或内置调用的编译期类型值。

类型本身也是一种只存在于编译期的值。类型函数使用普通函数调用语法，并在语义分析阶段求值：

```joky
fn Boxed(T: type) -> type {
    Option(List(T))
}

fn load(T: type, values: Boxed(T)) -> Boxed(T) {
    values
}
```

类型函数支持前向引用、嵌套调用和局部类型绑定。它们不会进入 HIR 或运行时函数表；类型绑定在 HIR 中擦除为 `Unit`。当前实现的类型函数参数必须是无约束的 `type` 参数；普通泛型函数才使用 `T: type + Trait` 形式，类型函数不能包含运行时参数或 effects。

类型函数也可以生成匿名 struct 和 enum。具体实例会注册到 `TypeTable`，并复用普通聚合类型的布局、变体构造、模式匹配和 ownership lowering；相同类型函数表达式与具体类型参数会复用实例，递归类型函数在求值时拒绝。

`Option(T)` 与 `Result(T, E)` 由 `std/joky/option.jk` 和 `std/joky/result.jk` 中的普通 Joky 类型函数定义。语义层会将这两个标准类型映射到现有的 canonical `Type::Option`、`Type::Result` ABI；它们的组合 API 经过普通泛型函数检查和单态化，HIR/MIR 仍使用专用 enum 表示。

`sema/prelude.rs` 将这两个模块的函数放入隔离的 `@prelude/` 命名空间，使用不与用户源码冲突的 NodeId 和 span。以对应包装类型作为首参数的公开泛型函数支持点调用；HIR 将接收者插入普通调用参数，查询的借用和解包的消费均来自函数签名。编译器不再按 `is_some`、`is_err`、`unwrap_or` 等方法名实现行为。标准库源码参与 `JOKY_BUILD_ID`，修改后会使模块和 AOT 缓存失效。

显式指定类型：

```joky
let x: Int64 = 42;
let y: Float32 = 3.14;
var z: UInt32 = 100;
```

## 类型推断

### TypeExpectation

类型推断使用 `TypeExpectation` 表达期望：

```rust
pub enum TypeExpectation {
    None,             // 自由推断
    Hint(Type),       // 提示推断
    Require(Type),    // 要求匹配
}
```

#### None - 自由推断

用于没有类型约束的场景：

```joky
// 块中的非最后表达式
{
    42;      // None -> Int32
    3.14;    // None -> Float64
    "hello"  // None -> String
}
```

#### Hint - 提示推断

建议使用某个类型，但可以使用其他兼容类型：

```joky
let x: Int64 = 100;
let y = 42 + x;  // 42 被提示为 Int64（而不是默认的 Int32）
```

**实现**:
```rust
// 整数字面量根据期望推断
let value_type = expectation
    .ty()
    .filter(|ty| ty.is_integer())
    .unwrap_or(Type::I32);  // 如果期望不是整数，使用默认 Int32
```

#### Require - 要求匹配

必须是指定类型，否则报错：

```joky
var x: Int32 = 100;
x = 200;        // 200 必须是 Int32
x = "text";     // 错误：expected Int32, found String
```

**实现**:
```rust
// 赋值要求值必须是变量的类型
self.check_expression(value, TypeExpectation::require(binding.value_type))?;
```

### 推断规则

#### 1. 字面量推断

```joky
42          // Int32 (默认)
42: Int64     // Int64 (显式注解)
3.14        // Float64 (默认)
3.14: Float32   // Float32 (显式注解)
"hello"     // String
true        // Bool
```

#### 2. 二元运算推断

```joky
// 规则：
// 1. 如果有期望且是数值类型，使用期望
// 2. 如果左侧是字面量，右侧不是，使用右侧的类型
// 3. 否则，使用左侧的类型

let x: Int64 = 100;
let y = 42 + x;       // 42 提示为 Int64，y 是 Int64

let a = 1 + 2;        // a 是 Int32（默认）
let b: Int64 = 1 + 2;   // 1 和 2 提示为 Int64，b 是 Int64
```

**实现**:
```rust
let value_type = if let Some(expected) = expectation.ty().filter(|ty| ty.is_numeric()) {
    // 情况 1：有数值类型的期望
    self.check_expression(left, TypeExpectation::require(expected))?;
    self.check_expression(right, TypeExpectation::require(expected))?;
    expected
} else if is_numeric_literal(left) && !is_numeric_literal(right) {
    // 情况 2：左侧是字面量，右侧不是
    let right_type = self.check_expression(right, TypeExpectation::none())?;
    self.check_expression(left, TypeExpectation::hint(right_type))?;
    right_type
} else {
    // 情况 3：使用左侧的类型
    let left_type = self.check_expression(left, TypeExpectation::none())?;
    self.check_expression(right, TypeExpectation::require(left_type))?;
    left_type
};
```

#### 3. if 表达式推断

```joky
let x = if condition {
    42        // 推断为 Int32
} else {
    100       // 必须是 Int32
};            // x 是 Int32
```

**规则**:
- then 分支推断类型
- else 分支必须匹配 then 分支

#### 4. 函数调用推断

```joky
println("hello");  // 参数必须是 String
```

## 类型检查器

### 检查器结构

```rust
pub(super) struct Checker {
    scopes: Vec<HashMap<String, Binding>>,  // 作用域栈
    types: HashMap<NodeId, Type>,           // 类型表
    loop_break_types: Vec<Option<Type>>,    // 循环 break 类型
}

struct Binding {
    value_type: Type,
    mutable: bool,
}
```

### 作用域管理

```rust
// 进入新作用域
self.scopes.push(HashMap::new());

// 绑定变量
self.bind("x", Type::I32, false);

// 查找变量
if let Some(binding) = self.lookup("x") {
    // 使用 binding
}

// 退出作用域
self.scopes.pop();
```

### 检查流程

```rust
pub fn check_expression(
    &mut self,
    expression: &Expr,
    expectation: TypeExpectation,
) -> Result<Type, SemanticError> {
    let actual = match &expression.kind {
        ExprKind::Integer(value) => { /* 推断整数类型 */ }
        ExprKind::Float(value) => { /* 推断浮点类型 */ }
        ExprKind::Binary { op, left, right } => { /* 检查二元运算 */ }
        // ... 其他表达式类型
    };

    // 如果有要求的类型，检查是否匹配
    if expectation.is_required() {
        if let Some(expected) = expectation.ty() {
            if actual != expected {
                return Err(type_mismatch(expected, actual, expression.span));
            }
        }
    }

    // 记录类型
    self.types.insert(expression.id, actual);
    Ok(actual)
}
```

### 特殊情况处理

#### 1. 取负运算

```joky
-42        // 检查负数范围
-(-128)    // Int8: 错误，-128 的负数超出 Int8 范围
```

**实现**:
```rust
fn check_negation(&mut self, expression: &Expr, expectation: TypeExpectation) -> Result<Type, SemanticError> {
    let value_type = /* 推断类型 */;
    
    // 无符号类型不能取负
    if value_type.is_integer() && !value_type.is_signed_integer() {
        return Err(SemanticError::CannotNegateUnsigned { ... });
    }
    
    // 检查负数范围
    if let ExprKind::Integer(value) = expression.kind {
        check_negative_integer(value, value_type, expression.span)?;
    }
    
    Ok(value_type)
}
```

#### 2. 循环 break 类型

```joky
let x = loop {
    if condition {
        break 42;    // 推断为 Int32
    } else {
        break 100;   // 必须是 Int32
    }
};                   // x 是 Int32
```

**实现**:
```rust
// 进入循环时，记录 break 类型
self.loop_break_types.push(expected);

// 检查 break
let value_type = /* 推断 break 值的类型 */;
let slot = self.loop_break_types.last_mut();
if let Some(expected) = *slot {
    if expected != value_type {
        return Err(type_mismatch(expected, value_type, span));
    }
} else {
    *slot = Some(value_type);  // 第一个 break 确定类型
}

// 退出循环
let result = self.loop_break_types.pop().unwrap_or(Type::Unit);
```

#### 3. 除零检测

```joky
10 / 0;  // 错误：division by zero
```

**实现**:
```rust
if operator == BinaryOp::Divide
    && value_type.is_integer()
    && constant_integer(right) == Some(0)
{
    return Err(SemanticError::DivisionByZero { span: right.span });
}
```

## 错误处理

### 类型错误

```rust
pub enum SemanticError {
    TypeMismatch {
        expected: Type,
        found: Type,
        span: Span,
    },
    UnknownValue {
        name: String,
        span: Span,
    },
    CannotNegateUnsigned {
        type_name: String,
        span: Span,
    },
    // ... 更多错误类型
}
```

### 错误消息

```
error: type mismatch
  ┌─ example.jk:2:9
  │
2 │     x = "text";
  │         ^^^^^^ expected Int32, found String
  │
```

## 验证函数

### 数值范围检查

```rust
pub(super) fn check_positive_integer(value: u64, ty: Type, span: Span) -> Result<(), SemanticError> {
    let max = match ty {
        Type::I8 => 127,
        Type::I16 => 32767,
        Type::I32 => 2147483647,
        Type::I64 => 9223372036854775807,
        Type::U8 => 255,
        Type::U16 => 65535,
        Type::U32 => 4294967295,
        Type::U64 => u64::MAX,
        _ => return Ok(()),
    };
    if value > max {
        return Err(SemanticError::IntegerLiteralOutOfRange { ... });
    }
    Ok(())
}
```

### 常量求值

```rust
pub(super) fn constant_integer(expr: &Expr) -> Option<i64> {
    match &expr.kind {
        ExprKind::Integer(value) => Some(*value as i64),
        ExprKind::Unary { op: UnaryOp::Negate, expression } => {
            constant_integer(expression).map(|v| -v)
        }
        ExprKind::Binary { op, left, right } => {
            let left = constant_integer(left)?;
            let right = constant_integer(right)?;
            match op {
                BinaryOp::Add => left.checked_add(right),
                BinaryOp::Subtract => left.checked_sub(right),
                BinaryOp::Multiply => left.checked_mul(right),
                BinaryOp::Divide => left.checked_div(right),
                _ => None,
            }
        }
        _ => None,
    }
}
```

## 类型表

### 分阶段的数据结构

`check_program` / `check_module_with_context` 返回 `CheckedTypes`，包含表达式
类型、局部绑定、注解、闭包捕获和调用解析等源码事实。HIR 与 MIR lowering
通过它查询已检查的语义；该类型不实现序列化，也不允许整表 Clone。

其中的 `ModuleTypes` 由两部分组成：

- `TypeTable`：结构和类布局、容器元素类型、effect ABI、ownership 等后端信息。
- `ModuleInterface`：导出签名与常量、泛型模板和请求、trait 声明/方法、导入身份
  和带类型的 struct 默认值。跨模块类型查询共享此接口及其对应布局。

`MirProgram` 只保留 `TypeTable`。`ModuleArtifact` 将接口独立保存，前端节点
映射在 lowering 后释放；后端不再根据源表达式的 NodeId 查询类型。

### 默认值边界

本模块的 struct 默认表达式保存在 `CheckedTypes`，供 HIR lowering 使用。
导入接口中的默认值转换为可重定位的 `ConstantValue`，不再依赖定义模块的
NodeId 或 AST 类型查询。合法但不能导出的计算默认值保留延迟诊断：本模块
仍可使用它，实际导入对应类型时报告原有的 unsupported constant 错误。
`@repr(c)` 禁止默认值的规则在源语义检查中验证，布局检查只负责 C 布局。

### 使用示例

```rust
let checked = check_program(&program)?;
let expr_type = checked.get(&expression); // 供 lowering 查询
let core = CoreProgram::lower(program, checked)?;
let mir = MirProgram::lower(&core)?;      // mir.types() 不包含源节点表
```

## 设计决策

### 为什么使用 NodeId 而不是 Span？

**问题**: Span 可能重复
```joky
let x = 1;  // Span(8, 9)
let y = 1;  // Span(8, 9) - 相同位置，不同节点
```

**解决**: 使用 NodeId
- 每个节点有唯一的 ID
- CheckedTypes 中的表达式事实使用 NodeId 作为键
- 避免冲突

### 为什么使用 TypeExpectation？

**问题**: `Option<Type>` 语义模糊
```rust
// Some(ty) 是"提示"还是"要求"？
self.check_expression(expr, Some(Type::I64))?;
```

**解决**: TypeExpectation
```rust
self.check_expression(expr, TypeExpectation::hint(Type::I64))?;   // 提示
self.check_expression(expr, TypeExpectation::require(Type::I64))?; // 要求
```

### 为什么不支持隐式转换？

**原因**:
- 避免意外的精度损失
- 更清晰的代码
- 更容易推理

**示例**:
```joky
let x: Int64 = 42;
let y: Int32 = x;  // 错误：不允许隐式转换
```

## 未来扩展

### 1. 类型推断改进

- 双向类型推断
- 更智能的字面量推断
- Effect 集合和函数类型中的 Effect 推断

### 2. 高级特性

- 闭包和结构化函数类型
- Cown/`when` 的 lease 类型检查
- scope/branch/race 的任务与 ownership 检查
- 生命周期和所有权系统

## 相关文档

- [架构文档](architecture.md)
- [错误处理](error-handling.md)

## 参考资料

- [Hindley-Milner Type System](https://en.wikipedia.org/wiki/Hindley%E2%80%93Milner_type_system)
- [Rust Type System](https://doc.rust-lang.org/reference/type-system.html)
- [TypeScript Type Inference](https://www.typescriptlang.org/docs/handbook/type-inference.html)
