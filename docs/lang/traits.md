# Trait 与动态分派

本文描述当前语言行为。章节导航见[语言索引](README.md)。

- [Trait](#trait)

## Trait

trait 声明类型支持的方法，`impl Trait for Type` 为具体类型提供实现。
泛型调用使用静态分派；动态分派使用 `Dyn(Trait)`，语法与类型构造器一致。

```joky
trait Describe {
    fn describe(&self) -> String
}

struct Point { let x: Int32 }

impl Describe for Point {
    fn describe(&self) -> String { "point" }
}

fn describe(T: type + Describe, value: T) -> String {
    value.describe()
}

fn main() {
    let point = Point(x: 7)
    println(point.describe())
    println(describe(point))
}
```

类型参数 `T` 可从实参推断；编译器检查约束并选择具体实现。trait 可以包含
多个方法，参数中可用 `Self` 表示实现类型。声明和实现的 receiver 模式
必须一致：`&self` 借用，`self` 消费，实现省略 receiver 时默认借用。
实现必须提供声明的全部方法和关联类型，签名必须匹配。

泛型约束同时限制 effect：实例化 `T: type + Trait` 时，每个非 static
实现方法声明的 effect 必须是对应 trait 方法声明的子集，包括泛型函数未
调用的方法，以及方法体未实际使用的声明 effect。嵌套泛型转发和跨模块
实例化遵守相同规则；不同模块的同名 effect 仍是不同的 effect。
static 方法在定义 impl 时检查此上界。

具体接收者的 `value.method()` 和 `Trait.method(value)` 按实现实际使用的
effect 检查，普通实例方法可以声明比 trait 更多的 effect，但此时不能作为
该泛型约束的实参。无对外 effect 不代表不能挂起：实现内部的 task wait
仍由现有挂起分析处理，泛型调用可以正常恢复。

使用 `Trait.method(value, ...)` 可以明确选择 trait，receiver 是第一个实参；
编译器从它推断实现类型，并检查对应的 trait 约束。例如：

```joky
fn describe(T: type + Describe, value: T) -> String {
    Describe.describe(value)
}
```

调用处不用写 `&value`：`&self` 声明决定借用，`self` 声明决定消费 receiver。
其余参数仍支持命名实参，例如 `Compare.same(value, other: other)`。
导入的 trait 使用 `util.Describe.describe(value)`；动态对象也可通过其接口中
的 trait 名调用方法。

同一类型可以实现多个带同名方法的 trait，签名和返回类型也可以不同。
`First.read(value)` 与 `Second.read(value)` 分别选择对应实现；
`value.read()` 在有多个候选时报告歧义。泛型中的普通方法调用只根据声明的
trait 约束选择候选，不会按约束顺序选择第一个方法。重复实现同一个 trait
仍然是错误，`Drop.drop` 仍只允许编译器自动调用。

内建 trait 也使用相同的方法身份规则：显式调用写为 `Show.show(value)`、
`Debug.debug(value)`。`print` / `println` 和字符串插值固定调用 `Show.show`，
`echo` 和递归调试表示固定调用 `Debug.debug`，自动析构固定调用 `Drop.drop`。
这些调用不会被类型自身或其他 trait 的同名方法替代。类型自身的方法可以与
内建 trait 的同名实现共存；普通 `value.method()` 有多个候选时仍报告歧义。

### PartialEq 与 Eq

`PartialEq` 是内建 trait，签名为 `fn equals(&self, other: &Self) -> Bool`。
`left == right` 固定分派到 `PartialEq.equals(left, right)`，`!=` 对同一次调用的
结果取反；两个操作数从左到右各求值一次。同名的普通方法或其他 trait 方法
不会替代相等运算。比较双方必须是同一类型，实现必须无未处理 effect，且不能挂起。

```joky
struct User { let id: Int32; let name: String }
impl PartialEq for User {
    fn equals(&self, other: &Self) -> Bool { self.id == other.id }
}
impl Eq for User {}

fn same(T: type + PartialEq, left: &T, right: &T) -> Bool {
    PartialEq.equals(left, right)
}
```

`Eq` 是不含方法的标记 trait，承诺比较满足自反性、对称性和传递性；编译器不证明
这些定律。`T: Eq` 自动满足 `PartialEq` 约束，可以使用 `==` 和
`PartialEq.equals`。用户类型通常需要先实现 `PartialEq`，再写 `impl Eq for Type {}`。

内建实现：

| 类型 | PartialEq | Eq | 比较方式 |
| --- | --- | --- | --- |
| 所有整数类型、Bool | 是 | 是 | 数值 |
| String、Bytes | 是 | 是 | 内容 |
| Unit | 是 | 是 | 总是相等 |
| Duration | 是 | 是 | 归一化时长 |
| 无载荷 enum（含 FileMode、SeekFrom） | 是 | 是 | variant |
| Float32、Float64 | 是 | 否 | IEEE：NaN 不等于自身，正负零相等 |
| 元组 | 所有元素满足 PartialEq | 所有元素满足 Eq | 按元素顺序比较 |
| Option(T) | T: PartialEq | T: Eq | None 与 None 相等，Some 比较载荷 |
| Result(T, E) | T、E: PartialEq | T、E: Eq | 同 variant 比较载荷，不同 variant 不相等 |
| List(T) | T: PartialEq | T: Eq | 按顺序比较元素及长度 |

上述组合类型递归调用成员的 `PartialEq.equals`，支持成员的自定义实现；遇到不等
立即停止。Option、Result 只访问当前 variant 的载荷，但所有可能的载荷类型都
必须满足 trait 约束。List 使用循环遍历；不会仅凭指针相同判等，因此含 NaN 的
列表与自身比较也不相等。带 owned 载荷的元组、Option、Result 比较借用其成员。

例如 `Some("hello") == Some("hel" + "lo")` 和 `List#{1, 2} == List#{1, 2}`
均为 true。比较的一侧已知类型时，可为另一侧的 `None`、`Ok(...)`、`Err(...)`
或空列表补齐类型。

class 操作数按普通借用规则处理，不被比较消费；现有的重叠 class 借用限制仍适用。
`&T` 实例化为 copy/shared 类型时使用普通复制或引用计数传参，实例化为 owned
类型时使用借用。具体类型参数也可使用同样的 `&Type` 写法。

Map、Set、MutMap、MutSet 的键要求 `Hash + Eq`。整数、Bool、String、Bytes、
Duration、Unit、无载荷 enum 和满足约束的元组可直接作为键。
由这些类型递归组成、且不含自定义相等字段的非空 struct 可以使用
`impl Hash for Key {}` 和 `impl Eq for Key {}`，编译器提供按字段比较的
`PartialEq`；String 字段按内容比较。这是结构体键的兼容规则。
显式实现自定义 `PartialEq` 的键必须同时显式实现 `Hash.hash`，不能使用空的
`impl Hash`；包含自定义相等字段的外层结构体也需要显式实现相等与哈希。
Option、Result、List 暂不提供 `Hash` 或 Map 键支持。
Map、Set、可变集合和有载荷 enum 暂不提供默认 `PartialEq`。示例见
[partial_eq.jk](../../examples/traits/partial_eq.jk) 和
[partial_eq_containers.jk](../../examples/traits/partial_eq_containers.jk)。

### Hash 与自定义键

`Hash` 的方法签名为 `fn hash(&self, state: &Hasher) -> Unit`。
`Hasher()` 创建独占的哈希状态，`state.finish()` 返回 `UInt64`，不消费状态。
通过 `Hash.hash(value, state)` 依次写入参与相等比较的字段：

```joky
struct UserKey { let id: Int32; let name: String }
impl PartialEq for UserKey {
    fn equals(&self, other: &Self) -> Bool { self.id == other.id }
}
impl Eq for UserKey {}
impl Hash for UserKey {
    fn hash(&self, state: &Hasher) -> Unit { Hash.hash(self.id, state) }
}
```

Map、Set、MutMap、MutSet 均调用键的 `Hash.hash`，再对哈希相同的候选键调用
`PartialEq.equals`；`Eq` 是相等关系满足自反、对称、传递性的标记。
因此 `UserKey(id: 7, name: "Alice")` 与 `UserKey(id: 7, name: "Alicia")`
是同一个键。碰撞不会合并不相等的键。

实现必须保证相等的键在相同初始状态下产生相同哈希，且相等与哈希结果不能随
调用次数改变。编译器不能证明这些性质。Hash 方法必须同步且无 effect。
哈希带进程随机种子，不适合持久化，也不能作为密码学摘要；String、Bytes 按内容
及长度写入，避免连续字段的边界歧义。Bytes 切片只哈希可见内容；Duration 使用
归一化时长；Unit 写入固定值；无载荷 enum 使用 variant。

元组在所有成员满足 `Hash` 时提供 `Hash`，依次调用成员的 `Hash.hash`。
作为集合键时，成员还必须满足 `Eq` 和不可变键布局限制；相等比较递归调用成员的
`PartialEq.equals`，遇到不等立即停止。因此元组可以直接组合自定义键：

```joky
let roles = Map#{(UserKey(id: 7, name: "Alice"), "project-a") => "admin"}
roles.get((UserKey(id: 7, name: "Alicia"), "project-a")) // Some("admin")
```

MutMap、MutSet 使用哈希桶索引，查询、插入、删除在哈希分布良好时平均 O(1)，
扩容为 O(n)，插入按摊销计。条目缓存哈希，扩容不会再次调用用户 Hash 方法。
碰撞链保留所有不相等的键；大量碰撞时最坏仍为 O(n)。删除后可复用容量，
不保证条目顺序。不可变 Map、Set 使用持久化 HAMT（哈希数组映射树），更新只
复制哈希路径，共享其他节点，旧版本保持不变。每层使用 5 位哈希，64 位哈希
最多经过 13 层；普通查询和更新随树深增长，完整哈希碰撞时仍可能退化为 O(n)。
`length()` 读取缓存计数，为 O(1)。这里“持久化”指保留旧版本，不是磁盘存储。
键布局支持整数、Bool、String、Bytes、Duration、Unit、无载荷 enum，以及由它们
递归组成的元组和非空 struct。不支持 class、浮点、可变引用、Option、Result 或
List 键。完整示例见 [hash_keys.jk](../../examples/traits/hash_keys.jk)。

### PartialOrd 与 Ord

`PartialOrd` 提供 `fn partial_compare(&self, other: &Self) -> Option(Ordering)`，
并要求类型实现 `PartialEq`。内建 `Ordering` 有 `Less`、`Equal`、`Greater` 三个
variant，可以直接构造、模式匹配、判等或通过 `Debug.debug` 格式化。

`left < right`、`<=`、`>`、`>=` 固定分派到 `PartialOrd.partial_compare(left, right)`。
`Some(Ordering.Less)`、`Some(Ordering.Equal)`、`Some(Ordering.Greater)` 分别表示
小于、相等、大于；`None` 表示不可比较，此时四个关系运算符全部返回 `false`。
因此部分有序类型的 `!(a < b)` 不一定等于 `a >= b`。
两个操作数必须同类型，从左到右各求值一次；比较借用操作数，实现必须无未处理
effect，且不能挂起。同名普通方法或其他 trait 方法不会替代关系运算符的分派。

`Ord` 提供 `fn compare(&self, other: &Self) -> Ordering`，要求类型同时实现
`Eq` 与 `PartialOrd`。它承诺全序：`partial_compare(a, b)` 应等于
`Some(compare(a, b))`，且比较结果必须与相等语义一致，满足反对称性及传递性。
这些定律由实现者保证，编译器检查签名与前置 trait，不证明定律。
`T: Ord` 自动满足 `PartialOrd + Eq + PartialEq` 泛型约束，`T: PartialOrd`
自动满足 `PartialEq`，因此泛型内可以直接使用对应运算符和 trait 方法。

```joky
fn compare(T: type + Ord, left: &T, right: &T) -> Ordering {
    Ord.compare(left, right)
}
```

内建实现：

| 类型 | PartialOrd | Ord | 顺序 |
| --- | --- | --- | --- |
| 所有整数类型 | 是 | 是 | 按各自有符号或无符号数值 |
| Bool | 是 | 是 | false < true |
| String | 是 | 是 | UTF-8 字节字典序，短前缀在前，不做 locale 排序 |
| Bytes | 是 | 是 | 无符号字节字典序，短前缀在前 |
| Duration | 是 | 是 | 归一化后的无符号毫秒值 |
| Ordering | 是 | 是 | Less < Equal < Greater |
| Float32、Float64 | 是 | 否 | IEEE 数值序；任一操作数为 NaN 时返回 None，正负零相等 |
| 元组 | 所有元素满足 PartialOrd | 所有元素满足 Ord | 按元素顺序的字典序 |
| Option(T) | T: PartialOrd | T: Ord | None < Some；Some 比较载荷 |
| Result(T, E) | T、E: PartialOrd | T、E: Ord | Ok < Err；同 variant 比较载荷 |
| List(T) | T: PartialOrd | T: Ord | 按元素的字典序，公共前缀相同时短列表在前 |

组合类型递归调用成员的排序 trait，支持成员的自定义实现。`PartialOrd` 在第一个
非 `Some(Ordering.Equal)` 结果处停止，包括 `None`；`Ord` 在第一个非 `Equal`
结果处停止。尚未比较的后续成员不会影响结果，例如 `(0, NaN) < (1, NaN)` 为真，
而 `(0, NaN)` 与自身不可比较。Option、Result 只比较当前 variant 的载荷，但所有
可能的载荷类型都必须满足对应约束。List 逐个遍历，不因指针相同而跳过成员比较。
带 class 载荷的元组、Option、Result 按借用规则比较，不消费操作数。

这些实现也让 `List#{(2, "b"), (1, "c"), (1, "a")}.sorted()`、
`List#{Some(2), None, Some(1)}.sorted()` 和嵌套列表排序可用。包含浮点数的组合类型
仅满足 `PartialOrd`，不能直接使用要求 `Ord` 的 `sort()` / `sorted()`。

struct 与 class 可以显式实现这两个 trait。Map、Set、可变集合以及其他 enum
不提供默认排序；Map 键规则保持不变。
完整示例见 [ordering.jk](../../examples/traits/ordering.jk) 和
[ordering_containers.jk](../../examples/traits/ordering_containers.jk)。

关联类型使用 `type Item` 声明，在实现中绑定具体类型；trait 内可直接写
`Item`，实现内可写 `Self.Item`，泛型签名中使用 `S.Item`：

```joky
trait Source {
    type Item
    fn read(&self) -> Item
}

struct Number { let value: Int32 }

impl Source for Number {
    type Item = Int32
    fn read(&self) -> Self.Item { self.value }
}

fn read(S: type + Source, source: S) -> S.Item {
    source.read()
}
```

完整程序见 [traits.jk](../../examples/traits/traits.jk) 和
[trait_associated_types.jk](../../examples/traits/trait_associated_types.jk)。CLI fixtures
位于 `tests/fixtures/trait_*.jk`，覆盖方法调用、关联类型和缺少实现的诊断；
运行 `cargo test --test cli run_trait` 可验证这些用例及示例。

### 动态 Trait

`Dyn(Trait)` 是拥有所有权的类型，`Dyn(Trait)(value)` 显式将具体 struct/class
封装为动态对象。它会擦除具体类型，通过运行时方法表选择实现：

```joky
fn display(value: &Dyn(Describe)) {
    println(value.describe())
}

let Described = Dyn(Describe)
let value = Described(Point(x: 7))
display(value)
```

动态对象统一遵守 move 规则，可以作为字段、参数和返回值。`&Dyn(Trait)`
只用于借用参数；`&self` 方法借用对象，`self` 方法消费对象，之后不能再次
使用原绑定。对象销毁时释放内部值；消费调用将内部值交给实现，避免重复析构。
`Dyn(Show)` 可以直接传给 `print` / `println`，包含 `Show` 的组合接口也可以。

使用 `+` 组合多个 trait：

```joky
trait Read { type Item; fn read(&self) -> Item }
trait Close { fn close(self) -> Unit }

fn process(file: Dyn(Read + Close, Item: String)) {
    println(file.read())
    file.close()
}
```

`Dyn(Read + Close, Item: String)(value)` 要求具体值同时实现两个 trait。
组合顺序不影响类型身份，`Dyn(A + B)` 与 `Dyn(B + A)` 是同一类型；重复列出
同一个 trait 会报错。组合后的方法表至少需要一个方法，仍不支持空的标记
trait。所有 trait 的关联类型都必须绑定；不同
trait 声明同名方法或同名关联类型时拒绝组合，即使签名相同也不自动合并。
消费任意一个 `self` 方法会消费整个对象，其余接口也不能继续使用。
可以显式向上转型为接口子集，沿用类型构造器语法：

```joky
let full = Dyn(Read + Close + Show, Item: String)(value)
let Readable = Dyn(Read + Show, Item: String)
let readable = Readable(full)
let shown = Dyn(Show)(readable)
println(shown)
```

目标 trait 必须是源接口的子集，保留的关联类型绑定必须一致。转换消费原绑定，
同类型转换也遵守 move 规则；不会复制内部值或重新分配对象，具体类型的析构
逻辑保持不变。转换后仍可借用或消费目标接口的方法，支持挂起和取消。
目前只支持显式所有权转换，不能从 `&Dyn(A + B)` 构造拥有所有权的 `Dyn(A)`，
也不提供借用视图转换或函数参数的隐式转换。

关联类型必须全部绑定，例如 `Dyn(Source, Item: Int64)`。可以在类型注解与
构造表达式中引用导入模块的 trait，如 `Dyn(util.Read)`，并使用
`impl util.Read for Number` 在其他模块提供实现。方法的普通参数和
结果不能含有未绑定的 `Self`；`fn equals(&self, other: Self)` 这样的 trait
只能用于静态分派。

动态接口需要显式保留实现所需的 effect：

```joky
trait Work {
    fn finish(self) -> String effects { time }
}
```

构造时检查实现的 effect 不超出接口声明，动态调用也检查调用方是否声明或
处理了这些 effect。实现可以挂起；恢复、取消和结果传递复用函数 Pending ABI。
普通 class 借用的现有挂起限制仍适用；动态对象的借用由尚未完成的调用方保活。

动态接口支持单个 trait 或多个 trait 的组合，关联类型绑定使用具体类型。
首次装箱的输入必须是具体 struct/class，尚不支持泛型模板内对类型参数直接装箱、
primitive 装箱、向下转型或运行时类型查询。带 effect 的 `Drop`
不能被擦除；无 effect 的自定义 `Drop` 可以使用。容器仍遵守既有元素限制，
例如不可变 `List` 不接受唯一所有权的动态对象。动态对象不直接暴露给 C ABI。

完整示例见 [dyn_traits.jk](../../examples/traits/dyn_traits.jk) 和
[dyn_trait_composition.jk](../../examples/traits/dyn_trait_composition.jk)，向上转型见
[dyn_upcast.jk](../../examples/traits/dyn_upcast.jk)。关联类型、借用和挂起
fixtures 位于 `tests/fixtures/dyn_*.jk`。运行
`cargo test --test cli dynamic::` 验证 JIT、缓存、debug AOT、跨模块调用和取消。
完整 debug/release AOT 矩阵用 `JOKY_TEST_AOT=full`。
检查资源计数时需链接启用 test-support 的 runtime archive：

```sh
cargo build --manifest-path crates/joky-runtime/Cargo.toml --features test-support
JOKY_RUNTIME_ARCHIVE="$PWD/crates/joky-runtime/target/debug/libjoky_runtime.a" \
  cargo test --features runtime-test-support --test cli dynamic::
```
