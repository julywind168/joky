# 命令行使用

先按 [README](../README.md#安装与产物) 安装 Joky；以下 `cargo run` 命令在源码仓库根目录执行，安装后可将 `cargo run --` 换成 `joky`。
完整参数见 `joky --help`，首次写程序见[入门教程](getting-started.md)。

## 创建项目与查看帮助

`joky --version` 输出简短版本；`joky --version --verbose`（也可用 `-V -v`）输出构建提交、
源码状态、目标平台、Cargo 构建模式、优化等级、Debug 断言状态、rustc 与最低 Rust 版本。
这些信息在编译时写入，安装后不依赖 Git 或 Rust。源码包没有 Git 信息时，提交与源码状态显示 `unknown`。
`source-state` 只检查编译器、工具、头文件、内嵌站点及构建配置等源码输入；文档或测试的独立改动不使它变为 `modified`。
构建模式沿用 Cargo 的 `debug`／`release` 分类，自定义 profile 的实际优化与断言配置应结合对应字段判断。

```sh
joky init hello
joky run hello
cd hello
joky check
joky fmt --check
```

`joky init [目录]` 创建最小应用：`joky.toml` 和 `src/main.jk`，运行输出 `Hello, Joky!`。
省略目录时在当前目录初始化；现有文件不覆盖，已有其他内容的目录可保留使用。
项目名默认采用目录名；目录名不符合 ASCII 字母、数字、`_`、`-` 规则时使用 `joky-app`，
也可通过 `--name <名称>` 指定。初始化不安装依赖，也不创建 Git 仓库。

`joky <命令> --help`、`joky <命令> -h` 和 `joky help <命令>` 显示该命令的参数与示例；
`joky --help` 显示总览。没有项目且未提供输入时，`joky run` 提示创建项目或运行单文件的示例。
已有项目中，`run`、`build`、`check`、`test`、`fmt`、`explain-parallel` 均可省略输入，
从当前目录向上寻找最近的 `joky.toml`。

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

`joky build` 在 stderr 依次显示 `Compiling`、`Linking`、`Finished`：
开始编译时显示输入路径，启动系统链接器前显示产物名，成功发布后显示产物路径与总构建耗时。
失败时停止在对应阶段，不输出 `Finished`。状态词在支持颜色的终端中加粗显示为绿色，支持 `NO_COLOR`。

## 严格同步栈准入

macOS arm64 可在 `joky run` 或 `joky build` 中添加 `--strict-native-stack`，单文件和项目均支持。
它在加载／编译时拒绝预算尚未覆盖的调用及清理依赖，执行时在拥有参数交接前检查间接目标地址。
默认关闭；当前只支持有限的同步计算和 String 等路径，I/O、宿主资源、任务及许多 std 操作会明确报错。
不能与 `--interpreter`、`--parallel` 或 `--task-workers` 组合；其他平台拒绝开启。
`check` 不接受此选项，静态语义检查通过不代表严格原生准入通过。
范围、嵌入 API 与验收边界见[严格同步准入](architecture.md#严格同步准入的首批范围)。

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
joky fmt src                 # 递归格式化目录内的源码
joky fmt                     # 自动发现项目，只格式化自身 source-root
joky fmt app/joky.toml       # 显式选择应用项目或库包
joky fmt --check file.jk     # 不写入；需要格式化时退出码为 1
joky fmt --check .           # 列出选中范围内所有需要格式化的文件
joky fmt --diff src          # 预览统一 diff，不写回；有差异时退出码为 1
joky fmt --stdin < file.jk   # 从标准输入读取，向标准输出写出
```

格式化使用语法信息区分泛型尖括号、比较与移位、导入路径与除法，以及闭包参数和
按位运算。非空函数、控制流、`impl` / `trait` 和分支块展开；空块保持 `{}`，
短记录和数组可保持单行布局。块内调用、声明、赋值和跳转等可独立成句的非末项，
仅用于连接下一条语句的分号改为换行。纯计算的显式丢弃、块末尾的分号和括号内的分号序列保留；
`match` / `join` / `race` 分支中的分号序列保持在同一分支内。

使用四空格缩进，以 100 个 Unicode 字符为行宽软上限。长参数、实参、数组、记录与泛型列表
按项展开；实参含非空多行块（例如闭包）时，外层调用也按项展开。
长表达式在运算符后续行；方法链一旦换行，同一条链的各段统一在点号前续行。续行额外缩进一级。
字面量、注释、标识符及明确跳过的区域可以超过软上限，不拆分它们的原文。
保留表达式之间已有的换行，连续空行最多保留一个，去除文件头和块首尾的空行。
顶层函数与相邻声明之间留一个空行，声明前的属性和注释保持跟随声明；
`else` 前没有注释阻隔时统一为 `} else {`。
新增换行使用文件中首个换行符的风格，无换行时使用 LF。
清理代码行尾空白，普通非空结果以一个换行结束。常规星号块注释只调整各行 `*` 前的缩进，
保留星号后的内容及空白；其他注释、字符与字符串原文
（包括插值内部和三引号字符串的缩进）不改写，原有行尾注释跟随原来的代码。
已有多行列表保持按项展开，不强制折回单行；导入顺序不变。

需要手工排版的部分使用独立成行、配对的注释标记，标记行及其间的完整源码原样保留：

```joky
// fmt: off
fn manually_spaced( value: Int32 ) -> Int32 { value }
// fmt: on
```

区域内仍须是有效语法。标记不嵌套，缺少配对标记时报告错误并停止写回；
字符串、块注释和代码行末的相同文本不作为指令。跳过区域位于文件末尾时也保留其原有末尾换行情况。

目录模式递归处理 `.jk` 文件，跳过隐藏目录、`target`、`dist`、嵌套项目与符号链接。
项目目录或清单指定了 `source-root` 时只处理该目录，不加载 path 依赖，不校验入口及运行时资源；
应用和库包均支持。普通目录不需要清单；空目录成功退出，不修改任何文件。

`fmt` 只要求语法有效，不解析依赖、不要求 `main`、不初始化 VM。
先按语法确认并转换可省略的块级分隔分号，再重新解析，严格核对其余 token、
注释内容（仅允许上述星号前的缩进变化）和以 token 位置表示的声明与表达式结构；此过程不推断调用的返回类型。
泛型关闭符 `> >` 在语法允许时规范为 `>>`；尾随逗号后的必要分隔空格保留，
比较和移位运算不因此改变。
先读取并格式化所有选中文件；任一文件有语法错误或结构校验失败时整批不写回。
写入阶段逐文件通过同目录临时文件完整写入后替换，并核对文件未被并发修改；
保留权限，显式选择单个符号链接文件时写入其目标。写入阶段的 I/O 错误不会回滚此前已成功替换的其他文件。
`--check` 可与 `--stdin` 组合，此时只返回状态，不输出格式化内容。
`--diff` 支持文件、目录、项目和 `--stdin`，向 stdout 输出统一 diff；有差异时返回 1，
已符合格式时返回 0 且无输出，也可与 `--check` 同用。预览同样先检查全部选中文件，
任一文件失败时不输出其他文件的部分 diff。预览将相距较远的变化拆成多个 hunk，各带前后三行上下文；上下文相接或重叠的变化合并展示。
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
