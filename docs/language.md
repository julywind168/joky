# 语言与内置模块

本文描述已实现的语言子集。运行命令见 [README](../README.md)，
嵌入与 ABI 约束见 [架构说明](architecture.md)。
目标语法和完整类型系统另见 [语法与类型系统设计草案](syntax.md)，
其中尚未实现的规则不改变本文描述的当前行为。

## 类型与函数

类型包括 `Int32`、`Bool`、`Unit`、不可变 UTF-8 `String` 和注册过的宿主 opaque 类型。
函数正常结束时最后一个表达式决定返回值，也可用 `return` 提前返回；不写返回注解时为 Unit。
普通函数不能调用 `@suspends` 函数。
`extern fn` 声明宿主实现，完整签名和挂起约定必须匹配注册信息。
String 支持字面量、参数和返回值，尚无插值、拼接或动态分配。

## 可变变量与循环

`let` 声明不可重新赋值的绑定，`var` 声明可重新赋值的绑定；两者都必须初始化，
支持类型推导或显式注解。赋值不能改变绑定的类型，函数参数不可重新赋值。

```joky
fn sum(n: Int32) -> Int32 {
    var i: Int32 = 0
    var total = 0
    while i < n {
        total = total + i
        i = i + 1
    }
    total
}
```

`while` 每轮先求值 `Bool` 条件，为 false 时退出。循环体有独立块作用域，
可以修改外层 `var`，内部同名声明会遮蔽外层绑定。声明、赋值和 `while` 正常结束时的结果
都是 `Unit`；循环体正常结束时最后一个表达式的值会被丢弃。

运算符从高到低为一元 `-` / `!`、乘除、加减、比较、`&&`、`||`、赋值、分号序列。
一元运算右结合，四则运算同级左结合：`1 + 2 * 3` 为 7，`(1 + 2) * 3` 为 9。
`-` 只接受 Int32，`!` 只接受 Bool。Int32 加、减、乘和取负采用 32 位环绕，
`-2147483648` 合法，再次取负仍为自身；正的 `2147483648`、`-(2147483648)` 及更大字面量报错。

Int32 支持 `==`、`!=`、`<`、`<=`、`>`、`>=`，Bool 支持 `==` 和 `!=`，结果均为 Bool。
两侧必须同型，不支持 Bool 有序比较，也不支持 String、Unit 或 opaque 类型比较。
比较不可连续书写：`1 < 2 == true` 是语法错误，`(1 < 2) == true` 合法。
表达式和函数实参从左到右求值，后续赋值不会改变已经读取的值。

