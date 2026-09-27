# 语言与内置模块

本文描述已实现的语言子集。运行命令见 [README](../README.md)，
嵌入与 ABI 约束见 [架构说明](architecture.md)。
目标语法和完整类型系统另见 [语法与类型系统设计草案](syntax.md)，
其中尚未实现的规则不改变本文描述的当前行为。

## 类型与函数

类型包括 `Int32`、`Bool`、`Unit`、不可变 UTF-8 `String`、名义记录、枚举和注册过的宿主 opaque 类型。
函数正常结束时最后一个表达式决定返回值，也可用 `return` 提前返回；不写返回注解时为 Unit。
普通函数不能调用 `@suspends` 函数。
`extern fn` 声明宿主实现，完整签名和挂起约定必须匹配注册信息。
String 支持字面量、参数和返回值，尚无插值、拼接或动态分配。
函数参数和调用实参之间必须写逗号，允许尾随逗号，例如 `fn f(x: Int32,)` 和 `f(42,)`；
空列表仍写 `()`，不允许开头逗号或连续逗号。换行和注释不能替代列表逗号。

## 类型别名

`type Name = Target` 在模块顶层声明透明别名，可以与函数声明交错，尾随分号可选：

```joky
type Count = Total
type Total = Int32

fn add(a: Count, b: Count) -> Total {
    let result: Total = a + b
    result
}
```

别名可前向引用其他别名，最终目标必须是 `Int32`、`Bool`、`Unit`、`String`、模块内记录、枚举或已注册的
宿主 opaque 类型，例如 `type Handle = vm.Handle`。别名不创建新类型、构造函数或运行时值，
`Count` 和 `Int32` 完全等价；不同 opaque 类型的身份仍然不同。
参数、返回值、局部注解、CLI 入口及 extern 签名均按展开后的类型检查，挂起约定不受影响。

别名名是单个标识符，与函数及局部变量使用独立命名空间；同一模块中别名名不能重复，
也不能覆盖基础类型、记录、枚举或已注册的同名 opaque 类型。所有别名都会检查，包括未使用的声明；
未知目标和循环引用报错，循环错误指出引用链及闭合环的位置。
当前不支持泛型别名、局部类型声明、限定别名声明名或独立的 newtype 声明；`struct` 可以表达名义包装类型。

别名仅属于当前源码模块，不进入宿主注册器；成功重载可重新定义别名，失败加载保留旧模块。
旧挂起任务继续使用编译时的类型和代码。Rust `function_signature` 返回展开后的签名，
注册宿主函数时仍使用实际基础或 opaque 类型。完整示例见 [aliases.jk](../examples/aliases.jk)。

## 记录

`struct` 在模块顶层声明名义记录，允许与别名、函数交错以及前向类型引用：

```joky
struct Point { x: Int32 y: Int32 }
type Position = Point

fn translate(p: Position, dx: Int32) -> Point {
    Point { x: p.x + dx, y: p.y }
}

fn example() -> Int32 {
    var p = Point { x: 1 y: 2 }
    let saved = p
    p.x = 10
    saved.x  // 1: copying a record preserves its value.
}
```

- 字段类型支持 `Int32`、`Bool`、`Unit`、嵌套记录及有限枚举，允许空记录 `struct Empty {}`。
  这些记录均按值复制，空记录仍保留名义类型身份，不等于 Unit。
  当前不接受 String 或宿主 opaque 字段；这些字段需要后续资源规则。
- 字段声明及构造项的逗号可选，允许尾随逗号；换行只作为空白。
  构造必须提供全部字段，不能重复、缺失或使用未知字段，不提供隐式默认值或字段简写。
  初始化按源码书写顺序求值，布局按声明顺序；Unit、空记录的初始化副作用也会执行。
- 字段访问支持 `p.x`、`shape.point.x`、`translate(p, 1).x` 和 `(Point { x: 1 y: 2 }).x`。
  `var` 根绑定允许字段赋值、嵌套字段赋值以及整个记录的替换；`let` 和函数参数不可修改。
  读取可变值时保留快照，后续初始化项或参数中的修改不影响已经求出的值。
- 不同声明的记录不能互换，即使名字或布局相同。重新加载源码创建新身份，
  原任务继续持有旧代码和类型。透明别名可用于注解，但构造仍使用原始记录名。
- 同步和 `@suspends` 函数都可传递和返回记录；分支、提前返回及 scope 参数遵循同样规则。
  `extern fn`、宿主注册和公开标量调用入口不接受记录签名，CLI main 仍限 Unit / Int32。
- 所有声明均检查，包括未使用的声明。直接或经别名形成的纯记录无限内联布局会报
  `recursive_record`；含枚举的环报 `recursive_type`，纯别名环仍报 `cyclic_type_alias`。当前每个记录最多展开为 65,536
  个标量槽，嵌套最多 256 层，超过限制报 `resource_limit`。
- 首版不支持泛型、方法、解构、默认值、整体相等比较或用户定义析构。

解析只依赖 token：名称之后的 `{}` 或以 `字段名:` 开始的花括号解析为构造，
其他花括号可作为相邻独立块；例如 `let x = 1 { let y = x y }` 保持原有行为。
因此 `value {}` 若意在先读变量再求空块，要写 `value; {}` 或 `(value) {}`。
`if` / `while` 条件及 `match` 被匹配表达式中未被括号、实参列表或内层块包住的构造受限，避免吞掉条件后的块；
使用 `if (Point { x: 1 y: 2 }).x > 0 { ... }` 明确边界。
`.` 字段访问优先于一元及二元运算；模块限定调用仍有效，局部值遮蔽模块名前缀时拒绝调用，
不把局部记录当作模块。完整示例见 [records.jk](../examples/records.jk)。

