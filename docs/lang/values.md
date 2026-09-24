# 字符串、常用值与调试

本文描述当前语言行为。章节导航见[语言索引](README.md)。

- [字符串字面量](#字符串字面量)
- [Debug 与 echo](#debug-与-echo)
- [Panic](#panic)
- [Option](#option)
- [Result](#result)
- [Duration 字面量](#duration-字面量)
- [字符串查询与转换](#字符串查询与转换)

## 字符串字面量

字符串有四种写法，由 `r` 前缀和三引号两个维度组合而成；`b` 前缀另行产生
`Bytes` 字面量，见本节末尾：

| 形式 | 转义 | 插值 | 跨行 |
| --- | --- | --- | --- |
| `"..."` | 是 | 是 | 否 |
| `"""..."""` | 是 | 是 | 是 |
| `r"..."`、`r#"..."#` | 否 | 否 | 否 |
| `r"""..."""`、`r#"""..."""#` | 否 | 否 | 是 |

普通字符串支持 `\"`、`\\`、`\n`、`\r`、`\t` 五个转义，不能包含裸换行。

原始字符串在 `r` 后写 N 个 `#`（N ≥ 0）再写引号，结束时引号后跟同样数量的 `#`。
定界符之间的内容原样保留：反斜杠就是反斜杠，`{` 就是 `{`，**不做插值**。
内容里出现引号时提升 `#` 的数量即可：

```joky
let re   = r"\d+\.\d+"
let json = r#"{"name": "joky"}"#
let win  = r"C:\Users\windy"
```

需要拼接时用 `+` 连接普通字符串，例如 `r"\d+" + "{count}"`。

多行字符串以 `"""` 开头，开定界符后同一行只能是空白并且必须换行，该换行不计入
内容；结束的 `"""` 必须独占一行，它前面的换行同样不计入。结束定界符的前导空白
是基准缩进，每行按逐字符相同的前缀剥掉（不做 tab 展开），缩进少于基准会报错，
纯空白行输出为空行。`\r\n` 与 `\r` 统一归一化为 `\n`：

```joky
fn query(name: String) -> String {
    """
    SELECT *
      FROM users
     WHERE name = '{name}'
    """
}
```

上面的结果是 `"SELECT *\n  FROM users\n WHERE name = 'Joky'"`，末尾没有换行；
想保留末尾换行就在结束定界符前空一行。两个维度可以组合，`r"""` 开头的多行
字符串同样剥缩进，但不解转义、不插值：

```joky
let pattern = r"""
    path ~ '\d+'
    """
```

字节串字面量以 `b` 为前缀，内容限 ASCII，类型是 `Bytes` 而非 `String`，
不解插值（`{` 是普通字符）；`b` 与 `r` 组合成 `br"..."`，也支持 `#` 定界与三引号：

```joky
let payload = b"chunked file copy\n"    // Bytes，转义照常解码
let pattern = br"\d+{x}"                // 原始字节串，反斜杠与花括号原样保留
```

非 ASCII 字符会报词法错误。需要反向构造时用 `Bytes.from_string(text)`；
`bytes.to_string()` 仍执行严格 UTF-8 解码，非法编码返回 `None`。

示例见 [string.jk](../../examples/basics/string.jk)。

## Debug 与 echo

内建 `Show.show(&self) -> String` 定义面向用户的文本，供 `print` / `println` 使用。
内建 `Debug.debug(&self) -> String` 定义调试表示，两种 trait 相互独立：

```joky
struct Point { let x: Int32 }
impl Debug for Point {
    fn debug(&self) -> String { "Point(x: " + Debug.debug(self.x) + ")" }
}
```

数字、Bool、String、Unit、Bytes 和 Duration 内建实现 `Debug`。字符串调试表示带双引号，并转义换行、
制表符、双引号、反斜杠等字符；Unit 表示为 `()`。
Bytes 按原始字节输出小写的两位十六进制，如 `Bytes[0x00, 0xff]`，空值为 `Bytes[]`，
不尝试解码 UTF-8。Duration 输出归一化的毫秒值，如 `1s500ms` 显示为 `1500ms`。
`Debug` 方法必须借用接收者、返回 String、不声明 effect 且不能挂起；建议实现不修改
被观察对象。

struct、class 和 enum 默认提供结构化 `Debug` 实现，无需声明派生。struct/class 使用
`Type(field: value, ...)`，enum 使用 `Type.Variant(field: value, ...)`；字段按声明顺序
输出。显式 `impl Debug for T` 优先于默认实现，可用于隐藏或重排字段。

元组、`Option(T)`、`Result(T, E)` 和不可变 `List(T)` 在所有成员类型都实现 `Debug` 时，自动支持递归
调试表示，包括成员的自定义实现和 `Dyn(Debug)`：

```joky
(1, "hello").debug()           // "(1, \"hello\")"
(1,).debug()                   // "(1,)"
Some((42, true)).debug()        // "Some((42, true))"
Option(String).None.debug()    // "None"
Result(Int32, String).Err("bad").debug() // "Err(\"bad\")"
```

格式化只借用成员，只访问当前变体的载荷。即使值为 `None`，或 `Result` 当前只有一侧
载荷，全部成员类型仍须满足 `Debug` 约束。递归 class 会在循环或深度过大时输出
`<cycle>` 或 `<max-depth>`；经过 List 成员的递归也保留该深度检查。
List 按顺序迭代格式化各元素，输出 `List#{...}`，例如 `List#{1, 2}`、
`List#{"hello", "world"}`；空列表输出 `List#{}`。它调用元素的 `Debug`，
包括自定义实现，不消费原列表。空列表的元素类型也必须满足 `Debug`。
Map、Set 和可变集合暂不支持默认递归格式化。

`Debug.debug(value)`、泛型 `T: Debug`、`Dyn(Debug)` 和 `echo` 均支持上述类型。
示例见 [debug_values.jk](../../examples/basics/debug_values.jk)。

`NativeHandle` 资源（例如 `TcpListener`、`TcpStream`、`File` 和 SQLite 连接）默认输出
`Type(id: n)`。标识在一次进程运行中稳定，调试不会读取、移动或关闭资源。

字符串支持使用 `Show` 的插值。`{expr}` 只求值一次并调用该表达式的 `Show` 实现，
因此插值适合面向用户的文本；需要调试表示时可以显式写 `{expr.debug()}`。`{{` 和 `}}`
分别表示字面量 `{` 和 `}`，暂不支持宽度、精度等格式说明：

```joky
let name = "Joky"
let count: Int32 = 3
println("hello {name}, count = {count}")
println("debug = {name.debug()}")
println("braces: {{ok}}")
```

插值中的表达式必须实现 `Show`，包括自定义 `impl Show` 和 `Dyn(Show)`；普通字符串
仍然是 `String` 值，不会自动改用 `Debug`。原始字符串不插值，其中的 `{` 是普通字符。

`echo` 是保留关键字，也是编译器内建的值观察操作，支持前缀、括号和管道形式：

```joky
fn twice(value: Int32) -> Int32 { value * 2 }
fn main() {
    echo "hello\nworld"
    let result = echo(21)
        |> twice
        |> echo
    println(result)
}
```

每次 `echo` 向 stderr 输出一条记录，格式为 `[src/main.jk:行:列] 调试表示`。
路径默认相对包根目录，包外源码使用完整路径；行列从 1 开始，列按 Unicode scalar
计数，位置指向该次 `echo` 关键字。直接编译源码字符串的 API 使用 `<source>`。
位置在编译时写入，跨模块、泛型实例化、缓存和 AOT 均保留原始定义位置，运行时不需要源码文件。

`echo` 要求操作数实现 `Debug`，也支持 `Dyn(Debug)`。操作数只求值一次，格式化时借用，
随后返回原值；`let result = echo(value)` 与 `let result = value` 遵循相同的所有权规则。
作为块中非末尾的独立表达式使用时，`echo value` 只观察，不消费绑定。
块末尾的 `echo` 仍是返回值表达式，适用普通返回类型与所有权规则。

`echo a + b` 观察整个 `a + b`；`echo(a) + b` 只观察 `a`。管道可跨行，
`value |> echo |> next` 将原值继续传给 `next`。`echo ()` 和 `echo()` 观察 Unit。
`echo (a, b)` 与 `echo(a, b)` 都观察一个元组，`echo(a,)` 观察单元素元组。
`echo` 不能作为函数值赋值或传给高阶函数。

debug 和 release 构建都执行 `echo`，不会移除操作数的副作用。输出无需声明额外的 effect，
并在完整记录写完前持有 stderr 锁，避免并发任务交错写入同一条记录。

## Panic

`panic(message: String)` 是内置的不可恢复失败操作。它输出消息并终止当前控制流：

```joky
if invalid {
    panic("invalid configuration")
}
```

## Option

`Option(T)` 是内置的不可变泛型值类型，用于表达“存在一个 `T` 或不存在值”，而不是使用 `null`：

```joky
fn find(found: Bool) -> Option(String) {
    if found { Some("value") } else { None }
}

let value = find(true)
let text = value.unwrap_or("fallback")
let exists = value.is_some()

match value {
    Some(text) => println(text)
    None => println("missing")
}
```

`Some(value)` 与 `None` 是兼容构造器；推荐使用 `Option(T)` 的变体入口。`is_some()`、`is_none()` 返回布尔值；`unwrap_or(default)` 返回其中的值或给定默认值。这些 API 由 [`std/joky/option.jk`](../../std/joky/option.jk) 中的普通 Joky 泛型函数实现，也可用 `import joky/option` 写成 `option.is_some(value)`。`Option(T)` 传递 `T` 的所有权语义：`T` 是 move-only 时，`Option(T)` 也是 move-only。

## Result

`Result(T, E)` 是内置的不可变泛型值类型，用于返回成功值或错误值：

```joky
fn parse(valid: Bool) -> Result(Int32, String) {
    if valid { Ok(42) } else { Err("invalid input") }
}

let value = parse(false)
let fallback = value.unwrap_or(0)

match value {
    Ok(number) => number
    Err(message) => 0
}
```

`Ok(value)` 与 `Err(error)` 是兼容构造器；推荐使用 `Result(T, E)` 的变体入口。`is_ok()`、`is_err()` 返回 `Bool`；`unwrap_or(default)` 在 `Ok` 时返回成功值，否则返回默认值。这些 API 由 [`std/joky/result.jk`](../../std/joky/result.jk) 中的普通 Joky 泛型函数实现，也可用 `import joky/result` 写成 `result.is_ok(value)`。`Result(T, E)` 递归保留两个 payload 的所有权语义：只要 `T` 或 `E` 是 move-only，`Result(T, E)` 也是 move-only。

查询函数借用参数，不消费 move-only 载荷；`unwrap_or` 消费包装值，并遵循普通函数的参数求值规则：即使是 `Some` / `Ok`，默认值也会先求值，未使用的默认值会被释放。需要延迟计算时使用 `unwrap_or_else`，Option 接收 `fn() -> T`，Result 接收 `fn(E) -> T`，只在失败分支调用闭包。

这两个类型由标准库中的类型函数和匿名 enum 定义。它们的公开普通泛型函数以包装值作为第一个参数时，也支持 `value.method(...)` 调用；新增此类 API 无需增加编译器方法分支。编译器仍识别标准类型身份，保留运行时 ABI 和后缀操作支持。

Option 和 Result 支持后缀解包：`!` 强制取得成功值，失败时触发 panic；`?` 在成功时取得值，失败时直接从当前函数返回原包装值。`?` 要求包装类型与当前函数返回类型一致：

```joky
fn load(value: Option(Int32)) -> Option(Int32) {
    let number = value?
    Some(number + 1)
}

fn required(value: Result(Int32, String)) -> Int32 {
    value!
}
```

## Duration 字面量

时间长度是不可变的 `Duration` 值。当前支持毫秒、秒、分钟和小时，并允许连续组合：

```joky
let short: Duration = 1000ms
let second = 1s
let timeout = 1h10m100s
```

字面量在编译期换算为毫秒并检查溢出。挂起操作使用 `time.sleep(duration)`；`time` 是标准 Effect，`Duration` 本身仍是纯值。

## 字符串查询与转换

字符串是不可变的共享值。字符串字面量会复制到 managed runtime 对象中；`+` 创建连接后的新值，`==` / `!=` 按 UTF-8 字节内容比较。常用查询方法是 `is_empty()` 和 `byte_count()`：

```joky
let greeting = "hello"
let message = greeting + " world"
let same = message == "hello world"
let bytes = message.byte_count()
let prefix = message.starts_with("hello")
let suffix = message.ends_with("world")
let found = message.contains("lo wo")
println(message)
```

`starts_with`、`ends_with` 和 `contains` 按 UTF-8 字节内容查询。字符串可以安全地绑定、传参和重复读取；底层引用计数与释放由 runtime 管理，不改变值语义。

字符串还提供 Unicode 查询：`scalar_count()` 返回 Unicode scalar value 数量，`grapheme_count()` 使用 `unicode-segmentation` 返回用户感知字符（grapheme cluster）数量，`length()` 是 `grapheme_count()` 的同义别名，`is_ascii()` 判断内容是否全部为 ASCII。`byte_count()` 仍然只统计 UTF-8 字节数；这些 API 语义不同，例如组合字符和 emoji 可能由多个字节或 scalar value 组成，但只显示为一个 grapheme cluster。

字符串变换 API 包括 `concat(other)`、`trim()`、`to_upper()` 和 `to_lower()`，均返回新的不可变字符串值。`split(separator)` 返回 `List(String)`，空分隔符按 UTF-8 scalar 切开。`replace(from, to)` 替换全部非重叠匹配。`get_byte(index)` 返回 `Option(UInt8)`。`slice(start, end)` 的下标是字节偏移，`end` 不含端点，两端都必须落在 UTF-8 边界上，否则返回 `None`。

`parse(T)` 要求 `T: FromString`，使用编译期类型值选择解析实现，并返回 `Result(T, String)`。
内置实现覆盖 `Int8`、`Int16`、`Int32`、`Int64`、`UInt8`、`UInt16`、`UInt32`、`UInt64`、
`Float32`、`Float64` 和 `Bool`，解析失败时返回 `Err(原始输入)`。

- 整数使用十进制，允许前导 `+`；有符号整数也允许 `-`。超出目标类型范围时失败，无符号整数拒绝负号（包括 `-0`）。
- 浮点数支持小数、科学计数法，以及忽略大小写的 `NaN`、`inf`、`infinity`，均可带正负号。按目标精度直接解析；溢出得到带符号无穷大，下溢按目标精度舍入，可能得到带符号零。
- `Bool` 仅接受小写 `true`、`false`。
- 所有内置解析都不自动去除空白，不接受数字分隔符或进制前缀；需要去除首尾空白时显式使用 `trim().parse(T)`。

```joky
let port = "8080".parse(Int32)
let ratio = "1.5".parse(Float64)
let count = "65535".parse(UInt16)
let enabled = "true".parse(Bool)

match port {
    Ok(value) => value
    Err(source) => 0
}
```

标准库中的完整签名如下：

```joky
trait FromString {
    fn from_string(value: &String) -> Result(Self, String)
}

@intrinsic
pub struct String {
    fn parse(&self, T: type + FromString) -> Result(T, String)
}
```

`from_string` 是没有 `self` 的静态 trait 方法。`text.parse(T)` 固定分派到
`FromString.from_string(T, text)`，同名普通方法不会替代此实现。
调用的第一个参数是编译期目标类型，其余参数是运行时参数；输入字符串只求值一次。
自定义 struct 和 class 可以实现该 trait，并自行决定错误字符串：

```joky
struct Port { let number: Int32 }

impl FromString for Port {
    fn from_string(value: &String) -> Result(Self, String) {
        let number = value.parse(Int32)?
        if (number < 0) || (number > 65535) { Err("port out of range") }
        else { Ok(Port(number: number)) }
    }
}

fn parse(T: type + FromString, text: String) -> Result(T, String) {
    text.parse(T)
}

let port = "8080".parse(Port)
let same = FromString.from_string(Port, "8080")
let generic = parse(Port, "8080")
```

`FromString` 实现不能声明 effect，也不能包含未处理的 effect。
静态 trait 方法通过 `Trait.method(T, ...)` 调用，不参与实例方法查找；包含静态方法的
trait 不能用于 `Dyn`。当前不支持为 enum 或其他内置数字类型添加用户实现。
完整可运行示例见 [from_string.jk](../../examples/basics/from_string.jk)。
