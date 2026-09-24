# HIR（High-level IR）

Joky 的 HIR 在代码中称为 `CoreProgram`，定义于 `src/hir/mod.rs`。它是语义分析和 CFG lowering 之间的稳定边界：源码已经通过类型检查，但仍保留适合表达式语言的树形结构。

## 为什么需要 HIR

AST 需要忠实保存语法，不适合作为后端契约：同一语义可能有多种语法写法，AST 也没有可靠的类型信息。HIR 将这些差异收敛为少量 `CoreExprKind`，并让每个 `CoreExpr` 携带 `Type`，因此 MIR 不必重新解释源码语法。

## 核心数据结构

```rust
pub(crate) struct CoreProgram {
    types: CheckedTypes,
    functions: Vec<CoreFunction>,
    struct_defaults: Vec<Vec<Option<CoreExpr>>>,
    class_defaults: Vec<Vec<Option<CoreExpr>>>,
}

pub(crate) struct CoreFunction {
    id: CoreFunctionId,
    module: CoreModuleId,
    name: String,
    visibility: Visibility,
    receiver: Option<Type>,
    receiver_mode: ReceiverMode,
    parameters: Vec<CoreParameter>,
    parameter_ownership: Vec<Option<CoreParameterOwnership>>,
    return_type: Type,
    effects: EffectSet,
    may_suspend: bool,
    body: CoreExpr,
}

pub(crate) struct CoreExpr {
    id: NodeId,
    ty: Type,
    kind: CoreExprKind,
}
```

`CoreExprKind` 覆盖字面量、名称、绑定、调用、元组、结构体构造、字段访问、赋值、`if`、`match`、循环、`break`、`continue`、block，以及闭包、`do ... with`、`scope`、`branch`、`race` 和 `when`。模式被规范化为 `CorePattern`，调用参数保留可选标签，字段默认值单独存于 program 中。每个可能挂起的函数还携带 Effect 集合和 `may_suspend` 信息。

## Lowering

`CoreProgram::lower(program, types)` 遍历 AST，并从 `CheckedTypes` 取出已解析类型。它同时为普通函数、struct 方法和 class 方法分配稳定的函数序号；导入模块的临时 `m<ID>_` 名称在 lowering 时恢复为 `CoreModuleId`。

```text
AST + CheckedTypes
       |
       v
CoreProgram::lower
       |
       +-- typed CoreFunction / CoreExpr
       +-- normalized CorePattern
       +-- struct/class default expressions
```

HIR 不展开控制流，也不生成运行时调用。下一步 [MIR lowering](mir.md#从-hir-到-mir) 才会把表达式树拆成基本块和显式值操作。

`CheckedTypes` 的局部绑定、函数 effect 和导入调用事实仍供 MIR lowering 使用。
生成 `MirProgram` 时只取其中的布局 `TypeTable`；模块接口由驱动器单独保存，
源码节点表随 `CoreProgram` 释放，不成为 MIR 或缓存数据。

## 设计约束

1. HIR 必须是类型化的：新增 `CoreExprKind` 时要同时定义其结果类型和 lowering 规则。
2. HIR 可以保留语言级概念（例如 `Match` 和 `Unwrap`），但不能让 codegen 依赖 AST 的语法节点。
3. HIR 的所有权信息由 MIR 根据 `TypeTable::is_owned` / `is_shared` 进一步决定；HIR 本身不模拟 drop 顺序。
