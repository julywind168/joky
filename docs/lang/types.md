# 类型与泛型

本文描述当前语言行为。章节导航见[语言索引](README.md)。

- [编译期类型值](#编译期类型值)
- [Struct](#struct)
- [Class](#class)
- [Enum](#enum)

## 编译期类型值

顶层函数也支持把类型参数写在普通参数列表前部：

```joky
fn identity(T: type, value: T) -> T { value }

fn ListOf(T: type) -> type { List(T) }

fn first(T: type, values: ListOf(T)) -> Option(T) {
    values.head()
}

fn Wrapped(T: type) -> type {
    let Payload: type = ListOf(T)
    Option(Payload)
}
```

类型参数本身是编译期值，类型构造器也使用普通调用语法：

```joky
fn first(T: type, values: ListOf(T)) -> Option(T) {
    values.head()
}

fn main() {
    let N = Int64
    let values: ListOf(N) = List(1, 2)
    let result: Option(N) = first(N, values)
}
```

`List(T)`、`Option(T)`、`Result(T, E)`、`Map(K, V)` 等内置构造器会在编译期求值，不会生成运行时调用。类型值可以绑定到局部名称，也可以作为类型函数的参数和返回值。类型函数只能包含类型参数，且必须返回 `type`；类型函数的函数体是编译期求值的表达式块。

内置代数数据类型也可以通过类型值访问变体：

```joky
let O = Option(Int64)
let value: O = O.Some(value: 42)
let missing: O = O.None

let R = Result(Int64, String)
let result: R = R.Ok(value: 42)
```

`Option(T)` 和 `Result(T, E)` 的变体仍使用专用的紧凑运行时表示，但构造、匹配和 ownership 规则与普通 enum 一致。旧的 `Some(...)`、`None`、`Ok(...)`、`Err(...)` 形式继续兼容。

```joky
fn Number() -> type { Int64 }

fn main() {
    let N: type = Number()
    let value: N = 42
    let Values = ListOf(N)
    let values: ListOf(N) = identity(Values, List(value))
    let result: Wrapped(N) = Some(values)
    let head: Option(N) = first(N, List(value))
}
```

`type` 表示编译期类型值，不是运行时反射对象。局部类型绑定遵循普通词法作用域和遮蔽规则，可出现在嵌套类型注解中，也可作为泛型调用的前导参数。类型绑定、类型函数和调用中的类型参数不会进入运行时 ABI，也不会成为闭包的运行时捕获。

类型注解使用结构化表达式，支持名称、关联类型成员、类型函数调用、元组和函数类型。参数、返回值、字段、Effect operation 和局部绑定等注解位置均可使用 `ListOf(Int64)`；声明顺序不影响类型函数的可见性。嵌套形式如 `Result(ListOf(Int64), String)`、`fn(ListOf(Int64)) -> Option(Int64)` 和 `(ListOf(Int64), String)` 也可使用。类型函数调用与局部类型值绑定复用同一个编译期应用规则。

内置类型构造器包括 `List(T)`、`MutList(T)`、`Option(T)`、`Result(T, E)`、`Map(K, V)`、`MutMap(K, V)`、`Set(T)` 和 `MutSet(T)`。这些调用直接表示编译期类型值，容器的所有权及键约束仍然适用。`List(1, 2)` 仍然构造运行时列表。

当前用户类型函数的支持范围：

- 返回类型必须写为 `-> type`，参数只能是无显式 trait 约束的类型参数，也允许零参数。
- 函数体可以引用类型、组合类型函数调用、创建局部类型绑定，以最后一个类型表达式作为结果。用户类型函数可前向引用，也支持标签参数。
- 类型函数不能声明或执行 Effect，不允许运行时计算、递归或隐式捕获调用者的局部名称；类型函数调用深度最多为 64。未调用的定义也会被检查。
- 类型函数可以生成并缓存匿名 struct 和 enum 实例；字段、变体 payload、默认值会随具体类型参数实例化，运行时构造复用普通 struct/enum 的布局和 ownership 规则。匿名 class 仍未开放。

匿名 struct 的字段类型和默认值也会在实例化时具体化：

```joky
fn Box(T: type) -> type {
    struct {
        let value: T = 0
    }
}

let value: Box(Int64) = Box(Int64)()
```

匿名 enum 可以像命名 enum 一样构造和匹配：

```joky
fn Maybe(T: type) -> type {
    enum {
        Some(value: T)
        None
    }
}

fn main() {
    let M = Maybe(Int64)
    let value: M = M.Some(value: 42)
    let result = match value {
        M.Some(value: number) => number
        M.None => 0
    }
}
```

生成类型按类型函数、匿名类型表达式和具体类型参数缓存；相同实例复用同一个布局，递归类型函数会在编译期被拒绝。不支持模块级 `const` 类型别名或把类型函数本身绑定、传递。

类型声明、类型注解和泛型调用统一使用普通括号及 `type` 参数；类型参数不会进入运行时 ABI。

## Struct

`struct` 定义带名字字段的不可变值类型。它隐式提供一个以字段为参数的构造函数；没有默认值的字段必须传入，带默认值的字段可以省略。标签参数可按任意顺序书写：

```joky
struct Point {
    let x: Int32
    let y: Int32 = 0
}

let point = Point(x: 1)
let x = point.x
```

`struct` 按值传递，字段不可赋值。包含 class 的 struct 仍是不可变值，但传递时会 move。

方法内读取字段时可以省略 `self.`。局部绑定或参数与字段同名时，裸名称优先表示局部值；此时使用 `self.x` 显式访问字段。

## Class

`class` 是唯一所有权的可变堆对象，用于性能敏感的底层和框架代码。字段默认可读；`let` 字段只在构造时赋值一次，`var` 字段只能由类自身的方法通过 `self` 修改。它同样隐式提供以字段为参数的构造函数：

```joky
class Counter {
    let name: String
    var value: Int32 = 0

    fn increment() {
        self.value = self.value + 1
    }
}

let counter = Counter(name: "requests")
counter.increment()
let value = counter.value
```

`let counter` 不可重新绑定，但它拥有的对象可以通过方法改变；局部 `var counter`
还允许替换整个对象。class 不可隐式共享，赋值、传参和返回默认转移所有权：

```joky
let first = Counter(name: "requests")
let second = first
second.increment()
// first 已被 move，不能再使用
```

需要第二个独立对象时必须显式 clone。方法省略 receiver 时默认借用；显式
`&self` 也表示借用，显式 `self` 表示消费所有权：

```joky
class Counter {
    var value: Int32 = 0
    fn increment(&self) { self.value = self.value + 1 }
    fn finish(self) -> Int32 { self.value }
}

fn increment(counter: &Counter) { counter.increment() }

let counter = Counter()
increment(counter)
counter.increment()
let result = counter.finish()
// counter 已被消费，不能再次使用
```

`&Counter` 是参数的借用模式，调用点直接传 `counter`。`&` 不创建可以保存的
引用值；借用不能返回、写入 owning 字段、捕获到闭包或移交给并发任务，也
不能传给消费参数。class 的内部可变性不因借用改变，`struct` 仍不可变，
不引入 `mut` 或 `&mut`。trait 方法也通过 `&self` / `self` 声明 receiver
模式，实现必须匹配；省略 receiver 的实现仍默认为借用。

同一次调用不能传入来自同一 owning 对象的重叠借用。参数求值时，不得消费
仍有借用待使用的对象，也不得通过父对象修改仍被借用的字段。当前检查按
owning local 保守判断，因此同一父对象的不同 class 字段也应分次访问。
这也禁止在后续实参中重新赋值已被 receiver、callee 或前面实参借用的 `var`。

普通 class 借用不能存活跨越挂起点。`File` 和 socket 借用传递稳定的堆句柄，
由挂起调用的所有者保持资源生命周期，可以跨 I/O 挂起；
`Cown` lease 仍遵守 `when` 的限制。可变容器 intrinsic 方法使用 `&self`。

## Enum

`enum` 定义不可变的代数数据类型。变体字段使用带标签的参数语法，无数据变体省略括号：

```joky
enum Shape {
    Circle(radius: Float32)
    Rectangle(width: Float32, height: Float32)
    Triangle
}

let circle = Shape.Circle(radius: 2.0)
let rectangle = Shape.Rectangle(height: 3.0, width: 2.0)
let triangle = Shape.Triangle
```

`match` 是表达式，必须覆盖全部变体，并且所有分支返回相同类型。绑定名与字段同名时可以简写；需要改名时使用标签：

```joky
match shape {
    Shape.Circle(radius) => radius * radius
    Shape.Rectangle(width: w, height: h) => w * h
    _ => 0.0
}
```

顶层的 `_` arm 匹配其余所有值，必须放在最后。

模式可以继续解构嵌套的 enum：

```joky
match wrapped {
    WrappedShape.Some(value: Shape.Circle(radius)) => radius
    WrappedShape.Some(value: _) => 0.0
    WrappedShape.None => 0.0
}
```

嵌套位置同样支持不带限定名的 `Some`/`None`/`Ok`/`Err`，因此
`Result` 与 `Option` 的组合可以直接一层写出：

```joky
match result {
    Ok(Some(number)) => number
    Ok(None) => 0
    Err(message) => panic(message)
}
```

嵌套和顶层都可以写元组模式。`(x)` 只是分组，`(x,)` 与 `(x, y)` 才是元组；
`for (index, item)` 仍表示带下标的迭代，条目解构用循环内的 `match`：

```joky
match pair {
    (left, right) => left + right
}

match result {
    Ok((left, right)) => left + right
    Err(message) => panic(message)
}

for entry in map {
    match entry {
        (key, value) => println(key + ": " + value)
    }
}
```
