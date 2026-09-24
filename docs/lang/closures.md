# 匿名函数与尾随闭包

> 本文描述匿名函数（闭包）字面量、尾随闭包调用形式和 `when` 绑定。可变捕获的规则见[绑定](basics.md#绑定)。

## 1. 匿名函数字面量

匿名函数使用 `|params| body` 形式书写，参数列表位于两个 `|` 之间，body 是表达式或代码块：

```joky
let double = |x: Int32| x * 2
let add    = |x: Int32, y: Int32| x + y
let greet  = || "hello"
```

无参数时写 `||`。参数类型可从上下文推导，有歧义时显式标注。body 是单个表达式时不需要 `{}`；需要多条语句时使用块，最后一个表达式是返回值：

```joky
let process = |x: Int32| {
    let doubled = x * 2
    doubled + 1
}
```

匿名函数的类型写作 `fn(T, U) -> R`，与具名函数类型一致。`||` 的类型是 `fn() -> R`：

```joky
let transform: fn(Int32) -> Int32 = |x| x * 2
```

### 捕获语义

普通闭包不能直接捕获外层 `var`；`move fn` 可以消费可变绑定，将其状态移入
闭包的持久环境。需要只读快照时，先在闭包外写 `let snapshot = value`，再捕获
该不可变绑定。闭包内部声明的 `var` 则每次调用重新初始化。

```joky
var n = 1
let read = move fn() -> Int32 { n = n + 1; n }
read() // 2
read() // 3
n = 100
read() // 4，外层 n 仍为 100
```

即使 `var` 持有 Copy 或 Shared 值，捕获也会消费外层绑定，重新初始化前不可读取
或再次捕获。函数值仍使用 `fn(...) -> R` 类型，具有唯一所有权并通过借用调用；
别名、传参、返回及移交任务均转移同一个环境，不能复制到多个任务。

可变捕获限制：不能实际挂起（包括无外泄 effect 的内部任务等待），不能
从借用环境移出 Owned 字段，也不能再次将环境中的可变槽移入嵌套闭包。可先声明
调用内的局部 `var` 再移入嵌套闭包。允许借用 Owned 字段并整体替换；共享字段读取
仍复制引用。保留已有 Drop 捕获限制，`CCallback.new` 不接受可变捕获。

匿名函数按值捕获外层绑定。捕获遵循普通所有权规则：不可变值和可复制值通过 `dup` 复制到闭包环境；唯一所有权值（class 或含 class 的结构）通过 `move` 移入闭包，之后外层绑定不可再使用：

```joky
let offset = 10
let shift = |x: Int32| x + offset   // 复制 offset

let counter = Counter(value: 0)
let bump = || {                       // 移入 counter
    counter.increment()
    counter.value
}
// counter 已被移入，此后不可读写
```

闭包不能捕获 Cown lease、`when` body 局部变量或跨挂起点存活的受约束值。
捕获的唯一所有权值不能被多个并行 branch 同时捕获；编译器会拒绝重复捕获。

### 作为值传递

匿名函数是一等值，可以传给接受函数类型参数的函数：

```joky
fn apply(f: fn(Int32) -> Int32, x: Int32) -> Int32 {
    f(x)
}

let result = apply(|x| x * 3, 7)   // 21
```

标准库的高阶函数（`list.map`、`list.filter` 等）均接受 `fn(T) -> U` 参数：

```joky
import joky/list

let items  = List(1, 2, 3, 4, 5)
let evens  = items |> list.filter(|x| x % 2 == 0)
let squares = items |> list.map(|x| x * x)
```

## 2. 尾随闭包

当函数的最后一个参数类型为函数类型时，可以把闭包写在调用括号之后，作为尾随闭包：

```joky
// 普通写法
list.map(items, |x| x * 2)

// 尾随闭包写法
list.map(items) |x| { x * 2 }
```

尾随闭包的 body 必须使用 `{}` 块，即使只有一个表达式。前面的括号中保留其余实参；若闭包是唯一参数，括号可以省略。0 参数闭包的 `||` 也可以省略，直接写 `{ ... }`；显式 `||` 仍然有效：

```joky
fn repeat(n: Int32, action: fn() -> Unit) -> Unit { ... }

// || 省略，括号可省略
repeat(3) { println("tick") }

// 显式 || 等价
repeat(3) || { println("tick") }
```

`||` 省略只适用于尾随位置；独立书写 0 参数闭包仍需 `||` 前缀（`let greet = || "hello"`）。

两种写法语义完全等价，尾随形式只是书写上的便利。参数类型仍从声明推导，显式标注与普通闭包一致：

```joky
fn with_index(
    items: List(String),
    f: fn(UInt64, String) -> Unit
) -> Unit { ... }

with_index(labels) |i, label| {
    println(i.to_string() + ": " + label)
}
```

一次调用只允许一个尾随闭包；若函数有多个函数类型参数，只有最后一个可以写成尾随形式，其余必须内联传入。

## 3. `when` 绑定

`when` 使用尾随闭包参数为 Cown payload 命名。

### 单 Cown

```joky
when (counter) |state| {
    state.value = state.value + 1
}
```

参数名可以自由选择，不必与 Cown 绑定名相同。

### 多 Cown

```joky
when (source, target) |src, dst| {
    let amount = src.value
    src.value = 0
    dst.value = dst.value + amount
}
```

参数数量必须与 Cown 列表长度相同。`_` 可丢弃不需要的 payload：

```joky
when (a, b) |_, y| {
    y.value = y.value + 1
}
```

### 隐式绑定（简单名称 Cown）

Cown 表达式是简单标识符时，可以省略 `|...|` 参数列表，payload 隐式绑定为与 Cown 同名的局部变量。多 Cown 场景下全部参数都是简单名称才允许省略：

```joky
// 单 Cown，payload 隐式绑定为 counter
when (counter) {
    counter.value = counter.value + 1
}

// 多 Cown，全部简单名称，隐式绑定
when (source, target) {
    let amount = source.value
    source.value = 0
    target.value = target.value + amount
}
```

Cown 表达式含方法调用、字段访问或其他非简单名称时，必须显式写出 `|...|`。实践中遇到命名歧义时推荐始终写明 `|...|`。

### 约束

`when` 受以下语义约束：

- lease 不跨挂起点、任务边界、堆存储或函数返回；
- body 不能包含 `@suspends` operation；
- body 不能启动捕获 lease 的 branch；
- acquire 按稳定 Cown ID 排序；
- 所有正常、失败、取消和 panic 路径释放 lease。

## 4. 约束与非目标

**暂不支持：**

- 参数模式解构（`|(a, b)| ...`、`|Point { x, y }| ...`）；
- 显式捕获列表（`[move x] |y| ...`）；
- 递归匿名函数；
- `@suspends` 或 `effects { ... }` 标注在匿名函数上（需要挂起时使用具名函数）；
- 多个尾随闭包；
- `when` body 内使用 `@suspends` operation。

**其他暂不支持：**

- 闭包的泛型类型参数；
- 返回类型标注在参数列表之后（`|x| -> T { ... }`）；
- 部分应用与柯里化语法。

可变捕获的约束见[绑定](basics.md#绑定)。
