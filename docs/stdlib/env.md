# 运行环境

`import joky/env`。函数声明 `effects { env }` 后即可读取环境变量、程序参数和目录。
五个 operation 均为 `@suspends`，通过现有 provider 机制调度，无需 `await`。

| API | 返回值 | 语义 |
| --- | --- | --- |
| `env.get(name)` | `Result(Option(String), String)` | 缺失返回 `Ok(None)`，空值返回 `Ok(Some(""))` |
| `env.vars()` | `Result(Map(String, String), String)` | 环境变量的独立快照，不保证遍历顺序 |
| `env.args()` | `Result(List(String), String)` | 用户程序参数，不包含程序名 |
| `env.current_dir()` | `Result(String, String)` | 查询调用时的工作目录，返回绝对路径 |
| `env.temp_dir()` | `Result(String, String)` | 系统选择的临时目录路径，不创建目录、不保证存在或可写 |

空变量名、包含 `=` 或 NUL 的变量名返回 `Err`；名称比较遵循平台规则
（Unix 区分大小写，Windows 不区分大小写）。所有返回的 String 严格使用 UTF-8，
不进行有损替换。`get` 只校验请求的值；`vars` 或 `args` 中任一项无法表示为
String，整次调用返回 `Err`。错误类型沿用文件 API 的 String。

环境变量在每次运行的 runtime scope 创建时捕获，`get` 和 `vars` 读取同一份快照，
宿主之后修改进程环境不会改变它。callback scope 共享父运行的快照和参数。
返回的容器遵循普通不可变 `Map` / `List` 语义，修改容器不修改进程环境。

## 参数透传

```sh
joky run app.jk -- a b
joky run -- a b                    # 当前 package 的 src/main.jk
joky run --legacy app.jk -- a b
joky build app.jk -o app
./app a b
```

上述程序的 `env.args()` 均返回 `List("a", "b")`。`--` 之后的内容不会再被 CLI
解释为选项；空参数和非 Unicode 的原始参数也会保留，后者在调用 `env.args()`
时返回 `Err`。不传参数时得到空 List。嵌入式 Compiler 默认参数为空，宿主可使用
`run_program_with_args` 或 `run_modular_program_with_cache_and_args` 显式传入参数。

## 测试中替换环境

本地 handler 优先于 native provider，包括通过辅助函数发起的请求。

```joky
import joky/env

fn fixture(name: String) -> Result(Option(String), String) {
    if name == "APP_MODE" { Ok(Some("test")) } else { Ok(None) }
}

fn read_mode() -> Result(Option(String), String) effects { env } {
    env.get("APP_MODE")
}

fn main() {
    let mode = do { read_mode() } with {
        env.get(name) => fixture(name)
    }
    println(mode!.unwrap_or("development"))
}
```

本模块不提供修改环境变量或工作目录的接口。示例见
[examples/io/env.jk](../../examples/io/env.jk)。当前 AOT runtime ABI 为 v25，旧 runtime
archive 需重新构建。
