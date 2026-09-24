# 循环与游标

本文描述当前语言行为。章节导航见[语言索引](README.md)。

- [范围表达式](#范围表达式)
- [顺序 for](#顺序-for)
- [Cursor 与容器适配](#cursor-与容器适配)
- [有界并发 for](#有界并发-for)
- [while、loop 与 continue](#whileloop-与-continue)

## 范围表达式

范围表达式产生惰性的 `Range(T)` 值，实现 `Cursor`，可以直接交给顺序 `for`：

```joky
0..5                 // 0, 1, 2, 3, 4
0..=5                // 0, 1, 2, 3, 4, 5
0..10 by 3           // 0, 3, 6, 9
5..=1 by -2          // 5, 3, 1

let small: Range(UInt8) = 0..=255
let squares = for i in 0..5 { i * i }
var sum = 0
for i in 1..=100 { sum = sum + i }
```

`..` 左闭右开，`..=` 两端包含；终点只有在步长恰好到达时才会产出。
默认步长为 `1`，不自动改变方向：`5..0` 是空范围，倒序需要显式负步长。
起点、终点、步长按书写顺序各求值一次，在创建范围时保存；之后修改端点变量
不会影响已经创建的范围。

三者必须使用同一种整数类型。字面量可从 `Range(Int64)` 等期望类型或非字面量
操作数获得类型，无类型信息时默认为 `Int32`；已确定类型的整数之间不隐式转换。
支持全部八种内建整数类型，`Range(T)` 的 `T` 必须是具体整数类型。
浮点数和 Duration 不支持，无符号整数不接受负步长。
常量零步长编译报错；动态零步长在创建范围时 panic，即使范围为空或没有被迭代。

范围运算符的优先级低于位运算、高于逻辑与比较。端点和 `by` 后的表达式按加减的优先级解析，因此包含 `+`、`-`、`*`、`/`、`%`，不吞掉更松的位运算：
`start + 1..=end * 2 by step + 1`，而 `a .. b & c` 是 `(a .. b) & c`。成员访问绑定到最近的操作数，对范围调用方法
需写 `(0..5).advance()`。不支持链式范围、缺失端点、无界范围和范围模式匹配。
`by` 仅在范围终点后作为上下文关键字，其他位置仍可用作名称。
换行沿用普通表达式规则：运算符后可续行，运算符前的换行结束表达式。

范围自身和每次推进都是 O(1)，不分配输入列表或堆上的游标。它是可复制值，
`advance(self)` 返回元素与后继范围，原绑定保留自己的位置；循环体仍可挂起。
耗尽后不会回绕：整数极值也可作为闭区间终点，下一步溢出时后继范围直接耗尽。
`break` 不再推进后续项。`@parallel for` 当前仅接受 List，因此不接受范围。
示例见 [range.jk](../../examples/collections/range.jk)。

## 顺序 for

`for` 默认顺序执行，是收集结果的表达式。输入可以是 `List(T)`、`Range(T)` 或实现
内建 `Cursor` 的 struct/class：

```joky
let items = List(1, 2, 3, 4, 5)
let results = for item in items {
    if item == 2 { continue } else { }
    if item == 4 { break } else { item * item }
}
// results 为 List(Int32)，包含 1、9。

let indexed = for (index, item) in items { (index, item) }
```

每次正常完成的迭代收集块末尾的值；`continue` 不产生结果，顺序 `break`
返回此前收集的结果。`for` 不接受 `break value`。所有正常完成的路径必须
产生相同类型；空输入返回该类型的空 List。循环体是 `Unit`、且周围没有
`List` 类型期望时，`for` 本身也是 `Unit`，不分配结果列表。`continue`
只用于收集循环里跳过某一项。`(index, item)` 专门表示索引
和元素绑定，不是元素的元组解构；索引从 0 开始，类型为 `UInt64`。

累加、计数等任务私有状态用局部 `var` 保存，不需要 class 包装。赋值
表达式返回 `Unit`，因此副作用循环可以直接写：

```joky
var sum = 0
for item in List(1, 2, 3) {
    sum = sum + item
}
```

## Cursor 与容器适配

内建 `Cursor` 的契约为 `type Item` 和
`fn advance(self) -> Option((Item, Self))`。`advance` 消费当前游标，返回
元素和后继游标；`None` 表示耗尽。实现必须按值接收 `self`：

```joky
struct CountDown { let remaining: Int32 }

impl Cursor for CountDown {
    type Item = Int32
    fn advance(self) -> Option((Int32, Self)) {
        if self.remaining > 0 {
            Some((self.remaining, CountDown(remaining: self.remaining - 1)))
        } else { None }
    }
}

fn collect(T: type + Cursor, source: T) -> List(T.Item) {
    for item in source { item }
}

let values = collect(CountDown(remaining: 3))
```

`Bytes.iter()` 产出内建字节游标，`Item` 为 `UInt8`；每步 O(1) 且不复制
内容，遍历期间游标持有 Bytes 的一个引用，原值可继续使用：

```joky
let data = b"abc"
let codes = for byte in data.iter() { byte }   // List(UInt8)
```

`joky/sqlite` 的 `statement.query()?` 返回 `SqliteRows`，可直接用于顺序
`for`。元素是 `Result(SqliteRow, String)`，每次推进只复制当前行，查询错误
出现一次后耗尽。调用函数需声明 `effects { sqlite }`：

```joky
for result in db.prepare("select name from users")?.query()? {
    let row = result?
    println(row.text(0)?!)
}
```

类型化 getter 区分 NULL、类型不符和索引越界。`break` 或 `?` 退出时释放
游标，保留的行仍可读取。该游标不能传给无 effect 的 `T: type + Cursor`
泛型，也不支持 `@parallel for`。完整契约见 [SQLite](../stdlib/sqlite.md)。

Map 与 Set 的 `entries()` / `keys()` / `values()` / `iter()` 产出内建
容器游标（`Item` 分别为 `(K, V)`、`K`、`V`、`T`）。`for` 对 Map / Set /
`MutMap` / `MutSet` / `MutList` 做 IntoCursor 转换：不可变 Map 与 Set
走 `entries()`，可变容器走消费式 `into_iter()`。Range 本身实现 Cursor，
`for i in 0..10` 无需转换。遍历顺序是哈希序，进程内确定但跨运行可能不同，
不要依赖它，需要有序结果时先收集再 `sorted()`。这些游标是唯一所有权值，
遍历期间整个容器保持存活，遍历后原容器不受影响：

```joky
let table = Map(String, Int32)().insert("a", 1)
let keys = for k in table.keys() { k }
let pairs = for entry in table { entry }
```

可变容器走同一套 `Cursor` 协议，但容器自己不是游标。`into_iter()` 消费
容器，产出拥有原容器的索引游标，每步 O(1)；游标析构时释放容器，因此
`for x in xs.into_iter()` 之后不能再使用 `xs`。需要遍历后继续用容器时，
先 `to_list()` 拿快照再对 List 遍历。过滤用 `retain`，不要在遍历中增删：

```joky
let items = MutList#{1, 2, 3, 4}
items.retain(|value| value > 2)
let snapshot = items.to_list()
let collected = for value in items.into_iter() { value }
```

List 的 `Item` 是其元素类型，支持 `list.advance()` 和
`Cursor.advance(list)`，也满足 `T: type + Cursor`。`for` 固定选择
`Cursor.advance`，不受同名普通方法影响。游标按其实际类型遵守 Copy、共享
引用或唯一所有权规则；唯一所有权游标交给循环后不能再次使用。
游标可以逐项产出唯一所有权 class，但收集的循环体结果仍须满足 List
的元素限制。`break`、`?`、abort 和取消会清理仍持有的元素与后继游标。

具体游标的 effect 按实现传播，可以在 `do` 中处理；前向引用尚未检查的
`advance` 方法体时，保守使用其声明 effect。泛型 `Cursor` 的 effect
契约为空，因此不接受声明了额外 effect 的实现；内部 task wait 仍可挂起
并恢复。`Cursor` 返回 `Self`，不支持 `Dyn(Cursor)`。
完整示例见 [cursor.jk](../../examples/collections/cursor.jk)。

## 有界并发 for

用 `@parallel(limit: N)` 把同一循环改为有界并发：

```joky
let results = @parallel(limit: 5)
    for item in items { item * item }
```

`N` 是一次求值的正 `UInt64` 表达式；输入先求值，随后求值上限。字面量 0
在编译时拒绝，动态 0 在运行时报告失败。注解只作用于紧随其后的 `for`。
当前并发输入仅支持 List；自定义 Cursor 使用顺序 `for`。
迭代可执行普通计算，也可通过同一套 Effect/Pending 协议调用文件或网络
provider；不需要单独的 batch I/O API。

并发版保留输入顺序，允许 `continue`，禁止退出该并发循环的 `break`；
嵌套的普通 `loop`、`while` 或顺序 `for` 仍可 `break`。不允许从任务或闭包
内部 `break`/`continue` 到外面的循环。并发迭代也不能用 `?` 从调用者函数
返回；可以收集 `Result`，或显式处理错误。未处理的 Effect 失败会取消同批
其他迭代，排空后传播；`Err` 本身只是一个结果值，不会自动取消批次。

上限包括正在计算和正在等待 I/O 的迭代。每批采用局部额度，嵌套上限为 1
仍可挂起父任务、推进子批次；多层额度不构成全局任务总量限制。外部唯一
所有权或借用值不能反复捕获进多个迭代，应在迭代内创建资源或使用 Cown。
每次迭代中的 `branch` 在迭代结束或 `continue`/顺序 `break` 前排空。
显式嵌套的任务和批次拥有自己的并发度，不计入外层的直接迭代上限。

运行时只创建 `min(N, 输入长度)` 个可复用逻辑任务，不会为每个输入先创建
任务再排队。输入和保留的完整输出仍可能占 O(输入量/输出量) 内存，限制
迭代数也不限制单次整文件读取的字节数。详见
[有界迭代与资源边界](../runtime/bounded-iteration.md) 和
[可运行示例](../../examples/concurrency/bounded_for.jk)。

## while、loop 与 continue

`while` 是返回 `Unit` 的表达式，条件必须是 `Bool`，循环体是一个代码块：

```joky
while true {
    println("tick")
    break
}
```

`loop` 创建无限循环，`break` 结束最近一层循环。`break` 可以携带一个值，因此 `loop` 是表达式；没有值的 `break` 返回 `Unit`：

```joky
let result = loop {
    if ready {
        break 42
    } else {
        println("waiting")
    }
}
```

`continue` 跳过当前循环体剩余部分，继续下一次最近一层循环的迭代；它不携带值：

```joky
while false {
    continue
}
```
