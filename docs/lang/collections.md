# 集合

本文描述当前语言行为。章节导航见[语言索引](README.md)。

- [List](#list)
- [Map](#map)
- [Set](#set)

## List

`List(T)` 是不可变的持久化单链表。`push_front` 复用原 list 的尾部，创建新值不会修改旧绑定：

```joky
let values = List(1, 2, 3)
let extended = values.push_front(0)
let first = extended.head().unwrap_or(-1)
let rest = extended.tail()
let count = extended.length()
let reversed = extended.reverse()
let empty = List(Int32)()
```

`is_empty()` 返回 `Bool`；`length()` 返回 `UInt64`；`reverse()` 返回一个新 List；`head()` 返回 `Option(T)`，`tail()` 返回 `Option(List(T))`。元素可以是数值、`Bool`、`String`、List、tuple、struct、enum、Option 或 Result。class 是唯一所有权引用，class 及内部含 class 的值不能作为 List 元素。

集合也支持带类型标签的字面量。`List#{...}`、`Set#{...}`、`Map#{...}` 产生不可变容器；`MutList#{...}`、`MutSet#{...}`、`MutMap#{...}` 产生唯一所有权的可变容器：

```joky
let values = List#{1, 2, 3}
let tags = Set#{"compiler", "language"}
let users = Map#{"alice" => 1, "bob" => 2}
let empty: List(Int32) = List#{}
let mutable_values = MutList#{1, 2, 3}
let mutable_tags = MutSet#{"compiler", "language"}
let mutable_users = MutMap#{"alice" => 1, "bob" => 2}
```

非空字面量会从元素推导类型；空字面量必须有类型上下文。Map/MutMap 的键必须满足 `Hash + Eq`，Set/MutSet 的元素也有同样要求。重复键会以后者覆盖，重复集合元素会自动去重。可变字面量中的表达式按从左到右求值。

标准模块 `joky/list` 提供同名的泛型函数，便于管道组合：

```joky
import joky/list

let count = List(1, 2, 3) |> list.length()
let reversed = List(1, 2, 3) |> list.reverse()
```

### 元素排序

标准库用方法级 `where` 声明排序的元素约束：

```joky
@intrinsic
pub class MutList(T: type) {
    fn sort(&self) -> Unit where T: Ord
}
@intrinsic
pub struct List(T: type) {
    fn sorted(&self) -> List(T) where T: Ord
}
```

这里展示的是排序方法的签名片段。`@intrinsic struct` 声明内置值类型，
`@intrinsic class` 声明内置对象类型；编译器会拒绝与内置语义不一致的声明。
`List` 保持不可变值语义，底层可共享存储；`MutList` 则允许通过共享引用修改。
标准库中的 `Bytes`、`Map`、`Set`、`String` 同样声明为 `@intrinsic struct`，
并列出现有普通实例方法；自由函数形式的模块 API 保持可用。
`String.parse(T)` 使用方法泛型声明 `fn parse(&self, T: type + FromString) -> Result(T, String)`。
上述普通实例方法以及可变容器方法的参数和返回类型由标准库声明驱动检查，
未导入模块时也使用同一份内置声明。重新声明可限制可用方法或增加方法级约束，
但不能更改内置操作的接收者、参数类型、返回类型，或新增编译器未实现的方法。
`Set(T)` 的实例方法当前仍按 `Map(T, Bool)` 的声明检查。
这些声明不定义普通用户类型的字段布局。`where` 仅限制对应方法的调用，
因此 `MutList(Float64)` 仍可创建、插入和读取，只是不能调用 `sort()`。
约束在编译期检查，泛型函数体也必须提供足够的 bound。
当前 `where` 支持泛型 `@intrinsic struct` 和 `@intrinsic class` 的方法声明，
以及普通泛型函数。主体可以是所属类型或方法的类型参数，也可以是关联类型路径
（`I.Item`），可以用 `+` 组合 trait，用逗号分隔多个参数，例如
`where K: Ord + Hash, V: Debug` 或 `where I.Item: Show`。
`where` 可另起一行，冒号、`+` 和逗号后也可以换行。
普通泛型函数仍可用 `T: type + Ord`，函数级 `where` 用于补充参数约束或写
关联类型约束。

`List(T).sorted()` 返回按升序排列的新列表，原列表不变；
`MutList(T).sort()` 借用并修改原容器，返回 `Unit`，不改变长度与容量。
两者都要求**元素 `T: Ord`**，使用 `Ord.compare(left, right)` 比较元素，
不要求容器本身实现 `Ord`，也不会选择同名普通方法或 `PartialOrd.partial_compare`。

```joky
let original = List#{3, 1, 2, 1}
let ordered = original.sorted() // List#{1, 1, 2, 3}
let values = MutList#{3, 1, 2}
values.sort()

fn ordered(T: type + Ord, values: List(T)) -> List(T) {
    values.sorted()
}
fn reorder(T: type + Ord, values: &MutList(T)) {
    values.sort()
}
```

排序稳定：`Ord.compare` 返回 `Ordering.Equal` 的元素保留原来的相对顺序。
实现使用非递归归并排序，时间复杂度 `O(n log n)`，辅助空间 `O(n)`；
`MutList.sort()` 更新原容器，但仍使用临时缓冲区。比较器必须满足 `Ord` 的全序契约。
接收者只求值一次。比较器 panic 遵循现有的不可恢复终止语义，不提供恢复或回滚。

整数、Bool、String、Duration、Ordering 及自定义 `Ord` 元素可排序。
浮点只有 `PartialOrd`，即使列表为空或实际没有 NaN，也不能直接排序。
本批尚不提供 `sort_by`、`sorted_by`、按字段排序或浮点全序 API。
标准模块也提供 `list.sorted(values)`，可用于 `values |> list.sorted()`。
示例见 [sorting.jk](../../examples/collections/sorting.jk)。

## Map

`Map(K, V)` 是不可变的持久化键值容器。`insert` 和 `remove` 返回新 Map，不会修改旧值。键必须满足 `Hash + Eq`；整数、`Bool`、`String`、`Bytes`、`Duration`、`Unit`、无载荷 enum，以及由它们组成的元组和非空不可变 `struct` 可以作为键。值遵循 List 的不可变值语义，不能包含 class；键同样不能包含 class。

```joky
let users = Map(String, Int32)()
let first = users.insert("alice", 41)
let updated = first.insert("alice", 42)

first.get("alice")       // Some(41)
updated.get("alice")     // Some(42)
updated.contains_key("alice")
updated.remove("alice")
updated.length()
updated.is_empty()
```

`import joky/map` 提供同名模块函数，方便和 `|>` 组合：

```joky
import joky/map

let value = Map(String, Int32)()
    |> map.insert(key: "alice", item: 42)
    |> map.get(key: "alice")
```

Map 提供三个遍历游标方法：`entries()` 的 `Item` 是 `(K, V)`，`keys()` 和
`values()` 分别产出键与值。`for` 会通过 IntoCursor 把 Map / Set 自动转成
`entries()` 游标，因此 `for entry in table` 与 `for entry in table.entries()`
等价；Set 条目是 `(T, Bool)`，只要键时仍写 `letters.iter()`。
遍历顺序是哈希序，进程内确定但跨运行可能不同，不要依赖它；需要有序结果时
先收集再用 `sorted()`：

```joky
let keys = for k in updated.keys() { k }
let ordered = keys.sorted()
let pairs = for entry in updated { entry }
```

游标是唯一所有权值：交给 `for` 或 `advance` 后原绑定不能再用，遍历期间
整个 Map 保持存活，遍历后原 Map 不受影响。

## Set

`Set(T)` 是不可变的持久化集合。它复用 Map 的键语义，因此 `T` 必须满足 `Hash + Eq`，并且不能包含 class。`insert` 和 `remove` 返回新的 Set，不会修改旧值。

```joky
import joky/set

let empty = Set(Int32)()
let values = empty |> set.insert(item: 1) |> set.insert(item: 2)
let present = values |> set.contains(item: 1)
let removed = values |> set.remove(item: 1)
let count = removed |> set.length()
```

`Set(T)` 在当前实现中复用 `Map(T, Bool)` 的运行时表示；用户通过 `joky/set` 模块使用集合 API。

`set.iter()` 产出集合遍历游标，`Item` 为 `T`；与 Map 游标一样是哈希序、
唯一所有权值，遍历不消费集合本身：

```joky
let members = for item in values.iter() { item }
let ordered = members.sorted()
```

泛型函数在调用点推导类型参数并单态化，HIR、MIR 和 codegen 只处理具体实例：

```joky
fn identity(T: type, value: T) -> T {
    value
}

let number = identity(42)
let flag = identity(Bool, true)
```

当前泛型函数、intrinsic 泛型 struct/class、intrinsic 泛型方法和类型函数都在调用点具体化；用户定义的泛型 struct、class、enum 与泛型方法仍未开放。未被调用的泛型模板不会进入 HIR。