JIT、AOT 和解释器共享类型检查与控制流规划；整数除法异常的后端差异见
[架构边界](architecture.md#已知边界)。`@suspends` 函数的循环条件及循环体都可以挂起，
恢复时继续当前调用，不重复执行此前的赋值或副作用；循环中的 `scope` 每轮创建新的分支。
默认情况下循环本身不会自动挂起；Rust 嵌入接口可为解释器设置 `InterpreterLimits`，
启用指令总预算、异步执行时间片和帧数量限制，详见 [解释器文档](interpreter.md#resource-limits)。
目前未提供 `+=` 等复合赋值。

[examples/loops.jk](../examples/loops.jk) 计算 0 到 9 的和，输出 `45`：

```bash
cargo run -- run examples/loops.jk
cargo run -- run --interpreter examples/loops.jk
```

## 条件表达式与短路逻辑

`if condition { ... } else { ... }` 是表达式，条件必须为 Bool，只求值一次，
仅执行选中的分支。正常结束的分支结果类型必须一致，包括 Unit、String 和同名宿主 opaque 类型；
提前跳转的分支不参与结果类型合并。支持 `else if`；省略 else 时结果为 Unit，
then 分支正常结束时也必须为 Unit，不能隐式丢弃其末值。

```joky
fn classify(value: Int32) -> Int32 {
    if value < 0 { -1 } else if value == 0 { 0 } else { 1 }
}

fn safe_divide(value: Int32, divisor: Int32) -> Int32 {
    if divisor != 0 && value / divisor > 0 { value / divisor } else { 0 }
}
```

`&&` 和 `||` 正常求值的操作数必须为 Bool，分别在左侧为 false 和 true 时跳过右侧。
右侧也可提前跳转，例如 `done || (return 0)`。
`&&` 高于 `||`，两者低于比较、高于赋值；如 `true || false && false` 为 true。
跳过的表达式不会产生副作用、失败或挂起，但所有分支仍接受名称、类型及 `@suspends` 检查。

分支各有独立块作用域；if / while 条件内的新声明仅在条件内有效。
短路右侧的新声明仅在该操作数内有效，不能泄漏到后续表达式；对已有外层 var 的赋值仍然生效。
圆括号与分号本身不创建作用域。条件、选中的分支及短路右侧都可挂起，
恢复时继续当前表达式，不重算条件或重放此前的副作用。

if 表达式可直接参与运算：`if true { 1 } else { 9 } + 2` 为 3。
else 可以换行，但不能用分号与 then 块分开。`||` 按最长匹配分词，不是两个 scope 分支符。
完整可运行示例见 [examples/conditionals.jk](../examples/conditionals.jk)。

## 分号与表达式序列

`;` 是可省略的表达式连接运算符：`a; b` 从左到右执行，丢弃 `a` 的值，
以 `b` 的值和类型作为整个序列的结果。它的优先级低于赋值，也可用于显式分隔相邻表达式。
括号用于将序列作为一个表达式，例如放进循环条件：

```joky
var checks = 0;
var bodies = 0
while (checks = checks + 1; checks < 4) {
    bodies = bodies + 1;
    999
}
```

结束时 `checks` 为 4，`bodies` 为 3。同一行可以写 `var x = 1; x = x + 1;`，
也可以写成 `var x = 1 x = x + 1`；语法可区分的相邻表达式不要求换行或分号。
换行只是空白，不自动插入分号。例如 `1 +` 换行接 `2` 仍是一个表达式；
`5` 换行接 `-2` 是减法，结果为 3。需要连接两个独立表达式时写 `5; -2`，结果为 -2。

尾随分号会丢弃最后一个值：`(1; 2)` 的结果是 Int32 的 2，`(1; 2;)` 和 `{ 42; }`
的结果是 Unit。因此循环条件最后不能加分号，否则会得到非 Bool 条件错误。
`let x = 1; next()` 中分号结束声明；要把序列放进初始值或赋值右侧，使用
`let x = (prepare(); value)` 或 `x = (prepare(); value)`。

序列及圆括号不创建新作用域，声明保留在当前作用域；`{ ... }` 引入块作用域。
条件和短路右侧的声明边界见上一节。
实参之间用逗号分隔，`f(prepare(); value, other)` 有两个实参，第一项是序列表达式；
也可写成 `f((prepare(); value), other)` 强调分组。
块内允许空分号，顶层 import、extern 和函数声明后也可加分号。
`@suspends` 序列恢复时不会重放此前的表达式。

[examples/sequences.jk](../examples/sequences.jk) 演示该循环，依次输出 `4`、`3`。

## 提前返回与循环跳转

`return value` 结束当前函数，值必须匹配声明的返回类型。裸 `return` 返回 Unit。
返回值按非序列表达式解析：`return (a; b)` 返回整个序列的值；`return a; b` 中的 `b`
不可达并报错。换行只是空白，`return` 换行接 `42` 仍返回 42；在 `;`、`}`、`)`、`,`、
scope 分支符 `|` 或输入末尾前才解析为裸 return。二元或一元运算中使用跳转需加括号或块。

```joky
fn positive(value: Int32) -> Int32 {
    if value <= 0 { return 0 }
    value
}
```

`break` 退出当前函数最近一层 `while`，`continue` 回到该循环条件重新求值。
二者不带值或标签，循环外使用报错；不能跳到调用者的循环。循环条件也在该循环的跳转范围内。
若条件在所有路径上都提前跳转，循环体必须为空：`while (break) {}` 正常结束，
`while (return 42) {}` 返回函数，`while (continue) {}` 持续重试条件。
条件可以正常产生 Bool 时，保守地认为循环可能结束，不推断 `while true` 必然无限执行。

跳转不产生正常值；尾随分号只丢弃正常值，`return 42;` 仍返回 42，`break;` 仍退出循环。
类型检查拒绝结构上不可达的后续表达式，包括序列、运算右侧和后续实参；不按常量折叠删除路径。
例如 `(return 1) + 2` 报不可达错误，`1 + (return 2)` 返回 2。
`if` 两条分支都提前跳转时，整个 if 也不会正常结束；仍可正常结束的路径必须满足原有类型约束。

`@suspends` 返回值和跳转之前的调用可多次挂起，恢复不重放已完成的求值。
失败或取消不会转为成功返回。scope 分支中的 return 仅结束该分支；调用者跳转前须等 scope 完成。
scope 的实参先于所有分支求值，实参中的跳转可以阻止整个 scope 启动。
完整示例 [examples/jumps.jk](../examples/jumps.jk) 依次输出 `0`、`4`、`13`。

## 内置标准库 `joky/vm`

直接在 Joky 程序中创建 VM、加载源码并调用函数：

```joky
import joky/vm

extern fn print_int(value: Int32)

fn main() -> Int32 {
    let engine: vm.Handle = vm.create()
    vm.load_source(engine, "fn add(x: Int32, y: Int32) -> Int32 { x + y }")
    print_int(vm.call_int32(engine, "add", 20, 22))
    vm.close(engine)
}
```

`vm.call_int32` 接收句柄、函数名和零到两个 Int32 参数。目标函数必须同步返回 Int32，
调用前检查完整签名。字符串中的花括号是普通文本，源码字符串不做插值。
`vm.create_interpreter()` 创建不生成机器码的解释器 VM；构建时的依赖仍见架构说明。
当前原生模块中的同步 Int32 函数可以作为宿主函数注册：

```joky
fn host_add(a: Int32, b: Int32) -> Int32 { a + b }
let status = vm.register_int32(engine, "host_add", host_add)
```

`vm.register_int32` 支持零到两个 `Int32` 参数；被加载的源码用同名 `extern fn` 声明即可调用。
作为函数值传入的 Joky 函数必须来自 JIT/AOT 原生代码；解释器函数不能转换成原生指针。
目标 VM 可以使用 JIT 或解释器，Rust 宿主也可以向两者注册受支持的原生函数。
注册和已加载的调用模块都会保留 JIT 代码所有权；替换注册只影响后续加载，
已有模块及 continuation 仍可调用旧入口。

| 接口 | 返回值与行为 |
|------|--------------|
| `vm.create()` | 创建默认 JIT `vm.Handle` |
| `vm.create_interpreter()` | 创建解释器 `vm.Handle` |
| `vm.close(handle)` | 释放句柄，成功返回 0，失败返回 1 |
| `vm.load_source(handle, source)` | 加载源码，成功返回 0，失败返回 1 |
| `vm.reload_source(handle, source)` | 替换模块，失败保留旧代码，状态码同上 |
| `vm.register_int32(handle, name, function)` | 注册当前模块中的同步 Int32 函数，成功返回 0 |
| `vm.call_int32(handle, name, ...)` | 返回函数结果；失败返回 0，并设置错误 |
| `vm.error_code()` | 当前线程最近一次操作的状态：0 成功，1 失败 |
| `vm.print_error()` | 将最近一次错误写到 stderr |

每次操作开始前清除上次错误，两个错误查询接口不清除错误。
调用结果为 0 时，通过 `vm.error_code()` 区分正常结果与失败；应在下一次操作前查询。
句柄属于创建它的线程，关闭后不会复用；复制句柄不会创建第二个 VM，使用完成后显式 close。
当前仅支持内置模块 `joky/vm`，尚无文件模块搜索、导入别名或标准库异步调用；
同步 `Int32` 宿主函数可通过 `vm.register_int32` 注册。

`@suspends` 函数可以用 `scope` 并发等待多个 `Unit` 分支：

```joky
fn main() @suspends {
    scope {
        | fetch_left()
        | fetch_right()
    }
}
```

每个分支必须是 `@suspends` 的函数调用；scope 轮询所有未完成分支，全部完成后继续执行。
分支失败会停止后续轮询并释放其他分支；解释器还立即释放整个执行状态。

[examples/vm.jk](../examples/vm.jk) 演示加载、调用、重载和关闭，依次输出 `42`、`100`：

```bash
cargo build --release
./target/release/joky run examples/vm.jk
./target/release/joky build examples/vm.jk -o /tmp/vm-demo
/tmp/vm-demo
```

原生构建使用配套 runtime archive；安装与查找规则见
[README](../README.md#安装与产物)。

C 嵌入接口见 [include/joky_vm.h](../include/joky_vm.h)，可运行示例为
[examples/vm_runtime.c](../examples/vm_runtime.c)。
