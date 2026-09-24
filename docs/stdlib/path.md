# 路径

`import joky/path` 提供纯函数，不需要 effect。路径使用 `String`，按运行平台的
路径规则处理，不读取文件系统，不展开 `~` 或环境变量，也不解析符号链接。

| API | 返回值 | 语义 |
| --- | --- | --- |
| `path.join(base, child)` | `String` | 拼接路径；绝对子路径通常替换 base，Windows 盘符/根路径遵循原生规则 |
| `path.is_absolute(value)` | `Bool` | 是否为完整绝对路径 |
| `path.parent(value)` | `Option(String)` | 父路径；`parent("file")` 为 `Some("")`，根路径为 `None` |
| `path.file_name(value)` | `Option(String)` | 最后一个普通路径组件 |
| `path.file_stem(value)` | `Option(String)` | 文件名移除最后一个扩展名 |
| `path.extension(value)` | `Option(String)` | 不包含点；`.gitignore` 为 `None`，`file.` 为 `Some("")` |
| `path.with_extension(value, extension)` | `String` | 替换最后一个扩展名；空字符串移除扩展名 |
| `path.strip_prefix(value, base)` | `Option(String)` | 按组件移除前缀；不匹配返回 `None` |
| `path.split_paths(value)` | `List(String)` | 拆分 PATH 风格的搜索路径；保留空组件 |
| `path.join_paths(values)` | `Result(String, String)` | 合并搜索路径；不可表示的组件返回 `Err` |

`..` 保持原样，不做可能改变符号链接含义的折叠。`strip_prefix` 比较完整组件，
例如 Unix 上 `/app2/file` 不包含 `/app` 前缀。重复分隔符和 `.` 的组件比较遵循
Rust `std::path::Path` 的规则；这不是规范化或路径安全检查接口。

`with_extension` 的扩展名不得含平台路径分隔符，违反此前置条件会终止程序。
`join_paths` 在 Unix 上拒绝包含 `:` 的组件；Windows 使用 `;`，并遵循原生引号规则。
路径 String 可以表示不存在的路径；实际访问时的 NUL、权限等错误由 I/O API 报告。

```joky
import joky/path

fn main() {
    let source = path.join("src", "main.jk")
    println(path.with_extension(source, "o"))
    println(path.file_stem(source)!)
}
```

路径语法由执行平台决定，暂不提供单独的 POSIX/Windows 模式。
