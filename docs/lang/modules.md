# Package 与模块

Joky package 是源码、清单和标准入口的最小发布单元：

```text
foo/
├── joky.toml
└── src/
    └── main.jk
```

`joky.toml` 当前只需要声明包名：

```toml
name = "foo"
```

使用 `joky new foo` 创建新 package。进入 package 根目录后，`joky run` 读取 `src/main.jk` 并执行其中的 `main` 函数；仍然可以使用 `joky run path/to/file.jk` 直接运行单个源码文件。

每个 `.jk` 文件对应一个模块。后续模块系统使用文件路径解析导入：

```joky
import joky/string // 标准库模块
import user        // 当前 package 的 src/user.jk
```

编译器首先建立 `ModuleGraph`：每个模块获得稳定的 `ModuleId`，边记录已解析的导入路径，模块符号表记录函数、常量及其 `Visibility`。`import user` 解析到当前 package 的 `src/user.jk`，`import joky/string` 解析到内置标准库的 `std/joky/string.jk`。`pub fn` 和 `pub const` 可以通过模块限定名访问，普通 `fn` 和 `const` 保持模块私有。当前 Cranelift 后端仍将图中的模块按拓扑序合并到一次编译单元中，但模块身份和导出信息已经在源码展开前固定，不再依赖逐行扫描导入声明。

编译前会先构建 package 的模块依赖图，使用强连通分量检测多模块循环和自循环，并在展开源码前报告依赖环。

管道表达式可以组合导入模块中的函数：

```joky
import joky/string

let ok = "hello" |> string.starts_with(prefix: "he")
```

模块常量可以省略类型标注；省略时使用与 `let` 相同的规则推断类型。值可以由字面量、常量引用、纯一元/二元运算、元组和 [include_bytes](values.md#编译期资源嵌入) 构成。常量在每个使用点内联，不会生成运行时全局初始化：

```joky
// src/config.jk
pub const PORT = 8080

// src/main.jk
import config

fn main() {
    println(config.PORT)
}
```