## 枚举与模式匹配

`enum` 声明模块内名义值类型，每个变体有零个或一个载荷；多个数据通过记录组合：

```joky
struct Point { x: Int32 y: Int32 }
enum Lookup { Missing Found(Point) }
type Result = Lookup

fn value(result: Result) -> Int32 {
    match result {
        | Lookup.Missing => 0
        | Found(point) => point.x + point.y
    }
}
fn answer() -> Int32 { value(Lookup.Found(Point { x: 19 y: 23 })) }
```

- 构造必须使用原声明的限定名：无载荷为 `Lookup.Missing`，有载荷为 `Lookup.Found(value)`。
  不接受 `Missing()`、省略限定前缀的构造或别名构造；`Done(Unit)` 仍须显式传入 `{}`。
  载荷类型限 Int32 / Bool / Unit / Copy 记录 / 有限嵌套枚举，所有这些值按值复制。
  记录和枚举可以相互组合，但不允许无限内联循环，暂不支持 String 或 opaque 载荷。
- 枚举至少一个变体，名称不能重复，`_` 保留给通配模式。变体声明的逗号可选；
  载荷类型和单个模式绑定允许尾随逗号。类型声明支持前向引用及透明别名。
  不同声明或不同次加载产生不同类型身份，即使名字、变体及布局相同。
- `match value { | pattern => expression ... }` 是表达式，被匹配值只求值一次，类型必须为枚举。
  模式支持 `Variant`、`Enum.Variant`、`Variant(binding)`、`Variant(_)` 和整个分支的 `_`。
  无限定模式由被匹配类型确定；有限定模式须使用原声明名，载荷模式必须匹配声明的有无。
  载荷绑定不可变，分支及被匹配表达式中的局部声明不会泄漏；修改外层 var 不改变已读取的快照。
- 必须穷尽所有变体，否则报 `non_exhaustive_match` 并列出缺失项；`_` 覆盖剩余变体。
  重复变体、完整覆盖后或 `_` 后继续写分支均报 `unreachable_code`。
  未知变体、错误限定名及模式载荷数量错误报 `invalid_pattern`。
- 所有正常完成的分支必须同型，也可返回 Unit、记录或枚举。`return` / `break` / `continue`
  等无正常值的路径不参与类型合并；不隐式贯穿，不根据常量被匹配值省略其他分支的静态检查。
  分支体是一个表达式，多步操作用块或分号序列；`;` 仍是可选的连接运算符，
  `| A => first(); value | B => fallback` 合法，分支末尾的分号将其正常值变为 Unit。
- 构造载荷、被匹配表达式和选中分支均可在 `@suspends` 内挂起；恢复不重放此前副作用。
  同步/CPS 参数、返回值、递归和 scope 参数支持枚举；取消、失败及重载继续遵循原任务生命周期。
  extern、宿主注册及公开标量调用入口不接受枚举签名，CLI main 仍限 Unit / Int32。
- 第一版布局为一个 Int32 标签，加上**所有变体载荷各自独立的扁平标量区**，
  大小为 `1 + sum(payload slots)`，不是最大载荷大小；构造会初始化全部标量区。
  记录及枚举共用 65,536 标量槽、256 层布局上限；含枚举的内联循环报 `recursive_type`，
  纯记录环和纯别名环分别保持 `recursive_record` / `cyclic_type_alias`。
  该布局是内部实现，紧凑 union 布局留待后续优化，不承诺宿主 ABI。

`enum` / `match` 现在是保留字；局部值遮蔽枚举构造名前缀时按局部值处理。
枚举声明名与模块前缀重名时，限定构造优先于模块调用；导入重写也遵守这一规则。
第一版不支持泛型枚举、guard、嵌套解构、字面量模式、借用匹配、整体相等比较或非 Copy 载荷。
示例见 [enums.jk](../examples/enums.jk)。

## 字符串字面量

双引号字符串支持 UTF-8 文本，以及 `\n`、`\r`、`\t`、`\0`、`\"`、`\\` 六种转义。
花括号是普通文本；转义只解码一次，`"\\n"` 的内容是反斜杠和字母 n。
`\0` 表示文本中的 NUL 字节，String 按字节长度传递，NUL 不截断后续内容。
未知转义、裸 LF / CR、反斜杠续行和未闭合字符串均为解析错误，定位使用原始源码字节范围。
迁移旧源码时，将裸换行改成 `\n` / `\r`，字面反斜杠改成 `\\`。

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

JIT、AOT 和解释器共享类型检查与控制流规划。Int32 除法向零截断，`-2147483648 / -1`
返回 `-2147483648`；除零是带源码范围的运行时失败，停止后续求值，不能当作正常的 0。
CLI 源码运行渲染出错表达式；构建的独立程序向 stderr 报告错误及源码字节范围，以状态 1 退出。
嵌入原生代码的错误协议见[架构说明](architecture.md#原生错误传播)。
`@suspends` 函数的循环条件及循环体都可以挂起，
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
实参之间必须用逗号分隔，`f(prepare(); value, other)` 有两个实参，第一项是序列表达式；
也可写成 `f((prepare(); value), other)` 强调分组。
块内允许空分号，顶层 import、extern 和函数声明后也可加分号。
所有 import 必须在第一个 `type` / `fn` / `extern fn` 前，前导注释、空白和空分号不受限制。
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
