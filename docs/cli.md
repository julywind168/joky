# 命令行使用

先按 [README](../README.md#安装与产物) 安装 Joky；以下 `cargo run` 命令在源码仓库根目录执行，安装后可将 `cargo run --` 换成 `joky`。
完整参数见 `joky --help`，首次写程序见[入门教程](getting-started.md)。

## 程序输入与输出

程序可通过 `joky/process` 读取启动参数、只读环境快照和启动目录，通过 `joky/io` 读取有界 stdin。
例如：

```sh
cargo run -- run examples/dns_lookup.jk -- localhost
printf '%s\n' '{"answer":42}' | cargo run -- run examples/stdin_json.jk
printf 'one\ntwo' | JOKY_LINE_PREFIX='item: ' cargo run -- run examples/stdin_lines.jk
```

`--` 后为程序参数，原生程序同样使用 `./program -- args...`；参数不含程序名。
`print/println` 和 `eprint/eprintln` 每次写入后刷新，无换行提示也能立即输出。
挂起入口、错误与 EOF 规则见[程序输入与标准流](language.md#程序输入与标准流)。

## 源码静态检查

```sh
cargo run -- check examples/captured_pipeline.jk
cargo run -- check examples/chat
```

安装后可直接使用 `joky check <file.jk | project-dir | joky.toml>`；省略输入时向上查找最近的项目清单。
检查覆盖语法、类型、泛型约束、所有权、借用、`@noescape` 和挂起限制，包括未调用的生产函数及泛型模板；`#test` 函数体由 `joky test` 检查。
错误沿用源码位置和错误码，写入 stderr。`--no-prelude` 关闭隐式导入；以 `-` 开头的文件名放在 `--` 后。
应用项目检查入口及导入依赖；`[package]` 库包检查自身源码根中的全部模块；详细边界见[静态检查](language.md#静态检查)。

移动后再次使用拥有值时，诊断会关联绑定声明、使值失效的移动或消费操作，以及再次使用的位置。
分支路径和循环中的重复消费会带相应说明；重新赋值后只追踪新值的移动。
如果之前的操作只需读取值，可以考虑改为借用，并同步调整接收方的参数或绑定类型。
String 还会提示在较早的移动位置显式调用 `.clone()`，但这会分配新字符串；
其他类型不会被一概建议复制。诊断保留原错误码，额外位置和建议也可从 Rust `Error.labels()` / `Error.help()` 读取。

## 用 Joky 写测试

测试函数前加 `#test`，使用已有的 `assert(condition, message)`：

```joky
fn add(left: Int32, right: Int32) -> Int32 { left + right }

#test
fn adds_numbers() {
    assert(add(2, 3) == 5, "addition result")
}
```

```sh
cargo run -- test examples/testing.jk
cargo run -- test --interpreter examples/testing.jk
cargo run -- test examples/testing.jk --filter threshold
cargo run -- test tests/joky --list
cargo run -- test tests/joky
cargo run -- test examples/testing_inputs.jk
cargo run -- test examples/testing_output.jk
```

[完整示例](../examples/testing.jk)还包含捕获局部阈值的流水线和 `@suspends` 测试；
[标准库测试](../tests/joky/stdlib_test.jk)用 Joky 检查 Array、String、Option 和 Result 的公开行为。
每个测试使用独立进程和临时目录，失败后继续；默认超时 30 秒，可用 `--timeout-ms` 调整。
[输入注入示例](../examples/testing_inputs.jk)用 `joky/testing` 设置参数、环境和文本 stdin，
再用普通 `process` / `io` / `fs` API 运行应用并检查输出文件；临时文件由父进程清理。
[输出断言示例](../examples/testing_output.jk)用 `testing.stdout()` / `testing.stderr()` 查询累计输出，
再用 `assert_eq` 检查内容；查询不清空，`--nocapture` 仍可显示应用输出。
详细发现范围、诊断与首版限制见[原生测试](language.md#原生测试)。
编译器内部、拒绝诊断、ABI 和资源清理仍由 Rust 测试负责。

## 源码格式化

```sh
joky fmt file.jk             # 原地格式化一个文件
joky fmt --check file.jk     # 不写入；需要格式化时退出码为 1
joky fmt --stdin < file.jk   # 从标准输入读取，向标准输出写出
```

第一版采用保守规则：花括号、圆括号和方括号内按四空格缩进，统一常见标点与
运算符周围的空格，清理代码行尾空白，并为非空结果补齐末尾换行。保留已有换行、
空行及 LF/CRLF；注释、字符与字符串原文（包括插值内部和三引号字符串的缩进）不改写。
泛型尖括号、比较/移位与 `/` 两侧保留原有的有无空格关系；暂不做长行折行、
导入排序或递归目录格式化。

`fmt` 只要求语法有效，不解析依赖、不要求 `main`、不初始化 VM。
写回前核对 token 内容和换行位置，语法错误或格式化失败时不覆盖原文件；
文件通过同目录临时文件完整写入后替换，保留权限，符号链接写入其目标。
`--check` 可与 `--stdin` 组合，此时只返回状态，不输出格式化内容。
路径以 `-` 开头时使用 `joky fmt -- -file.jk`。
开发时无需安装，可用 `cargo run -- fmt --check file.jk`。


## 本地源码依赖

本地复用通过 `[dependencies]` 声明 `path`，独立库使用无入口的 `[package]` 清单。
示例：`joky run examples/packages/app`、`joky check examples/packages/common`、
`joky test examples/packages/common`，配置见 [应用清单](../examples/packages/app/joky.toml) 和
[库清单](../examples/packages/common/joky.toml)。

## TCP 状态域聊天室

[chat 项目](../examples/chat/README.md) 用 `joky.toml` 声明 host 入口、源码根和运行时资源。
在仓库根目录执行：

```sh
cargo build --bin joky --lib
target/debug/joky run examples/chat
# 原生项目构建，产物可整体搬动后直接运行：
target/debug/joky build examples/chat -o /tmp/chat-app --parallel
/tmp/chat-app/chat --task-workers 3
```

聊天端口为 `127.0.0.1:7777`，管理端口为 `127.0.0.1:7778`。
修改 `examples/chat/script/main.jk` 后向管理端发送 `reload`；`shutdown` 或 SIGINT/SIGTERM 停止准入并排空会话输出，5s 后取消剩余任务。
目录说明、协议、配置与验证命令见[应用说明](../examples/chat/README.md)。

预留模式示例：`cargo run -- run examples/reservation.jk`，演示并发重复请求、回复丢失后的幂等重试、
有界结果保留、预留到期回收与迟到回复校验，详见[语言参考](language.md#跨挂起业务的预留模式)。
