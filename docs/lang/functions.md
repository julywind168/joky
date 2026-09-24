# 函数与调用

本文描述当前语言行为。章节导航见[语言索引](README.md)。

- [函数](#函数)
- [管道表达式](#管道表达式)

## 函数

函数参数必须标注类型，返回类型使用 `->`；函数体最后一个表达式就是返回值：

```joky
fn add(a: Int32, b: Int32) -> Int32 {
    a + b
}

let total = add(b: 2, a: 1)
```

调用可以使用参数标签；带标签的参数按名称匹配，因此书写顺序无关。未标记的参数仍按位置匹配，也可以与标签参数混用。

返回 `Unit` 的函数可以省略返回类型，也可以显式写成 `-> ()`：

```joky
fn announce() -> () {
    println("hello")
}
```

匿名函数使用同样的签名语法，可以绑定到变量并调用；函数类型也可以写在类型注解中：

```joky
let inc: fn(Int32) -> Int32 = fn (x: Int32) -> Int32 {
    x + 1
}
let value = inc(41)
```

函数值也支持按标签重排参数；保存于 struct 或 class 字段中的函数值可以直接调用：

```joky
let add: fn(left: Int32, right: Int32) -> Int32 =
    fn (first: Int32, second: Int32) -> Int32 { first + second }
let value = add(right: 2, left: 1)
let holder = Holder(callback: add)
let again = holder.callback(right: 2, left: 1)
```

参数名是命名函数类型的一部分。`fn(Int32, Int32) -> Int32` 是仅支持位置参数的简写。
当闭包位于命名函数类型的上下文中，参数和返回类型可以省略：

```joky
let holder = Holder(callback: fn (left, right) { left + right })
```

没有函数类型上下文时，闭包参数仍需显式标注类型。

普通闭包只会捕获可复制或共享的外部值。捕获 `class` 等唯一所有权值时，必须显式使用 `move fn`：

```joky
let offset = 10
let add = fn (x: Int32) -> Int32 { x + offset }
let counter = Counter(value: 1)
let read = move fn () -> Int32 { counter.value }
```

闭包在创建时将捕获值写入独立的堆环境；每次调用从环境恢复捕获，离开作用域时会递归释放其中的托管值。函数值采用统一的代码指针加环境指针 ABI，因此也可以作为参数、返回值或 struct/class 字段保存。

## 管道表达式

编译期类型值通过普通参数传入，例如 `identity(Int64, 1)`；类型参数不会进入运行时参数列表。

管道表达式 `|>` 将左侧值插入右侧函数调用的第一个参数位置。一步操作使用成员调用，多步组合可以使用管道：

```joky
let found = "hello world" |> string.starts_with(prefix: "hello")
```

上式等价于 `String.starts_with("hello world", prefix: "hello")`。

匿名函数、尾随闭包和捕获规则详见[闭包](closures.md)。
