# 模块系统

模块系统位于 `src/module.rs`，负责 package 内 `.jk` 文件的发现、导入解析、函数/常量导出元数据和依赖环检测。它与 HIR/MIR lowering 解耦：模块图先确定稳定的 `ModuleId`，编译流水线再处理源码。

## Package 布局

```text
foo/
├── joky.toml       # name = "foo"
└── src/
    ├── main.jk
    └── user.jk
```

每个 `.jk` 文件是一个模块。`import user` 解析到当前 package 的 `src/user.jk`；`import joky/string` 解析到仓库内置标准库的 `std/joky/string.jk`。路径组件不能包含空组件、`.` 或 `..`。

## ModuleGraph

```rust
pub struct ModuleGraph {
    graph: DiGraph<ModuleId, ()>,
    nodes: Vec<ModuleInfo>,
    by_path: HashMap<PathBuf, ModuleId>,
}

pub struct ModuleInfo {
    pub id: ModuleId,
    pub path: PathBuf,
    pub imports: Vec<(String, ModuleId)>,
    pub exports: HashMap<String, Visibility>,
}
```

`ModuleGraph::load(entry, package_root)` 从入口递归发现文件，规范化路径并去重。每条边表示“当前模块依赖被导入模块”。模块导出的函数名、常量名和 `Visibility` 从 AST 收集；普通 `fn`/`const` 是私有，`pub fn`/`pub const` 才能作为跨模块 API。

## 循环检测

依赖边使用 `petgraph::DiGraph` 保存，`kosaraju_scc` 计算强连通分量。大小大于 1 的分量表示多模块循环；单节点自环也会拒绝。错误在源码 lowering 前返回，因此不会让后端看到不完整的符号环境。

```mermaid
flowchart TD
    entry[entry module] --> discover[discover imports]
    discover --> graph[ModuleGraph + ModuleId]
    graph --> scc[kosaraju_scc]
    scc -->|acyclic| exports[export metadata]
    scc -->|cycle| diagnostic[diagnostic error]
    exports --> lowering[AST / Sema / HIR / MIR]
```

## 当前编译边界

`joky check` / `joky run` / `joky build` 默认走模块独立编译：`ModuleGraph` 解析导入后，`Frontend` 按模块检查、实例化泛型、读写 `.joky/cache/` 中的 `.jabi` / `.jmir`，再链接成一份已验证 `MirProgram`。`Compiler::run_program` 仍是单段源码的嵌入入口（`joky run --legacy` 也走这条路径）。JIT 每次 `run` 生成机器码；AOT 复用同一套 MIR 缓存。循环依赖仍在 lowering 前拒绝。

## 示例

```joky
// src/user.jk
pub const GREETING: String = "hello "

pub fn greet(name: String) -> String {
    GREETING.concat(name)
}

// src/main.jk
import user

fn main() {
    println(user.GREETING)
    println(user.greet(name: "Joky"))
}
```
