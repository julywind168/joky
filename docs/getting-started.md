# 从单文件到第一个 Joky 项目

先完成 [README 的安装](../README.md#安装与产物)，确认 `joky --version` 可用。
本教程不需要数据库、网络服务或编辑器插件；原生构建一步需要系统 `cc`。
每节的 Joky 代码块都是完整程序：保存为所示文件名，然后运行对应命令。
除特别说明外，命令在保存文件的目录执行。

想直接创建项目，可以先执行：

```sh
joky init hello
cd hello
joky run
joky check
joky fmt --check
```

初始化生成 `joky.toml` 和 `src/main.jk`，首次运行输出 `Hello, Joky!`。
后续可直接编辑 `src/main.jk`；`joky run --help` 查看运行命令的参数和示例。
下面继续使用单文件示例，便于分别试验每项语言能力。

## 1. 变量与函数

保存为 `hello.jk`：

```joky
fn add(left: Int32, right: Int32) -> Int32 {
    left + right
}

fn main() {
    let name = "Joky"
    var total = add(20, 21)
    total += 1
    println("Hello, {name}!")
    println(total)
}
```

```sh
joky run hello.jk
```

预期输出：

```text
Hello, Joky!
42
```

`let` 创建不可重新赋值的绑定，`var` 允许重新赋值。函数块的末表达式是返回值；
不写 `-> 类型` 时返回 Unit。`main` 是程序入口，不带参数。
普通字符串支持 `{name}` 插值。默认 prelude 已提供 `println`，不需要手动导入。

## 2. 容器与遍历

保存为 `collections.jk`：

```joky
fn main() {
    let scores = [10, 20, 30]
    var total = 0
    for score in &scores {
        total += *score
    }
    println(total)
    println(scores.len())
}
```

执行 `joky run collections.jk`，预期两行输出为 `60`、`3`。
数组元素类型从字面量推导。`&scores` 借用数组，循环中的 `score` 是引用，
`*score` 读取它的值；遍历后仍可使用数组。
写 `for score in scores` 则消费数组，后面不能继续使用它。
进阶遍历见[for 示例](../examples/for.jk)。

## 3. 所有权与借用

保存为 `ownership.jk`：

```joky
fn greet(name: &String) {
    println(name)
}

fn main() {
    let name = "Joky"
    greet(&name)
    let renamed = name
    println(renamed)
}
```

执行 `joky run ownership.jk`，预期两行均为 `Joky`。
`greet` 只借用字符串，不拿走所有权。`let renamed = name` 则移动字符串，之后应使用 `renamed`。

可以在末尾尝试添加 `println(name)`，再运行 `joky check ownership.jk`：它应报移动后使用错误。
删除这一行即可恢复。遇到这种错误，先判断调用方是否还需要这个值：只读操作通常应借用，
需要接管资源的操作才按值传递。详细规则见[语言参考](language.md)。

## 4. 用 Result 表达可能失败的操作

保存为 `results.jk`：

```joky
fn half(value: Int32) -> Result<Int32, String> {
    if value % 2 == 0 {
        Ok(value / 2)
    } else {
        Err("expected an even number")
    }
}

fn main() {
    let result = half(84)
    match result {
        Ok(value) => println(value),
        Err(message) => println(message),
    }
}
```

执行 `joky run results.jk`，预期输出 `42`。
将 `84` 改为 `83` 后，输出 `expected an even number`。
这是程序显式处理的业务失败，程序仍正常退出。
`Result`、`Ok`、`Err` 默认可用；需要在返回 Result 的函数内提前传播错误时可使用 `?`，
见 [Option / Result 示例](../examples/option_result.jk)。

## 5. 创建项目并写测试

在新目录创建以下布局；当前还没有 `joky init`，直接创建两个文件即可：

```text
first-joky/
├── joky.toml
└── src/
    └── main.jk
```

`joky.toml`：

```toml
[project]
name = "first-joky"
entry = "src/main.jk"
source-root = "src"
```

`src/main.jk`：

```joky
fn add(left: Int32, right: Int32) -> Int32 {
    left + right
}

#test
fn adds_numbers() {
    assert_eq(add(20, 22), 42)
}

fn main() {
    println(add(20, 22))
}
```

在 `first-joky` 目录执行：

```sh
joky check
joky fmt src/main.jk
joky test
joky run
joky build
./dist/first-joky
```

`check` 成功时无输出；`test` 应报告 1 个测试通过；两次运行均输出 `42`。
省略输入时，主要项目命令向上查找最近的 `joky.toml`；`fmt` 目前仍需要指定单个文件。
`check` 不检查 `#test` 函数体，因此修改测试后还需要执行 `joky test`。
项目构建生成 `dist/` 目录包，可以整体搬动。

拆分模块和本地依赖时，参考[包示例](../examples/packages/app/joky.toml)与
[命令行指南](cli.md#本地源码依赖)。目前支持本地 path 依赖，尚无 registry 下载与锁文件。

## 6. 可挂起任务

保存为 `tasks.jk`：

```joky
import joky/task

fn main() @suspends {
    println("Starting")
    task.sleep(10ms)
    println("Ready")
}
```

执行 `joky run tasks.jk`，预期依次输出 `Starting`、`Ready`。
`@suspends` 表示函数可能挂起，调用方式仍是普通函数调用，不使用 `async` / `await`。
`10ms` 是 Duration 字面量；等待期间运行时可推进其他就绪任务。
默认使用本地任务执行模式；可选并行模式的准入和限制见[任务执行模式](language.md#任务执行模式)。

## 继续探索

- 对上述单文件使用 `joky run --interpreter 文件.jk` 可切换到解释器。
- 使用 `joky build 文件.jk -o 程序名` 生成原生程序；输出目录需要事先存在。
- 使用 `joky doc --open` 查看离线标准库文档，语言规则以[语言参考](language.md)为准。
- 读取参数和标准输入、测试筛选、格式化选项见[命令行指南](cli.md)。
- 需要宿主嵌入时，从 [Rust 示例](../examples/embedding.rs)和[架构说明](architecture.md)开始。
