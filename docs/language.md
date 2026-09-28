# 语言与内置模块

本文描述已实现的语言子集。运行命令见 [README](../README.md)，
嵌入与 ABI 约束见 [架构说明](architecture.md)。
目标语法和完整类型系统另见 [语法与类型系统设计草案](syntax.md)，
其中尚未实现的规则不改变本文描述的当前行为。

## 类型与函数

类型包括 `Int32`、`Int64`、`UInt32`、`UInt64`、`Bool`、`Unit`、不可变 UTF-8 `String`、名义记录、枚举、结构化元组和注册过的宿主 opaque 类型。
函数正常结束时最后一个表达式决定返回值，也可用 `return` 提前返回；不写返回注解时为 Unit。
普通函数不能调用 `@suspends` 函数。
`extern fn` 声明宿主实现，完整签名和挂起约定必须匹配注册信息。
String 支持字面量、参数和返回值，尚无插值、拼接或动态分配。
函数参数和调用实参之间必须写逗号，允许尾随逗号，例如 `fn f(x: Int32,)` 和 `f(42,)`；
空列表仍写 `()`，不允许开头逗号或连续逗号。换行和注释不能替代列表逗号。

## 泛型函数

函数可声明一个或多个类型参数，由调用实参推导，也可在调用时完整指定：

```joky
fn identity<T>(value: T) -> T { value }
fn first<T, U>(left: T, right: U) -> T { left }
fn swap<T, U>(pair: (T, U)) -> (U, T) {
    let (a, b) = pair;
    (b, a)
}

fn example() -> Int64 {
    let small = identity(42)                 // Int32
    let wide = identity::<Int64>(4294967295)
    let (valid, saved) = swap((wide, true))
    if valid { first(saved, small) } else { 0 }
}
```

- 类型形参和显式类型实参用逗号分隔，允许尾随逗号；不接受空列表、重复形参或 `_` 占位。
  函数的 `::<...>` 后必须有调用参数列表，不能独立产生函数值。普通函数、整数转换内建函数、
  非泛型枚举构造及 VM 特殊调用不接受类型实参；`<`、`>`、`<<`、`>>` 仍按原运算符解析。
- 类型参数只在所属函数签名及函数体的类型注解内有效，可以进入元组。不同函数中的同名参数具有不同身份。
  参数遮蔽同名模块类型；不能把被遮蔽的模块记录或枚举当作该参数的构造函数。
- **首版类型实参限 Copy 类型**：四种整数、Bool、Unit、现有记录、枚举和 Copy 元组。
  透明别名按目标类型处理；String 和宿主 opaque 类型不能绑定到 T，尚无 `T: Copy` 等约束语法。
  泛型函数仍可声明固定类型的 String / opaque 参数或返回值，它们遵循原有宿主与生命周期规则。
- 推导先收集实参中已有的类型，再为仍未确定的整数字面量默认 Int32；例如 `first(1, true)`
  得到 `T = Int32, U = Bool`。重复出现的同一类型参数必须完全一致，没有隐式宽化。
  显式类型实参可以给未定型字面量提供上下文，`identity::<UInt64>(0xffff_ffff_ffff_ffff)` 合法。
- 外层预期返回类型不反向决定泛型实参：`let n: Int64 = identity(1)` 报类型不匹配，
  应写 `identity::<Int64>(1)` 或先声明 Int64 局部值。没有实参约束的参数、只有提前跳转的实参
  及其他无法局部推导的情况需要显式类型实参；不做跨绑定或全局约束求解。
- **所有泛型函数在定义时检查，包括未使用的函数**。T 是任意允许的 Copy 类型，不能对它执行算术、
  比较、整数转换、未知字段访问或构造；`fn bad<T>(x: T) -> T { x + x }` 因缺少能力约束而报错。
  可以传递、复制、返回 T，操作显式元组结构，并调用其他泛型函数；固定 Int32 等类型上的操作不受影响。
- 同步及 `@suspends` 泛型调用共用现有求值顺序、快照、失败、Pending、取消与代码保活机制。
  普通函数不能调用挂起函数；`scope` 分支仍须调用返回 Unit 的挂起函数。
- 编译期按“函数声明 + 具体类型实参”缓存实例。别名复用实例，不同名义类型分别实例化；
  同型递归与互递归可用。每个函数最多 64 个类型参数，每个模块最多 256 个实例，
  新实例依赖链最多 32 层；超限在编译期报错，缓存命中的递归不消耗新的层数。
- 泛型模板及内部实例不公开导出，也不能直接经 `VmEngine` 或 `vm.register_int32` 调用 / 注册。
  为宿主声明普通、具体签名的包装函数，例如 `fn identity_int32(x: Int32) -> Int32 { identity(x) }`。
  `__joky_generic_` 为编译器保留的函数前缀，CLI 的 `main` 不允许类型参数。

