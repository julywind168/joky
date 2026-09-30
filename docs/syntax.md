# Joky 语法与类型系统设计（第一版草案）

状态：设计草案，2026-09-29。本文定义 v0.1 核心语法和完整类型系统的分阶段目标，
不表示这些能力已经实现，也不要求在 v0.1 一次完成全部类型机制。
本文随实现推进标记已完成的子集，其余条目仍是设计目标。
当前可用语法以[语言与内置模块](language.md)为准，执行与 ABI 边界见[架构说明](architecture.md)。
Arena、无显式生命周期的借用与 FFI 实施步骤见[专项设计](arena-borrowing-ffi.md)。

阅读入口：[表达式连接](#5-表达式连接块与分号)、[完整类型系统](#10-完整类型系统目标)、
[核心文法](#11-核心语法ebnf)、[实施顺序](#12-实施顺序与验收)。

## 1. 设计取舍

- 静态类型、表达式导向：函数和块用末尾表达式返回值，不做隐式类型转换。
- `let` 不可重新赋值，`var` 可重新赋值；函数签名显式，局部类型可推导。
- 用 `{}` 划分作用域、`,` 分隔参数；`;` 是连接表达式的序列运算符，不强制使用。
  块可以顺序容纳多个表达式，空白和换行不影响语义。
- 挂起沿用 `@suspends`，调用形式与普通函数一致；结构化并发使用 `scope` 和 `race`。
- v0.1 先完成函数、分支、循环和宿主交互；完整目标还包括名义数据类型、泛型与能力约束、
  函数值以及所有权与借用。资源规则先确定，再开放会捕获或跨挂起保存资源的功能。

### 与当前实现的关系

| 能力 | 当前实现 | 设计目标与阶段 |
| --- | --- | --- |
| 函数、`extern fn`、`let` / `var`、块、`while` | 已实现 | 保留，规范语句边界 |
| `Int32`、`Int64`、`UInt32`、`UInt64`、`Bool`、`Unit`、`String`、宿主 opaque 类型 | 已实现 | 保留，不增加隐式转换 |
| `type` 透明别名 | 已实现 Copy 类型参数、模块导入、前向引用、链及环检查 | 保持透明展开，无新名义身份或构造函数 |
| `@suspends`、`scope`、`race`、`import joky/vm` | 已实现 | race 等待首个终态并取消其余分支 |
| 显式整数转换 | 已实现 `int32/int64/uint32/uint64` 内建 wrapping 转换 | `joky/integer` 提供返回 Option 的 `to_*()` 方法；Integer 约束留待后续 |
| 四则、位运算与移位 | 已实现优先级、同型整数运算与移位范围检查 | 保留，见第 6 节 |
| 分号序列 | 已实现，分号可省略 | 保留表达式连接语义，不要求语句终止符 |
| 参数与实参逗号 | 已实现，缺失逗号报错 | 相邻元素之间必须写，允许尾随逗号 |
| 用户文件模块与公开声明 | 已实现入口目录解析、模块别名、选择性导入、私有默认及 `pub` | 包管理、重导出与通配导入留待后续 |
| 导入位置 | 已实现，必须在所有类型及函数声明前 | 保留；注释、空白和空分号不影响位置 |
| `if` / `else` / `else if` | 已实现 | 分支同型；无 else 时为 Unit |
| `return`、`break` / `continue` | 已实现 | 共享 IR 检查类型、跳转范围和不可达路径 |
| 一元 `-` / `!` / `~`、Bool 相等比较 | 已实现 | 保留；比较不可连续书写 |
| 短路 `&&` / `\|\|` | 已实现 | 保留；跳过的路径仍静态检查 |
| 字符串转义 | 已实现固定转义集合，未知转义及裸换行报错 | 保留，见第 3 节 |
| 其他位宽整数、浮点、Rune | 未实现 | 类型基础阶段，见第 10 节 |
| 名义记录 | 已实现 Copy 类型参数、构造推导、嵌套、字段访问/赋值及同步/CPS 传参与返回 | 解构及非 Copy 字段留待扩展 |
| 泛型枚举与 match | 已实现 Copy 类型参数、载荷 / 上下文推导、有限布局、穷尽检查及同步/CPS | 复杂模式、递归容器和非 Copy 载荷留待扩展 |
| Copy 元组与解构绑定 | 已实现结构类型、嵌套、let/var 模式及同步/CPS | 非 Copy 元素及复杂模式留待扩展 |
| 泛型函数 | 已实现 Copy 类型参数、调用推导、显式实参、定义检查与有界实例化 | trait、函数值和常量泛型留待后续 |
| 固有 impl、关联常量 | 已实现具体 Copy 类型及完整泛型记录 / 枚举目标、self / &self / &mut self、关联函数、方法类型参数和标量常量 | 特化与 trait 实现留待后续 |
| 函数值、trait、所有权容器 | 未实现 | 在资源规则确定后分阶段开放 |
| Arena、引用来源检查 | 已实现隐式 arena、&T / &mut T、移动、重借用、共享引用字段、受检只读循环图、arena 引用跨挂起及 scope 分支借用、同步局部值/字段借用与独立字段访问、异步宿主借用 v1 | 显式 Arena、经引用解引用的字段取借用、局部引用跨挂起与异步取消/draining 后续开放；不引入显式生命周期标注 |
| 同步 C 接口 | 已实现 `#repr(C)`、unsafe 块、合计 0–4 个引用/Ptr 参数、整数/Ptr/Unit 结果、三后端互操作；Ptr.null()/Ptr.is_null() | 资源拥有包装与异步保留协议另行扩展 |
| 闭包、dyn Trait | 未实现 | 后续类型扩展，不属于当前可运行子集 |

以下除明确写出“当前实现”的内容外，均为目标规则。第一版不是对现有行为的无条件冻结；
最后一节列出需要迁移和补齐的部分。

## 2. 程序与函数

当前顶层允许 `import`、`type`、`struct`、`enum`、`impl`、`extern fn` 和 `fn`；后续类型扩展再加入
`trait`。没有顶层执行语句或全局变量。
导入必须放在文件开头；函数可以先调用后定义，允许递归，不允许同名重载、重复参数或嵌套定义。

下面是**当前 JIT、解释器和 AOT 都可运行**的完整程序，也是目标语法的共同子集：

```joky
extern fn print_int(value: Int32)

fn sum(n: Int32) -> Int32 {
    var i = 0
    var total = 0
    while i < n {
        total = total + i
        i = i + 1
    }
    total
}

fn main() {
    print_int(sum(10))
}
```

输出 `45`。`fn main()` 返回 `Unit`；省略 `-> Type` 总是表示 `Unit`，不推导函数返回类型。
普通函数参数必须写类型，impl 的首个 `self` / `&self` / `&mut self` 例外；实参按位置传递；没有默认参数、命名参数或可变参数。
函数体正常结束时，其末尾表达式必须匹配返回类型。

```text
fn name(parameter: Type, ...) [@suspends] [-> Type] { ... }
extern fn name(parameter: Type, ...) [@suspends] [-> Type]
```

这里的方括号表示可选部分，不是源码字符。`@suspends` 固定写在参数列表之后、返回类型之前。
`extern fn` 没有函数体，宿主注册必须匹配完整签名和挂起约定；指针与代码所有权规则不属于语法。
顶层声明后可选写分号；这是声明间的分隔便利，不要求声明本身成为表达式。

CLI 入口继续限于同步、无参数的 `main`，返回 `Unit` 或 `Int32`。
CLI 提供 `print_int` 宿主；它不是关键字，也不是任意嵌入环境中自动存在的函数。

## 3. 词法与名称

源码为 UTF-8，区分大小写。支持 `//` 行注释，不支持块注释。
标识符以 `_` 或 Unicode 字母开头，后续可为 `_`、Unicode 字母或数字；不做 Unicode 规范化。
项目示例与宿主名称使用英文 `snake_case`，类型使用 `PascalCase`。

v0.1 核心目标保留字为：

```text
fn extern unsafe import pub type struct enum impl const match let var if else while break continue return scope race
true false mut Int32 Int64 UInt32 UInt64 Bool Unit String Ptr
```

`self` / `Self` 在 impl 内具有接收者 / 目标类型含义。类型系统扩展另引入 `trait for dyn`，
以及第 10 节列出的数值与 Rune 类型名。这里的 `for` 仅用于 trait 实现，不引入 for 循环。
这些名称在对应阶段启用时才变成关键字，不提前改变当前源码的解析结果。

`suspends` 只在 `@suspends` 位置有特殊含义。声明属性使用 `#`，当前仅开放 struct 前的 `#repr(C)`，
不开放通用属性或用户属性宏；后续语法方向见 [Arena 设计 §2.5](arena-borrowing-ffi.md#25-后续属性系统的语法方向)。
函数名、参数名和局部名称是单个标识符；`vm.call_int32` 是限定调用名，
`vm.Handle` 是限定类型名。`.` 支持字段访问、固有方法调用和关联常量读取。
局部值遮蔽模块或类型前缀时按值的方法查找，找不到即报错。
宿主 opaque 类型名继续使用当前注册器接受的 ASCII 标识符段，段间用 `.` 连接。
运行时函数名前缀 `__joky_cps_`、`joky_cps_`、`joky_vm_`、`__joky_runtime_`、
`joky_runtime_` 保留，用户不能声明。

### 核心字面量

| 类型 | 形式 | 规则 |
| --- | --- | --- |
| `Int32` / `Int64` / `UInt32` / `UInt64` | `42`、`1_000`、`0xff`、`0b1010` | 十/十六/二进制；单个下划线仅在数字之间，无后缀或浮点数 |
| `Bool` | `true`、`false` | 不与整数隐式转换 |
| `String` | `"hello"` | 不可变 UTF-8，无插值、拼接或字符字面量 |
| `Unit` | `{}` | 空块产生 Unit；第一版不增加 `()` 别名 |

负号是一元运算符。四种整数字面量的上下文定型已实现，无约束局部绑定默认 Int32；
大数字不自动升级。范围和最小值拼写见[当前整数语义](language.md#整数类型与字面量)。
直接负号允许 `-2147483648` / `-9223372036854775808`，但括号内正数必须先满足选定类型的范围。
`-` 与数字之间允许空白或注释；UInt32 / UInt64 不接受一元负号，包括 `-0`。显式整数转换及返回 Option 的整数方法已实现；其他数值类型仍是第 10 节的后续目标。

进制前缀只接受小写 `0x` / `0b`，十六进制数字可大小写混用；不支持八进制。
进制不会改变类型或将高位解释为符号位，直接负号在各进制下均允许对应有符号最小值。
数值 token 包括紧邻的字母和下划线，非法数字、后缀或分隔符整体报错，不能拆成后续表达式。
完整边界和示例见[进制与分隔符](language.md#进制与分隔符)。

字符串转义仅包括 `\n`、`\r`、`\t`、`\0`、`\"`、`\\`，当前实现已支持。
不允许裸 LF / CR，也不支持反斜杠续行；未知转义和未闭合字符串报错，花括号是普通文本。
转义只解码一次，`"\\n"` 表示反斜杠和字母 n；`\0` 是字符串内的 NUL 字节，不截断文本。
错误范围使用原始源码的 UTF-8 字节偏移，不使用解码后的字符串下标。

## 4. 绑定、赋值与作用域

```joky
fn increment(start: Int32) -> Int32 {
    let step: Int32 = 1
    var value = start
    value = value + step
    value
}
```

- 所有绑定必须初始化；显式类型注解必须与初始值一致。
- 参数和 `let` 绑定不可重新赋值；`var` 赋值不能改变类型。
- 核心阶段赋值目标只能是现有局部变量，结果是 `Unit`；类型扩展后，字段赋值和可变借用
  还须满足所有权规则。暂不提供 `+=` 等复合赋值。
- 允许同名遮蔽；新声明的初始值先在旧环境求值，随后新绑定生效。
- `{ ... }` 引入词法作用域；新声明不能逃逸，外层 `var` 的赋值可以保留。
- 括号和分号序列不创建作用域；`let` 的不可变性不意味着 opaque 句柄所指资源不可变。

## 5. 表达式连接、块与分号

`;` 是最低优先级的**表达式连接运算符**：`a; b` 先求值 `a`，丢弃其结果，
再求值 `b`，整个序列取 `b` 的值和类型。它恰好也能充当传统语言的语句分隔符，
但声明、赋值、调用和控制流都不强制以分号结束。

1. 块按源码顺序执行其中的表达式，丢弃非末尾表达式的值，以末尾表达式为结果；空块为 Unit。
2. 没有分号时，只要语法可以确定表达式边界，就可以继续写下一个表达式；不要求换行。
   例如 `let x = 1 let y = 2 x + y` 可以写在一行，也可以分行。
3. 换行只是空白，不自动结束或连接表达式。`1 +` 换行接 `2` 仍是加法；
   引入一元负号后，`a` 换行接 `-b` 仍优先解析成减法，需要显式分开时可以写 `a; -b`。
4. 尾随分号丢弃最后一个正常值，序列正常结果为 Unit；不吸收 return、break 或 continue。
   连续分号允许作为空连接，不产生副作用；单独的分号不产生有用值。
5. 声明、赋值、while、scope 和 race 本身返回 Unit，是否追加分号不改变它们正常结束时的结果类型。

```joky
fn answer() -> Int32 { 42 }
fn discard() { 42; }
fn grouped() -> Int32 { (1; 2) }
fn empty_result() { (1; 2;) }
```

序列不限于括号内，`{ a; b }` 直接连接两个表达式。括号用来控制结合方式，
不创建新作用域。初始化与赋值右侧默认解析到分号之前：`let x = a; b` 先绑定 `a`，
再执行 `b`；要把序列整体作为初始值，写 `let x = (a; b)`，赋值同理。

实参中的序列按逗号或右括号结束：`f(a; b, c)` 有两个实参，第一项是序列 `a; b`，
第二项是 `c`；也可写成 `f((a; b), c)` 强调分组。分号不增加实参数量。
while / if 条件中的序列使用括号，例如 `while (advance(); ready()) { ... }`。
控制流也可连接：`return value; next()` 中 `next()` 不会执行，按不可达代码规则报错。

## 6. 运算符与求值顺序

从高到低：

| 优先级 | 形式 | 结合性与约束 |
| --- | --- | --- |
| 最高 | `name(...)`、`(...)`、`{...}` | 调用、分组、块 |
| 一元 | `-x`、`!x`、`~x` | 右结合；分别要求有符号整数、Bool、任一整数 |
| 乘除 | `*`、`/` | 左结合；同型 Int32 / Int64 / UInt32 / UInt64 |
| 加减 | `+`、`-` | 左结合；同型 Int32 / Int64 / UInt32 / UInt64 |
| 移位 | `<<`、`>>` | 左结合；两侧同型整数，移位量为 0 到位宽减一 |
| 按位与 | `&` | 左结合；两侧同型整数 |
| 按位异或 | `^` | 左结合；两侧同型整数 |
| 按位或 | `\|` | 左结合；两侧同型整数，分支边界处需定界 |
| 比较 | `==`、`!=`、`<`、`<=`、`>`、`>=` | 不结合；禁止链式比较 |
| 逻辑与 | `&&` | 左结合、短路；Bool |
| 逻辑或 | `\|\|` | 左结合、短路；Bool |
| 赋值 | `=` | 右结合，结果为 Unit |
| 最低 | `a; b` | 从左到右连接表达式，取末项结果 |

当前算术和有序比较接受 Int32 / Int64 / UInt32 / UInt64，但两侧必须类型相同；`==` / `!=` 还接受两侧同为 `Bool`，
不支持 `String`、`Unit`、opaque 类型比较。条件必须为 `Bool`，没有 truthy/falsy 规则。
`a < b < c` 必须改写为 `a < b && b < c`；后者会按源码重复求值 `b`。
赋值返回 Unit，因此 `a = b = 1` 对两个 Int32 变量是类型错误。

普通二元运算、调用实参按从左到右求值；已经读取的 Int32 等值不受后续赋值影响。
引用是对原值的访问能力，不会自动复制其指向的数据；冲突访问由借用检查拒绝。
`&&` 左侧为 false 时不求值右侧，`||` 左侧为 true 时不求值右侧。
短路规则同样适用于右侧副作用和挂起调用；跳过的路径仍须通过名称、类型和挂起约定检查。
左操作数必定执行，其序列中的声明遵守当前作用域；右操作数可能不执行，其中的新声明
仅在该操作数内有效。对已有外层 var 的赋值仍然生效，圆括号本身不引入新作用域。

四种整数加、减、乘采用各自位宽环绕；仅有符号整数支持一元负号，也按位宽环绕。
除法向零截断，有符号 `MIN / -1` 规定为 `MIN`；无符号比较和除法保留非负数含义。
除零是运行时失败，不能变成正常的 `0`。四种整数的这些规则已在 JIT、AOT 和解释器实现；
原生生成代码显式检查除数，不依赖机器陷阱捕获。错误协议见[架构说明](architecture.md#原生错误传播)。

`~` 按位宽反转，左移截去高位，有符号右移补符号位，无符号右移补零。
移位两侧都要求同型，包括移位量；负数或大于等于位宽的移位量在执行时失败，即使它是常量。
先完整检查移位量，再执行机器移位，不采用处理器的掩码/取模行为；错误传播与除零一致。
`flags & mask == 0` 按 `(flags & mask) == 0` 解析，`1 << 2 + 1` 按 `1 << (2 + 1)` 解析。
`&` / `|` / `^` 均求值两侧，不能替代 Bool 短路运算。

match 分支结果与 scope/race 分支最外层的 `|` 保留为分隔符；按位或写为 `(a | b)` 或 `{ a | b }`。
括号、调用参数、元组、记录字段和块内部允许按位或；闭合定界符后恢复外层分支规则。
嵌套 match 自己管理分支分隔符，换行没有特殊意义，不进行按名称或后续 `=>` 猜测。

当前 `1 + 2 * 3` 为 `7`；依赖旧版同优先级规则得到 `9` 的代码应迁移为 `(1 + 2) * 3`。
一元运算、算术、位运算、移位、比较和短路 `&&` / `||` 均已实现。

## 7. 条件、循环与提前结束

`if` / `else`、`else if`、`while`、`return`、`break` 和 `continue` 均已实现。
以下示例当前可运行：

```joky
fn absolute(value: Int32) -> Int32 {
    if value < 0 { -value } else { value }
}

fn sum_until(n: Int32, limit: Int32) -> Int32 {
    if n <= 0 { return 0 }
    var i = 0
    var total = 0
    while i < n {
        i = i + 1
        if i == 2 { continue }
        if i > limit { break }
        total = total + i
    }
    total
}
```

`absolute(MIN)` 仍为 `MIN`，按 32 位环绕规则计算，不承诺数学意义上的非负结果。

- `if condition { ... } else { ... }` 是表达式，两条正常结束路径的结果类型必须相同。
  条件只求值一次，只执行选中的分支；允许 `else if`。
- if 条件中序列的新声明仅在条件内有效，不进入任一分支或后续语句；
  对已有外层 `var` 的赋值仍然生效，与 while 条件采用相同规则。
- 无 `else` 时整个 `if` 为 Unit；then 分支正常结束时也必须为 Unit。
- `while` 每轮先检查条件，正常结束结果为 Unit，循环体正常结束的末值丢弃。条件内序列的新声明
  不泄漏到循环体或循环外，循环体每轮有独立的局部绑定。
  条件求值也处于该循环的跳转范围内；所有条件路径都提前跳转时，循环体必须为空。
  例如 `while (break) {}` 正常结束，`while (return 42) {}` 返回函数，`while (continue) {}` 持续循环。
- `break` 退出当前函数中最近一层 `while`；`continue` 回到该循环的条件检查，均不要求分号。
  循环外使用报错，不支持标签或 `break value`。
- `return value` 结束当前函数；没有值的 `return` 返回 Unit，必须匹配函数返回类型。
  返回值按非序列表达式解析，返回整个序列的值需写 `return (a; b)`。
  换行不切断返回值；在 `}`、`;` 等边界前的裸 `return` 才没有返回值。
  不允许从函数体内部返回到调用者的调用者。
- 提前结束的路径不参与分支结果类型合并；其他可达结束路径仍须匹配类型。
  不引入用户可写的 `Never` 类型；明确跳转后的表达式、运算右侧或后续实参报不可达错误。
  可达性按结构判断，不进行常量或无限循环推断；静态检查仍将 `while true { return 1 }` 视作可能正常结束为 Unit。
  跳转参与运算时须用括号或块，例如 `1 + (return 2)`；尾随分号不改变跳转。

## 8. 挂起与结构化并发

```joky
extern fn wait_tick() @suspends

fn worker(count: Int32) @suspends {
    var i = 0
    while i < count {
        wait_tick()
        i = i + 1
    }
}

fn run_pair() @suspends {
    scope {
        | worker(2)
        | worker(3)
    }
}
```

此形式当前已经支持，但需要嵌入方提供 `wait_tick` 并通过异步调用入口驱动，不能直接作为 CLI 入口运行。

- 普通函数只能调用同步函数；`@suspends` 函数可调用两者，挂起约束沿调用链传播。
  不另加 `async` / `await`，标注也不保证调用必定挂起。
- 恢复必须继续挂起位置，不重放已完成的实参求值、赋值或宿主副作用。
- `scope` 和 `race` 只能在挂起函数中使用，至少有一个分支；每个分支必须直接调用返回 Unit 的挂起函数。
  按分支书写顺序、实参从左到右准备全部参数，之后开始分支轮询。
  引用及含共享引用聚合可传入分支；共享读允许重叠，独占借用不能与其他分支的读写重叠。
  较早参数的借用在后续参数求值期间已生效；普通分支借用直到整个操作完成或取消才结束。
  分支可分配，所有嵌套分支共享根任务 arena 和预算；join 不回收对象。具有分配能力的分支
  保守保留全部引用输入的借用到任务结束，沿用普通调用的存储前置条件。
- 所有分支完成后 `scope` 才结束，结果为 Unit；分支失败使整个 scope 失败，并取消其余分支。
  父任务取消也取消其子任务。核心阶段没有 `defer` 或用户析构语法；所有权扩展后，
  取消必须释放已初始化且仍由任务拥有的值，清理动作同步完成、不可挂起，见第 10 节。
- `race { | first() | second() }` 按源码顺序轮询，首个观察到的完成或失败决定结果：
  完成返回 Unit，失败终止整个任务；同轮就绪按轮询顺序处理。父函数继续前同步清理
  其余分支及嵌套子帧，后续分支可能从未被轮询，已发生副作用不回滚。
  普通分支借用在此边界结束，可分配分支的任务期存储借用及 arena 对象仍保持到根任务结束。
  首版没有获胜编号或分支返回值，不新增外部异步取消 ABI。
- `scope` / `race` 表示单线程协作并发，不承诺并行、抢占或 CPU 循环公平性；执行限额由嵌入 API 配置。
- `return` 只完成当前分支函数；不能逃出父函数或父 scope。
  `break` / `continue` 不得跨函数边界，两者都不接受任意语句块作为分支。

## 9. 模块和宿主边界

当前支持宿主模块 `joky/vm` 和源码库 `joky/option`、`joky/result`、`joky/integer`、`joky/prelude`。
导入位于全部声明之前，尾随分号可选；模块路径用 `/`，限定类型、构造器和函数用 `.`。

```joky
import joky/option as opt.{type Option as Maybe, Some as Present}

fn make() -> Maybe<Int32> { Present(1) }
fn empty() -> opt.Option<Int32> { Maybe.None }
```

模块别名和选择性导入已实现；`as opt` 也可放在成员列表后。
`import joky/option.{type Option}` 引入类型名及其构造器命名空间，允许 `Option.Some` / `Option.None`；
`type Option as Maybe` 保留该能力。只导入 `Some` 不会引入裸类型名 Option。
模块别名下的 `opt.Some` 与 `opt.Option.Some` 指向同一构造器。

默认 prelude 提供 Option / Result、Some / None / Ok / Err、状态判断 / 默认值 / 组合方法、整数受检转换方法与边界常量。
Option / Result 的方法均为普通按值源码调用；默认值和备选值会急切求值，完整表见[基础方法](language.md#option-与-result-基础方法)。
因此通常无需 import 就能写 `Some(42)`、`None::<Int32>` 和 `n.to_u64()`。
`--no-prelude` 或嵌入 API 可关闭默认导入；`joky/vm` 始终显式导入。
显式选择的名称不能重复；局部值可遮蔽导入名称。同级函数可遮蔽 prelude 裸构造器，
同级 Option / Result 类型声明则需要关闭 prelude。完整规则和配置入口见[语言文档](language.md#导入与-prelude)。

`import` 只绑定声明，不在加载时执行用户代码。多个标准模块依赖同一枚举时复用声明身份，
不受导入顺序影响；三后端共享解析结果。用户文件模块和 `pub` 已实现，
CLI 以入口文件目录为源码根目录；嵌入方显式提供源码集。声明默认私有，公开枚举同时公开构造器，
impl 的方法和常量分别标记 `pub`。完整规则见[用户文件模块](language.md#用户文件模块)。
通配导入、公开重导出和包管理留待后续。

`vm.call_int32` 和 `vm.register_int32` 是内置模块的受限调用形式，不代表一般函数值或函数重载。
核心阶段函数引用仍限于原生宿主注册；第 10 节的普通函数值、闭包和间接调用属于后续扩展。
完整接口保持在[内置标准库](language.md#内置标准库-jokyvm)，不在本文重复定义。

## 10. 完整类型系统目标

本节主要是后续设计；以各小节明确标注的已实现子集为准。类型系统由基础值、组合类型、
泛型约束、可调用契约和资源规则共同组成；ADT 只覆盖其中的积类型与和类型。
除非明确允许，类型必须精确匹配，不存在通用 Any、隐式数值转换、结构体鸭子类型或类继承。

### 10.1 基础类型与字面量推导

**已实现**：Int32 / Int64 / UInt32 / UInt64、局部上下文整数字面量定型、同型运算、环绕和除零失败，
以及位运算、受检移位和显式 wrapping 转换，覆盖三后端同步 / CPS。
其他整数位宽、浮点、Rune、全局约束推导和泛型约束尚未实现；受检整数转换由固有方法提供。

| 类别 | 类型 | 规则 |
| --- | --- | --- |
| 布尔 | `Bool` | true / false，不隐式转成数字 |
| 有符号整数 | `Int8`、`Int16`、`Int32`、`Int64` | 固定位宽，二进制补码 |
| 无符号整数 | `UInt8`、`UInt16`、`UInt32`、`UInt64` | 固定位宽，非负整数 |
| 平台尺寸 | `USize`、`ISize` | 当前目标平台的指针位宽，用于长度和索引，不直接作为持久化格式 |
| 浮点 | `Float32`、`Float64` | IEEE 754 二进制浮点，保留 NaN、无穷和有符号零 |
| 字符 | `Rune` | 一个 Unicode 标量值，不等于一个字节或一个用户感知字符 |
| 文本 | `String` | 拥有不可变 UTF-8 文本；拥有型文本的实现属于后续资源阶段 |
| 无信息结果 | `Unit` | 唯一值用空块 `{}` 表示，表示正常完成 |
| 不返回 | 内部 `Never` | 没有正常完成路径；暂不开放用户书写 |

- 整数字面量由参数、返回值或注解等上下文定型，无约束时默认为 Int32；浮点默认 Float64。
  泛型仍有歧义时要求显式类型参数或注解，不按函数名猜测类型。
- 整数支持十进制、十六进制和二进制及数字间分隔符；未来浮点写作 `1.0` 或 `1.5e-3`，必须有小数点且两侧都有数字。
  `1` 可以在明确的 Float64 上下文中成为浮点字面量，但已经定型的 Int32 值不会因此隐式转型。
- 各整数类型按自己的范围检查字面量，负号和有符号最小值采用第 3 节相同规则。
  正的最小值绝对值只在紧邻的一元负号下被允许；无符号类型不接受负数字面量或一元负号。
- 同一算术运算的两个已定型操作数必须具有相同数值类型。整数加减乘以及有符号负号按位宽环绕，
  除法向零截断，有符号 MIN / -1 为 MIN，整数除零失败。浮点按自身规则计算，不套用整数除零规则。
- 新增数值类型的比较也要求两侧同型。浮点 NaN 不等于自身，有 NaN 参与的有序比较为 false；
  因此原始浮点不实现要求等价关系的 Eq，不能直接用作 Map 键，需使用定义了相等与哈希规则的包装类型。
- Rune 字面量写作 `'a'` 或 `'\n'`，解码后必须恰好一个合法标量值；代理项非法。
  Rune 的 32 位存储宽度不意味着任意 UInt32 都能构成 Rune。
- String 不提供按整数下标返回“字符”的语法；字节访问与 Rune 遍历使用不同的库接口。
  `String` 的不可变性约束文本内容，`var` 绑定仍可替换为另一个 String。

转换通过具名函数完成，不提供一个含多种隐式行为的 `as`。四个 wrapping 整数转换函数的目标签名是：

```joky
fn int32<T: Integer>(value: T) -> Int32
fn int64<T: Integer>(value: T) -> Int64
fn uint32<T: Integer>(value: T) -> UInt32
fn uint64<T: Integer>(value: T) -> UInt64
```

这里 `Integer` 表示静态整数约束，首版接受 Int32 / Int64 / UInt32 / UInt64；
不包含 Bool、浮点、文本或仅在底层使用整数存储的 opaque 类型。
**当前转换仍是编译器内建调用；无约束的泛型函数已实现，但以上 Integer 约束声明仍不可直接编写**，也未引入普通函数重载。
类型参数从实参自身推导，结果类型不反向约束实参；无约束字面量仍默认 Int32。
因此 `int64(2147483648)` 越界，而先绑定 `let n: Int64 = 2147483648` 后调用 `int32(n)` 得 `-2147483648`。

wrapping 转换窄化保留低位，宽化按源类型符号扩展或零扩展，同位宽保持位模式；
函数名决定如何解释结果。转换自身不失败，但实参的失败、挂起和提前跳转仍正常传播。
这些小写名字保留在函数命名空间，不能由用户函数、extern 或宿主绑定替换；
当前具体调用与迁移规则见 [显式整数转换](language.md#显式整数转换)。

受检转换已采用实例方法，不再规划额外的 `try_int32` 内建函数：

```joky
// The prelude supplies this ordinary Joky implementation.
impl Int32 {
    fn to_u64(self) -> Option<UInt64> {
        if self < 0 { Option.None } else { Option.Some(uint64(self)) }
    }
}
```

`to_i32/to_i64/to_u32/to_u64` 一律返回 Option，越界为 None；强制转换继续写 `int32/int64/uint32/uint64`。
`MIN` / `MAX` 是各整数类型的关联常量，范围检查和转换包装由 Joky 库源码实现；无需先实现 Integer trait。
库导入、名义类型身份及明确的当前限制见[固有方法与关联常量](language.md#固有方法与关联常量)。
浮点到整数需另行显式选择截断或舍入并检查范围，不自动加入上述四个整数函数。
普通库调用失败通过 Result 表达；分配耗尽等运行时失败不能悄悄伪装成成功的值。

### 10.2 记录、元组、别名与名义身份

**已实现的记录子集**：`struct` 名义值类型、Copy 类型参数、空记录、嵌套、字段读取/赋值、完整构造，
同步及挂起函数的参数/返回值。字段支持 Int32 / Int64 / UInt32 / UInt64 / Bool / Unit / Ptr、共享引用及这些类型组成的记录、有限枚举和元组，均可复制；
含共享引用的值可用于同步/挂起调用链及受检的 scope 分支；
类型注解写 `Pair<Int64>`，构造支持字段 / 上下文推导或 `Pair::<Int64> { ... }`。
String、opaque 资源、记录解构和整体相等比较尚未开放；具体记录及完整泛型记录的固有方法已实现。
准确的解析消歧、实现限额及宿主边界见[当前记录语义](language.md#记录)。
**已实现的元组子集**：结构化 Copy 元组、单元素尾随逗号、嵌套 let / var 解构、`_` 丢弃及整体注解；
支持字段、枚举载荷和同步/CPS 传参返回。访问元素先解构，尚无 `.0`、索引或解构赋值。
求值顺序、相邻括号的消歧及迁移规则见[当前元组语义](language.md#元组与解构绑定)。
下面示例的 UInt64 已实现；拥有型 String 及 Float64 仍是后续目标。

```joky
struct User {
    id: UInt64
    name: String
    active: Bool
}

fn make_user() -> User {
    User { id: 42, name: "Ada", active: true }
}

fn coordinates() -> (Float64, Float64) {
    (12.5, 30.0)
}

type UserNumber = UInt64

struct UserId { value: UInt64 }
struct OrderId { value: UInt64 }
```

- struct 和 enum 是名义类型：声明不同即类型不同，字段相同也不能互换。
  `UserId` 与 `OrderId` 因而不同；type 别名则完全等同于被引用类型，不创建新身份。
  **已实现**模块级 `type Count = Int32` 和 `type PairOf<T> = (T, T)`，目标也可为记录、枚举、元组、已有基础类型、注册的 opaque 类型或其他别名。
  支持前向引用、任意顺序及别名链；重复别名、与已有类型冲突、未知目标或别名环均拒绝，即使未使用。
  类型与值命名空间独立；别名在定义模块解析，公开别名可导入及重命名，重载不污染宿主类型注册器。
  别名不能作构造函数或成员命名空间；不支持局部声明或带点的声明名。泛型实参限 Copy 类型，
  包括未用于目标的参数；每次规范化最多 256 个不同别名应用（含刚性参数应用），共享目标不重复展开。
  参数最多 64 个，目标沿用布局上限；不能形成别名环，经过未使用参数的依赖环也保守拒绝。
- 记录构造必须恰好提供所有字段，不重复、不依赖隐式默认值；字段按源码书写顺序求值。
  字段声明和初始化项的逗号可选，换行仍只是空白；不能以此省略函数实参的逗号。
- 元组按元素类型、数量和位置匹配；`(T)` 是分组，`(T,)` 才是单元素元组类型，
  `(value,)` 同理。`()` 暂不作为另一种 Unit 表示。
- 可用 `let (x, y) = coordinates()` 解构元组；普通 let / var 模式必须不可失败。
  enum 分支解构放入 match，不能通过可能失败的 let 隐式丢掉其他情况。
- 字段写作 `user.name`；拥有值的可变字段位置需要 var 或独占可变借用。
  读取非 Copy 字段须明确是借用还是移动；首阶段不开放会留下半初始化记录的部分移动，
  要取得所有权可消费整个记录并解构。

### 10.3 容器与递归布局

目标是以 Joky 编写这些参数化库类型，编译器与运行时提供泛型、借用、布局和通用存储原语，
不为每个容器增加关键字。Option / Result 的 Copy 子集已实现；下列数组、切片、Map / Set
仍是规划。完成 arena/FFI 阶段不足以自动获得这些容器，需另行推进
[容器与所有权扩展](arena-borrowing-ffi.md#61-容器与所有权扩展)。

| 类型 | 所有权或布局 | 必要约束 |
| --- | --- | --- |
| `Array<T>` | 拥有动态长度的连续元素 | 不隐式复制 T；可变操作需要独占访问 |
| `FixedArray<T, N>` | 内联存放 N 个元素，N 属于类型 | N 为可求值的非负 USize 常量，布局必须可表示 |
| `Slice<T>` | 非拥有的连续元素视图，动态大小 | 通常经 `&Slice<T>` / `&mut Slice<T>` 使用，不能裸值传递 |
| `Map<K, V>` | 拥有键和值 | K 满足 Eq、Hash |
| `Set<T>` | 拥有元素 | T 满足 Eq、Hash |
| `Option<T>` | None 或 Some(T) | 没有跨类型通用 null |
| `Result<T, E>` | Ok(T) 或 Err(E) | 业务错误是数据，不自动取消 scope 的其他分支 |

泛型类型参数默认要求有确定的值大小；dyn 和 Slice 等动态大小类型须放在明确的引用或
拥有指针后面。`Array<T>`、`Box<T>` 等通过间接存储允许有限布局的递归类型；
`struct Node { next: Node }` 的无限内联布局非法。
容器类型默认不变：即使两种元素有相关能力，也不能把 `Array<A>` 当作 `Array<B>`。
引用生命周期的缩短由借用规则处理，不作为任意容器协变的理由。

Array 与 Slice 提供返回 Option 的受检访问，不把越界转换成一个伪造元素；
后续若加入索引运算符，必须另行明确越界行为，不能绕过引用有效性检查。
数组按 `Array<Int32>` → Copy 元素泛型数组 → 非 Copy 元素与嵌套容器逐步验收。
即使元素是 Copy，独占管理缓冲区的数组也不可隐式复制；其管理权、已初始化范围和退出清理
必须受检。元素或切片借用期间禁止冲突的扩容、删除等操作，借用结束后可继续修改容器。
这种临时借用与共享对象图的永久封存分别建模，不能直接把含引用对象的封存规则套到所有容器上。

arena 数组适合任务内临时数据，扩容后的旧缓冲区保留到 arena 结束；长期可变容器还需
可独立回收或复用的存储。只读链表和封存循环图不等于可变链表，后者需另验收节点所有权、
链接修改、删除后的借用失效及回收。具体容器存储接口和拥有型 String 仍待设计。

### 10.4 枚举、模式匹配与动态数据

**已实现的子集**：带 Copy 类型参数的名义枚举、零或单个 Copy 载荷、有限嵌套、`match` 表达式、
穷尽/重复/不可达检查、载荷不可变绑定及 `_`；同步与 CPS 均支持。
构造须写 `Enum.Variant` / `Enum.Variant(value)`；只有模式可从被匹配类型推导限定名。
载荷限 Int32 / Int64 / UInt32 / UInt64 / Bool / Unit / Ptr / Copy 记录、枚举和元组，单个载荷可用记录或元组组合多个值。
首版 match 不含 guard、字面量模式/元组模式/嵌套解构、借用匹配和非 Copy 数据。
解析边界、存储成本、限额及完整规则见[当前枚举语义](language.md#枚举与模式匹配)。
以下 Option / Result 声明已可编译。实例载荷目前仍限 Copy 类型，String 载荷、文本格式化与动态数据模型属于目标设计：

```joky
enum Option<T> {
    None
    Some(T)
}

enum Result<T, E> {
    Ok(T)
    Err(E)
}
```

构造支持 `Option.Some(42)`、`Option.None::<Int32>` 与 `Result.Ok::<Int32, Bool>(42)`，
缺少类型信息时要求注解；详细推导及限额见[当前泛型枚举](language.md#泛型枚举)。
下例的非 Copy String 载荷仍未开放：

```joky
fn present(result: Result<Int32, String>) -> String {
    match result {
        | Ok(value) => format_int(value)
        | Err(message) => message
    }
}
```

示例中的 `format_int` 是未来的文本库函数。match 是表达式，要求穷尽所有情况，
正常完成的分支具有相同结果类型；不提供隐式贯穿。返回、失败等终止分支不参与结果类型合并。
目标模式包括字面量、绑定、`_`、元组和枚举构造；`_` 覆盖尚未列举的情况。
分支书写为 `| pattern => expression`，多步分支可用块或括号序列，不要求尾随分号。
目标设计中同名构造项由被匹配类型或期望类型确定，无法唯一确定时需写 `Result.Ok` 等限定名。

按值匹配会消费非 Copy 的被匹配值；匹配 `&value` 时，载荷绑定获得共享借用，
匹配 `&mut value` 时获得独占借用。借用匹配不能悄悄把非 Copy 载荷移出原值。
匹配分支有自己的绑定作用域；条件中或分支中的局部声明不会泄漏。

动态输入使用显式数据模型，而非让每个类型都可接受任意值：

```joky
enum Value {
    Null
    Bool(Bool)
    Number(Float64)
    Text(String)
    List(Array<Value>)
    Object(Map<String, Value>)
}
```

这里 `Null` 只属于 Value，取值需要匹配或受检转换。此简化模型不保证精确保留所有 Int64 / UInt64，
需要无损数字往返的解析库应提供另外的数字变体。Object 依赖 String 的 Eq、Hash 库实现。

### 10.5 泛型、trait 与分派

**已实现**最小泛型函数：类型参数、从实参局部推导、显式 `::<...>` 调用、定义时刚性检查，
以及三后端共用的有界编译期实例化。第一版的类型实参限现有 Copy 类型；
String / opaque 类型实参和 trait 约束不在该子集中；Copy 参数化记录、枚举及透明别名已实现。

```joky
fn identity<T>(value: T) -> T { value }
fn first<T, U>(left: T, right: U) -> T { left }
fn wide() -> Int64 { identity::<Int64>(42) }
```

已有实参类型优先，未确定的整数字面量默认 Int32；外层返回值注解不反推类型参数。
缺少局部信息时需显式指定全部类型实参；没有部分 `_` 推导。
即使模板未使用，也检查其函数体；不能因某次实例恰好是整数或记录而放行无约束的操作。
类型参数只在所属函数内有效，支持元组与枚举类型应用；具体函数实例不成为模块公开入口，宿主使用普通包装函数。
实例缓存、编译限额和完整限制见[当前泛型语义](language.md#泛型函数)。

具体类型及 Copy 参数化记录 / 枚举的固有 `impl` 已实现，
泛型目标形如 `impl<T> Pair<T>`，方法继承 impl 参数，也可独立声明 `fn method<U>`，两层参数共同复用函数实例化。
关联函数支持 `Pair.make::<Int64>(...)` 和 `Pair::<Int64>.make(...)`；实例接收者确定类型实参。
完整规则和限制见[泛型固有 impl](language.md#泛型固有-impl)。以下 trait 实现、借用和能力约束仍是**目标设计**：

```joky
trait Eq {
    fn equal(left: &Self, right: &Self) -> Bool
}

impl Eq for UserId {
    fn equal(left: &Self, right: &Self) -> Bool {
        left.value == right.value
    }
}

fn same<T: Eq>(left: &T, right: &T) -> Bool {
    T.equal(left, right)
}
```

类型实参通常由调用推导；需要消歧时写 `identity::<Int32>(42)`，不把表达式中的 `<`
同时猜作泛型列表和比较。上下文不能确定类型时必须补注解。
`T: Eq + Hash` 表示同时具备两种能力，不表示类继承或值类型相加。
泛型函数只能使用声明的约束，不能因某个具体实例恰好有某字段而通过类型检查。

trait 描述能力，impl 为具体类型提供实现；默认静态分派。
Eq 必须满足自反、对称和传递；Hash 必须保证 Eq 相等的值具有相同哈希。
编译器检查签名和约束，不能一般性证明用户实现满足这些代数规律，需要实现方与测试保证。
同一个 trait / 类型组合不能有重叠实现；将来支持外部模块时，trait 或被实现类型至少有一个
必须由实现所在模块定义。用户运算符重载不自动随 trait 上线，先使用明确的能力方法。
关联类型与更高阶的类型参数留到后续，不把未规定的规则写成现成能力。

常量泛型写作 `<T, const N: USize>`；初期类型实参只接受常量字面量或已声明常量参数，
不用任意运行时表达式参与类型等价判断。N 不同的 FixedArray 是不同类型。

### 10.6 函数值、闭包能力与挂起效果

函数类型包含参数、返回类型和挂起约定：

```text
fn(Int32, Int32) -> Int32
fn(String) @suspends -> Result<String, ReadError>
```

```joky
fn apply<T, U>(value: T, transform: fn(T) -> U) -> U {
    transform(value)
}

fn apply_once<T, U, F: FnOnce(T) -> U>(value: T, transform: F) -> U {
    transform(value)
}
```

`fn(...)` 是不捕获环境的语言函数指针类型，可存储、传参和间接调用；其代码所有者必须保活。
它不自动等于任意 C 函数裸指针。泛型函数在确定实例参数后才有这样的具体函数值。
函数参数与返回类型精确匹配，暂不加入函数参数逆变等隐式子类型转换。

每个闭包有独立的环境类型，捕获关系参与借用和销毁检查，由编译器推导；
可调用能力决定如何访问或消费该环境：

| 能力 | 调用需要的访问 | 可保证的行为 |
| --- | --- | --- |
| `Fn(A) -> R` | 共享访问 | 可重复调用，不要求独占捕获环境 |
| `FnMut(A) -> R` | 独占访问 | 可重复调用，允许修改捕获环境 |
| `FnOnce(A) -> R` | 消费环境 | 只保证一次调用，可移出捕获值 |

Fn 同时满足 FnMut 和 FnOnce，FnMut 同时满足 FnOnce。
闭包字面量与显式捕获写法后续单独确定；本节先固定可调用类型契约，不把环境强转成 fn 指针。
非捕获闭包转成 fn 指针也必须由后续规则明确允许。

挂起能力写作 `FnOnce(A) @suspends -> R` 等；约束必须保留效果信息，
传进泛型或转换成动态接口都不能把挂起能力擦除。
同步和挂起入口不按函数指针直接互换，必要时生成符合目标约定的包装函数。
Result 表达可处理的错误结果，`@suspends` 表达执行可能暂停，二者相互独立。
目前只确定这一种效果，不预设通用效果行、I/O 效果推导或效果处理器语法。

### 10.7 所有权、借用与释放

第一版优先采用 [Arena 与隐式生命周期方案](arena-borrowing-ffi.md)，以下为目标规则。
Box / Rc 等其他拥有类型属于后续候选能力，不是 arena 的实现前提。
数据引用已可来自 arena 分配或同步函数内无引用 Copy 局部值的 `&local` / `&mut local`。
阶段 4a/4b 已实现整个局部值及其内联记录字段借用；@suspends 函数内局部借用以及显式 Arena 的 `&mem` 仍是规划。
根任务自动拥有一个隐式 arena；`alloc` 返回 `&mut T`，`share` 或受检的
赋值/参数共享化得到 `&T`。显式 `Arena.new()` 作为高级子 arena 接口保留，用户可以编写合法的封装函数。

| 形式 | 含义 | 约束 |
| --- | --- | --- |
| `T` | 拥有值 | 非 Copy 值默认移动，移动后原位置不可再读 |
| `&T` | 共享只读借用 | 可以有多个，不能与冲突的独占访问同时有效 |
| `&mut T` | 独占可变借用 | 不可复制，借用期间排除对同一位置的其他冲突访问 |
| 隐式任务 arena | 当前任务的一批稳定存储 | 不作为用户值暴露；任务完成、失败、取消或 continuation 丢弃时整体释放 |
| `Arena` | 显式拥有一批稳定存储 | 非 Copy，高级接口；离开作用域整体释放，引用不得超过拥有者寿命 |
| `Ptr` | 不透明的 C 对象指针（阶段 5b 已实现） | Copy、可空，Ptr.null() 创建空值，Ptr.is_null(value) 判空；无所有权或自动释放；禁止解引用、地址运算、整数及 Joky 引用转换 |
| `Box<T>` | 独占拥有堆上对象 | 随拥有者释放，可承载递归或动态大小对象 |
| `Rc<T>` | 单线程共享所有权 | 显式 clone 增加所有者；引用计数不解决强引用环 |

数值、Bool、Rune、Unit 和无捕获函数指针属于 Copy 值；复合类型只有所有字段都满足
Copy 且没有析构逻辑时才可声明 Copy；当前记录、枚举和元组子集自动满足这一规则，后续开放非 Copy
字段时再提供显式能力声明。资源句柄不能自动获得该能力。
String、Array、Box、Rc 默认不是 Copy；需要复制文本、元素或共享所有权时使用显式 clone，
具体成本由类型说明，不承诺所有 clone 都是深拷贝或常量成本。

let / var 控制绑定可变性，借用类型控制访问权限，两者分开检查。
当前对局部拥有值取 `&mut` 要求 var；`let view: &mut T` 已持有独占能力，允许修改其所指值，
不允许重新绑定 view。参数绑定不可重新赋值，也不妨碍 `&mut T` 参数修改所指对象。
`&mut T -> &T` 是单向共享化：短暂调用重借用在调用结束后恢复，赋值、返回或存入对象的共享视图
会冻结原独占访问，不能从 `&T` 重新得到 `&mut T`。
借用不能比所有者活得更久；禁止返回局部拥有值的引用。字段访问自动解引用，
整个受支持的 Copy 值通过 `memory.read` / `memory.write` 读写；不引入 `*reference` 或裸指针星号语法。
自动解引用不能隐式复制或移动整个借用对象，绑定赋值仍表示重新绑定。
局部引用不得存进任务 arena，或传给具有 arena 分配能力的函数；不能借用 String、opaque、
引用本身或含引用聚合。参数引用可以从 helper 返回，按值参数的局部引用不能从该函数返回。
局部字段按根对象和字段路径检查：不同字段可分别独占；同一字段及父子路径属于重叠位置。
例如借用 p.left.x 时可以修改 p.left.y，但不能冲突地覆盖 p.left 或 p；字段引用受根绑定的词法寿命约束。
根必须是无引用 Copy 局部值，当前取借用路径不穿过引用，也不投影元组元素或枚举载荷。
具体例子、作用域和存储限额见[同步局部借用](language.md#同步局部借用)。

```joky
fn make_point() -> &Point {
    share(alloc(Point { x: 1 y: 2 }))
}

fn move_x(point: &mut Point, distance: Int32) {
    point.x = point.x + distance
}

fn choose(flag: Bool, left: &Point, right: &Point) -> &Point {
    if flag { left } else { right }
}
```

示例 Point 为包含两个 Int32 字段的记录。方法接收者可以受检地重借用，不能让已有 `&mut T`
因每次方法调用被无意永久移走。不要求用户写生命周期参数：返回引用依赖所有含引用参数的
并集，函数体按该契约检查，调用方只看签名；不生成或缓存函数体借用摘要。
使用隐式任务 arena 的函数具有同样的存储前置条件；显式签名有 `&Arena` 时，其他引用输入的对象
必须覆盖目标 arena，相关借用保守地保持到目标 arena 释放。没有分配能力的 choose 只按返回值使用期延长借用。
新分配与输入重借用的区分、多个 arena 参数和无来源返回等规则见专项 §3.2。
已有引用作为实参时自动重借用，返回值若携带该重借用，其占用范围相应延长。

专项阶段 1 禁止引用作为类型实参或进入聚合；阶段 2 才开放共享引用字段及 Option / Result
等 Copy 聚合，并按实例检查来源和存储前置条件。`&mut T` 不可复制，不能作为当前 Copy
泛型的实参；T 含引用时由 `alloc` 先产生构造期 `&mut T`，共享发布后必须 `seal`，不能用字段赋值
或 memory.write 绕过。循环共享图通过 `graph { ... }` 中的 graph.alloc 构造句柄、
graph.link 字段连接并在块末尾自动封存；没有独立 graph.seal。阶段 2b 已实现构图子集，
阶段 3a 允许 graph 位于挂起函数中，块内仍不可挂起，封存后对象可跨后续挂起保留。
具体限制见[受检循环图构造](language.md#受检循环图构造)。`&mut T` 字段和可变循环图仍拒绝。
无引用对象的普通 alloc 结果仍可独占修改。

离开作用域时，仍拥有且已初始化的非 Copy 值按逆声明顺序释放，已移动的值不重复释放；
赋值替换拥有值时释放旧值。提前返回、循环跳转、任务失败和取消都必须遵守相同清理规则。
析构不得挂起、传播可恢复业务错误或越过 native 边界 unwind；需要这些行为的资源提供显式
close 操作，析构仅做同步兜底。用户析构声明语法另行规定，不能只实现正常结束路径的清理。
第一阶段不提供多线程共享指针，也不把 Rc 类型当作可跨线程对象。

### 10.8 动态能力、挂起资源和宿主边界

`&dyn Reader` 表示具体类型被擦除但读取能力确定，`Box<dyn Reader>` 再承担对象所有权。
只有可生成固定调用入口的能力可动态化：方法有明确接收者，不能带未实例化类型参数，
也不能在接口中要求返回未知大小的裸 Self；效果仍保留在方法签名中。
第 10.5 节的静态 `Eq.equal(left, right)` 示例没有动态接收者，因此不自动可用作 dyn Eq。
dyn 不代表任意值，读取动态数据仍用 Value 等明确模型。

挂起任务保存的每个拥有值、借用、闭包环境和代码入口都有生命周期：

- 被暂停执行保留的借用必须在所有后续恢复期间有效；可移动拥有值或显式使用 Rc 保活，
  不能靠 `@suspends` 消除借用错误。
- scope 内子任务可借用父范围的数据，前提是所有退出路径都结束或取消子任务后再释放数据；
  不允许子任务携带此借用逃出 scope/race。分支实参按现有规则准备，借用冲突同样需要检查。
  对异步 FFI，取消请求不等于停止访问：已实现的宿主借用 v1 要求 stop 返回前同步停止访问，才可释放借用对象和 arena；异步取消/draining 后续设计。
- 生命周期检查尚不能覆盖挂起任务时，先拒绝跨挂起点保留普通借用，再逐步放开受检场景；
  不允许通过裸指针暂时绕过规则。
- 当前 Rust Continuation API 没有表达任意借用宿主值的生命周期，新借用能力不能直接套进
  现有入口；必须先设计任务生命周期约束或要求任务持有完整拥有者。

当前 String 的 native ABI 是借用的长度前缀视图，并不是这里的拥有型 String 布局；
引入拥有型文本后必须增加显式边界转换和保活契约，不能悄悄改变已有 ABI。
当前 `vm.Handle` 是线程绑定、显式 close 的 opaque 值，也不能仅靠名称自动升级成拥有型资源；
需要另行提供拥有包装与借用接口，并保持旧接口的关闭、重载与旧代码保活语义。
新的 Rune、元组、泛型、借用或 dyn 类型不会自动成为合法的 extern 参数；
须先定义对应 ABI 与宿主适配器，无法验证的裸入口仍需 unsafe 契约。

## 11. 核心语法（EBNF）

`[ ... ]` 表示可选，`{ ... }` 表示重复，`|` 表示选择；引号内为源码终结符。
`Identifier`、`Integer`、`StringLiteral` 按第 3 节识别，排除保留字。
下面描述 v0.1 核心及已实现的泛型函数 / 记录 / 枚举 / 透明别名、match、元组、解构绑定与固有 impl；trait / 函数值等扩展尚未合入表达式文法。
类型、挂起约束和可达性仍需语义检查，不能把这份核心文法当成所有后续扩展已冻结的规范。
当前 arena 引用可以指向含共享引用字段的 Copy 数据；经过引用的递归类型已实现。
一元取借用已实现，接受同步函数内无引用 Copy 局部变量及其内联记录字段；临时表达式和经引用解引用的路径在语义检查中拒绝。
固定 `#repr(C)` 属性、`unsafe extern "C"` 与 unsafe 块已实现，语义边界见
[Arena 设计 §2.4](arena-borrowing-ffi.md#24-词法与解析)。

```ebnf
Module       = { ";" }, { Import, { ";" } }, { Item, { ";" } } ;
Import       = "import", Identifier, { "/", Identifier }, [ ".", "{", ImportMember, { ",", ImportMember }, [ "," ], "}" ], [ "as", Identifier ] ;
ImportMember = [ "type" ], Identifier, [ "as", Identifier ] ;
Item         = [ "pub" ], (Function | TypeAlias | RecordDef | EnumDef) | External | CRecord | CExternal | ImplDef ;
ImplDef      = "impl", [ TypeParameters ], Type, "{", { [ "pub" ], (MethodDef | ConstDef), { ";" } }, "}" ;
MethodDef    = "fn", Identifier, [ TypeParameters ], MethodParams, [ Effect ], [ Result ], Block ;
MethodParams = "(", [ (Receiver | Parameter), { ",", Parameter }, [ "," ] ], ")" ;
Receiver     = [ "&", [ "mut" ] ], "self" ;
ConstDef     = "const", Identifier, ":", Type, "=", NonSequence ;
EnumDef      = "enum", Identifier, [ TypeParameters ], "{", VariantDef, [ "," ], { VariantDef, [ "," ] }, "}" ;
VariantDef   = Identifier, [ "(", Type, [ "," ], ")" ] ;
RecordDef    = "struct", Identifier, [ TypeParameters ], "{", { Parameter, [ "," ] }, "}" ;
TypeAlias    = "type", Identifier, [ TypeParameters ], "=", Type ;
Function     = "fn", Identifier, [ TypeParameters ], Parameters, [ Effect ], [ Result ], Block ;
TypeParameters = "<", Identifier, { ",", Identifier }, [ "," ], ">" ;
External     = "extern", "fn", Identifier, Parameters, [ Effect ], [ Result ] ;
CRecord      = "#", "repr", "(", "C", [ "," ], ")", [ "pub" ], RecordDef ;
CExternal    = "unsafe", "extern", '"C"', "fn", Identifier, Parameters, [ Result ] ;
Parameters   = "(", [ Parameter, { ",", Parameter }, [ "," ] ], ")" ;
Parameter    = Identifier, ":", Type ;
Effect       = "@", "suspends" ;
Result       = "->", Type ;
Type         = "Int32" | "Int64" | "UInt32" | "UInt64" | "Bool" | "Unit" | "String" | "Ptr" | Name, [ AppliedArguments ] | TypeGroup | TupleType | RefType ;
RefType      = "&", [ "mut" ], Type ;
AppliedArguments = "<", Type, { ",", Type }, [ "," ], ">" ;
TypeGroup    = "(", Type, ")" ;
TupleType    = "(", Type, ",", [ Type, { ",", Type }, [ "," ] ], ")" ;
Name         = Identifier, { ".", Identifier } ;
Integer      = Decimal | "0x", HexDigits | "0b", BinaryDigits ;
Decimal      = Digit, { [ "_" ], Digit } ;
HexDigits    = HexDigit, { [ "_" ], HexDigit } ;
BinaryDigits = BinaryDigit, { [ "_" ], BinaryDigit } ;
Digit        = "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" ;
HexDigit     = Digit | "a" | "b" | "c" | "d" | "e" | "f" | "A" | "B" | "C" | "D" | "E" | "F" ;
BinaryDigit  = "0" | "1" ;

Block        = "{", { ";" | Expression }, "}" ;
BlockForm    = Block | UnsafeBlock | If | While | Scope | Match | Graph ;
UnsafeBlock  = "unsafe", Block ;
Graph        = "graph", Block ;
Expression   = NonSequence, { ";", [ NonSequence ] } ;
NonSequence  = Binding | Jump | Assignment ;
Binding      = ("let" | "var"), BindPattern, [ ":", Type ], "=", NonSequence ;
BindPattern  = Identifier | "_" | "(", BindPattern, ")" | TuplePattern ;
TuplePattern = "(", BindPattern, ",", [ BindPattern, { ",", BindPattern }, [ "," ] ], ")" ;
Jump         = "return", [ NonSequence ] | "break" | "continue" ;
Assignment   = Place, "=", NonSequence | LogicalOr ;
Place        = Identifier, { ".", Identifier } ;
LogicalOr    = LogicalAnd, { "||", LogicalAnd } ;
LogicalAnd   = Comparison, { "&&", Comparison } ;
Comparison   = BitOr, [ CompareOp, BitOr ] ;
BitOr        = BitXor, { "|", BitXor } ;
BitXor       = BitAnd, { "^", BitAnd } ;
BitAnd       = Shift, { "&", Shift } ;
Shift        = Addition, { ("<<" | ">>"), Addition } ;
CompareOp    = "==" | "!=" | "<" | "<=" | ">" | ">=" ;
Addition     = Product, { ("+" | "-"), Product } ;
Product      = Unary, { ("*" | "/"), Unary } ;
Unary        = ("-" | "!" | "~"), Unary | "&", [ "mut" ], Unary | Postfix ;
Postfix      = Primary, { ".", Identifier, [ TypeArguments ], [ Arguments ] } ;
Arguments    = "(", [ Expression, { ",", Expression }, [ "," ] ], ")" ;
Primary      = Integer | StringLiteral | "true" | "false"
             | Identifier | PrimitiveMember | RecordValue | TupleValue | Call | TypedName | Group | BlockForm ;
PrimitiveMember = ("Int32" | "Int64" | "UInt32" | "UInt64" | "Bool" | "Unit" | "Ptr"), ".", Identifier, [ Arguments ] ;
RecordValue  = Name, [ "::", AppliedArguments ], "{", { FieldInit, [ "," ] }, "}" ;
FieldInit    = Identifier, ":", NonSequence ;
Call         = Name, [ TypeArguments ], "(", [ Expression, { ",", Expression }, [ "," ] ], ")" ;
TypeArguments = "::", AppliedArguments ;
TypedName    = Name, TypeArguments ;
Group        = "(", Expression, ")" ;
TupleValue   = "(", Expression, ",", [ Expression, { ",", Expression }, [ "," ] ], ")" ;
If           = "if", NonSequence, Block, [ "else", (If | Block) ] ;
While        = "while", NonSequence, Block ;
Scope        = ("scope" | "race"), "{", Branch, { Branch }, "}" ;
Branch       = "|", NonSequence, [ ";" ] ;
Match        = "match", NonSequence, "{", MatchArm, { MatchArm }, "}" ;
MatchArm     = "|", Pattern, "=>", Expression ;
Pattern      = "_" | Name, [ "(", Identifier, [ "," ], ")" ] ;
```

impl 目标必须是支持的具体类型或按顺序应用全部 impl 参数的记录 / 枚举，不能特化或以别名声明。
const 的表达式子集、方法 self 位置及 scope/race 分支必须为挂起调用等规则由语义检查收窄。
Postfix 上的类型实参用于泛型枚举构造、关联函数或方法自己的类型参数。
实例接收者或已绑定类型命名空间后的列表只包含方法参数；未绑定命名空间后的列表依次包含 impl 和方法参数。
显式列表必须完整，省略时从实参推导；详见[方法类型参数](language.md#方法类型参数)。
TypedName 可作类型命名空间的接收者或空载荷泛型构造，不产生类型值或函数值。

表达式按优先级尽可能继续解析；遇到不能继续当前表达式、但能开始下一表达式的 token 时，
块才开始下一项。`;` 优先并入当前 Expression，连接其后可解析的 NonSequence，
在逗号、右括号、右花括号或 scope / race / match 分支边界前允许尾随。连续分号优先并入同一序列。
末尾单独的空分号也使块结果为 Unit。换行不参与消歧，不能把 `if ... + value` 截断为独立的 if。
Binding、Assignment、Jump 的内部不直接吞入分号序列；通过 Group 显式将序列作为一个值。
Jump 没有正常完成时的值，分号不吸收返回、跳转、失败或挂起效果；语义检查负责其合法位置。
记录初始化项内部也用 NonSequence，需要序列值时使用 Group 或 Block。
元组元素使用 Expression；括号内逗号决定是否形成元组，单元素必须有逗号。
let / var 的 `_` 只丢弃，不引入名称；元组模式内名称必须唯一。
Name 后的 `{}` 或以 `Identifier :` 开始的块优先解析为 RecordValue；其他块保持独立。
条件及 match 被匹配表达式中未分组的 RecordValue 受限，精确规则见[当前记录语义](language.md#记录)。
`Name` 后紧接 `(` 或 `::<...>(` 时按 Call 解析，`::<...>` 不能独立取函数值。
空载荷泛型构造可独立写 `Option.None::<Int32>`，无括号且不是函数值。
类型注解中的 Name 先匹配当前声明的类型参数，且形参不能重复或使用 `_`；impl 参数也不能命名为 `Self`。
方法参数不能遮蔽 impl 参数、impl 目标名称或 `Self`，两层参数合计最多 64 个。
连续运算符按最长匹配分词；`||` 不是两个分支符。类型列表解析时按需拆分 `>>` 的闭合符，
或 `>=` 中的闭合符与赋值符；表达式中的比较和移位保持原规则。
EBNF 的 BitOr 在 match / scope / race 分支最外层禁用；进入括号、参数、字段、元组或块时恢复，退出时还原。
数值 token 需要整体满足 Integer，不能把 `0b102` 拆成 `0b10` 和 `2`。
枚举构造复用限定名 / Call 文法，由类型检查辨认；模式中的 `_` 不能被限定或带载荷。
模式载荷只能绑定一个标识符（含 `_`），变体声明必须与之匹配；空枚举及不穷尽的空 match 在语义阶段拒绝。

## 12. 实施顺序与验收

迁移现有源码时，保留现有可选分号与表达式序列写法；需补齐列表逗号，将导入移到文件开头，
为依赖旧四则结合方式的表达式加括号，并检查新增保留字占用的旧名称。
字符串中的裸换行改为 `\n`，需要保留反斜杠的未知转义改为显式 `\\`。
`type`、`struct`、`enum`、`match`、`impl`、`const` 已启用为保留字，以它们命名的旧函数、参数或局部变量需要重命名。
mut 已在 Arena 专项阶段 1、unsafe 已在阶段 5a 启用为保留字；同名旧标识符需要迁移。
memory 是内建读写名称，不新增 ref 关键字。
借用表达式中，`a` 换行接 `&b` 仍按 `a & b` 解析，分开表达式应写 `a; &b`；
多层引用无论写成 `&&T`、`& &T` 或通过别名组合，首版均拒绝。
`Int64`、`UInt32`、`UInt64`、`Ptr` 已启用为类型关键字，原先使用这些名称的用户标识符需改名。
`int32`、`int64`、`uint32`、`uint64` 现为内建转换函数名，同名用户函数、extern 和宿主绑定需改名；
同名局部变量和类型名称仍合法。转换参数默认定型规则与目标返回类型相互独立。
`__joky_generic_` 前缀保留给编译器实例，使用该前缀的用户函数或 extern 需要改名。
数值后紧邻标识符须添加空白或 `;`，如 `1foo` 不再拆成两个表达式。
旧的 `let _ = ...` 再读取 `_` 的代码须给绑定改名；它现在是丢弃模式。
名称后独立空块需要写成 `value; {}` 或 `(value) {}`，避免被识别为记录构造。
第一版先采用单一目标语法，不设计兼容模式或按文件切换语法版本的指令。

1. **规范解析边界**：运算符优先级、一元运算和 Bool 相等比较已实现；保留分号连接及可省略规则。
   参数与实参必须用逗号分隔、允许尾随逗号；字符串转义和文件开头导入规则已实现。
   依赖旧优先级的测试已加括号；现有示例和三个循环基准符合新解析规则。
   旧省略逗号、后置导入、未知转义和裸换行写法需要迁移，不能宣称保持旧程序语义。
2. **补齐控制流**：if、短路、return、break、continue 已通过共享 IR 实现，
   JIT、AOT 和解释器覆盖一致的求值顺序、类型检查与不可达诊断。
3. **覆盖挂起路径**：条件、分支、短路右侧、返回值和循环跳转均已覆盖多次 Pending、不重放、
   失败、取消及重载；scope 完成与跳转顺序、参数提前返回及解释器时间片也有回归测试。
4. **完成跨后端一致性**：四种整数除零失败、位宽环绕、无符号比较/除法及有符号 `MIN / -1`、同步/CPS/scope 失败传播及源码范围
   已覆盖三个后端；仍保留[诊断及资源限制差异](architecture.md#已知边界)，不宣称所有运行时能力相同。

关键验收用例：`1 + 2 * 3` 为 7；`false && side_effect()` 不调用右侧；无 else 的 Int32 分支
报错；`let x = 1; x = 2;` 报错；循环外 break 报错；同步函数调用挂起函数报错；
`fn f() -> Int32 { let x = 1 let y = 2 x + y }` 无分号也返回 3；
`f(a; b, c)` 只有两个实参；函数末尾 `42` 与 `42;` 的结果类型不同；
`f(a b)` 报缺少逗号，`f(a, b,)` 合法；字符串转义跨后端保持字节一致，包含 NUL；
悬挂任务恢复后每个副作用只执行一次。

类型系统按依赖继续推进，不在 v0.1 核心阶段同时实现全部内容：

5. **类型基础与资源规则**：数值推导、转换、类型别名、记录、元组、枚举与 match；
   Int32 / Int64 / UInt32 / UInt64 上下文定型、显式 wrapping 转换、透明别名、Copy 记录/枚举/元组、解构绑定、穷尽 match 及共享类型解析已实现，包含三后端同步/CPS、值快照、
   宿主边界拒绝、Pending 不重放、失败/取消及重载后的旧任务保活检查。
   建立移动、Copy、借用和退出清理的静态规则，先不开放未经验证的跨挂起资源。
6. **泛型与静态能力**：最小 Copy 泛型函数已实现，覆盖定义检查、推导与显式实参、
   递归实例缓存和限额、三后端同步 / CPS、Pending 不重放及旧任务保活。
   Copy 参数化枚举已实现，支持 Option / Result、嵌套实例、构造推导与穷尽匹配。
   Copy 参数化记录已实现，支持字段 / 上下文推导、显式构造、嵌套实例、字段更新及跨模块身份。
   固有 impl、按值方法、关联常量及以 Joky 编写的受检整数转换已实现。
   Copy 参数化记录 / 枚举的泛型固有 impl 已实现，支持 Self、关联函数推导及三后端同步 / CPS 实例化。
   Copy 参数化透明别名已实现，支持嵌套应用、跨模块定义作用域、循环检查与有界展开。
   方法独立 Copy 类型参数已实现，包含两层参数推导、显式调用和 Option.zip / Result.and 的源码库用例。
   Option / Result 的状态判断、默认值、and / or 组合及 Result 到 Option 的载荷转换已实现，均不依赖闭包或 trait。
   后续加入 trait、约束求解、函数指针类型与受检间接调用，再扩展常量泛型。
   容器按其实际依赖推进：数值动态数组不以完整 trait、常量泛型或通用迭代器为前提。
7. **资源与动态调用**：按 [Arena 专项步骤](arena-borrowing-ffi.md#6-实现步骤与验收)推进内存布局、
   隐式任务 arena、`alloc`/`share` 借用契约、共享引用字段与封存图、挂起任务、普通局部借用、
   同步 C 接口、不透明 Ptr 往返及异步宿主借用；外部资源拥有包装与自动清理另行设计。
   生命周期覆盖任务恢复、失败、取消和重载后，再开放携带借用的挂起任务。
   拥有型 String、Box / Rc、闭包能力和 dyn 后续单独扩展。
8. **纯 Joky 容器与所有权**：按 [容器扩展步骤](arena-borrowing-ffi.md#61-容器与所有权扩展)，
   先用 Joky 实现 `Array<Int32>` 的受检访问、push/pop 与扩容，再推广 Copy 元素泛型容器，
   最后支持资源型元素、嵌套容器和可变链表。同步容器无需等待异步 FFI；跨挂起使用另行
   验证保活与取消。源码库算法、存储原语和三后端行为共同构成验收。

类型验收还需覆盖：UserId / OrderId 不混用，别名等价，字面量溢出拒绝、窄化显式，
match 不穷尽报错，移动后使用拒绝，独占借用不重叠，不能返回局部引用，闭包调用次数受约束，
挂起效果不能经泛型或 dyn 擦除，所有退出路径的资源只释放一次。

后续尚待单独设计的表面语法：闭包字面量与捕获、用户析构、关联类型、容器字面量和索引。
本草案暂不加入：异常捕获、通用效果处理器、类继承、高阶类型、`for` 循环、复合赋值、
包管理、反射和多线程调度。类型布局与 ABI 变更必须有独立的迁移方案。
