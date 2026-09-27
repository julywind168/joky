# 语言与内置模块

本文描述已实现的语言子集。运行命令见 [README](../README.md)，
嵌入与 ABI 约束见 [架构说明](architecture.md)。
目标语法和完整类型系统另见 [语法与类型系统设计草案](syntax.md)，
其中尚未实现的规则不改变本文描述的当前行为。

## 类型与函数

类型包括 `Int32`、`Bool`、`Unit`、不可变 UTF-8 `String` 和注册过的宿主 opaque 类型。
函数最后一个表达式决定返回值；不写返回注解时为 Unit。普通函数不能调用 `@suspends` 函数。
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
可以修改外层 `var`，内部同名声明会遮蔽外层绑定。声明、赋值和 `while` 的结果
都是 `Unit`；循环体最后一个表达式的值会被丢弃。

运算符从高到低为一元 `-` / `!`、乘除、加减、比较、赋值、分号序列。
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
目前未提供 `break`、`continue`、`+=`。

[examples/loops.jk](../examples/loops.jk) 计算 0 到 9 的和，输出 `45`：

```bash
cargo run -- run examples/loops.jk
cargo run -- run --interpreter examples/loops.jk
```

### 分号与表达式序列

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

序列及圆括号不创建新作用域，声明保留在当前作用域；只有 `{ ... }` 引入块作用域。
实参之间用逗号分隔，`f(prepare(); value, other)` 有两个实参，第一项是序列表达式；
也可写成 `f((prepare(); value), other)` 强调分组。
块内允许空分号，顶层 import、extern 和函数声明后也可加分号。
`@suspends` 序列恢复时不会重放此前的表达式。

[examples/sequences.jk](../examples/sequences.jk) 演示该循环，依次输出 `4`、`3`。

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