泛型记录和枚举已实现，见[泛型记录](#泛型记录)和[泛型枚举](#泛型枚举)。泛型别名、extern、trait、泛型 impl、
常量泛型和通用函数值仍未实现。完整示例见 [generics.jk](../examples/generics.jk)。

## 整数类型与字面量

已实现 32 / 64 位有符号补码整数 `Int32` / `Int64`，以及无符号整数 `UInt32` / `UInt64`。整数字面量保持未定型，
在共享语义规划阶段从注解、函数实参、返回值、赋值目标、记录字段、枚举载荷或元组元素获得类型：

```joky
fn add(a: Int64, b: Int64) -> Int64 { a + b }
fn example() -> Int64 {
    let base: Int64 = 3000000000
    add(base, 42)
}
```

- `42` 在这里由参数定为 Int64。无约束的局部绑定如 `let n = 42` 在绑定完成时默认 Int32；
  后续使用不会反向改变它的类型，`let wide: Int64 = n` 报错。没有隐式宽化或窄化。
- 运算中的已定型操作数也能约束字面量，`wide + 1` 和 `1 + wide` 同样有效；
  算术表达式、分组、序列末项、if / match 的结果分支会传递上下文。
  被分号丢弃的项和已完成的局部绑定不从后面的返回类型获取上下文。语句、实参和元素仍按源码顺序求值。
- 这是局部上下文定型，不是跨函数或跨绑定的全局约束求解。复杂表达式不能提供足够类型信息时，
  为局部绑定或整个解构模式添加注解。范围检查在类型选定后进行，大数字不会自动升级位宽或改为无符号类型。
- Int32 范围为 `-2147483648..2147483647`；Int64 范围为
  `-9223372036854775808..9223372036854775807`。直接的一元负号允许对应最小值的数值拼写，
  负号与数字间允许空白或注释。Int64 上下文中的 `-(9223372036854775808)` 仍非法：括号里的正值先越界。
  Int32 上下文中的 `-(2147483648)` 同理；在 Int64 上下文中该值则合法。
- UInt32 范围为 `0..4294967295`，UInt64 范围为 `0..18446744073709551615`。
  无符号上下文拒绝负数字面量和一元负号，包括 `-0`、`-(1)`、`-value`；减法 `0 - value` 合法并按位宽环绕。
  词法幅值超过 UInt64 最大值时为解析错误，其余字面量按选定类型检查范围。
- 四种整数的加减乘按各自位宽环绕；有符号取负也环绕。除法向零截断，有符号 `MIN / -1` 为 MIN，除零失败。
  无符号比较和除法按非负数解释高位，例如 UInt64 最大值大于 1，除以 2 得 `9223372036854775807`。
  运算和比较要求两侧类型完全一致，位宽或符号不同都不能混用；Bool 不作为数字使用。
  尚无其他位宽和浮点；受检整数转换见下文 `to_*()` 方法。
- 四种整数均可用于别名、记录、枚举和元组，以及同步 / CPS 参数与结果。64 位值在挂起时保留所有位。
  Rust 提供 `call_int64_0/1/2`、`call_uint32_0/1/2`、`call_uint64_0/1/2`，以及
  `call_async_int64`、`call_async_uint32`、`call_async_uint64`；这些入口均检查精确签名。
  异步入口接受同型整数参数及同型整数 / Unit 结果，`cps_result()` 返回 u64 原始位，
  UInt32 零扩展，Int64 调用者用 `as i64` 解读。原有 Int32 API 不变。
- CLI 支持 `extern fn print_int64(value: Int64)`、`print_uint32(value: UInt32)` 和 `print_uint64(value: UInt64)`；
  main 仍限 Unit / Int32，`joky/vm` 的 `call_int32` / `register_int32` 保持 Int32 契约。
  示例见 [int64.jk](../examples/int64.jk)、[unsigned.jk](../examples/unsigned.jk)。

### 进制与分隔符

整数支持十进制 `1_000`、十六进制 `0xff` / `0xAB_CD` 和二进制 `0b1010_0011`。
前缀只能写小写 `0x` / `0b`，十六进制数字可大小写混用；没有八进制、类型后缀或浮点语法。
单个 `_` 只能出现在两个数字之间，不能紧接前缀、连续出现或位于末尾。
`0x`、`0b102`、`0x_ff`、`1__0`、`1_`、`123abc` 都是完整数值 token 的解析错误；
`_123` 仍是标识符。以数字开头的相邻字母不能再被拆成下一个表达式，应显式用空白或 `;` 分隔。

不同进制均表示非负幅值，使用相同的上下文定型与范围检查，不将最高位自动解释为负数。
例如 `let n: Int32 = 0xffff_ffff` 越界，`let n: UInt32 = 0xffff_ffff` 合法；
`-0x8000_0000` 可表示 Int32 最小值，`-(0x8000_0000)` 在 Int32 上下文仍越界。

### 位运算与移位

四种整数支持 `&`、`|`、`^`、`~`、`<<`、`>>`。二元运算两侧必须是完全相同的整数类型，
移位量也遵守此规则；未定型字面量可由另一侧或结果上下文定型，例如 `let high: UInt64 = 1 << 63`。
`~` 按当前位宽反转所有位。左移丢弃超出位宽的高位、低位补零；有符号右移补符号位，无符号右移补零。

移位量必须满足 `0 <= count < 位宽`，即 32 位类型只接受 0–31，64 位类型只接受 0–63。
负数、等于位宽或更大的值都在执行时产生带源码范围的 `RuntimeError`，包含常量移位量；
不会对移位量取模或截断。失败停止后续操作数、实参和序列表达式，短路跳过的移位不执行。
`&` / `|` 不接受 Bool，也不短路；Bool 逻辑继续使用 `&&` / `||` / `!`。

`match` 的裸分支结果及 `scope` 分支最外层的 `|` 始终是分支分隔符。
在该位置使用按位或须加括号或块：`| Empty => (a | b)` 或 `| Empty => { a | b }`。
调用参数、元组元素、记录字段及块内部已由括号或花括号定界，可直接使用 `|`，如 `scope { | worker(a | b) | worker(c) }`。
嵌套分支遵守同一规则，换行不参与消歧。示例见 [bitwise.jk](../examples/bitwise.jk)。

### 显式整数转换

`int32(value)`、`int64(value)`、`uint32(value)`、`uint64(value)` 是全局内建函数，无需导入或 `extern` 声明。
均要求恰好一个整数实参，支持四种整数之间全部 16 种组合，返回类型由函数名固定。
透明整数别名同样接受；Bool、Unit、String、记录、枚举、元组和宿主 opaque 值不接受。

转换采用 wrapping 规则，不检查数值是否落在目标范围，也不会因为丢位产生运行时失败：

- 窄化保留目标位宽的低位；宽化时，有符号源进行符号扩展，无符号源进行零扩展。
- 同位宽保留位模式，再按目标类型解释；同类型转换保持原值。
- 实参只求值一次，保留原有副作用、失败、提前跳转和挂起语义；转换自身不会挂起。

```joky
let bits: UInt32 = 0xffff_ffff
let signed = int32(bits)          // -1
let wide = int64(bits)            // 4294967295
let unsigned = uint64(-1)         // 18446744073709551615
let large: UInt64 = 0x1_0000_002a
let low = int32(large)            // 42
```

**转换目标不参与实参的字面量定型。** 实参按自身上下文检查，无约束时仍默认 Int32；
所以 `int64(2147483648)` 和 `uint32(0xffff_ffff)` 报 Int32 字面量越界。
应先写 `let n: Int64 = 2147483648` 或 `let bits: UInt32 = 0xffff_ffff` 再转换；
如果只需相应类型的字面量，直接用类型注解即可，无需转换。
同理，`int64(0x7fff_ffff + 1)` 先在 Int32 中环绕，再扩展为 `-2147483648`。
外层返回值、赋值或运算上下文不改变转换的返回类型；`let n: Int64 = int32(1)` 仍报类型不匹配。

这些小写名称不是词法关键字，但保留在全局函数命名空间：用户函数、extern 和宿主绑定不能重定义。
局部变量及类型名称仍使用各自命名空间，例如 `let int32 = 7 int32(int32)` 返回 7。
转换函数没有可取用的原生函数地址，不作为模块导出；需要传给宿主时可定义签名明确的普通包装函数。
当前由共享 IR 的内建转换实现；Integer trait 和用户函数重载尚未实现，
这些内建转换不使用用户泛型函数的 `::<...>` 调用形式。
目标泛型签名和受检转换设计见 [类型设计](syntax.md#101-基础类型与字面量推导)。
完整可运行示例见 [conversions.jk](../examples/conversions.jk)。

## 固有方法与关联常量

具体类型可用 `impl Type { ... }` 声明方法、关联函数和常量；可拆成多个 impl，成员名不得重复。
首版支持四种整数、Bool、Unit、模块内 Copy 记录及非泛型枚举；不支持 String、opaque、元组、
泛型 impl 或泛型方法。透明别名的值仍能调用目标类型的方法，但不能用别名声明 impl，
也暂不提供 `Alias.member` 的类型级访问。

```joky
struct Pair { left: Int32 right: Int32 }
impl Pair {
    const ANSWER: Int32 = 40 + 2
    fn sum(self) -> Int32 { self.left + self.right }
    fn make(left: Int32, right: Int32) -> Self {
        Self { left: left, right: right }
    }
}
fn example() -> Int32 {
    let pair = Pair.make(20, 22)
    pair.sum() + Pair.ANSWER
}
```

- `self` 是首个、不写类型注解的按值参数，类型由 impl 给出；它与普通参数一样不可重新赋值。
  `Self` 在 impl 内保留，在类型、构造和关联成员位置表示目标类型；没有借用接收者或隐式修改调用方的能力。
- 有 self 的函数支持 `value.method(args)` 和显式 `Type.method(value, args)`；无 self 的关联函数只能用类型调用。
  普通成员查找不按实参数量或类型重载；枚举变体与关联成员也不能同名。
- 接收者可以是局部变量、字段、调用、分组、块或构造结果，支持链式调用；负数字面量作接收者写 `(-1).method()`。
  接收者先求值一次，再从左到右求值其他实参；可变值按值快照，后续实参中的修改不会改变已传入的接收者。
  接收者的类型独立确定，无约束整数仍默认 Int32，不从方法返回注解反推类型。
- 全部方法体在定义时检查，包括未调用者；递归和互递归静态解析为普通内部调用。
  `@suspends`、失败、提前返回与 Pending 遵循普通函数规则，`scope` 也支持返回 Unit 的挂起方法调用。
  泛型函数可以对已知具体类型调用方法，不能对无约束 T 猜测方法能力。
- `const NAME: Type = expression` 必须有注解，目前限四种整数和 Bool。
  支持标量字面量、其他关联常量、整数内建转换、一元操作和二元算术/位运算/比较；不支持普通函数调用、
  控制流、局部绑定或运行时状态。前向引用可用，依赖环报错，每个表达式最多 256 个语法节点，依赖链最多 32 层。
  常量在编译期按普通整数规则检查并计算；环绕保持一致，除零和非法移位为编译错误。常量读取无需运行时函数。
- 成员不作为 VM / AOT 公开入口，也不能取作宿主函数地址；宿主仍使用普通函数包装。
  `impl` / `const` 为新增保留字；分号依旧可省略。CLI 暂不支持挂起方法。

### 受检整数方法

默认 prelude 提供标准 `Option<T>`、四种整数的 `MIN` / `MAX`，
以及 `to_i32()`、`to_i64()`、`to_u32()`、`to_u64()`：

```joky
import joky/integer

fn example() -> Option<UInt64> {
    let n = -1
    n.to_u64()                   // None
}
fn forced() -> UInt64 { uint64(-1) } // 18446744073709551615
fn maximum() -> Int32 { Int32.MAX }  // 2147483647
```

16 种源/目标组合一律返回 `Option<目标整数>`：数值可表示则 Some，否则 None，即使同型或保证不丢失的宽化也如此。
`uint64()` 等四个全局函数保留原来的 wrapping 强制转换语义；没有另一套 `try_to_*()` 或 `try_int*()` 内建。
接收者的错误仍正常传播，不能把除零等运行时错误转换成 None。

这些方法和边界定义在 [integer.jk](../src/stdlib/integer.jk)，Option 定义在 [option.jk](../src/stdlib/option.jk)，范围检查用普通 Joky 比较、分支和内建转换实现。
关闭 prelude 后，`import joky/integer` 仍可显式启用这些成员及 Option 类型。
`joky/integer` 和 `joky/option` 共用同一个 Option 声明，可以同时导入且不依赖顺序。
重复模块路径或重复整数成员仍报错。示例见 [integer_methods.jk](../examples/integer_methods.jk)。

## 导入与 prelude

源码库包括 `joky/option`、`joky/result`、`joky/integer` 和 `joky/prelude`；
`joky/vm` 是宿主绑定模块。路径使用 `/`，模块成员与枚举构造器使用 `.`。
导入必须位于全部声明之前，不执行用户代码；支持用户文件模块，尚无通配导入。

```joky
import joky/option as opt.{type Option as Maybe, Some as Present, None as Missing}

fn make() -> Maybe<Int32> { Present(42) }
fn empty() -> opt.Option<Int32> { Missing::<Int32> }
fn read(value: Maybe<Int32>) -> Int32 {
    match value { | Maybe.Some(n) => n | opt.Option.None => 0 }
}
```

- 默认模块名为路径末段，例如 `option`；`as opt` 替换该模块名。
  `import joky/option.{type Option as Maybe} as opt` 也合法。
  模块别名与成员选择可同时使用，`opt.Option`、`opt.Some`、`opt.Option.Some` 指向同一枚举声明。
- `import joky/option.{type Option}` 引入类型名及其构造器命名空间，因而允许 `Option.Some(42)`。
  类型别名导入 `type Option as Maybe` 同样允许 `Maybe.Some(42)`；这与透明类型声明 `type X = ...` 不同。
  裸 `Some(42)` 由单独选择的构造器或 prelude 提供；仅选择 `Some` 不会顺带引入裸类型名 Option。
  `None::<Int32>`、`Missing::<Int32>` 可显式指定无载荷构造器的类型参数，不加调用括号。
- 宿主模块也支持别名及选择性导入，例如
  `import joky/vm as engine.{type Handle as Engine, create as new, call_int32 as invoke}`。
  `engine.Handle` / `Engine` 保留原 opaque 身份，`engine.create()` / `new()` 绑定原宿主入口。
  `call_int32` / `register_int32` 的别名保留原有参数及 ABI 限制。
- 默认 prelude 引入 `Option`、`Some`、`None`、`Result`、`Ok`、`Err`，以及整数的 16 个 `to_*()` 方法与 `MIN` / `MAX`。
  四个 wrapping 转换函数 `int32` / `int64` / `uint32` / `uint64` 属于语言内建，不受 prelude 开关影响。
  prelude 不引入 VM 或打印函数。显式 `import joky/prelude` 可在关闭默认 prelude 后恢复这些名称；
  `import joky/prelude as p` 只提供限定名称和整数方法。
- CLI 的 `run` / `build` 支持 `--no-prelude`；Rust 中用
  `TypeRegistry::with_prelude(false)` 或 `set_prelude_enabled(false)`，
  `VmEngine::set_prelude_enabled(false)`，以及编译开始前的 `AotCompiler::set_prelude_enabled(false)`。
  已规范化 AST、已加载模块和挂起任务保留其绑定；开关作用于后续源码编译。
- 显式选择性导入的重复名称、重复模块路径、模块别名与同级声明冲突均报错；`_` 不能用作导入名称。
  局部值遮蔽导入的构造器、函数和模块前缀；局部字段或方法仍按原规则查找。
  同级函数可以遮蔽 prelude 的裸构造器，限定构造器仍可用。
  本地类型声明与 prelude 的 Option / Result 同名会报错；需要自定义同名类型时关闭 prelude。
- 标准声明在同一个 TypeRegistry 环境及其克隆之间共享身份，重复规范化、显式导入和重载不会复制标准枚举。
  用户枚举仍在每次解析时获得新身份。导入只更改可见名称，不复制声明，也不会让用户同名变量捕获库内部引用。

标准库源码使用独立的模块作用域：`integer.jk` 显式导入 `joky/option.{type Option, Some, None}`，
`prelude.jk` 列出默认提供的类型和构造器。库自身不隐式导入 prelude；用户的同名声明或新增方法不会改变库内部的名称解析。
这些内嵌源码模块共享缓存和类型身份。用户模块使用相同的作用域与导入绑定机制，按每次编译的依赖图加载。

## 用户文件模块

CLI 以入口文件所在目录为唯一源码根目录。`import app/math` 对应该目录下的
`app/math.jk`；依赖文件里的导入也从同一个根目录解析，不相对于依赖文件自身。
不搜索当前工作目录、环境变量或包管理器；路径由标识符段和 `/` 组成，不能包含 `..`，
`joky` 命名空间保留给标准库。符号链接解析后的依赖文件也必须位于源码根目录内。

`app/math.jk`：

```joky
pub fn add(a: Int32, b: Int32) -> Int32 { a + b }
```

`main.jk`：

```joky
import app/math.{add}
fn main() -> Int32 { add(20, 22) }
```

上述程序在 JIT、解释器和 AOT 下均返回退出码 42。打印结果并正常退出的完整示例见
[examples/modules/main.jk](../examples/modules/main.jk)。

- 函数、记录、枚举、透明类型别名、impl 内的方法和常量默认私有；`pub` 标记对其他模块公开的声明。
  `pub enum` 同时公开全部构造器；`pub struct` 公开其构造与字段，当前没有字段级可见性。
  impl 本身不写 `pub`，按成员分别写 `pub fn` / `pub const`。
- 模块别名、选择性导入及导入重命名沿用标准库规则；例如 `import app/data as d.{type Choice, Value}`。
  公开类型可用 `d.Choice`；仅选择 `type Choice` 即可写 `Choice.Value(...)`，
  裸构造器仍需单独选择。模块别名与同名函数冲突时，显式重命名模块别名。
- `pub type Number = Int32` 导出透明别名；别名本身不创建新的构造器或类型级成员命名空间。
  公共 API 可以使用依赖模块的类型；同一次加载中的菱形依赖共享同一个名义身份。
- 每个用户模块继承本次编译的 prelude 设置，且保留自己的导入和私有辅助声明。
  调用方不能补入缺失名称，也不能通过规范化后的内部名称访问私有声明。
  普通 import 不重新导出选中的名称；暂不支持 `pub import`、通配导入或 package 配置。
- CLI 的入口仍是入口文件的 `main`，无需 `pub`。`pub` 控制源码模块间访问，
  不改变 VM 对入口源码函数的既有导出约定；依赖函数与方法不会成为 VM/AOT 的公开调用入口。
- 缺失文件、非法路径和完整依赖环在发布模块前报错，依赖深度最多 128 层。
  编译与 JIT/解释器运行时诊断定位真实文件；VM 错误保留源码快照，重载及销毁 VM 不影响旧任务的诊断。
  解释器还提供逐帧调用轨迹。独立 AOT 可执行文件仍输出错误及字节范围，暂不嵌入源码或新增原生调用栈。

嵌入 VM 通过显式 `ModuleSources` 提供源码，加载时不隐式访问文件系统：

```rust
use joky::{ModuleSources, vm::VmEngine};

let mut modules = ModuleSources::new();
modules.insert("app/math", "pub fn add(a: Int32, b: Int32) -> Int32 { a + b }")?;
let mut vm = VmEngine::new();
vm.load_source_with_modules(
    "import app/math.{add} fn answer() -> Int32 { add(20, 22) }",
    &modules,
)?;
assert_eq!(vm.call_int32_0("answer")?, 42);
```

`insert_file` 可传入带诊断文件名的 `SourceFile`；替换输入只影响后续加载。
`VmEngine::type_registry()` 提供包含宿主声明的只读环境，
`TypeRegistry::resolve_with_modules` 解析显式源码集；
`TypeRegistry::resolve_file` 是主动文件 I/O 入口，可供 CLI 或嵌入方显式调用。
`VmEngine::load_module` 和 `AotCompiler::compile_module` 接收该解析结果，不重新读取文件。
原来的 `load_source` 保持单源码入口；加载、解析或编译失败均保留旧 VM 模块。

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

别名可前向引用其他别名，最终目标必须是 `Int32`、`Int64`、`UInt32`、`UInt64`、`Bool`、`Unit`、`String`、模块内记录、枚举、Copy 元组或已注册的
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

- 字段类型支持 `Int32`、`Int64`、`UInt32`、`UInt64`、`Bool`、`Unit`、嵌套记录、有限枚举及 Copy 元组，允许空记录 `struct Empty {}`。
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
- 记录不支持解构、默认值、整体相等比较或用户定义析构；非泛型记录的方法通过独立的 `impl` 声明。

解析只依赖 token：名称之后的 `{}` 或以 `字段名:` 开始的花括号解析为构造，
其他花括号可作为相邻独立块；例如 `let x = 1 { let y = x y }` 保持原有行为。
因此 `value {}` 若意在先读变量再求空块，要写 `value; {}` 或 `(value) {}`。
`if` / `while` 条件及 `match` 被匹配表达式中未被括号、实参列表或内层块包住的构造受限，避免吞掉条件后的块；
使用 `if (Point { x: 1 y: 2 }).x > 0 { ... }` 明确边界。
`.` 字段访问优先于一元及二元运算；模块限定调用仍有效，局部值遮蔽模块名前缀时按该值的方法解析，
找不到方法即报错，不回退为模块调用。完整示例见 [records.jk](../examples/records.jk)。

### 泛型记录

记录可声明 Copy 类型参数，构造使用字段推导、上下文注解或显式 `::<...>`：

```joky
struct Pair<T> { left: T right: T }
struct Marker<T> {}
fn first<T>(p: Pair<T>) -> T { p.left }
fn make<T>(value: T) -> Pair<T> { Pair { left: value right: value } }
fn example() -> Int64 {
    let inferred = Pair { left: 4294967295 right: int64(7) }
    let annotated: Pair<Int64> = Pair { left: 4294967295 right: 0 }
    let explicit = Pair::<Int64> { left: 42 right: 0 }
    let phantom: Marker<Bool> = Marker {}
    first(inferred)
}
```

- `struct Pair<T, U>` 的参数具有声明局部身份，最多 64 个，不能重复或使用 `_`。
  参数只能实例化为整数、Bool、Unit 或 Copy 记录、枚举、元组；即使参数未出现在字段中，也不接受 String 或 opaque 类型。
- 类型位置写 `Pair<Int64>`，构造位置写 `Pair { ... }` 或 `Pair::<Int64> { ... }`。
  支持嵌套应用及模块限定构造，如 `model.Pair::<Int64> { ... }`。非泛型透明别名可指向具体实例，但不新增构造器。
- 推导先收集已知字段类型，再为仍未确定的整数字面量默认 Int32；声明顺序不改变初始化的源码求值顺序。
  返回类型、绑定注解、调用形参、外层字段和载荷的预期类型可以向内传递。
  `var` 字段赋值、整个记录替换、分支及 match 沿用普通记录的类型与快照规则。
- 未出现在字段中的 phantom 参数需要注解或显式实参；不会按后续语句反向推断已经完成的绑定。
  不同实例（包括零字段实例）按声明身份及全部类型实参区分；跨模块重命名导入保留身份，重新解析 / 重载产生新身份。
- 泛型函数可以构造、传递、返回 `Pair<T>` 并读写其已声明字段；模板即使未调用也接受刚性检查。
  `T` 本身不获得字段或整数运算能力；`impl Pair`、`impl Pair<Int64>` 和泛型 impl 尚不支持。
- 记录实例在类型规范化与 IR 规划阶段分别缓存，每阶段最多 256 个；仍限 65,536 标量槽和 256 层嵌套。
  phantom 参数也计入嵌套深度；所有声明依赖环保守拒绝，不支持递归容器。
  JIT、解释器和 AOT 共用实例布局，同步 / CPS 调用、Pending、失败、取消和重载沿用现有聚合值协议，宿主 ABI 不接收记录。

完整可运行示例见 [generic_records.jk](../examples/generic_records.jk)。

## 枚举与模式匹配

`enum` 声明模块内名义值类型，每个变体有零个或一个载荷；多个数据通过记录或元组组合：

```joky
struct Point { x: Int32 y: Int32 }
enum Lookup { Missing Found(Point) }
type LookupResult = Lookup

fn value(result: LookupResult) -> Int32 {
    match result {
        | Lookup.Missing => 0
        | Found(point) => point.x + point.y
    }
}
fn answer() -> Int32 { value(Lookup.Found(Point { x: 19 y: 23 })) }
```

- 本地枚举构造使用声明名：无载荷为 `Lookup.Missing`，有载荷为 `Lookup.Found(value)`。
  导入的枚举可用模块或类型导入别名限定，也可用显式导入 / prelude 提供的裸构造器。
  不接受无载荷构造器的 `Missing()` 或透明类型别名构造；`Done(Unit)` 仍须显式传入 `{}`。
  载荷类型限 Int32 / Int64 / UInt32 / UInt64 / Bool / Unit / Copy 记录 / 有限嵌套枚举 / Copy 元组，所有这些值按值复制。
  记录、枚举和元组可以相互组合，但不允许无限内联循环，暂不支持 String 或 opaque 载荷。
- 枚举至少一个变体，名称不能重复，`_` 保留给通配模式。变体声明的逗号可选；
  载荷类型和单个模式绑定允许尾随逗号。类型声明支持前向引用及透明别名。
  不同用户声明或不同次解析产生不同类型身份，即使名字、变体及布局相同；标准库身份按编译环境共享。
- `match value { | pattern => expression ... }` 是表达式，被匹配值只求值一次，类型必须为枚举。
  模式支持 `Variant`、`Enum.Variant`、`Variant(binding)`、`Variant(_)` 和整个分支的 `_`。
  模式中的导入名称按声明身份检查；其余无限定模式由被匹配类型确定。
  有限定模式可使用声明名或导入别名，载荷模式必须匹配声明的有无。
  例如本地 `Choice.None` 与 prelude 的 `None` 属于不同枚举，应使用 `Choice.None` 消除歧义。
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
  记录、枚举及元组共用 65,536 标量槽、256 层布局上限；含枚举的内联循环报 `recursive_type`，
  纯记录环和纯别名环分别保持 `recursive_record` / `cyclic_type_alias`。
  该布局是内部实现，紧凑 union 布局留待后续优化，不承诺宿主 ABI。

`enum` / `match` 现在是保留字；局部值遮蔽枚举构造名前缀时按局部值处理。
枚举声明同时提供类型名和构造器命名空间，例如 `Option`、`Option.Some`、`Option.None`。
`type Option` 的选择性导入也能使用限定构造器；裸构造器必须单独导入或由 prelude 提供。
第一版不支持 guard、嵌套解构、字面量模式、借用匹配、整体相等比较或非 Copy 载荷。
示例见 [enums.jk](../examples/enums.jk)。

### 泛型枚举

枚举可声明 Copy 类型参数，用 `Option<Int32>` 等类型应用指定实例。Option / Result
是普通枚举名，没有特殊运行时表示；`joky/option` 和 `joky/result` 提供标准声明，
默认 prelude 已引入这两个类型及其构造器，以下示例可直接使用：

```joky
fn unwrap_or<T>(value: Option<T>, fallback: T) -> T {
    match value {
        | None => fallback
        | Some(value) => value
    }
}

fn example() -> Int32 {
    let present = Option.Some(42)
    let absent: Option<Int32> = Option.None
    let explicit = Option.None::<Int32>
    let nested: Option<Option<Int32>> = Option.Some(present)
    unwrap_or(unwrap_or(nested, absent), unwrap_or(explicit, 0))
}
```

- 类型应用支持嵌套、元组及别名：`Option<(Int64, Bool)>`、`Result<Option<Int32>, Bool>`、
  `type MaybeInt = Option<Int32>` 均可用。`>>` 在类型列表中按两个闭合符解析；表达式中的右移不变。
  类型参数列表非空、逗号分隔、允许尾随逗号，最多 64 个参数；参数只属于当前声明。
- 构造可使用 `Enum.Variant`、导入别名或已导入的裸构造器；载荷变体须带恰好一个参数，空变体没有括号。
  显式实例写 `Option.Some::<Int64>(42)`、`Option.None::<Int64>` 或 `Result.Ok::<Int32, Bool>(42)`；
  不接受 `Option<Int32>.Some`、`Option::<Int32>.Some` 或部分 `_` 推导。
- 从载荷已有类型和预期枚举类型推导，未约束整数字面量默认 Int32。注解、普通函数参数、返回值、
  字段及聚合成员可以提供上下文；例如 `let n: Option<Int64> = Option.Some(4294967295)` 合法。
  `Option.None` 单独使用时无从确定 T，`Result.Ok(42)` 也无法确定 E，均要求注解或显式类型实参。
  分支可共享可推导的枚举结果类型，`if flag { Option.None } else { Option.Some(42) }` 得到 Option<Int32>。
- 泛型函数可接受 / 返回 `Option<T>`、匹配并传递其 T 载荷；实例化替换嵌套类型及具体布局。
  函数实参的其他约束也可定型构造，例如 `unwrap_or(Option.None, 42)`。函数自身不从外层返回注解反推类型参数；
  复杂局部推导或未出现于载荷中的参数仍需显式指定。
- 实例身份为枚举声明身份与全部类型实参：`Option<Int32>` 不等于 `Option<Bool>`，
  即使参数未用于载荷也保持区别；透明别名不创造新身份，重载后的用户同名声明具有新身份。
  已绑定的枚举值不会因上下文改变实例，也没有隐式整数转换。
- 声明时检查所有变体，包括未实例化的模板；类型实参及载荷均限 Copy 类型，String / opaque 不接受。
  仍不支持递归内联数据。经类型应用、元组、记录或别名形成声明依赖环会报 `recursive_type`，
  第一版保守拒绝这些环，包括经过未存入载荷的类型参数形成的环。
- 编译期间缓存实例，规范化和 IR 规划各自最多 256 个不同枚举实例（含刚性参数实例）；
  函数实例限额独立计算。布局最多 65,536 标量槽，类型嵌套最多 256 层，未用于载荷的类型实参也计入嵌套深度。
  共享类型图的比较、替换及身份检查避免重复展开。
- 枚举实例沿用标签加独立载荷区的内部布局，支持三后端同步 / CPS、值快照、Pending、取消和旧任务保活。
  没有新增运行时泛型值、extern 聚合 ABI 或公开聚合调用入口。

泛型别名声明、trait、泛型 impl 和非 Copy 载荷仍留待后续；Copy 泛型记录见[泛型记录](#泛型记录)。
完整可运行示例见 [generic_enums.jk](../examples/generic_enums.jk)。

## 元组与解构绑定

元组是按元素类型、数量和位置确定的结构类型，不需要具名声明：

```joky
type Inspection = (Int32, Bool)
fn inspect(n: Int32) -> Inspection { (n, n > 0) }

fn answer() -> Int32 {
    let (value, positive) = inspect(42)
    var (x, y) = (1, 2)
    x = x + 10
    if positive { value + x + y } else { 0 }
}
```

- `(value)` / `(Type)` 仍然是分组；`(value,)` / `(Type,)` 才是单元素元组。
  多元素以逗号分隔，可尾随逗号；换行不能替代逗号。`()` 不表示空元组或 Unit，Unit 值仍用 `{}`。
- 元素限 Int32 / Int64 / UInt32 / UInt64 / Bool / Unit 和由它们组成的记录、枚举、元组；首版均为 Copy，
  不接受 String 或宿主 opaque 元素。支持透明别名、任意有限嵌套、记录字段和枚举的单个载荷。
  `(Int32, Bool)` 与同结构别名等价，与 `(Bool, Int32)`、`(Int32,)` 不同；
  元组中的名义类型仍按原身份比较，重载产生的新记录或枚举不会因外层元组而变得兼容。
- `let (x, y) = pair` / `var (x, y) = pair` 只接受不可失败的模式：名称、`_`、元组及嵌套组合。
  `let (x, (_, y)) = value` 合法，形状及元素数量必须一致；整个模式可有类型注解，
  如 `let (x, flag): (Int32, Bool) = inspect(42)`。不支持每个叶子单独写注解或 rest 模式。
- 初始化器从左到右且只求值一次，成功完成后才引入全部名称；模式内名称不能重复，`_` 可重复。
  可以遮蔽外层名称，例如 `let (x, y) = (y, x)` 的右侧读取原有绑定。
  `let` 叶子不可修改，`var` 的每个名称获得独立可变存储，修改它不影响初始化器或其他副本。
  绑定仍只属于当前块；解构不会产生变量之间的引用关系。
- `_` 在 let / var 中表示丢弃，不创建可读或可赋值的变量。`let _ = effect()` 和
  `let (_, _) = value` 仍执行初始化器的全部副作用、失败和挂起；被丢弃项仍做类型检查。
  迁移旧的 `let _ = ...` 后读取 `_` 的写法时，需要为该绑定改一个实际名称。
- 元组元素是表达式，支持分号序列，例如 `(first(); 1, 2)` 有两个元素。
  `;` 仍可省略；但名称后紧跟 `(` 会继续解析为调用，换行不改变此规则。
  若返回元组前一项以名称结束，写 `let (x, flag) = pair; (x + 1, flag)`，
  或用 `return (x + 1, flag)` 明确边界，不把分号规定为通用终止符。
- 同步/CPS 参数、返回值、if/match 合并、递归及 scope 参数都支持元组，挂起恢复不重放前面的元素。
  `(Unit, Empty)` 这类零存储元组仍有结构类型，构造和调用的副作用不会被省略。
  元组沿用聚合值的 65,536 标量槽、256 层布局上限；类型/绑定模式语法嵌套也有限额。
  经过元组的循环依赖继续按参与声明报别名环、记录环或枚举环错误。
- 首版通过解构访问元素，不提供 `.0`、索引、解构赋值、参数模式、整体比较或非 Copy 元素。
  match 仍只匹配枚举；元组载荷先用 `| Value(pair) => { let (x, y) = pair ... }` 解开。
  extern、宿主注册和公开标量入口暂不接受元组，CLI main 仍限 Unit / Int32。

完整示例见 [tuples.jk](../examples/tuples.jk)。

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

运算符从高到低为一元 `-` / `!` / `~`、乘除、加减、移位、`&`、`^`、`|`、比较、`&&`、`||`、赋值、分号序列。
移位和二元位运算在各自优先级内左结合；位运算优先于比较，`flags & mask == 0` 表示 `(flags & mask) == 0`。
加减优先于移位，`1 << 2 + 1` 为 8，`1 << 1 << 2` 为 8。
一元运算右结合，四则运算同级左结合：`1 + 2 * 3` 为 7，`(1 + 2) * 3` 为 9。
`-` 接受 Int32 / Int64，`!` 只接受 Bool。整数加、减、乘和取负采用对应位宽环绕，
最小值再次取负仍为自身；字面量定型和边界规则见[整数类型与字面量](#整数类型与字面量)。

Int32 / Int64 / UInt32 / UInt64 支持 `==`、`!=`、`<`、`<=`、`>`、`>=`，Bool 支持 `==` 和 `!=`，结果均为 Bool。
两侧必须同型，不支持 Bool 有序比较，也不支持 String、Unit 或 opaque 类型比较。
比较不可连续书写：`1 < 2 == true` 是语法错误，`(1 < 2) == true` 合法。
表达式和函数实参从左到右求值，后续赋值不会改变已经读取的值。

JIT、AOT 和解释器共享类型检查与控制流规划。Int32 / Int64 除法向零截断，`-2147483648 / -1`
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
模块与成员别名见[导入与 prelude](#导入与-prelude)。Joky 内的 `vm.load_source` 不隐式搜索文件，
显式多源码加载使用 Rust API；尚无标准库异步调用；
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
