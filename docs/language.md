# 语言与内置模块

本文描述已实现的语言子集。运行命令见 [README](../README.md)，
嵌入与 ABI 约束见 [架构说明](architecture.md)。
目标语法和完整类型系统另见 [语法与类型系统设计草案](syntax.md)，
其中尚未实现的规则不改变本文描述的当前行为。

## 注释

`//` 注释延续到行尾；`/* ... */` 块注释可以跨行并支持嵌套。
块注释中的引号、花括号和 `//` 都是普通文本，只有 `/*` 和 `*/` 改变嵌套深度。
注释分隔前后 token，不会拼接标识符或运算符；其中的换行仍参与现有的表达式分隔规则。
字符串和字符字面量内的注释标记按字面量文本处理，字符串插值表达式内则支持正常注释。
未闭合块注释报错，诊断从最外层 `/*` 开始。

## 类型与函数

类型包括 `Int8` / `Int16` / `Int32` / `Int64`、`UInt8` / `UInt16` / `UInt32` / `UInt64`、`Float32` / `Float64`、Unicode 标量 `Rune`、`Bool`、`Unit`、不透明 C 对象指针 `Ptr`、不可变 UTF-8 `String`、名义记录、枚举、结构化元组、动态数组 `Array<T>`、无序字典 `Map<K, V>` 、注册过的宿主 opaque 类型和非 Copy 外部资源类型。
函数正常结束时最后一个表达式决定返回值，也可用 `return` 提前返回；不写返回注解时为 Unit。
普通函数不能调用 `@suspends` 函数。
`extern fn` 声明宿主实现，完整签名和挂起约定必须匹配注册信息。
String 是非 Copy 的不可变 UTF-8 拥有值；字面量和 clone 在任务预算内分配，按值传参、赋值及返回移动所有权。文本组合使用 concat、join、StringBuilder 或 `"{value}"` 插值（普通字符串字面量默认插值），尚无 `+`。
函数参数和调用实参之间必须写逗号，允许尾随逗号，例如 `fn f(x: Int32,)` 和 `f(42,)`；
空列表仍写 `()`，不允许开头逗号或连续逗号。换行和注释不能替代列表逗号。

宿主可注册独立资源类型（例如 Socket）。资源没有语言构造器，不能复制或从整数/Ptr
转换；按值赋值、参数和返回转移所有权。它可进入 Result、记录、元组和拥有元素容器，
无引用资源聚合可跨语言挂起并存入 State，退出和取消自动释放。
宿主创建入口使用注册的同步 `(UInt64) -> Resource` 或异步
`(UInt64) @suspends -> Resource` 工厂；普通 extern/C ABI
不能传递资源。资源类型并不自动提供网络方法，网络模块进度见
[网络运行时计划](network-runtime.md)。

宿主还可通过 `register_resource_io` 注册两种有界字节原语：读接收
`(&mut Resource, UInt64)` 并异步返回 `Array<UInt8>`；写接收
`(&mut Resource, Array<UInt8>)` 并异步返回已写入的 `UInt64` 字节数。
发送消费数组并创建宿主快照。单次最多 1 MiB，读上限必须为正；空读取表示 EOF，
写允许部分成功，非空写零进度为失败。整个 Pending 期间保持资源独占借用，stop 后归还
权限。注册的 Resource 局部或拥有记录中的 Resource 字段可按此规则跨挂起借用；
局部视图仍不能逃逸或存入容器，同一拥有记录字段借用仍保守地借用整个拥有者。
这些是嵌入层原语；普通脚本使用下文的 [TCP 模块](#内置-tcp-模块-jokytcp)，
由统一 Mio 驱动等待，并通过 `Result` 处理业务 I/O 错误。

## 同步 C 借用接口

```joky
#repr(C)
struct CPoint { x: Int32, y: Int64 }
unsafe extern "C" fn update(point: &mut CPoint)
unsafe extern "C" fn inspect(point: &CPoint) -> Int64

fn run() -> Int64 {
    var point = CPoint { x: 19, y: 20 }
    unsafe {
        update(&mut point)
        inspect(&point)
    }
}
```

- `#repr(C)` 位于 struct/pub 之前，保证字段按声明顺序使用目标 C 字节布局；普通记录不自动兼容 C。
  记录必须非空、无类型参数，字段可以是八种定宽整数、Float32 / Float64、其他 `#repr(C)` 记录，
  或递归包含这些类型的非空 `FixedArray`，
  透明别名按目标处理。浮点字段按 IEEE binary32 / binary64 与 C 布局逐项核对。
  不接受 Bool、Unit、String、Ptr、引用、元组、枚举、packed、union 或位域。
- C 导入写成 `unsafe extern "C" fn`，接受合计零到四个引用或 Ptr 参数，结果为数值、Ptr 或 Unit。
  引用所指类型可以是定宽整数、浮点、C 记录或兼容固定数组；不接受其他按值输入、返回引用、变参、泛型或 @suspends 导入。
  数组及含数组记录不能按值传入或返回；`register_c_fn` 当前结果仍限整数、Ptr 或 Unit。
- 调用必须处于词法 `unsafe { ... }` 块内。块保留普通块的类型、返回、局部作用域和借用规则；
  允许 arena 对象、同步局部值及其字段，不能绕过共享/独占冲突、移动和局部逃逸检查。
- C 调用本身同步完成，可位于 @suspends 函数的挂起点之间；不允许 C 保留引用实参的地址（包括作为 Ptr 返回）、释放/重分配借用对象、
  越界、跨边界 unwind 或以回调重入制造借用冲突。C const 不能证明这些保证，由绑定者承担 unsafe 契约。
- Rust 嵌入用 `VmEngine::register_c_fn(name, signature, ptr, owner)`，与旧 direct/CPS 注册分开校验。
  记录引用及数组内记录的注册签名使用实际记录名，导入记录用规范名称；加载时递归解析名字，
  核对元素名义身份、各维长度、可变性及完整签名，不以总字节数代替类型匹配。
  可用 `joky::ffi::c_header(&module)` 生成本机目标的 C11 声明和 sizeof/align/offset 断言，
  数组额外断言各维长度与元素步长。
  AOT 的 C 导入保持原生符号名，由链接器提供实现，不随 Joky symbol_prefix 改名。
- `unsafe` 已是保留字；`@` 仍用于函数效果，`#` 用于声明属性。当前开放 `#repr(C)`、[JSON 派生](#struct-与类型化-json)的 `#derive(...)`、[域根声明](#when-状态域操作)的 `#state` 与[周期声明](#周期声明-every)的 `#every(...)`，
  允许尾随逗号；未知属性、重复属性或错误使用位置报错。通用属性及属性宏尚未实现；异步借用见下节。

布局和注册细节见 [FFI 设计](arena-borrowing-ffi.md#5-ffic-布局与引用参数)。

### 固定数组与 C 指针适配

C 兼容数组要求每一维 N>0，元素为非零大小的兼容类型。语言内合法的空数组、Unit 数组，
以及 Bool、Rune、String、普通枚举、Ptr、引用、动态容器等元素都不能借此进入 C 布局。
普通泛型记录不会因字段兼容而获得 `#repr(C)` 身份。

| Joky 类型位置 | C 声明 |
| --- | --- |
| 字段 `data: FixedArray<UInt8,16>` | `uint8_t data[16]` |
| 字段 `matrix: FixedArray<FixedArray<Int32,3>,2>` | `int32_t matrix[2][3]` |
| 参数 `values: &FixedArray<Int32,4>` | `const int32_t (*values)[4]` |
| 参数 `values: &mut FixedArray<Int32,4>` | `int32_t (*values)[4]` |

数组引用直接指向已初始化的原存储，不分配动态 Array 包装，不复制元素。
C 的 `int32_t values[4]` 参数会调整为首元素指针，不能替代上表的 pointer-to-array 原型。
真实 API 若接受 `int32_t *` / `const int32_t *`，应提供 C 包装，在包装内用 `*values`
显式取得首元素指针；不得强转函数指针来掩盖原型差异。
包装负责检查有效长度不超过容量，再转成真实 API 的长度类型。

[读写包装 fixture](../tests/fixtures/fixed_array_f4.c) 的 `cfa_read` / `cfa_write` 接受
容量为 4 的数组引用和 UInt64 长度引用，先检查上限，再调用接受 `size_t` 的首元素指针 API。
写入要求独占借用；C 返回错误状态不会回滚已经合法完成的写入，调用结束后借用正常结束。
地址只能在本次同步调用内使用，不能跨 Pending 保留。

UInt8 数组仅表示字节。容量、有效字节数、文本编码分别约定；不能自动视为 String 或越界寻找 NUL。
[字节示例](../tests/fixtures/fixed_array_f4.jk) 使用容量 8、有效长度 5 的 UTF-8 `é中`，
检查有效长度后只复制有效前缀到 Array，再用 `string.from_utf8` 校验并转换；超长和截断编码分别拒绝。
此转换是显式文本处理，C 数组调用本身不执行该复制，也不扩展 TextFfiV1。

可运行的 [Rust 嵌入示例](../examples/fixed_array_ffi.rs) 使用 `Type::FixedArray` 与
`FixedArrayType { element, length: ConstArgument::Value(N) }` 构造注册签名，分别运行 JIT/解释器：

```sh
cargo run --example fixed_array_ffi
cargo run --example fixed_array_ffi -- /tmp/joky-fixed-array
```

第二条还生成 `bindings.h` 与 `fixed_array.o`，可与 [C 示例](../examples/fixed_array_ffi.c)
及同一 Debug `libjoky.a` 联编。独立 AOT 联编执行由 `fixed_array_f5_c_embedding_example_executes`
回归覆盖；Rust 示例和 C 示例共用[调用源码](../examples/fixed_array_ffi.jk)。

### 异步宿主借用

```joky
#repr(C) struct Sample { value: Int32 }
extern fn read_sample(sample: &mut Sample) @suspends
extern fn timeout() @suspends

fn run() @suspends -> Int32 {
    let sample = alloc(Sample { value: 0 })
    race { | read_sample(sample) | timeout() }
    sample.value = 42
    sample.value
}
```

- `@suspends` extern 的引用参数选择独立的借用协议 v1。接受 1–4 个定宽整数、浮点或
  `#repr(C)` 记录的共享/独占引用，结果限整数或 Unit。
  不接受固定数组或含数组记录，也无 Ptr/String/引用聚合输入，不返回引用；可借用稳定的局部或容器元素位置。
- Rust 通过 unsafe `register_borrowing_suspending_fn(name, signature, BorrowingHostV1, owner)` 注册；
  源码调用受普通借用与挂起效果检查。旧 `register_suspending_fn` 不能注册这种签名。
- 描述符版本为 1，提供 poll 和强制 stop 回调。poll 返回 0 完成、1 Pending、其他值失败。
  运行时在成功、失败或取消时执行 `stop(frame, reason)`，停止所有外部访问并清理操作状态后才继续；
  reason 分别为 `STOP_COMPLETED`、`STOP_FAILED`、`STOP_CANCELLED`。未启动分支不调用 stop，已启动操作只收尾一次。
- 宿主可以跨 Pending 保存对象地址，不能保存 frame/slots 地址、改变 slots 大小、释放/重分配借用对象或越权访问。
  `host_context` 可存放操作状态，stop 负责清理；stop 必须同步、幂等、不 unwind。无法同步停止的 I/O 不支持。
- 上例中 timeout 胜出时，read_sample 的 stop 返回后，父任务才获得 sample 的写权限。
  任务失败、调度器取消和关闭同样先停止访问，再释放 arena；重载或 VM drop 不破坏独立 continuation 的保活。
- AOT 导入 `__joky_cps_borrow_v1_<symbol>` 常量描述符，C 接口见 `include/joky_cps.h`。
  symbol 带配置的 AOT 前缀，旧 CPS 函数不能代替描述符；本功能不附带 I/O 驱动或自动就绪调度。

### 不透明指针 Ptr

`Ptr` 是内置、无类型参数、可复制的 C 对象指针；用来接收 C 返回的地址并原样传回 C：

```joky
unsafe extern "C" fn resource_create() -> Ptr
unsafe extern "C" fn resource_update(resource: Ptr)
unsafe extern "C" fn resource_destroy(resource: Ptr)

fn run() -> Bool {
    let resource = unsafe { resource_create() }
    if Ptr.is_null(resource) { return false }
    unsafe {
        resource_update(resource)
        resource_destroy(resource)
    }
    true
}
```

- `Ptr.null() -> Ptr` 创建空指针；`Ptr.is_null(value: Ptr) -> Bool` 判空。两者是无类型参数的安全内建操作。
- Ptr 可作为别名目标、泛型实参、普通记录/元组字段和枚举载荷，也可保存在 arena 中或跨 Joky 挂起点传递。
  保存和复制只保留地址，外部对象及其所属库的寿命由绑定和调用方保证。
- Ptr 不可解引用、访问字段、进行算术/比较或与整数及 Joky 引用互转；不表示函数指针。
  `*p / *p = next` 不能接收 Ptr 来读写外部对象，但可通过 `&Ptr` / `&mut Ptr` 读写 Joky 自己保存的指针值。
- Rust 注册使用 `Type::Ptr`；C 头文件和调用 ABI 映射为 `void *`。只能经 `register_c_fn` 跨外部接口，
  旧 direct/CPS 注册、`#repr(C)` 字段及 C 的 `&Ptr` 参数暂不开放。
- Ptr 不记录分配器或拥有权，没有自动释放、重复释放检查或释放后的副本失效；
  正常退出、失败和取消都不会代调 C 释放函数。unsafe 调用方负责满足外部 API 的类型、空值、寿命、别名及释放契约。

## 任务 arena 与引用

同步根调用或挂起任务在需要时自动创建 arena；用户用内建函数分配，无需创建或传递分配器：

```joky
struct Point { x: Int32, y: Int32 }
fn make_point() -> &mut Point { alloc(Point { x: 1, y: 2 }) }
fn move_point(p: &mut Point) { p.x = p.x + 1 }
fn sum(p: &Point) -> Int32 { p.x + p.y }
fn run() -> Int32 {
    let p = make_point()
    move_point(p)
    move_point(p)            // 调用自动重借用，调用后 p 仍可使用
    let view: &Point = p
    let value = sum(view)
    p.y = value             // view 的最后一次使用已结束，可恢复写入
    p.x + p.y
}
```

| 操作 | 当前规则 |
| --- | --- |
| `alloc(value)` | 返回 `&mut T`；先求值，再分配并完整初始化；追加分配不移动已有对象 |
| `&*reference` / `&mut *reference` | 显式共享/独占重借用；独占重借用要求原引用可写；注解、参数或返回位置也可把 `&mut T` 收窄为 `&T` |
| `*reference` | 复制整个所指 Copy 值；支持标量、记录、枚举、元组和 Unit |
| `*reference = value` | 要求 `&mut T`，替换所指 Copy 或拥有型值，清理旧拥有值，返回 Unit |
| `mem.replace(reference, value)` | 要求 `&mut T`，放入新值并返回旧值；支持 Copy 与拥有型值 |
| `mem.swap(left, right)` | 两个同类型且不重叠的 `&mut T`，交换整值，不析构，返回 Unit |
| `mem.drop(value)` | 消费一个值，立即结束其清理责任；Copy 值或共享引用不销毁被引用对象；独占引用被消费后不能再次使用 |
| `reference.field` / 字段赋值 | 自动解引用，支持嵌套记录；共享引用不能写入 |

`joky/mem` 通过 `import joky/mem` 导入；也支持模块别名和选择导入。
`mem.replace(&mut T, T) -> T` 是通用机制，支持数组、拥有型记录、元组和任意枚举。
目标地址先求值并保留，再完整求值新值；借用检查通过后才交换内容，交换本身不调用 Drop。
返回的旧值归调用方所有，新值由原存储负责清理。旧值最后使用后可移动或释放原容器，
但仍存活的目标载荷借用会阻止替换。两侧类型必须相同，类型由引用目标推导，也支持 `mem.replace::<T>` / `mem.swap::<T>` 显式类型参数。
拥有型替换可以在挂起函数中使用，目标仍须满足借用与稳定存储规则；arena Copy 值也可在挂起函数中替换。

```joky
import joky/mem
fn take<T>(value: &mut Option<T>) -> Option<T> {
    mem.replace(value, None)
}
```

标准库 `Option.take()` 使用上述机制。也可将 `Array.borrow_mut_ref(index)` 或
拥有字段的 `&mut` 作为目标；带 Drop 的父记录仍保持完整初始化，其钩子会观察更新后的字段。
新值求值或交换准备失败时，两侧仍保持各自的清理责任；正常退出、失败与取消均恰好清理一次。
示例见 [memory_replace.jk](../examples/memory_replace.jk)。

`mem.swap` 保留两个位置的完整初始化状态和清理责任，支持 Copy、String、数组、拥有记录、元组与枚举。
Copy 局部记录的不同字段可以分别借用后交换；拥有记录字段仍沿用整拥有者冲突检查；同一数组的元素仍按整数组检查借用，使用 `Array.swap(i, j)`。
`mem.drop` 不要求 `Drop` trait；资源会执行自定义钩子及嵌套清理，移动闭包也会释放捕获的拥有值。
直接调用 `Drop.drop` 仍被拒绝；`mem.drop` 消费参数后，该拥有值不能再次使用。

`*p` 只复制 Copy 值，不能从借用中移出拥有者。`*p = next` 在右侧成功求值后提交替换并析构旧值；
需要保留旧值时使用 `mem.replace`。`&*p` / `&mut *p` 只借用位置，不读取或移动所指值，
因此也能重借用 dyn 视图；dyn 本身不能按值读取、赋值或交换。`(*p).field` 支持字段读写，
`&(*p).field` / `&mut (*p).field` 沿用隐私、来源和重叠路径检查。
`Ptr` 仍是不透明的外部地址，不能用 `*` 访问外部存储。
完整表达式后的换行会分隔下一表达式，因此 `let p = &mut value` 后可直接换行写 `*p = 42`。
旧的 `memory.read/write/replace` 和全局 `share` 内建已删除，没有兼容入口。

- 分配目标支持八种定宽整数、Float32 / Float64、Rune、Bool、Unit、Ptr，以及含这些值和共享引用字段的 Copy 记录、枚举和元组。
  暂不允许 String、opaque 句柄、独占引用字段或直接分配引用本身；零大小对象也有独立分配身份。
- `let` / `var` 控制绑定是否可重新赋值，与所指对象是否可写分开。
  `let p = alloc(...)` 可以修改对象；`p = other` 表示替换绑定，要求 var。
- `&mut T` 不可复制：`let q = p` 移动独占引用，此后不能使用 p。
  普通调用自动重借用；返回值携带借用时，输入的冻结延长到结果最后使用。
  `&T` 可以复制，仍受任务生命周期约束，不能反向转换为 `&mut T`。
- 共享视图存活期间不能写入或移动原独占引用；独占重借用存活期间不能通过原引用读写。
  单次赋值先保留目标地址，再求 RHS；`p.x = p.x + 1` 和
  `*p = *p + 1` 合法，最终写入时检查活动借用。
- 分支和循环进行保守的来源、移动及最后使用分析；arena 对象仍按整对象检查冲突，局部字段规则见下节。
  跨函数按签名合并引用输入的依赖，不分析被调函数体来放宽权限。
  循环重复执行同一分配点可能保守合并来源；分析超出预算会报编译错误。
- 可定义 `type View = &Point` 或 `fn read<T: Copy>(p: &T) -> T`。
  普通 `&T` 可以作为 Copy 类型实参、元组元素、记录字段或枚举载荷；`&mut T` 不可以。
  数组视图 `&Array<Int32>` 另有不可存储限制，见[动态数组](#动态数组)。
  仍不支持引用到引用，如 `&&T` 或 `&mut &T`。
- 带引用参数或返回值（含聚合内引用）的函数只作为 Joky 内部 helper，允许跨源码模块调用；不导出为
  Rust VM / C 任务入口。旧宿主注册及原生回调不接受这些引用，宿主引用结果仍不开放；
  同步 C 引用输入和挂起宿主引用输入分别通过独立 FFI 契约开放。
- 同步及 `@suspends` 函数都可分配、读写、传递和返回 arena 引用，
  包括带共享引用的 Copy 聚合和已封存图。挂起只保留对象，不解除已有借用限制。
  `parallel` 分支可以接收受检任务 arena 引用，共享读取同一对象或独占访问不同对象；显式 arena 与局部值借用见下节；同步 C 和异步宿主借用接口见前节。
- 根任务正常结束、提前返回、运行时失败、取消或 continuation 丢弃时释放全部对象；
  Pending、内部 helper 返回、分支/parallel 完成及 break/continue 不提前释放。终态 continuation 即使仍被调度器保存，也不继续占有 arena。
  当前每次分配使用独立稳定存储，尚未做分块优化。每根任务固定 64 MiB 计费预算，
  包括对齐后的对象大小、零大小占位和逐项管理成本，不等同于进程实际内存上限。
  预算耗尽、布局溢出或分配失败经运行时错误通道传播，不产生空引用；尚无 free/reset。

`alloc` 是保留的内建函数名，`mut` 是保留字。示例见
[arena.jk](../examples/arena.jk)，JIT、解释器与 AOT 输出均为 13、16。
后续规划见 [Arena 设计](arena-borrowing-ffi.md)。

### 同步显式 arena

需要按循环、批次或局部作用域提前释放普通对象时，可显式导入 `joky/arena`：

```joky
import joky/arena.{type Arena}

fn make(arena: &Arena, value: Int32) -> &mut Int32 {
    arena.alloc(value)
}

fn run() -> Int32 {
    let arena = Arena.new()
    let first = make(&arena, 20)
    let second = arena.alloc(22)
    (*first) + (*second)
}
```

`Arena.new()` 创建非 Copy 的拥有者；`arena.alloc(value)` 在指定 arena 中分配，返回新的 `&mut T`。
载荷沿用 `alloc` 的 Copy 限制，支持标量、记录、枚举、元组及受检共享引用字段；不接收 String、Array 或其他拥有型值。
对象地址稳定，多次分配不独占整个 arena，已有对象可以同时保持独占引用。

- 拥有者退出词法作用域、提前 return、break/continue 或运行时失败时整体回收；`mem.drop(arena)` 可主动提前回收。
  无活动借用时可移动拥有者；已有引用仍有效时不能移动、销毁或替换它。
- `&Arena` helper 可返回新分配对象的引用，返回值依赖传入的 arena。
  局部创建的 arena 引用不能通过块结果、函数返回值或含引用聚合逃逸，也不能存入任务 arena。
- 较短寿命 arena 可以保存较长寿命对象的共享引用，保存的借用持续到目标 arena 释放。
  因此内层 arena 释放后，可以恢复对外层对象的独占访问；反向存储被拒绝。
- 显式 arena 的控制块和对象与隐式 task arena 共用 64 MiB 总额度，释放立即退还额度。
  每次 `Arena.new()` 不会获得新的独立任务额度。
- 首版只支持同步使用。Arena 能力及其引用不能跨语言挂起点；解释器内部时间片及取消仍按拥有者清理。
  Arena 不能作为聚合字段、普通泛型实参、闭包捕获或持久状态，也不能由用户函数按值返回；创建使用 `Arena.new()`。
  不提供 `&mut Arena`、reset 或逐对象 free。

显式分配不改变 `alloc(value)` 的目标，也不会把 helper 内的 String、Array 等分配重定向到该 arena。
三后端共享用例见 [explicit_arena.jk](../tests/fixtures/explicit_arena.jk)。

### 局部值借用

函数可以直接借用局部值，把修改结果留在原变量中，不需要先调用 `alloc`：

```joky
struct Point { x: Int32 }
fn increase(p: &mut Point) { p.x = p.x + 1 }
fn inspect(p: &Point) -> Int32 { p.x }
fn run() -> Int32 {
    var point = Point { x: 41 }
    increase(&mut point)
    inspect(&point)
}
```

- `&local` 产生共享只读引用；`&mut local` 产生独占可变引用，要求原绑定是 `var`。
  `let`、按值参数、解构绑定和 match 载荷均可共享借用；参数和 match 载荷绑定不可变。
- Copy 局部目标包括定宽整数、浮点、Rune、Bool、Unit 和不含引用的 Copy 记录、枚举、元组；
  不支持 opaque 句柄、引用本身或含引用的聚合。泛型按具体实例检查；String 使用后文的拥有值借用规则。
- 引用沿用自动重借用、共享化和最后使用规则；通过原变量读写或修改字段也检查冲突。
  局部对象中明确不重叠的字段可独立借用；已有视图最后使用后，原位置可恢复相应访问权限。
- 引用不能超过原绑定的词法作用域，不能返回指向本函数局部值或按值参数的引用。
  普通 helper 可以把引用参数的视图返回给调用者；Copy 聚合中的引用同样保留来源和有效范围。
- 局部引用不能存进任务 arena；传给具有 arena 分配能力的 helper 时，编译器必须证明对应参数
  不会保存借用（可由函数体推导或显式 `@noescape` 保证）。返回借用仍跟踪来源。
  仅使用局部借用不会获得 arena 分配能力。
- 接受整个局部标识符及其内联记录字段或元组元素，如 `&point.x`、`&mut player.position.x`、`&pair.0`。
  根局部值必须是不含引用的 Copy 数据；可投影记录字段与元组元素，不接受经引用解引用的字段或临时值 `&42`。
  借用得到的引用仍可通过字段访问及 *p / *p = next 操作，不增加引用算术。
- `@suspends` 函数中的 Copy 局部引用可跨挂起调用、parallel/race、branch 注册和 scope join，
  也可传给满足不保留借用契约的挂起函数或受检异步 FFI。借用期间原位置保持稳定，
  原绑定必须活到调用完成或取消，冲突访问仍被拒绝。统一规则见下文[跨挂起借用](#跨挂起借用)。
- 局部存储由函数调用拥有，原生同步函数使用稳定栈位置，挂起函数使用 CPS 帧的固定存储，
  解释器使用独立的帧属存储，内部时间片暂停也保持有效。循环再次进入
  同一声明时重新初始化并复用存储，不累积任务 arena 分配。每个具体函数的被借用存储保守限额为
  64 KiB（计入每处声明的零大小占位和对齐余量），不等同于整个调用栈或进程内存上限。

#### 独立字段与重叠位置

```joky
struct Player { hp: Int32, mp: Int32 }
fn increase(value: &mut Int32) {
    *value = *value + 1
}
fn run() -> Int32 {
    var player = Player { hp: 20, mp: 20 }
    let hp = &mut player.hp
    player.mp = player.mp + 1
    increase(hp)
    player.hp + player.mp
}
```

这里 hp 借用只约束 player.hp；player.mp 仍可读取、写入或单独取借用。
`&mut player.hp` 和 `&mut player.mp` 也可同时传给一个 helper。

- 同一字段、整对象与任意字段、父字段与子字段都属于重叠位置；重叠访问中有独占权限时拒绝冲突。
  例如 `&mut player.position` 与 `&player.position.x` 不能同时有效，而 position.x 与 position.y 可独立借用。
- 判断依据是对象来源和字段路径，不是字节地址不同；零大小字段仍有各自的逻辑位置。
- 原变量的字段读取、字段赋值、整值读取和整体赋值使用同一套冲突规则。
  写目标先预留，右侧可只读访问该位置并操作其他独立字段；最终写入时不能留下冲突借用。
- 复制、聚合包装、分支合并、循环和 helper 返回会保留字段来源。跨函数仍按签名合并输入依赖；
  例如从 hp/mp 两个引用输入返回一个共享视图时，调用方保守地认为结果可能指向任一字段。
- 字段引用不能超出整个根局部值的词法寿命，不能返回本函数局部字段的引用或将其存入任务 arena。
  只借用一个字段仍物化完整根对象，沿用 64 KiB 存储限额，不为每个字段单独分配。

### 跨挂起保留对象

```joky
extern fn wait_tick() @suspends
struct SkillState { damage: Int32 }
fn prepare() -> &mut SkillState { alloc(SkillState { damage: 41 }) }
fn execute_skill() @suspends -> Int32 {
    let state = prepare()
    wait_tick()
    state.damage = state.damage + 1
    state.damage
}
```

宿主通过 `VmEngine::call_async` 启动任务，再调用 continuation 的 `resume`；
使用内置定时器、主动让出、登记的 I/O 或外部唤醒的标量任务可交给
`runtime::driver::block_on`，优先传入 `prepare_async` 的未轮询根；CLI 也支持挂起 main。
等待期间及恢复后使用同一对象；多次 Pending 不重放 prepare、分配或实参副作用。
同一调用链的同步 / 挂起 helper 共享任务 arena，子函数返回引用不会使对象失效。
模块重载或 VM 丢弃不影响仍存活的旧 continuation；其代码与 arena 一起保活到任务终止。
上例没有把 state 传给宿主；宿主保留对象地址须使用独立的异步宿主借用 v1，不能使用旧 CPS 注册。

### parallel 分支借用

`race` 沿用本节的参数和借用冲突规则；它的普通分支借用在获胜分支完成、其余分支取消后结束。
分支实际保留的任务期存储借用不因取消提前结束。

```joky
struct Counter { value: Int32 }
extern fn wait_tick() @suspends
fn increase(counter: &mut Counter) @suspends {
    wait_tick()
    counter.value = counter.value + 1
}
fn update_pair() @suspends -> Int32 {
    let left = alloc(Counter { value: 20 })
    let right = alloc(Counter { value: 20 })
    parallel { | increase(left) | increase(right) }
    left.value + right.value
}
```

- 分支可以共享读取同一对象，也可以像上例一样独占修改不同对象；同一对象的独占/独占或独占/共享
  分支会在编译时被拒绝，即使运行时是单线程。`parallel { | increase(left) | increase(left) }` 不合法。
- 参数按分支顺序从左到右求值，全部准备好后才开始分支。先前参数的借用在后续参数求值和挂起时
  已经生效；不能在后续参数中冲突地读写或移动其对象。
- 普通分支借用持续到整个 parallel 的所有分支完成，包括多次 Pending；单个分支提前完成不缩短借用。
  join 后可恢复原引用的权限，如上例读取 left/right；含共享引用的聚合、泛型和嵌套 parallel 使用相同规则。
- 如果分支具有分配能力（包括间接调用和嵌套 parallel），沿用任务期存储契约：全部引用实参的借用
  保守保留到根任务结束，包含独占实参及返回 Unit 的分支；不能仅因 parallel 已完成就恢复冲突访问。
- 所有分支共享根任务的 arena 和 64 MiB 计费预算。分支可自行分配，但分支或 parallel 结束不回收这些对象；
  根任务成功、失败或取消时统一释放。外部异步保留地址另受宿主借用 v1 的停止协议约束。

## 共享引用字段与只读对象图

记录、枚举和元组可以保存共享引用。允许经过 `&` 的递归类型；对象仍由当前任务统一释放：

```joky
struct Node { value: Int32, next: Option<&Node> }
fn push(head: Option<&Node>, value: Int32) -> &Node {
    (&*(alloc(Node { value: value, next: head })))
}
fn sum(head: Option<&Node>) -> Int32 {
    match head {
        None => 0
        Some(node) => node.value + sum(node.next)
    }
}
fn run() -> Int32 {
    let a = push(None, 20)
    let b = push(Some(a), 22)
    sum(Some(b)) // 42
}
```

- `Option<&Node>`、`Result<&Node, Int32>`、引用元组及参数化记录均可用；复制、解构、match、
  泛型函数、固有方法和跨模块调用都会保留借用依赖。`None` 的未选载荷没有实际对象来源。
- 含引用对象的 `alloc` 结果是**构造期** `&mut T`，可在当前函数内填充或替换字段。
  首次 `&*p` 或隐式收窄为 `&T` 时立即封存，原独占能力失效；共享视图最后使用后也不恢复。
  封存覆盖整个对象，包括非引用字段。无引用的 Point 继续遵循上一节的临时借用规则。
- 构造期 `&mut T`（T 含引用）不能作为函数参数或返回值；先封存，再传递 `&T`。
  该限制同样检查具体泛型实例。当前没有独立的 seal 函数，`&*p` 就完成这一步。
- 引用保存到任务 arena 对象后，其借用保留至任务结束，即使存储结果被丢弃、字段被覆盖，或变量已经不用。
  仅保存在普通局部 Copy 聚合里的引用则仍按最后使用结束；两者按实际存储操作区分。
- 调用可分配的函数时，所有含普通引用的实参都按可能被保存处理，包括 Unit 返回的函数和间接调用链。
  分配能力随调用图传播，不根据函数是否恰好保存某个参数来放宽规则；只读、无分配 helper 不延长到任务结束。
  返回依赖仍按签名合并。存入任务对象的引用必须来自当前隐式任务 arena；局部引用和显式子 arena 引用被拒绝。
  显式 arena 的存储及提前结束借用规则见[同步显式 arena](#同步显式-arena)。
- 当前可构造链表、树及共享子节点的无环图；自环和循环共享图使用下一节的受检构图块。
  不允许独占引用字段或可变循环图。直接内联递归布局仍拒绝。

示例见 [arena_graphs.jk](../examples/arena_graphs.jk)，三后端均输出 42、42。
可变双向链表见下一节；共享只读对象图仍使用上述引用规则和 graph 构造契约。

## 可变 List

显式导入 `joky/list.{type List}` 使用 `List<T>`；未加入默认 prelude。
容器是非 Copy 拥有者，作用域退出自动清理，也可消费式 release。方法和模块函数共用
[list.jk](../src/stdlib/list.jk) 的算法；构造方法写 `List.new::<T>()`、`List.with_capacity::<T>(n)`。

T 沿用 Array 的存储资格：Copy 数据、普通共享引用及含共享引用的 Copy 聚合，
以及 String、Drop 记录和递归无引用的拥有型聚合、嵌套容器。不需要 Eq / Hash 协议。
栈引用、独占引用和含引用的拥有型聚合仍不能作为元素存储；移出共享引用后保留其实际来源。

所有 index 均为 UInt64，表示从首节点起的零基逻辑位置。插入或删除会改变后续位置；
位置不能作为稳定节点身份。内部三个数组保存紧凑载荷、反向索引及双向连接，字段与槽号均私有。
搬迁载荷不改变逻辑顺序，不提供公开节点句柄、稳定地址或连续逻辑切片。

| 方法（模块函数增加首个 List 参数） | 行为 |
| --- | --- |
| `new()` / `with_capacity(n)` | 空表；预留至少 n 个活动元素的容量 |
| `len()` / `capacity()` / `is_empty()` | 活动长度、三组数组容量的最小值、是否为空 |
| `push_front(value)` / `push_back(value)` | 消费 value，加入相应端点 |
| `pop_front()` / `pop_back()` | 移出相应端点的 Option<T>；空表返回 None |
| `insert(index, value) -> Bool` | 支持 index == len；越界返回 false，并消费、清理 value |
| `remove(index) -> Option<T>` | 命中则移出；越界返回 None，原表不变 |
| `replace(index, value) -> Option<T>` | 命中移出旧值并存入新值；越界返回 None，并清理 value |
| `get(index)` / `front()` / `back()` | Copy T 的 Option<T> 读取；拥有型 T 编译拒绝 |
| `borrow(index)` / `borrow_mut(index)` | Copy T 的共享/独占 Element 视图，可用 element.read/write；支持边界沿用 Array |
| `borrow_ref(index)` / `borrow_mut_ref(index)` | 无引用 Copy 或拥有型 T 的 &T / &mut T，沿用 Array 的边界 |
| `reverse()` | 翻转连接及首尾；不重排、复制或析构元素 |
| `reserve(additional)` | 检查 len + additional，使三组容量满足目标；空闲槽计入可复用空间 |
| `clear()` / `release()` | clear 清理全部元素及连接、保留容量；release 消费整个容器 |
| `List.from_array(values)` / `lists.from_array(values)` | 消费 Array<T>，保持数组顺序 |
| `extend(values)` | 消费 Array<T>，按输入顺序追加至尾部 |
| `into_array()` | 消费 List，按链的逻辑顺序移出为 Array<T> |
| `append(&mut other)` | 按 other 的链顺序移入尾部；正常返回后 other 为空且保留容量 |

借用越界产生带源码调用位置的终止性 RuntimeError。元素视图约束整个 List；
存活期间拒绝冲突增删、替换、扩容、反转、移动与释放，最后使用后恢复权限。
append 的两参数不能别名同一 List。拥有型元素可通过直接引用借用，替换整个拥有值用 mem.replace；
不允许从借用记录中移走拥有字段。接口、泛型包装及模块别名保留这些来源和权限规则。

首尾增删及首尾 Copy 查询的节点操作为 O(1)，插入可能扩容；按位置定位从较近的一端开始，
为 O(min(index, n - 1 - index))。reverse、clear 和批量转移为线性节点/元素操作，
append 的节点转移次数对源元素数量线性；扩容还可能搬迁目标中的既有载荷。
复杂度不包含分配器、用户 Drop 和运行时清理快照成本。
反复按位置 get 遍历可能为 O(n²)；消费式遍历可以反复 pop_front 或使用 into_array。
删除复用空闲槽，存储可能反映历史峰值，不提供 shrink 或槽位压缩。

分配、移出后失败和 Drop 失败都按各数组及局部拥有值完成清理，保留首个错误，
不依赖活动链仍完整；不承诺操作失败后的可继续执行状态或按逻辑次序运行 Drop。
无引用元素的 List 及其元素视图可跨语言挂起，取消和旧代码重载继续遵循任务清理规则；
容器宿主 ABI 尚未开放。
示例见 [lists.jk](../examples/lists.jk)，三后端输出 last、0、last、before、extra、tail。
阶段证据见 [L1–L4 验收记录](list-set-storage-ownership.md#52-l1l4-验收记录)。

## 无序 Set

显式导入 `joky/set.{type Set}`；未加入默认 prelude。Set 是非 Copy 拥有者，
由私有 `Map<T, Unit>` 保存元素；模块函数和方法共用 [set.jk](../src/stdlib/set.jk) 的算法。
T 沿用 Map key 的存储准入：递归无引用的 Copy 值、String、拥有型记录或枚举；
构造不要求相等/哈希协议，插入、查询、批量导入、关系及集合运算要求 `BorrowEq + BorrowHash`。
旧 Copy `Eq + Hash` 通过 blanket impl 适配。相等元素须具有相同哈希，驻留期间协议结果须稳定。

| 方法（模块函数增加首个 Set 参数） | 行为 |
| --- | --- |
| `Set.new::<T>()` / `Set.with_capacity::<T>(n)` | 空集合；至少容纳 n 个不同元素 |
| `len()` / `capacity()` / `is_empty()` | 不同元素数、容量及是否为空 |
| `reserve(additional)` | 保证 len + additional 的空间，沿用 Map 的容量及删除标记整理规则 |
| `insert(value: T) -> Bool` | 消费 value；新增 true，重复 false，保留原实例并清理新传入实例 |
| `contains(value: &T) -> Bool` | 借用查询；不消费或克隆查询元素 |
| `remove(value: &T) -> Bool` | 命中则清理驻留元素并返回 true，查询元素仍归调用者；未命中 false |
| `pop() -> Option<T>` | 移出任意元素，空集合 None；不调用 Hash/Eq |
| `elements() -> &Slice<T>` | 全部元素的只读视图，不提供可变元素接口 |
| `clear()` / `release()` | clear 清理内容、保留容量；release 消费拥有者并回收存储 |
| `Set.from_array(values)` / `extend(values)` | 消费 Array<T>，按输入次序处理，保留首次驻留实例 |
| `into_array() -> Array<T>` | 消费集合并移出所有元素，输出顺序未指定 |
| `is_subset(other: &Self)` / `is_disjoint(other: &Self)` | 只读查询，允许与自身比较 |
| `union(other: Self) -> Self` | 消费两边，重复元素保留左侧实例 |
| `intersection(other: &Self) -> Self` | 消费左侧，保留右侧也包含的左侧元素；右侧不变 |
| `difference(other: &Self) -> Self` | 消费左侧，保留右侧不包含的左侧元素；右侧不变 |
| `symmetric_difference(other: Self) -> Self` | 消费两边，保留仅属一边的元素，重复双方实例均清理 |

contains/remove 的 `@noescape(value)` 契约经过方法、模块函数和用户包装保持；
适用时可直接传入拥有值表达式，由调用隐式借用。查询结束后可替换或释放查询 String。
pop 与导出转移元素所有权；移出的拥有值在容器释放后仍可用。

元素视图约束整个 Set；最后使用前阻止冲突修改、移动和释放，不能逃逸局部容器；无引用元素视图可跨语言挂起。
拥有元素用 slice.borrow_ref 读取，Copy 元素也可用 slice.get；驻留元素不可变借用。
消费式运算不能同时消费或借用同一个拥有者；交集/差集结果不依赖右侧借用。

Set 不保证遍历、导出或 Drop 次序，也不提供稳定元素地址、节点句柄或查询式移出 take。
复用 Map 的哈希探测、重建与失败边界；均匀哈希时关系查询和集合运算为线性期望成本，
极端碰撞沿用 Map 的最坏成本。批量与集合运算可能分配、同时持有输入和输出，不承诺原地零分配。
分配、容量上限或 Hash/Eq/Drop 失败均为终止性错误，保留首错并完成清理，不保证失败后恢复容器。
实现仅调用公开 Map/Array 接口，没有新增 ABI 或存储原语。

示例见 [sets.jk](../examples/sets.jk)，三后端输出 2、true、3、true、true、3；
验证范围见 [S1–S3 与联合验收记录](list-set-storage-ownership.md#53-s1s3-与-ls1-验收记录)。

## 无序 Map

显式导入 `joky/map.{type Map}` 使用 `Map<K, V>`；容器是非 Copy 拥有者，
作用域退出自动清理，也可消费式 `release`。实现全部在 [map.jk](../src/stdlib/map.jk)，
复用 Array 的受检存储和所有权规则；字段、桶类型和算法 helper 均私有。

K 必须可存储且递归无引用，支持 Copy 数据、String 和无引用拥有型记录/枚举。
构造与 insert 实例化时检查真实存储资格；协议本身不会赋予存储权限。
insert、from_entries、extend 与借用查询需要 `K: BorrowEq + BorrowHash`；原有按值查询
保留 `K: Eq + Hash`，通过源码 blanket impl 接入同一借用查找算法，自定义 Copy key 无需迁移。
空表构造、长度、容量、reserve、clear、release、切片视图、pop 和 into_entries 不要求协议。
直接/嵌套引用 key 被拒绝；元组 key 可先用名义记录包装。
V 支持普通 Copy 数据、普通共享引用及含共享引用的 Copy 记录/枚举/元组，
以及 String 和无引用拥有型记录/枚举（含 StringBuilder、Array、Map、拥有型 Option/元组）。
共享引用存入后沿用 Array 的任务期来源保留；栈引用、独占引用和含引用的拥有型聚合不能存入。

```joky
import joky/map.{type Map}
fn example() -> Int32 {
    var scores = Map.new::<Int32, Int32>()
    scores.insert(1, 20)
    let old = scores.insert(1, 22)
    old.unwrap_or(0) + scores.remove(1).unwrap_or(0)
}
```

模块函数与方法复用同一算法。模块函数使用完整 `<K, V>` 参数（可由普通实参推导）；
构造方法写 `Map.new::<K, V>()` 或 `Map.with_capacity::<K, V>(n)`，其他实例方法自动借用接收者。

| 模块函数（省略共同类型参数） | 方法与行为 |
| --- | --- |
| `new() -> Map<K, V>` | `Map.new::<K, V>()`；长度/容量为零 |
| `with_capacity(n: UInt64) -> Map<K, V>` | 至少容纳 n 项 |
| `len(&Map) -> UInt64` / `is_empty(&Map) -> Bool` | entry 数与是否为空 |
| `capacity(&Map) -> UInt64` | 桶负载和全部载荷容量共同允许的 entry 数 |
| `contains_key(&Map, key: K) -> Bool` | 是否存在 |
| `get(&Map, key: K) -> Option<V>` | 仅 Copy V；未命中 None，拥有型 V 拒绝复制读取 |
| `insert(&mut Map, key: K, value: V) -> Option<V>` | 新增 None；替换保留原 key，返回 Some(old)，旧值归调用方 |
| `remove(&mut Map, key: K) -> Option<V>` | 命中移出 value；未命中 None；内部顺序可改变 |
| `borrow(&Map, key: K) -> &Element<V>` | Copy V 的只读元素视图；用 `element.read` 读取 |
| `borrow_mut(&mut Map, key: K) -> &mut Element<V>` | Copy V 的独占元素视图；用 `element.read/write` 读写 |
| `borrow_ref(&Map, key: K) -> &V` | 无引用 Copy 或拥有型 V 的共享引用，可读取字段或访问嵌套容器 |
| `borrow_mut_ref(&mut Map, key: K) -> &mut V` | 无引用 Copy 或拥有型 V 的独占引用；Copy 用 `*p = next`，拥有者可用 `mem.replace` 返回旧值 |
| `iter(&Map) -> &Iter<K, V>` / `iter_mut(&mut Map) -> &mut Iter<K, V>` | 成对借用游标；for 每轮绑定 Entry 视图，键始终只读 |
| `keys(&Map) -> &Slice<K>` | 全部 key 的只读切片，不提供 keys_mut |
| `values(&Map) -> &Slice<V>` / `values_mut(&mut Map) -> &mut Slice<V>` | 全部 value 的只读/独占切片 |
| `pop(&mut Map) -> Option<(K, V)>` | 移出任意一项；空表 None，不调用 Hash/Eq |
| `into_entries(Map) -> Array<(K, V)>` | 消费 Map，逐项移出，导出顺序未指定 |
| `from_entries(Array<(K, V)>) -> Map<K, V>` | `Map.from_entries(entries)`；消费输入并构造 Map |
| `extend(&mut Map, Array<(K, V)>)` | 消费输入并合并，重复 key 保留原 key、最后 value 生效 |
| `reserve(&mut Map, additional: UInt64)` | 保证 len + additional 的 entry 空间 |
| `clear(&mut Map)` | 清理内容，保留容量 |
| `release(Map)` | 消费拥有者，回收存储及任务预算 |

insert 消费 key；重复时保留驻留旧 key，清理新 key，并返回旧 value。remove 清理被移除 key；
pop/into_entries 移交 key 和 value，clear/release 清理两者。
String 与其他拥有型 key 使用下列借用入口，不消费、不 clone 查询 key；首版只支持相同 K。

| 借用 key 入口（模块函数和方法同名） | 行为 |
| --- | --- |
| `contains_key_by_ref(&Map, &K) -> Bool` | 是否存在 |
| `get_by_ref(&Map, &K) -> Option<V>` | Copy V 查询，未命中 None |
| `remove_by_ref(&mut Map, &K) -> Option<V>` | 清理驻留 key，移出 value |
| `value_ref(&Map, &K) -> &V` | 无引用 Copy 或拥有型 V 的共享借用 |
| `value_mut(&mut Map, &K) -> &mut V` | 无引用 Copy 或拥有型 V 的独占借用 |

value_ref/value_mut 未命中时报错，返回借用依赖整个 Map；`@noescape(key)` 保证查询 key
不成为返回引用的来源，因此调用结束后可释放查询 String。返回的拥有值可在 Map 释放后继续使用。
替换已有 key 不增长、不重建，不承诺拥有值清理快照零临时分配。
成功 reserve 后，在没有删除等中间修改时，接下来 additional 次新增无需分配容器缓冲区；
这不包含用户协议自己的分配或后端清理快照。删除标记可能使后续新增在 len < capacity 时重建索引，
同时保留新旧索引的峰值内存仍计入预算。
Hash/Eq 在相关查找完成、修改开始前调用；重建只用缓存哈希。
每次探测有桶数上限；协议违规可能查不到或重复逻辑 key，不能产生无界探测。
容量加法、布局上限或桶倍增失败产生 `Map capacity exceeds the supported limit` 终止性错误；
分配失败和用户 Hash/Eq/Drop 失败沿用首错保留及完整清理，不承诺错误后继续使用旧 Map。
Map 无稳定顺序、元素地址或业务可依赖的析构顺序。

四种元素借用方法需要 `K: Eq + Hash`，未命中产生 `Map key not found` 终止性错误；
保留标准库检查位置及实际 Map 调用位置。解释器提供调用轨迹，JIT 提供源码调用点，
独立 AOT 程序输出来源 ID 和字节范围。查询 Hash/Eq 失败同样传播并清理拥有者。
无引用 Copy V 可使用 Element 或直接引用接口；含普通共享引用的 Copy V 使用 Element。
拥有型 V 使用 borrow_ref/borrow_mut_ref 或 value_ref/value_mut。

普通 `&Map` / `&mut Map`、元素视图及切片均依赖整个 Map；共享视图允许其他共享读取，
独占视图阻止冲突读取与写入，活动视图阻止结构修改、移动和释放；最后一次使用后恢复权限。
不支持不同 key 的同时独占借用，也不能把这些视图放入 Option/记录/元组或交给宿主。
无引用元素视图可跨语言挂起；helper、泛型函数、别名及跨模块返回视图沿用同一来源规则。
`get`、`remove`、`insert` 返回的旧值及 `element.read` 复制出的普通共享引用依赖实际内容来源，
不依赖 Map 的存储；Map 移动/释放后仍可读取，原来源的冲突修改直到任务结束仍被拒绝。

keys/values 的相同下标在借用期间对应同一项；顺序未指定，不承诺插入顺序或稳定遍历顺序。
无引用元素可使用 `slice.iter(table.keys())`、`slice.iter(table.values())` 或
`slice.iter_mut(table.values_mut())` 配合 for 遍历，见下文 [Slice 与 Map 单列遍历](#slice-与-map-单列遍历)。
Copy value 也可用 get/set 或 Element；含引用的 Copy value 仍使用这些接口配合 while。
拥有型 value 用 borrow_ref/borrow_mut_ref，可原地修改或 mem.replace，不能用 get/set 复制读取或替换。
切片使用既有视图头分配及预算规则，不复制载荷。没有公开桶下标或可变 key 切片；
Map.iter/iter_mut 提供独立的成对借用游标，见 [Map 成对借用遍历](#map-成对借用遍历)；
Map 本身不实现普通 Iterator 协议。

from_entries/extend 按原输入顺序处理；每次替换的旧 value 在处理下一项之前清理。
它们以输入长度为 reserve 上界，即使大量 key 重复也不承诺只按最终大小分配；
容量运算受检，分配、协议或析构失败按终止性错误清理 Map、当前项及剩余输入。
into_entries 消费 Map 并移动各项到新数组；pop/导出得到的拥有值和普通共享引用可继续使用。
输入数组、已导出的 Map 均不可复用；普通共享引用的实际来源保留沿用 Array 规则。
无引用存储树的 Map 拥有者与借用视图可跨语言挂起；含引用元素仍受既有挂起资格限制。
Map 不能隐式复制或部分移出字段。示例 [maps.jk](../examples/maps.jk) 与 [map_batches.jk](../examples/map_batches.jk)
覆盖频次统计、自定义 key、嵌套拥有值、借用修改、共享引用及批量导入/遍历/导出；
两个示例在三个后端合计输出六行 42。

## key 协议

显式导入 `joky/eq.{type Eq}`、`joky/hash.{type Hash}` 可使用普通静态 trait：

```joky
pub trait Eq { fn eq(self, other: Self) -> Bool }
pub trait Hash { fn hash(self) -> UInt64 }
```

标准实现覆盖 Int32、Int64、UInt32、UInt64、Bool、Unit；无引用 Copy 记录/枚举可自行实现。
Eq 必须是等价关系，相等 key 必须有相同 hash，存储期间结果应保持稳定。
这些 trait 本身不改变 Copy 或引用分类；需要无引用 Copy 存储的容器还会检查真实类型资格。
整数使用 UInt64 wrapping 混合，Int32 符号扩展、UInt32 零扩展；Bool 映射 0/1、Unit 映射 0。
hash 输出不作为持久化格式，也不承诺抵抗碰撞攻击。
泛型可写 `K: Eq + Hash`，模块/成员别名、条件 impl 和固定 trait 方法选择沿用普通语言规则。
实现见 [eq.jk](../src/stdlib/eq.jk)、[hash.jk](../src/stdlib/hash.jk)。

借用协议位于相同模块，均为普通静态 trait：

```joky
pub trait BorrowEq { fn eq_ref(&self, other: &Self) @noescape -> Bool }
pub trait BorrowHash { fn hash_ref(&self) @noescape -> UInt64 }
```

标准库提供 `impl<T: Eq> BorrowEq for T` 与 `impl<T: Hash> BorrowHash for T`，
适配时只读取 Copy 值；拥有型 key 应直接实现借用协议。String 已实现 UTF-8 字节相等和
64 位 FNV-1a wrapping hash；不做规范化，不承诺抗碰撞或持久化格式。
实现自动继承 trait 的 noescape 约束，编译器检查返回、存储及转交调用的引用来源。
同一 key 的相等和 hash 必须稳定，代数一致性由实现者保证。


## 数组字面值

`[a, b, c]` 从左到右求值各元素一次，允许尾随逗号；所有元素必须同型。
局部注解、形参、返回类型、聚合字段和构造器载荷提供上下文：

```joky
import joky/array.{type Array}
import joky/fixed_array.{type FixedArray}

var dynamic = [1, 2, 3]                    // Array<Int32>
dynamic.push(4)
let bytes: Array<UInt8> = [1, 255]
let fixed: FixedArray<Int32, 3> = [1, 2, 3]
let empty: Array<String> = []
let matrix: FixedArray<FixedArray<Int32, 2>, 2> = [[1, 2], [3, 4]]
```

无容器上下文时选择 `Array<T>`；先从元素的已有类型推导 T，再使用数值字面量默认类型。
空数组必须能从上下文确定 T，不根据后续 push 反推。固长上下文检查字面值项数等于 N，
固长形参中的常量参数也可由字面值项数推导。`let` / `var` 只控制绑定可变性，不选择容器类型。
已经定型的 Array 与 FixedArray 不隐式互转；也没有隐式复制拥有元素或数值类型转换。

字面值通过编译器私有别名引入数组库，不需要 prelude，也不向用户作用域引入 `Array`、
`array` 等名称；书写类型注解仍需普通 import。动态构造逐项移动拥有值，必要时扩容，
中途返回、失败和取消清理已初始化内容；元素表达式可按普通规则挂起，恢复不重放副作用。
固长构造复用受检初始化令牌，仅在完整初始化后发布值，保留原有 Copy 元素和同步使用限制。
数组项中的 `..expr` 展开一个序列，可在任意位置出现多次：

```joky
let tail = [2, 3]
let values = [1, ..tail, 4]                // 消费 tail，得到 [1, 2, 3, 4]
let copy = [..&values]                    // 复制 Copy 元素，values 仍可用
let fixed: FixedArray<Int32, 2> = [20, 22]
let joined: FixedArray<Int32, 5> = [..fixed, 0, ..&fixed]
```

| 展开表达式的类型 | 行为 |
| --- | --- |
| `Array<T>` | 消费数组并移动元素；即使 T 是 Copy 或数组为空，源绑定也不可再用 |
| `FixedArray<T, N>` | 复制元素，源绑定仍可用 |
| `&Array<T>`、`&FixedArray<T, N>`、`&Slice<T>` 及其独占引用 | 仅允许 Copy 元素；独占引用按重借用处理 |

展开不会隐式 clone；例如 `[..&names]` 在 names 为 `Array<String>` 时被拒绝。
展开源表达式只求值一次，复制或移动完它的全部元素后才求值下一项，整体保留从左到右的顺序。
失败、提前返回或取消均清理部分构造的目标和尚未转移的源元素，拥有值不会重复析构。
默认目标仍是 Array；FixedArray 目标要求每个展开源都是具有具体长度的 FixedArray 或其引用，
展开后总长度必须等于目标长度，也可用于推导固长形参的常量参数。
Array 和 Slice 的运行期长度不能参与固长构造，即使源写作 `..[1, 2]` 也不例外。
借用展开保留所复制引用的来源约束；复制容器不会延长被引用对象的生命周期。

重复构造 `[x; n]` 尚未实现；下标访问见下节。

## 数组与切片下标

`Array<T>`、`FixedArray<T, N>` 及其引用、`&Slice<T>` / `&mut Slice<T>` 支持 `values[index]`。
下标为 `UInt64`；整数常量按上下文定型，负数、浮点、Bool 和其他已定型整数不隐式转换。
每次访问均检查初始化长度，切片下标相对当前切片。越界终止当前任务并报告下标表达式的源码范围，
包括空数组和零大小元素；JIT/解释器 CLI 展示文件与行，独立 AOT 程序沿用源码字节范围诊断。
需要显式处理越界时使用 `get` 等返回 Option 的 API。

```joky
var values = [20, 22]
let sum = values[0] + values[1]
values[0] = values[1] + 20
values[1] += 1
let item = &mut values[0]
*item = 42
```

- 值位置的 `values[i]` 复制 Copy 元素，包括固长数组；拥有元素不能隐式移出，
  使用 `&values[i]` 读取，或用 `remove` / `pop` 取得所有权。借用接收者的方法可自动借用下标位置。
- `&values[i]` / `&mut values[i]` 返回元素引用；独占访问要求可变数组绑定或独占容器引用。
  动态下标不建立不重叠证明，即使下标是不同常量，两个独占元素借用也不能同时存活。
  元素借用仍阻止冲突写入、扩容、移动及释放原数组，不允许逃逸局部存储；无引用目标可跨语言挂起。
- `values[i] = next` 返回 Unit，替换 Copy 或拥有元素。拥有替换转交新值并清理旧值；
  RHS 失败或提前返回时保留旧值的清理责任，Drop 失败及取消也不重复释放或遗失新值。
- 数组表达式、下标、右值从左到右各求值一次；边界检查在右值之前。
  赋值预留目标位置，允许右侧共享读取，禁止冲突修改或使存储失效。
  `+=` 等复合赋值先保存旧元素再求值右侧，沿用普通数值运算和溢出检查。
  元素地址在赋值右侧及其挂起期间保持有效；恢复后写回同一位置。
- 支持嵌套下标、元素字段和临时数组的 Copy 读取，例如 `matrix[0][1]`、`points[i].x`、`[42][0]`。
  左方括号不隐式续接上一行；下标内部可换行，仍只接受一个索引表达式，不支持区间索引。
- 含共享引用的 Copy 元素可读写；复制出的引用保留原始对象来源，数组释放不撤销它。
  不开放对含引用元素存储的显式借用，避免暴露可修改引用来源的元素视图。

完整行为和清理回归见 [indexing.jk](../tests/fixtures/indexing.jk) 与
[indexing_cleanup.jk](../tests/fixtures/indexing_cleanup.jk)。

## 动态数组

显式导入 `joky/array` 后可使用 `Array<T>`。元素支持八种定宽整数、Float32 / Float64、Rune、Bool、Unit、Ptr，
普通共享引用，以及由这些类型组成的 Copy 记录、元组和枚举；还支持 String 与递归无引用的拥有型聚合和嵌套容器；
长度、容量和索引为 UInt64；方法调用和下标访问均不需要 unsafe 指针操作。
数组不加入 prelude，关闭默认 prelude 后也可单独导入使用。
构造函数写 `Array.new::<T>()` / `Array.with_capacity::<T>(n)`，实例支持 `values.push(x)`、`values.get(i)` 等方法。
原有 `array.new::<T>()` / `array.push(&mut values, x)` 等模块函数保持兼容，两种调用风格可混用。
`Array.new` 等泛型函数不从返回注解或后续写入反推元素类型；其他方法从接收者推导 T。
数组字面值的上下文定型见上节。
零大小元素仍受逻辑长度/容量检查，但不分配元素载荷；数组自身始终不是 Copy。

```joky
import joky/array.{type Array}

fn answer() -> Int32 {
    var values: Array<Int32> = Array.new::<Int32>()
    values.push(20)
    let view = &values
    let first = view.get(0).unwrap_or(0)
    values.push(22) // view 最后使用后恢复修改权限。
    let moved = values       // 移动；之后不能再使用 values。
    let result = first + moved.get(1).unwrap_or(0)
    moved.release()     // 消费拥有者，立即回收数组存储；result 仍可使用。
    result
}
```

以下列出兼容的模块函数签名。除构造使用 `Array.new::<T>()` / `Array.with_capacity::<T>(n)` 外，
其余同名方法把首个数组参数作为接收者；`&self` / `&mut self` 自动借用，`release(self)` 消费拥有者。

| 函数 | 行为 |
| --- | --- |
| `new::<T>() -> Array<T>` | 新建长度和容量均为 0 的数组 |
| `with_capacity::<T>(UInt64) -> Array<T>` | 指定容量，长度仍为 0 |
| `release(Array<T>)` | 消费无活动借用的拥有者，立即释放当前载荷和控制块并退还任务预算 |
| `len(&Array<T>) -> UInt64` | 已初始化元素数 |
| `capacity(&Array<T>) -> UInt64` | 当前容量 |
| `is_empty(&Array<T>) -> Bool` | 是否没有元素 |
| `get(&Array<T>, UInt64) -> Option<T>` | 复制元素，越界返回 None |
| `set(&mut Array<T>, UInt64, T) -> Bool` | 替换已有元素，越界返回 false 且不修改数组 |
| `push(&mut Array<T>, T)` | 必要时扩容，追加元素 |
| `pop(&mut Array<T>) -> Option<T>` | 移出末元素；空数组返回 None，不缩小容量 |
| `into_iter(Array<T>) -> IntoIter<T>` / `values.into_iter()` | 消费数组，按原下标正序逐项移出 T；无需 Copy |
| `iter(&Array<T>) -> &Iter<T>` / `values.iter()` | 按正序借用为 &T，数组保留所有权 |
| `iter_mut(&mut Array<T>) -> &mut Iter<T>` / `values.iter_mut()` | 按正序独占借用为 &mut T，数组保留所有权 |
| `replace(&mut Array<T>, UInt64, T) -> Option<T>` | 替换有效下标并返回 Some(old)，长度、容量及其他元素不变；越界返回 None 并消费/清理 next |
| `swap_remove(&mut Array<T>, UInt64) -> Option<T>` | 移出指定元素，以末元素补位；成功时长度减少、容量不变且不保序；越界返回 None，不修改数组 |
| `remove(&mut Array<T>, UInt64) -> Option<T>` | 移出指定元素并保留其他元素顺序；越界返回 None，不修改数组；容量不变 |
| `insert(&mut Array<T>, UInt64, T) -> Bool` | 在 index <= len 处插入并保序，必要时扩容；index > len 返回 false，不修改数组 |
| `reserve(&mut Array<T>, UInt64)` | 参数是额外元素数；保证容量至少为 len + additional，长度不变 |
| `truncate(&mut Array<T>, UInt64)` | 保留指定长度的前缀，逆序析构尾部；长度不增加，容量不变 |
| `clear(&mut Array<T>)` | 清空所有元素，保留容量 |
| `swap(&mut Array<T>, UInt64, UInt64) -> Bool` | 交换两个有效位置，长度/容量不变；任一索引越界返回 false，不修改数组 |

- 拥有者不是 Copy；绑定、赋值、按值传参和返回均执行移动。`var` 可在移动后重新赋值，
  只有可变绑定能取 `&mut`；独占视图可在同步 helper 中继续重借用。
- `&Array<T>` 是可在局部复制的共享视图，`&mut Array<T>` 是独占视图。
  有效视图阻止冲突修改、移动及覆盖；视图最后使用后恢复权限。
  返回视图保留签名中引用输入的依赖，不能返回局部或按值参数拥有者的视图。
- get/pop 返回 T 的复制值，不借用数组控制块；含引用结果仍保留其指向对象的来源和权限。
  即使数组随后扩容、pop、移动或离开作用域，复制出的共享引用仍可读取。先读取所需元素，再传给 push；
  `array.push(&mut values, array.get(&values, 0).unwrap_or(0))` 因实参求值期间借用冲突而拒绝。
- 数组拥有者可存入下述拥有记录；视图仍不能存入任何聚合。两者均不能作为 Copy 泛型实参、进入 alloc/graph，
  也不能通过 *p / *p = next、Ptr 或宿主 ABI 访问控制块。
  元素存储树不含引用的 `Array<T>` 拥有者可以直接作为 `@suspends` 的参数、返回值和中间值；
  该能力也覆盖无引用拥有记录、枚举和元组（含自定义 Drop）。整个拥有对象的共享/独占引用
  可作为内部挂起参数并进入结构化并发；无引用元素的 Element、Slice 及直接元素引用同样支持，含引用元素仍受限制。
  挂起函数可调用内部使用数组的同步 helper。解释器内部时间片、取消、VM 重载及丢弃都保留正确存储寿命。
- 创建和扩容使用当前任务 arena。数组视图不可被任务期对象保存，因此即使 helper 分配，
  无借用返回值时也会在调用后归还数组权限；普通引用输入仍遵守原来的任务期保留规则。
- push/set 等存储不能保存局部栈借用；写入的共享引用保留至根任务结束，pop、覆盖及丢弃数组都不释放该借用。
  带有含引用元素的可写数组输入（拥有者或独占视图）的 helper 对普通引用实参施加这一契约，即使不分配或索引越界。
  规则按签名、泛型实例和规范化别名判定；不分析被调函数体来放宽条件。泛型推导不隐式收窄独占引用，使用 (&*(alloc(...))) 创建共享元素。
- 控制块、载荷及分配记录计入根任务的 64 MiB 预算。扩容迁移期间同时计费新旧存储，
  成功替换后立即回收旧载荷与临时源头部。`array.release(values)` 消费拥有者并立即释放当前载荷和头部、退还预算。
  release 前必须结束所有存储借用；调用后不能再次使用 values。复制出的共享元素引用仍可使用，已保留的共享借用仍持续到任务结束。
  pop 不缩容量；拥有者离开作用域或被覆盖时自动释放，移动转移清理责任；切片头部仍由根任务兜底回收。
  容量溢出、地址范围溢出和分配失败终止任务并报告运行时错误；get/set 越界是正常返回值。

replace 支持模块函数和 `values.replace(index, next)` 方法，Copy、拥有型及共享引用元素均沿用 Array 存储规则。
有效替换将旧值交给调用者，不析构 old 或 next，不增长或新建元素缓冲区；拥有参数清理快照仍可能需要临时内存，耗尽时终止并完整清理。
越界时原数组不变，拥有型 next 恰好清理一次。

swap_remove 同时支持 Copy 与拥有型元素，可写 `values.swap_remove(index)` 或
`array.swap_remove(&mut values, index)`；索引按调用实参顺序求值一次。
索引有效时返回原位置的完整元素并转移其清理责任，末元素移动到该位置，不运行元素 Drop；
删除末元素时无需补位。空数组、index >= len（包括 UInt64.MAX）返回 None，不改变元素、
长度或容量。复杂度相对于数组长度为 O(1)，存储交换工作与元素大小成正比。
最后使用完元素/切片视图后才允许删除；返回的共享内容引用沿用任务期保留契约。
算法和交换原语不分配，但拥有型返回值的局部存储或清理快照仍可能分配失败并终止任务。
示例见 [swap_remove.jk](../examples/swap_remove.jk)，三后端输出 20、22、2、1。

remove / insert 同时支持 Copy 与拥有型元素，方法与模块函数写法均可用。
`remove(index)` 将后续元素依次向前移动，返回被移出值，不运行元素 Drop；
空数组或 index >= len（包括 UInt64.MAX）返回 None，容量不变。
`insert(index, value)` 在 index <= len 时消费 value 并插入，index == len 等同追加。
它先完成扩容和追加，再把新元素向前交换到目标位置，保留原元素顺序。
index > len 时返回 false，不扩容、不修改数组，传入的拥有值仍会按参数退出规则析构；
该 Drop 若失败，任务失败，不能正常返回 false。所有实参从左到右求值一次，
即使索引无效也会求值 value；value 求值失败发生在插入之前。

两个保序操作相对于被移动的元素数为 O(n)，复用整元素交换，交换期间不分配或运行 Drop；
有效插入的扩容可能分配，拥有型返回值仍可能需要局部存储和清理快照。
数组元素/切片借用活跃时禁止这些操作，最后使用后恢复权限；内容引用仍沿用任务期保留契约。
不可恢复分配失败、Drop 失败及解释器内部取消沿用任务清理路径，所有已构造资源恰好清理一次。
示例见 [ordered_arrays.jk](../examples/ordered_arrays.jk)，三后端输出 21、22、20、0。

truncate / clear / swap 支持 Copy 与拥有型元素，提供同名方法和模块函数。
`truncate(length)` 只在 length < len 时删除尾部；length >= len（包括 UInt64.MAX）
不修改数组。删除按索引逆序运行 Drop 并清理子拥有者，容量及外层数组存储保留。
`clear()` 等价于 truncate(0)，清空后可继续 push/insert 并复用容量。
已删除元素拥有的子缓冲区会立即回收并退还预算，Copy 共享内容引用所指对象仍存活，
此前任务期保留的借用不会因清空解除。某个 Drop 失败仍继续清理本次删除的其他元素，
保留最早错误并终止任务；任务退出再清理保留的前缀，不重复清理已删除元素。
Copy 元素没有析构逻辑，截断只更新长度；拥有型元素的清理工作与删除数量及其子资源成正比。

`swap(first, second)` 在两个索引均小于 len 时交换完整元素并返回 true；
相同且有效的索引返回 true，无需移动。任一索引越界（包括空数组）返回 false，
长度、容量及元素均不变。交换不分配、不调用 Drop，不移出任何资源，
工作与元素大小成正比、与数组长度无关。两个索引仍按实参顺序各求值一次。

三种操作要求数组独占访问，活跃元素/切片视图阻止冲突操作，最后使用后恢复权限；
这些操作可在挂起函数中调用，无引用元素视图也可跨挂起保留。完整示例见
[array_management.jk](../examples/array_management.jk)，三后端输出 22、1、4、42、0。

共享引用元素示例：

```joky
import joky/array

fn answer() -> Int32 {
    var values = array.new::<&Int32>()
    array.push(&mut values, (&*(alloc(42))))
    let copied = array.get(&values, 0)
    array.pop(&mut values); // copied 借用原对象，不借用数组载荷。
    match copied { None => 0, Some(value) => (*value) }
}
```

### 拥有型数组元素

`Array<T>` 也支持 String 及无引用内容的拥有型记录、枚举、元组，包括 StringBuilder、数组本身、混合字段和多拥有字段记录。
`Array<String>`、`Array<StringBuilder>` 和嵌套 `Array<Array<String>>` 可用；元素的底层数组内容不能含普通共享引用。拥有型元素均整体移动，不能隐式复制或部分移出。

- `push` 消费实参；扩容先成功分配目标载荷，再整体搬迁已初始化前缀，不运行元素 Drop。
- `set` 成功时立即析构旧元素并移动新元素；越界返回 false，新实参仍按函数退出规则析构。
- `pop(&mut Array<T>) -> Option<T>` / `values.pop()` 支持拥有型元素，非空时转移末元素，空数组返回 None。
  `borrow_ref(&Array<T>, UInt64) -> &T` / `values.borrow_ref(index)` 支持拥有型元素的只读借用；
  `borrow_mut_ref(&mut Array<T>, UInt64) -> &mut T` / `values.borrow_mut_ref(index)` 提供独占借用。
  越界（包括空数组）产生带位置的运行时错误；不分配、复制或移出元素。
- `take_last(&mut Array<T>) -> T` / `values.take_last()` 整体移出末元素，保留容量；空数组产生带位置的运行时错误。
  Copy 数组也支持该方法；`get`、Element 视图和 Slice 的复制读写仅支持 Copy 元素。
- `borrow_ref` 返回资源本身的共享引用，可读取 Copy 字段、调用 `&self` 方法，并重借用内部拥有字段。
  多个共享借用可以同时读取；每个派生引用都保留整个原数组的借用依赖，不按索引或字段证明独立性。
  最后一次使用前，禁止扩容、push、set、pop、take_last、release、移动及覆盖原数组；之后恢复修改权限。
  同步 helper 可传递和返回来自借用输入的引用；不能返回局部或按值输入数组的元素引用。
  这两个直接引用接口接受拥有型元素及无引用 Copy 元素；含引用 Copy 元素继续使用 Element 读写。
- `borrow_mut_ref` 允许原地修改 Copy 字段、调用 `&mut self` 方法及重借用内部拥有字段，
  也可直接匹配枚举元素：`match values.borrow_mut_ref(index)`。
  引用保留整个原数组的独占借用，活跃时不能再借用另一个索引或修改原数组；最后使用后恢复权限。
  可通过 `mem.replace` 替换整个元素或其拥有字段并取得旧值；带 Drop 的记录仍禁止单独替换拥有字段。直接按值读取不能移出资源。
  String 元素只允许整体替换，不能修改文本字节；派生的 `bytes()` 视图继续借用整个原数组。
- `release`、覆盖、作用域退出和失败清理按索引逆序析构剩余元素。单个记录先运行自身 Drop，再按字段声明顺序清理子拥有者。
  某个 Drop 失败时继续清理其他元素，保留最早错误；失败替换的旧元素不重复析构，右侧新实参也会清理。
- 普通函数、方法、记录和透明别名可以使用拥有型泛型实参，特化后重新检查移动与借用；枚举和元组也支持拥有者。
  无引用元素的 `Array<T>` 拥有者、数组视图及元素资源引用可以进入普通 `@suspends` 调用链、
  `parallel`、`race` 和 `scope/branch`；含引用元素数组、显式 Arena 能力和容器宿主 ABI 仍受原有限制。
  元素资源引用不能存入聚合，不能通过 `*p` 移出资源；可通过 `*p = next` 替换整个资源，也不能通过共享引用取得独占访问。
  枚举元素支持 `match values.borrow_ref(index)`，载荷借用仍依赖整个原数组。

```joky
import joky/array.{type Array}
struct Resource { id: Int32, values: Array<Int32> }

fn inspect(values: &Array<Resource>) -> Int32 {
    let resource = values.borrow_ref(0)
    resource.id // 元素仍在数组中，由数组负责 Drop。
}
```

完整示例见 [owning_arrays.jk](../examples/owning_arrays.jk)，输出 42、20、22。

元素与切片借用使用独立的只可借用目标 `Element<T>`、`Slice<T>`，分别从 joky/element 和 joky/slice 导入：

```joky
import joky/array
import joky/element
import joky/slice

fn borrowed_answer() -> Int32 {
    var values = array.new::<Int32>()
    array.push(&mut values, 20)
    array.push(&mut values, 21)
    let region = array.slice_mut(&mut values, 0, 2)
    let item = slice.borrow_mut(region, 1)
    let old = element.read(item)
    element.write(item, old + 1)
    // item 最后使用后恢复 region，region 最后使用后恢复 values。
    let answer = slice.get(region, 0).unwrap_or(0) + slice.get(region, 1).unwrap_or(0)
    array.push(&mut values, 99)
    answer
}
```

- array.borrow / borrow_mut 返回 `&Element<T>` / `&mut Element<T>`；element.read 复制 T，element.write 替换 T。
- array.slice / slice_mut 返回 `&Slice<T>` / `&mut Slice<T>`。区间是半开 [start, end)，必须满足 start <= end <= len，允许空切片。
  借用构造越界会报告带位置的运行时错误；slice.get / set 的越界仍返回 None / false。
- slice 提供 len、is_empty、get、set、borrow、borrow_mut、slice、slice_mut；子切片索引相对当前切片。
  元素借用不分配；每次创建切片/子切片均分配一个计入任务预算的头部，不复制元素，并在根任务结束时回收。
- 普通 API 派生的视图借用整个原始缓冲区，不证明不同索引或区间互不重叠；
  序列模式在成功长度检查后可证明同一模式中的元素和余段不重叠。共享视图允许共享读；
  独占视图阻止其他冲突读写，子视图最后使用后恢复父权限。视图存活时不能冲突修改、扩容、pop、移动或覆盖拥有者。
- element.read / slice.get 的复制值不延长存储借用；复制出的普通共享引用仍保留元素内容来源。
  Element/Slice 不能按值使用，其视图继承数组视图的不可存储和宿主限制；无引用元素视图可跨挂起。
  可通过明确声明形状的泛型 helper 或跨模块别名传递，不能返回局部拥有者的视图。
- 取独占借用后求值的后续实参不能冲突读取原数组；先计算复制值，再调用 element.write / slice.set。
  含共享引用元素的写入沿用任务期存储契约，不能写入局部栈借用。

### 拥有型元素的切片借用

拥有型元素也支持 `Array.slice` / `slice_mut` 及 `slice.slice` / `slice_mut` 子切片，
沿用半开区间检查；索引相对当前切片，空切片可以继续创建空子切片。

- `slice.borrow_ref(&Slice<T>, UInt64) -> &T` /
  `slice.borrow_mut_ref(&mut Slice<T>, UInt64) -> &mut T` 接受拥有型及无引用 Copy 元素，
  越界产生带位置的运行时错误。可调用方法、借用字段、共享/独占匹配枚举，
  并通过 `mem.replace` 替换整个元素或其拥有字段，旧值交给调用者清理。
- `len` / `is_empty` 可用于拥有型切片；`get` / `set` 及 Element 接口
  继续仅支持 Copy 元素，不能从借用切片直接移出资源。
- 每层子切片和元素引用都保留整个原数组的借用依赖，不证明不同区间或索引独立。
  活跃子借用阻止冲突访问父视图；最后使用后恢复父权限，所有视图最后使用后恢复数组权限。
  切片不拥有元素，不运行元素 Drop；替换后的新元素仍由原数组负责清理。
- 同步泛型 helper 和跨模块别名可传递、返回来自借用输入的切片或元素引用；
  不能返回局部或按值输入数组的视图，不能存入聚合、arena 对象或宿主 ABI；无引用元素视图可传入挂起函数。
  每个切片头部计入任务预算，在根任务结束时回收。

完整示例见 [owning_slices.jk](../examples/owning_slices.jk)，三后端输出 21、42、3。

源码接口见 [element.jk](../src/stdlib/element.jk) 和 [slice.jk](../src/stdlib/slice.jk)。

实现见 [array.jk](../src/stdlib/array.jk)，示例 [arrays.jk](../examples/arrays.jk) 在三后端均输出 42。
push/pop 和扩容策略写在 Joky 中；初始化前缀的搬迁由受检 transfer 原语完成。`joky/internal/buffer` 仅供编译器绑定的标准库定义使用，
普通源码不能导入或重导出它，也不能通过同名文件或 extern 获得原语能力。
单个或多个拥有字段的记录及整体移动的拥有型数组元素已开放；无引用元素的 `Array<T>` 已可跨普通语言挂起，
但独占引用元素、数组视图元素及宿主 ABI 仍未开放；
详见[数组存储与所有权契约](array-storage-ownership.md)。

### 拥有记录与 Copy 元数据

Array 是名义结构体，唯一字段为私有的内部 Buffer；点调用来自普通 `impl<T> Array<T>`，不对数组方法做特殊解析。
用户也可定义具有一个或多个拥有字段和无引用 Copy 元数据的结构体，并继续逐层包装：

```joky
import joky/array.{type Array}

struct Batch<T> {
    version: UInt64
    values: Array<T>
    sorted: Bool
}
impl<T> Batch<T> {
    fn new() -> Self { Self { version: 0, values: Array.new::<T>(), sorted: true } }
    fn push(&mut self, value: T) {
        self.values.push(value)
        self.version = self.version + 1
        self.sorted = false
    }
    fn len(&self) -> UInt64 { self.values.len() }
    fn into_array(self) -> Array<T> { self.values }
    fn release(self) { self.values.release() }
}
```

- 记录可包含一个或多个拥有字段和任意数量无引用 Copy 字段（包括 Unit），拥有字段不必位于首位。
  各拥有字段可有不同的元素类型，也可嵌套其他拥有记录。记录本身非 Copy；传参、返回和绑定整体移动全部拥有权。
  若所选字段的祖先记录均未实现 Drop，可移出该字段（包括多缓冲区子记录）；这会消费整个原记录，之后连兄弟字段和元数据也不能使用。
  元数据支持直接及经借用包装读取/赋值；只开放拥有字段重借用，暂不支持元数据字段借用。
  活跃字段视图借用整条拥有记录；var 可整体重新赋值，但不能通过单字段赋值复活已消费记录。
- 借用记录或其字段保守地依赖整条记录的底层缓冲区，沿用全部冲突检查和共享元素引用来源。
  从 `&Batch<T>` / `&mut Batch<T>` 可重借用拥有字段；独占字段借用可交给 `mem.replace`，直接按值读取仍被拒绝。
  模块内可写 `&self.values` / `&mut self.values`；同步 helper 可返回来自借用输入的字段视图。
- 字段默认私有，仅定义模块内可访问，适用于构造、读取、赋值和借用；显式 `pub` 字段在类型
  可见时可跨模块访问。模块内函数与 impl 均可访问自己的私有字段，跨模块泛型实例保留定义作用域。
  旧 `private` 修饰不再支持并报迁移诊断；`private: Int32` 仍是名为 private 的合法字段声明。
- 单字段包装链复用稳定缓冲区句柄；混合记录及其外层包装使用编译器私有的稳定局部存储。
  var 拥有记录可替换拥有字段：先求值右侧，再析构旧字段，成功后转入新值；其他字段保持有效。
  移出后的剩余字段仍在原记录作用域退出、失败或整体覆盖时自动清理；单一拥有者的显式 release 仍可提前释放。

多个拥有字段使用相同的 struct/impl 语法，无需额外标记：

```joky
struct Columns {
    names: Array<Int32>
    scores: Array<Int64>
    version: UInt64
}
fn columns() -> Columns {
    Columns { scores: Array.new::<Int64>(), version: 1, names: Array.new::<Int32>() }
}
```

字段按构造表达式顺序初始化，完整记录按字段声明顺序析构；构造中途退出只清理已初始化字段。

```joky
fn take_scores() -> Array<Int64> {
    let value = columns()
    value.scores  // 返回该字段；value 不再可用，names 在此作用域结束时清理
}
fn replace_scores() -> UInt64 {
    var value = columns()
    value.scores = Array.new::<Int64>()  // 清理旧 scores，保留 names 和 version
    value.version
}
```

嵌套路径同样适用，但路径上任何祖先实现 Drop 时仍禁止移出或替换；所选字段本身可以实现 Drop。
拥有局部字段可直接赋值；显式解引用字段 `(*record).field = next` 也可替换拥有字段。需要返回旧值时使用 `mem.replace(&mut record.field, next)`。右侧不能消费目标记录后再用字段赋值复活它。

## 受检循环图构造

`graph { ... }` 使用当前任务的隐式 arena，完成后统一封存，并返回所选根节点的 `&T`：

```joky
struct Node { value: Int32, next: Option<&Node> }
fn make_cycle() -> &Node {
    graph {
        let a = graph.alloc(Node { value: 20, next: None })
        let b = graph.alloc(Node { value: 22, next: None })
        graph.link(a.next, b)
        graph.link(b.next, a)
        a
    }
}
```

- `let a = graph.alloc(value)` 创建**构造句柄**，不是可读取的普通引用。必须直接写在构图块顶层，
  使用无类型注解的 let；初始化值必须是完整的 Copy 记录，类型由初始化值决定。
  泛型记录可在初始化表达式中显式指定类型参数，graph.alloc 本身不接受类型实参。
- 句柄只能用于 graph.link 的端点或块末尾的根节点；不能读取字段、赋值、复制、装进普通值、
  &*p、作为普通函数/方法实参或带出构图块。普通数据在初始化前按现有表达式规则计算。
- `graph.link(a.next, b)` 把目标字段写为 Some(b)。两端都必须是当前组的句柄；
  目标必须是标准库 Option<&T>，T 与 b 的记录类型一致。别名和显式导入保持名义身份；
  自定义的同形枚举不算 Option。允许 `a.edges.next` 这样的内联记录路径，不允许穿过引用解引用。
- 支持自环、多节点环、异型节点、共享子节点和泛型节点。未连接的字段保留初始化值（例如 None）；
  同一字段可以连接多次，最后一次生效。初始化也可保存已有共享引用，但其借用仍保留至任务结束。
- 块末尾必须是本组的一个裸句柄名，不能带尾随分号；正常结束时撤销**所有节点**的构造能力，
  返回根的共享引用，无需也不提供 graph.seal。封存后整个组只读，包括数据字段及未选作根的节点。
- 首版 alloc/link 为直线构造操作：不能放进条件、循环或嵌套普通块；不支持嵌套 graph。
  可以在外部循环中反复执行完整的构图块，也可以在块内用 if/match/while 计算普通数据。
  return 或跳到块外循环的 break/continue 被拒绝；块内普通循环可正常 break/continue。
- graph 块不创建独立 arena，也不在退出时回收；包括未连接节点在内的所有分配随根任务统一释放。
  初始化、后续计算或分配失败走同一任务清理路径，不发布半成品。构图块本身必须同步，
  可以位于 `@suspends` 函数内，但块内不能调用挂起函数；封存后的图可跨后续挂起保留。
  不开放跨函数构造句柄、跨 arena 连接或运行中可变图。遍历环时仍需自行限定步数或记录访问状态。

`graph` 仅在表达式中接 `{` 时引入构图块。块内的 graph.alloc/graph.link 是构造操作；
块外已绑定的同名模块和普通值仍按原有规则解析。示例见
[arena_cycles.jk](../examples/arena_cycles.jk)，JIT、解释器和 AOT 均输出 42、42。

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
  泛型函数的完整显式实参也可独立产生函数值，如 `identity::<Int32>`。普通函数、整数转换内建函数、
  非泛型枚举构造及 VM 特殊调用不接受类型实参；`<`、`>`、`<<`、`>>` 仍按原运算符解析。
- 类型参数只在所属函数签名及函数体的类型注解内有效，可以进入元组。不同函数中的同名参数具有不同身份。
  参数遮蔽同名模块类型；不能把被遮蔽的模块记录或枚举当作该参数的构造函数。
- **普通泛型调用的类型实参允许 Copy 类型及拥有者**：支持的整数、Rune、Bool、Unit、函数值、共享引用、记录、枚举和元组，拥有者包括 String、StringBuilder 和数组。
  透明别名按目标类型处理；数组视图、独占引用和宿主 opaque 类型不能绑定到 T。
  无约束 T 按可移动值检查；重复使用、从共享引用按值读取等复制操作需要声明 `T: Copy`。
  缺失约束在模板定义时拒绝，不会等到 String 等拥有型实参出现才报错。
  独立泛型函数值使用相同规则，`identity::<String>` 合法。
  泛型函数可声明固定类型的 String / opaque 参数或返回值，它们遵循各自宿主与生命周期规则。
- 推导先收集实参中已有的类型，再为仍未确定的整数字面量默认 Int32；例如 `first(1, true)`
  得到 `T = Int32, U = Bool`。重复出现的同一类型参数必须完全一致，没有隐式宽化。
  显式类型实参可以给未定型字面量提供上下文，`identity::<UInt64>(0xffff_ffff_ffff_ffff)` 合法。
- 外层预期返回类型不反向决定泛型实参：`let n: Int64 = identity(1)` 报类型不匹配，
  应写 `identity::<Int64>(1)` 或先声明 Int64 局部值。没有实参约束的参数、只有提前跳转的实参
  及其他无法局部推导的情况需要显式类型实参；不做跨绑定或全局约束求解。
- **所有泛型函数在定义时检查，包括未使用的函数**。T 是任意允许的值类型，不能对它执行算术、
  比较、整数转换、未知字段访问或构造；`fn bad<T>(x: T) -> T { x + x }` 因缺少能力约束而报错。
  可以移动、返回 T，操作显式元组结构，并调用满足约束的其他泛型函数；固定 Int32 等类型上的操作不受影响。
  `Copy` 是编译器内建的结构能力，不能由用户实现；它可与普通 trait 约束组合，如 `T: Copy + Eq`。
  记录、枚举与透明别名也支持 `T: Copy`，例如 `struct Pair<T: Copy> { left: T, right: T }`；实例化时检查约束。
  类型声明暂不支持普通 trait 或 callable 约束；`Copy` 也不能与 `Fn` / `FnMut` / `FnOnce` 组合。
  仅复制元素的接口（例如 Array.get、List.front 和 Slice.get）要求 Copy，拥有型元素通过借用或移出接口访问。
- 复制模板应明确写出能力：`fn duplicate<T: Copy>(value: T) -> (T, T) { (value, value) }`。
  分支、循环、别名及聚合中的重复移动也在定义时检查；关联类型未确定时同样不假定 Copy。
- 同步及 `@suspends` 泛型调用共用现有求值顺序、快照、失败、Pending、取消与代码保活机制。
  普通函数不能调用挂起函数；`parallel` 分支须调用挂起函数，返回按源码顺序组成的结果元组（全 Unit 时仍为 Unit）。
- 编译期按“函数声明 + 具体类型实参”缓存实例。别名复用实例，不同名义类型分别实例化；
  同型递归与互递归可用。每个函数最多 64 个类型与常量参数；每次模块编译的泛型实例与隐藏闭包入口
  合计最多 4096 个，同一次编译中导入的标准库实例也计入。新实例依赖链最多 32 层；缓存命中不增加实例或层数。
  数量超限分别报告已用泛型实例、闭包入口及本次请求数；深度超限报告请求深度，均在编译期报 `resource_limit`。
- 泛型模板及内部实例不公开导出，也不能直接经 `VmEngine` 或 `vm.register_int32_0/1/2` 调用 / 注册。
  为宿主声明普通、具体签名的包装函数，例如 `fn identity_int32(x: Int32) -> Int32 { identity(x) }`。
  `__joky_generic_`、`__joky_closure_`、`__joky_callable_` 为编译器保留的函数前缀，CLI 的 `main` 不允许类型参数。

泛型记录、枚举、透明别名、固有 impl 和方法独立类型参数已实现，见[泛型记录](#泛型记录)、[泛型枚举](#泛型枚举)、[泛型类型别名](#泛型类型别名)、[泛型固有 impl](#泛型固有-impl)和[方法类型参数](#方法类型参数)。静态 trait 与能力约束见[静态 trait 与泛型约束](#静态-trait-与泛型约束)。泛型 extern 仍未实现；常量参数见下节。同步及挂起函数值、同步匿名函数与闭包见下一节。完整示例见 [generics.jk](../examples/generics.jk)。

### 常量参数与 FixedArray 类型

函数、记录、透明别名和固有 impl 可以声明 `const N: UInt64`，与类型参数按声明顺序混排，
总数最多 64 个。常量实参接受非负整数字面量或当前声明的常量参数；不同进制的同值字面量
规范化为同一值。N 在函数体中是只读 UInt64，具体实例直接使用立即值，不增加运行时形参。

```joky
fn constant<const N: UInt64>() -> UInt64 { N }
fn forward<const N: UInt64>() -> UInt64 { constant::<N>() }
struct Tag<T, const N: UInt64> { value: T }
impl<T, const N: UInt64> Tag<T, N> {
    fn length(&self) -> UInt64 { N }
}
```

省略显式参数时，可从普通输入的类型结构推导 N，例如 `Tag<Int32, 4>`；不从预期返回类型猜测。
显式列表必须给齐所有非 callable 参数，仅允许省略末尾可推导的 callable 参数；泛型函数取值
必须给齐全部参数。常量参数不能被普通形参、局部绑定或闭包形参遮蔽，可以遮蔽模块常量。
枚举声明、trait 声明与 trait impl 暂不接受常量参数，依赖生成 trait impl 的 derive 也不开放。
不接受负数、超 UInt64 范围值、运行时变量、模块/关联常量、常量运算、默认值或 `_` 占位。
关联常量定义也不能依赖尚未实例化的 N；需要读取 N 时使用普通方法返回值。

`import joky/fixed_array.{type FixedArray}` 提供 `FixedArray<T, N>` 类型；它不在 prelude 中。
F1 已支持该类型及透明别名、嵌套形状、Copy 记录/元组/枚举中的布局元数据，
长度属于类型身份。元素限固定数值、Bool、Rune、Unit 及递归无引用 Copy 聚合；
Ptr、函数值、opaque、引用、拥有者和闭包不能作为元素，零长度与 phantom 参数也不能绕过检查。
在泛型声明中使用 `FixedArray<T, N>` 时，T 必须声明 `Copy`；具体元素的无引用布局仍在实例化时检查。

**F2 已支持数组值及聚合组合。** T 可以是上述元素，包含有限的 Copy 记录、元组、枚举和嵌套数组；
数组及含数组的 Copy 聚合具有值语义，赋值、同步直接/函数值间接参数和返回、整数组解引用读取得到独立副本。
支持整个数组的共享/独占引用、解引用写入，以及 `joky/mem` 的 `replace/swap`。

从 `joky/fixed_array` 导入以下函数；除构造函数外，也可通过数组的固有方法调用：

| 函数 | 行为 |
| --- | --- |
| `repeat::<T, N>(value)` | value 只求值一次，复制为 N 个元素 |
| `from_fn::<T, N>(init)` | 接受 `FnMut(UInt64) -> T`，按 0 到 N−1 顺序调用，支持函数指针、Fn 和 FnMut 闭包 |
| `len(&array)` / `is_empty(&array)` | 返回 UInt64 长度 / Bool；方法为 `array.len()` / `array.is_empty()` |
| `get(&array, index)` | 返回元素副本 `Option<T>`，越界返回 None |
| `set(&mut array, index, value)` | 成功写入返回 true，越界返回 false；value 在检查前按普通实参求值 |
| `borrow_ref(&array, index)` / `borrow_mut_ref(&mut array, index)` | 返回 `&T` / `&mut T`；越界产生受检运行时错误 |
| `slice(&array, start, end)` / `slice_mut(&mut array, start, end)` | 返回 `&Slice<T>` / `&mut Slice<T>`；检查 `start <= end <= N`，允许空区间 |

N=0 和零大小元素合法；空数组的 from_fn 不调用回调，但仍清理回调环境。
单维长度与完整值逻辑工作量均不超过 65,536，对象字节数不超过 64 MiB 且受目标地址空间限制。
载荷和清理登记占用任务 arena 预算，退出作用域后回收；数组的语言 Copy 语义不意味着共享载荷。

支持含数组记录/元组/Option/Result 的构造、字段操作、模式匹配/解构及分支合并。
嵌套数组的 `get` 返回独立的 `Option<FixedArray<…>>`；from_fn 回调也可以返回数组或含数组的 Copy 聚合。
外层包装仍计入完整值限额，例如内层工作量已达 65,536 时，额外 Option 标签会使 get 的结果超限。

动态 `Array<T>` 可内联存储上述含数组 Copy 值，支持读取副本、覆盖、扩容、移出和释放；
已有副本不受容器迁移或后续修改影响。F3 已开放其元素引用/切片，以及固定数组的元素引用和切片。
切片复用 `joky/slice` 的读取、写入、子切片和元素借用操作，可从嵌套数组、记录字段及 Array 元素继续派生。
同一固定数组的所有下标保守共用借用范围；空切片、N=0 和零大小元素也保留来源与独占限制。
视图存活时禁止冲突访问、整体替换或结束来源；来自 Array 元素的视图也阻止父容器扩容、替换及移出。
最后一次使用后可重新赋值或扩容。同步 helper 可返回来自引用参数的视图，不能返回局部数组或按值参数副本的视图；
模块函数、方法、函数值和泛型 helper 沿用相同的来源与 noescape 检查。
元素引用不分配视图头部；每次切片/子切片的头部计入任务预算并保留到根任务结束，但不拥有或保活载荷。
重复创建切片可能耗尽头部预算；载荷仍按作用域及时回收。
List/Map/Set 和其他拥有聚合尚不接受数组或其 Array 包装。
尚不支持数组捕获、显式 Arena/alloc 存储、挂起函数中的数组值、持久实例/状态域及普通 direct/CPS 宿主值边界。
普通挂起函数可以调用内部使用数组、签名不含数组的同步 helper。
兼容数组字段与同步引用可按[固定数组与 C 指针适配](#固定数组与-c-指针适配)跨 C 边界；数组字面值及下标访问见上文；尚无默认 Eq。
首版 F0–F5 已验收，覆盖与限制见 [FixedArray 记录](fixed-array-storage-ownership.md#f5集成验收与文档发布)。

## 函数值

同步或挂起的无捕获语言函数都可作为 Copy 值保存、传参、返回，也可存入记录、枚举、元组和 Array。
类型写作 `fn(Int32) -> Int32`；参数及返回类型精确匹配，不隐式转换函数签名。

```joky
fn increment(x: Int32) -> Int32 { x + 1 }
fn identity<T>(x: T) -> T { x }
fn apply<T, U>(value: T, transform: fn(T) -> U) -> U { transform(value) }
fn choose() -> fn(Int32) -> Int32 { increment }
fn example() -> Int32 {
    let transform: fn(Int32) -> Int32 = increment
    let same = identity::<Int32>
    let selected = choose()
    if selected(41) == 42 { apply(same(41), transform) } else { 0 }
}
```

- 普通函数名及导入别名直接取值；限定名遵守模块可见性。泛型函数取值必须提供完整显式类型实参，
  检查 Copy 限制和 trait 约束，然后复用普通单态化缓存。不能把整个泛型模板作为值传递。
- 调用沿用 `f(args)` 和记录函数字段 `holder.run(args)`；先读取并快照被调用值，再从左到右计算实参。
  实参修改原绑定或挂起后恢复，不会更换已经选中的目标。函数值局部绑定优先于同名函数；
  非函数局部值不改变原有普通函数名和整数转换内建函数的查找规则。
- 调用后缀适用于任何表达式：`(f)(41)`、`choose()(41)`、`callbacks[0](41)` 和
  `optional!(41)` 都调用求得的函数值。每层调用先求出目标，再从左到右计算实参；
  不可调用的值报类型错误。同一行的独立表达式须用分号分隔，换行规则见后文。
- 参数和返回类型的别名、泛型替换、跨模块名义身份都递归检查。共享函数类型图缓存比较与替换，
  函数类型嵌套最多 256 层，引用目标中的函数类型展开另限 32 层；递归函数类型别名
  和包含自身签名的递归值布局仍拒绝。
- 间接调用保守视为可能失败、分配并保留普通引用。它沿用 arena 的存储和独占借用检查，
  不承诺“不保留参数”：局部借用不能传入未知函数值，独占借用被保留后也不能重新访问原对象。
  需要这种调用时使用满足现有存储契约的 arena 引用；精确的非保留效果留待后续设计。
- 被引用的目标必须是语言函数；extern、转换内建函数与方法取值尚未开放；零捕获匿名函数可转换成同步函数指针。
  需要宿主操作时声明效果匹配的语言包装函数。函数值都可跨 Pending 保存，调用时保留同步或挂起约定。
- 函数值不能跨 C / 宿主 ABI，也不与 Ptr 或 `vm.Function` 互换；没有空函数值、整数转换、相等比较或裸地址访问。
  含函数值的语言签名不作为宿主入口导出。标量入口保留原接口；任务和旧模块快照持有其可达代码，
  在重载、取消或 VM 丢弃时，仍不会提前释放尚可执行的函数代码。

完整示例：[function_values.jk](../examples/function_values.jk)。

### 同步匿名函数与闭包

匿名函数写作 `|x: Int32| { x + 1 }`，零参数写作 `|| { 42 }`；返回类型可推导或显式标注，如 `|| -> Int32 { 42 }`。
旧的 `fn(...) { ... }` 与 `move fn(...) { ... }` 匿名写法不再支持，具名函数与 `fn(...) -> T` 类型保持原语法。
`||` 在表达式起始位置引入零参数闭包，在已有左操作数后表示逻辑或；闭包体必须用块。
已知函数类型或 callable bound 可给参数提供上下文；单独的 `let f = |x| { x + 1 }`
缺少输入类型而拒绝。立即调用写 `(|x: Int32| { x + 1 })(41)`；与其他表达式共用调用后缀，
`factory()(x)` 和 `(f)(x)` 同样表示函数调用。

```joky
fn apply<T, U, F: FnOnce(T) -> U>(value: T, transform: F) -> U { transform(value) }
fn example() -> Int32 {
    let offset = 10
    let add = |x: Int32| { x + offset }
    apply(32, add)
}
```

普通闭包自动借用自由变量的整个根绑定；`move |参数|` 复制 Copy 捕获、转移拥有值与其他闭包。
每个字面量在所属函数实例内有独立的非 Copy 环境类型，零捕获也如此；只有无捕获且签名匹配者
可消费环境、转换成 `fn(...) -> ...` 指针。`move` 是保留字。

| 能力 | 直接调用规则 |
| --- | --- |
| `Fn` | 共享访问，可重复调用；`let` 即可 |
| `FnMut` | 独占访问，可重复调用；按值环境须绑定为 `var`，也可经 `&mut F` 调用 |
| `FnOnce` | 消费环境，只保证一次调用，可移出无引用拥有捕获；不要求 `var` |

`Fn` 满足 `FnMut` 与 `FnOnce`，`FnMut` 满足 `FnOnce`。三者是编译器内建的同步泛型约束，
不是可由用户实现的普通 trait，也不能被同名类型、trait、类型参数或类型导入别名遮蔽。
同步语言函数指针也满足这些约束。模板按声明的能力检查，包括未使用的定义；具体实例再复核
布局、借用与清理。仅声明 `FnOnce` 的 helper 不能调用两次，`FnMut` 的按值参数须先移入 `var`。

输入类型确定后由闭包返回路径推导 U，F 绑定为环境类型；允许省略显式泛型列表末尾的 callable
参数，例如 `Some(1).map::<Int32>(|x| { x + 1 })`。其余显式类型参数须完整，独立泛型函数取值
仍需全部显式类型实参。不同闭包字面量不自动合流为相同类型。

首版环境只允许局部绑定、局部嵌套捕获及受检同步 helper。不得返回环境或其引用，也不能进入
元组、记录、枚举、容器、arena、持久实例、C/宿主 ABI 或挂起帧；普通无约束泛型 T 不接收闭包。
`@suspends` 函数体只可将零捕获字面量立即转换成同步函数指针，不能持有环境。
捕获保持期间检查共享/独占冲突，普通借用捕获不能消费非 Copy 值，回调不得返回捕获来源的引用；
普通实参的引用返回仍按既有规则检查。同步 helper 的不逃逸效果自动证明，无需用户手写 `@noescape`。

闭包初始化与实参仍按源码顺序只求值一次；未调用、失败和取消均沿用拥有值的清理。
泛型实例与隐藏闭包入口合计最多 4096 个，生成依赖深度最多 32；环境类型深度沿用 256 层，
稳定局部存储沿用每函数 64 KiB 限制。示例见 [anonymous_functions.jk](../examples/anonymous_functions.jk)。

### 函数值的借用契约

函数类型可在参数列表后声明 `@noescape`，表示输入借用既不会被返回，也不会被保存：

```joky
fn read(value: &Int32) -> Int32 { *value }
fn example() -> Int32 {
    let f: fn(&Int32) @noescape -> Int32 = read
    let value = 42
    f(value)
}
```

编译器根据目标函数体检查契约；返回或保存输入借用的函数不能转成该类型。
声明本身带 `@noescape` 的函数在无注解取值时保留契约。没有声明契约的函数可在直接取值时
通过上述类型注解请求检查；先保存到普通函数类型会丢失该保证，之后不能给未知函数值补强契约。
零捕获闭包转换、别名、泛型替换以及函数值的存储与返回保留该效果。
具有契约的函数值可转换成同签名的普通函数值；反方向需要可检查的目标函数。
函数类型没有参数名，因此只接受无参数的 `@noescape`。函数声明的按参数契约继续用于直接调用。

### 挂起函数值

挂起函数类型写作 `fn(T) @suspends -> U`，`@suspends` 位于参数列表之后、箭头之前。
挂起效果参与类型等价、别名规范化、泛型推导和替换；不能隐式地把同步与挂起函数值互换。
同步函数可以保存或返回挂起函数值，只有标注 `@suspends` 的调用者才能实际调用它。

```joky
extern fn pause(x: Int32) @suspends -> Int32
fn increment(x: Int32) @suspends -> Int32 { pause(x) + 1 }
fn apply<T, U>(value: T, transform: fn(T) @suspends -> U) @suspends -> U {
    transform(value)
}
fn compute() @suspends -> Int32 {
    let transform: fn(Int32) @suspends -> Int32 = increment
    apply(41, transform)
}
```

- 间接挂起调用在首次进入时快照目标及实参，Pending 后只继续同一个子调用，不重新求值。
  标量、Unit、空聚合、多槽聚合和函数值返回均沿用既有 CPS 结果约定。
- `parallel` / `race` 分支可以调用挂起函数值，包括记录函数字段；parallel 返回按源码顺序组成的结果元组，
  race 要求各分支返回相同的无引用类型，结果为获胜分支的值。
  各分支的目标与实参按源码顺序求值，在开始轮询任何分支前完成快照；失败和取消沿用现有结构化规则。
- 挂起函数值默认保守地处理引用保留；可写 `fn(&String) @suspends @noescape -> R` 声明不逃逸。
  局部借用、独占借用及宿主 ABI 仍遵循原有存储和生命周期限制。
  语言包装函数可调用异步借用宿主；取消仍先终止宿主访问，再释放任务存储。
- Option / Result 的 `map`、`map_err`、`and_then` 仍接受同步回调；挂起回调需要独立的挂起包装函数。
- CLI 可直接执行挂起 main，包括挂起函数值调用；内置等待见 `joky/task`。
  自定义宿主事件通过嵌入 API 的 `call_async` 启动任务并恢复，或使用 AOT CPS 入口；
  可运行示例：[suspending_function_values.rs](../examples/suspending_function_values.rs)，
  命令为 `cargo run --example suspending_function_values`。


## 整数类型与字面量

已实现 8 / 16 / 32 / 64 位有符号补码整数 `Int8` / `Int16` / `Int32` / `Int64`，
以及 8 / 16 / 32 / 64 位无符号整数 `UInt8` / `UInt16` / `UInt32` / `UInt64`。整数字面量保持未定型，
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
- UInt8 范围为 `0..255`，UInt32 范围为 `0..4294967295`，UInt64 范围为 `0..18446744073709551615`。
  无符号上下文拒绝负数字面量和一元负号，包括 `-0`、`-(1)`、`-value`；减法 `0 - value` 合法并按位宽环绕。
  词法幅值超过 UInt64 最大值时为解析错误，其余字面量按选定类型检查范围。
- 八种整数的加减乘按各自位宽环绕；有符号取负也环绕。除法向零截断，有符号 `MIN / -1` 为 MIN，除零失败。
  无符号比较和除法按非负数解释高位，例如 UInt64 最大值大于 1，除以 2 得 `9223372036854775807`。
  移位量必须落在 `0..位宽`，Int8 / UInt8 只接受 0–7，Int16 / UInt16 只接受 0–15，越界在执行时报错。
  运算和比较要求两侧类型完全一致，位宽或符号不同都不能混用；Bool 不作为数字使用。
  浮点类型与运算见下文专节，受检整数转换见 `to_*()` 方法。
- 八种整数均可用于别名、记录、枚举、元组和 Array / Map 元素，以及同步 / CPS 参数与结果。
  窄整数对象按真实字节布局：Int8 / UInt8 为 1 字节、Int16 / UInt16 为 2 字节，Array / Slice
  元素步长与之一致；`#repr(C)` 记录字段与 C 引用参数接受全部整数（含 UInt8），布局与 C
  编译器逐项核对。64 位值在挂起时保留所有位。Rust 提供 `call_<t>_0/1/2` 全套（t 为
  int8 / int16 / int32 / int64 / uint8 / uint16 / uint32 / uint64）与对应 `call_async_<t>`，
  这些入口均检查精确签名。跨语言边界的窄整型参数与返回以 I32 符号值传递，C 侧的窄参数与
  窄返回函数与此完全兼容；异步结果槽仍为零扩展位模式，Int64 调用者用 `as i64` 解读。
- `joky/io.println` 通过 Show 支持全部整数类型；
  main 仍限 Unit / Int32，`joky/vm` 的 `call_int32_0/1/2` / `register_int32_0/1/2` 保持 Int32 契约。
  示例见 [int64.jk](../examples/int64.jk)、[unsigned.jk](../examples/unsigned.jk)。

### 进制与分隔符

整数支持十进制 `1_000`、十六进制 `0xff` / `0xAB_CD` 和二进制 `0b1010_0011`。
前缀只能写小写 `0x` / `0b`，十六进制数字可大小写混用；没有八进制或类型后缀。
整数 token 不含小数点，浮点语法见[浮点类型与运算](#浮点类型与运算)。
单个 `_` 只能出现在两个数字之间，不能紧接前缀、连续出现或位于末尾。
`0x`、`0b102`、`0x_ff`、`1__0`、`1_`、`123abc` 都是完整数值 token 的解析错误；
`_123` 仍是标识符。以数字开头的相邻字母不能再被拆成下一个表达式，应显式用空白或 `;` 分隔。

不同进制均表示非负幅值，使用相同的上下文定型与范围检查，不将最高位自动解释为负数。
例如 `let n: Int32 = 0xffff_ffff` 越界，`let n: UInt32 = 0xffff_ffff` 合法；
`-0x8000_0000` 可表示 Int32 最小值，`-(0x8000_0000)` 在 Int32 上下文仍越界。

### 位运算与移位

五种整数支持 `&`、`|`、`^`、`~`、`<<`、`>>`。二元运算两侧必须是完全相同的整数类型，
移位量也遵守此规则；未定型字面量可由另一侧或结果上下文定型，例如 `let high: UInt64 = 1 << 63`。
`~` 按当前位宽反转所有位。左移丢弃超出位宽的高位、低位补零；有符号右移补符号位，无符号右移补零。

移位量必须满足 `0 <= count < 位宽`，即 UInt8 只接受 0–7，32 位类型只接受 0–31，64 位类型只接受 0–63。
负数、等于位宽或更大的值都在执行时产生带源码范围的 `RuntimeError`，包含常量移位量；
不会对移位量取模或截断。失败停止后续操作数、实参和序列表达式，短路跳过的移位不执行。
`&` / `|` 不接受 Bool，也不短路；Bool 逻辑继续使用 `&&` / `||` / `!`。

`match` 分支之间用逗号或真实换行分隔，分支结果可直接写按位或，如 `Empty => a | b`。
`parallel` / `race` 分支最外层的 `|` 仍是分隔符，按位或须加括号或块。
调用参数、元组元素、记录字段及块内部可直接使用 `|`，如 `parallel { | worker(a | b) | worker(c) }`。
示例见 [bitwise.jk](../examples/bitwise.jk)。

### 显式整数转换

`int8(value)`、`int16(value)`、`int32(value)`、`int64(value)`、`uint8(value)`、`uint16(value)`、`uint32(value)`、`uint64(value)` 是全局内建函数，无需导入或 `extern` 声明。
均要求恰好一个整数实参，支持八种整数之间全部 64 种组合，返回类型由函数名固定。
透明整数别名同样接受；Bool、Unit、String、记录、枚举、元组和宿主 opaque 值不接受。
另允许 `uint32(rune_value)` 读取合法 Rune 的标量数值，`Rune.to_u32()` 复用此操作；
其他整数转换不接受 Rune，整数到 Rune 必须使用返回 Option 的 `Rune.from_u32()`。

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
五个整数函数不接受浮点实参，浮点转换见下文 `float32` / `float64`。
目标泛型签名和受检转换设计见 [类型设计](syntax.md#101-基础类型与字面量推导)。
完整可运行示例见 [conversions.jk](../examples/conversions.jk)。

## 浮点类型与运算

已实现 IEEE 754 binary32 / binary64 浮点 `Float32` / `Float64`。浮点字面量必须
有小数点且两侧都有十进制数字，如 `1.0`、`-2.5`、`1.5e-3` 或 `2.5E+3`；
不支持 `.5`、`1.`，也没有 `1e3` 或类型后缀。尾数与指数内可用与整数相同的
`_` 分隔符；`0x` / `0b` 前缀后不出现小数点。

```joky
fn scale(v: Float64) -> Float64 { v * 1.5 }
fn tiny() -> Float32 { 1.5e-3 }
```

- 浮点字面量保持未定型，由注解、实参、返回值等上下文选择 Float32 或 Float64，
  无约束时默认 Float64。明确的浮点上下文也接受未定型整数 token，如 `let x: Float64 = 1`；
  已经定型为整数的值不会隐式转为浮点，`Float64` 与 `Int32` 相加仍报类型不匹配。
- 字面量数值超出目标类型的有限范围是编译错误，不会静默变成无穷；极小幅值按
  舍入规则成为次正规数或零。NaN 和无穷没有字面量拼写，由运算产生，
  例如 `0.0 / 0.0` 与 `1.0 / 0.0`。
- 同型的加、减、乘、除、一元负号和全部比较运算已实现，要求两侧同为 Float32
  或 Float64。`&`、`|`、`^`、`~`、移位和 `%` 不接受浮点；浮点余数留待库函数。
- IEEE 语义按位保留：NaN 不等于自身，`!=` 恒为 true，有序比较遇 NaN 为 false；
  有符号零参与除法；次正规数、舍入到最近偶数、溢出为无穷的行为与 Rust 的
  f32 / f64 一致。整数除零失败的规则不适用于浮点除零。
- ARM64 与 x86_64 的编译/执行边界自动设置上述浮点环境，返回宿主时恢复原控制位和状态标志，
  包括失败、挂起与取消清理；宿主回调改变舍入或次正规数配置不会影响后续 Joky 运算。
  x86_64 保护的是标量 SSE 的 MXCSR，不修改宿主的 x87 环境；其他架构目前不支持构建。
- 常量折叠按目标精度执行：`Float32` 表达式不先经 Float64 舍入再窄化，
  `0.1 + 0.2` 在两种宽度下产生各自的标准位模式。
- 原始浮点不提供 Eq / Hash，不能直接作为 Map 键；可作为 Map 值与 Array 元素。
  Float32 / Float64 也可作记录字段、元组元素与枚举载荷，参与同步 / CPS 参数、
  返回值和函数值调用；挂起与恢复保留全部位模式，含 NaN 与有符号零。

`float32(value)` / `float64(value)` 是与整数转换同族的全局内建函数，无需导入。
它们接受七种数值实参（Float32 / Float64 与五种整数），整数按目标精度舍入，
Float64 到 Float32 的有限溢出饱和为同号无穷；实参只求值一次，保留失败与挂起语义。
两个小写名称同样保留在全局函数命名空间，不能被用户函数、extern 或宿主绑定重定义。

### 浮点固有方法与常量

`joky/float` 进入默认 prelude，`Float32` / `Float64` 固有方法开箱可用：

- 分类与绝对值：`is_nan()`（`self != self`）、`is_finite()`、`abs()`。
  `abs` 把 `-0.0` 规范化为 `+0.0`，NaN 保持 NaN。
- 数学原语：`sqrt()`、`floor()`、`ceil()`、`trunc()` 与 ties-to-even 的 `round()`。
  三后端共享同一 IEEE 内核，位结果与 Rust 的 f32 / f64 一致：`sqrt(-1.0)` 为 NaN、
  `sqrt(-0.0)` 保持负零、`(-2.5).round()` 为 `-2.0`、次正规数全程保精度。
- 位重解释：`to_bits()`（Float64 → UInt64、Float32 → UInt32）与关联函数
  `Float64.from_bits(bits)` / `Float32.from_bits(bits)`。这是受检的宽度匹配接口，
  不与数值转换混用；往返保留每个位，包括 NaN payload 与负零。
- 关联常量 `Float64.INFINITY`、`Float64.NAN`（Float32 同名），由常量折叠产生。
- 格式化：`to_text() -> String`，区域设置无关的最短往返拼写；NaN、无穷与负零
  固定拼写为 `"NaN"`、`"inf"`、`"-inf"`、`"-0"`。文本解析不在本阶段。

受检浮点转整数先显式选择截断或舍入，再检查范围并返回 Option；
不混入 `int32(...)` 等 wrapping 接口：

```joky
7.9.trunc_to_i32()    // Some(7)：向零截断
7.5.round_to_i32()    // Some(8)：ties-to-even 后检查
(-0.9).trunc_to_i64() // Some(0)：负小数截断为零
```

`trunc_to_*` / `round_to_*` 覆盖全部八种整数目标。
范围判断针对截断或舍入后的数学整数，边界都是可精确表示的 2 的幂，
因此 `9223372036854775808.0.trunc_to_i64()`（2^63 本身）为 None，
而略小的最大浮点整数值成功；NaN、±无穷与越界值一律 None。
落在 `[-1, 0)` 的负小数截断或舍入为负零，按 0 接受进无符号目标，
更小的负数拒绝。挂起函数中的浮点原语跨恢复保留位模式。

### 浮点宿主接口

extern 值传递接受浮点标量：`extern fn scale(x: Float64) -> Float64`
使用目标平台的浮点寄存器调用约定，三后端一致；混合标量签名如
`(Int32, Float64) -> Float64` 同样开放。`#repr(C)` 记录字段与 C 引用参数
接受 Float32 / Float64 与含浮点的记录（布局与 C 编译器的 size、alignment、
offset 逐项核对过）。C 声明接受浮点结果，但 `register_c_fn` 的注册结果矩阵仍限整数、Ptr 或 Unit；
固定数组 F4 不扩展这一既有差异。浮点输出使用
`joky/io.println` 与 Show。

Rust 侧提供 `call_float32_0/1/2`、`call_float64_0/1/2`、`call_float64_3`、
混合签名的 `call_float64_f64_i32` / `call_float64_i32_f64`，以及
`call_async_float32` / `call_async_float64`，均检查精确签名，不会把位模式
重解释成整数。同步入口直接返回 f32 / f64；`cps_result()` 返回原始位模式，
Float32 零扩展到 64 位。C API `joky_vm.h` 提供
`joky_vm_call_float64_0/1/2_v2`，失败时返回 0.0 并置错误码；
`joky/vm` 内的 `vm.call_float64_0/1/2` 绑定同名符号。

## 固有方法与关联常量

具体类型可用 `impl Type { ... }` 声明方法、关联函数和常量；可拆成多个 impl，成员名不得重复。
支持四种整数、Bool、Unit、Copy 记录及枚举；参数化记录 / 枚举使用下文的泛型 impl。
支持 String 固有方法，不支持 opaque 或元组目标。方法可声明自己的类型参数。透明别名的值仍能调用目标类型的方法，但不能用别名声明 impl，
也暂不提供 `Alias.member` 的类型级访问。

```joky
struct Pair { left: Int32, right: Int32 }
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

- 首个不写类型注解的接收者可为 `self`、`&self` 或 `&mut self`，分别对应目标值、共享引用和独占引用。
  self 绑定不可重新赋值；&mut self 可通过引用修改调用方对象。`Self` 在 impl 内保留，在类型、构造和关联成员位置表示目标类型。
- 有 self 的函数支持 `value.method(args)` 和显式 `Type.method(receiver, args)`；显式调用必须按签名传值或引用，无 self 的关联函数只能用类型调用。
  普通成员查找不按实参数量或类型重载；枚举变体与关联成员也不能同名。
- 按值接收者可以是局部变量、字段、调用、分组、块或构造结果，支持链式调用；负数字面量作接收者写 `(-1).method()`。
  接收者先求值一次，再从左到右求值其他实参；可变值按值快照，后续实参中的修改不会改变已传入的接收者。
  接收者的类型独立确定，无约束整数仍默认 Int32，不从方法返回注解反推类型。
- 全部方法体在定义时检查，包括未调用者；递归和互递归静态解析为普通内部调用。
  `@suspends`、失败、提前返回与 Pending 遵循普通函数规则，`parallel` 也支持挂起方法调用，结果规则与普通分支相同。
  泛型函数可以对已知具体类型调用方法，不能对无约束 T 猜测方法能力。
- `const NAME: Type = expression` 必须有注解，目前限八种整数、`Float32` / `Float64` 和 Bool。
  支持标量字面量、模块常量与关联常量引用、整数和浮点内建转换、一元操作和二元算术/位运算/比较；
  不支持普通函数调用、控制流、局部绑定或运行时状态。前向引用可用，依赖环报错，
  每个表达式最多 256 个语法节点，依赖链最多 32 层。
  常量在编译期按对应类型的普通规则检查并计算；整数环绕保持一致，除零和非法移位为编译错误，
  浮点按目标精度与 IEEE 规则折叠。常量读取无需运行时函数。
- 成员不作为 VM / AOT 公开入口，也不能取作宿主函数地址；宿主仍使用普通函数包装。
  `impl` / `const` 为新增保留字；分号依旧可省略。挂起 main 可调用挂起方法。

### 借用接收者

`&self` / `&mut self` 方法复用普通引用参数及来源检查，适用于具体类型、完整泛型 impl 和带独立类型参数的方法。

```joky
struct Counter { value: Int32 }
impl Counter {
    fn get(&self) -> Int32 { self.value }
    fn add(&mut self, amount: Int32) { self.value = self.value + amount }
    fn view(&self) -> &Self { self }
}
fn borrowed_example() -> Int32 {
    var counter = Counter { value: 20 }
    let view = counter.view()
    let previous = view.get()
    counter.add(22) // view 最后使用后，允许修改。
    if previous == 20 { Counter.get(&counter) } else { 0 }
}
```

- 点调用在需要时自动借用局部接收者或其内联字段；&mut self 要求根绑定为 var。
  已是引用的接收者直接按普通实参规则重借用，&mut T 可收窄为 &T；self 返回的引用继续约束原对象。
- 借用从接收者求值时生效，先于后续实参；`counter.add(counter.get())` 因独占/共享冲突被拒，先计算参数再调用。
  局部独立字段可分别调用借用方法；重叠字段、整体覆盖、移动及返回引用逃逸遵循既有检查。
- 值接收者支持自动借用局部值、参数或内联字段；经引用解引用的内联字段暂不能直接取借用。
  同步、不逃逸的 `&self` 方法也可用于临时值：接收者只求值一次，保存到调用期临时存储，调用结束后清理。
  含共享引用的 Copy 值，以及无法直接借用的 Copy 字段，沿用这一路径对求值时的快照进行查询。
  临时值不支持 `&mut self`，也不能通过借用方法返回或保存其引用；显式 `&临时表达式` 仍被拒绝。
  `alloc(...).method()` 的接收者本身是引用，可以调用对应借用方法。按值方法不隐式读取引用，需要显式 *p。
- 含共享引用对象先按原规则封存，再通过共享引用调用 &self 方法；自动借用不放宽构造期独占引用或任务期存储契约。
  局部引用可传入已证明对应参数不保留借用的方法，也可跨语言挂起；局部与 arena 引用的方法调用均遵守 @suspends / parallel 保活和取消规则。
- 不支持自定义解引用和接收者重载；trait 支持静态派发，见后文；数组拥有者的公开别名仍使用 array 模块函数。

完整示例见 [borrowed_methods.jk](../examples/borrowed_methods.jk)，三后端均输出 42。

### 泛型固有 impl

参数化记录和枚举可声明 `impl<T, U> Type<T, U> { ... }`。
类型实参必须按 impl 形参的声明顺序各出现一次，覆盖目标的全部类型参数；
形参名可以不同于记录 / 枚举定义，例如 `impl<V> Pair<V>`。

```joky
struct Pair<T> { left: T, right: T }
impl<T> Pair<T> {
    const SIZE: Int32 = 2
    fn make(left: T, right: T) -> Self { Self { left: left, right: right } }
    fn first(self) -> T { self.left }
    fn swap(self) -> Self {
        let Pair { left, right } = self
        Self.make(right, left)
    }
}
fn example() -> Int64 {
    let p = Pair.make(0, int64(42))
    p.swap().first()
}
```

- impl 类型参数覆盖目标、成员签名和方法体，离开 impl 后不可用；每个成员体均按声明的 Copy / 移动能力检查，
  包括未调用的方法。可操作 `Pair<T>` 的已声明字段和方法，不能假定无约束 T 具有算术或成员能力。
- `Self` 携带当前 impl 的全部参数，支持类型注解、`Self { ... }`、`Self.make(...)`、
  `Self.SIZE` 和枚举的 `Self.Some(value)` / `Self.None`；不再为 Self 单独提供类型实参。
- 实例调用 `p.first()` 的 impl 实参由接收者独立决定；first 没有方法类型参数，不能再写 `p.first::<...>()`。
  关联调用 `Pair.make(1, 2)` 从普通实参推导，也可写 `Pair.make::<Int64>(1, 2)`
  或 `Pair::<Int64>.make(1, 2)`。显式接收者形式 `Pair.first(p)` 同样可用。
  类型命名空间已绑定 impl 实参时不能在成员名后重复提供；成员自己的额外类型参数见下节。模块限定名称遵循相同规则。
- 返回注解不反推关联函数的 impl 实参；空记录等幻影参数需显式指定，如 `Marker::<Bool>.make()`。
  泛型自由函数可调用已知 `Pair<T>` / 枚举实例的方法；递归、同步和 `@suspends` 调用复用有界函数实例化。
- 泛型 impl 的关联常量仍只允许固定整数或 Bool 类型，不能声明 `const VALUE: T`。
  常量表达式不依赖具体类型实参，按成员计算一次；`Pair.SIZE` 与 `Pair::<Bool>.SIZE` 读取同一值，
  后一种写法仍检查类型实参数量和 Copy 限制。
- 同一名义类型可拆分为多个泛型 impl，但成员名不能重复；模块导入保持原声明身份、定义作用域和 `pub` 可见性。
  泛型方法模板与实例均不成为 VM / AOT 公开入口，宿主使用具体包装函数。
- 暂不支持 `impl Pair<Int32>` 特化、部分具体参数、重排 / 重复参数、`impl<T> T`；函数、方法和 impl 参数支持 trait 约束。
  别名不能作 impl 目标或成员命名空间。

完整可运行示例见 [generic_impls.jk](../examples/generic_impls.jk)。

### 方法类型参数

方法和关联函数可声明额外的类型参数；具体类型的 impl 也可使用，例如 `impl Bool { fn choose<T>(...) ... }`。
impl 参数作用于所有成员，方法参数只作用于本方法的签名和函数体，不能泄漏到其他方法、常量或 impl 外部。

```joky
struct Pair<T> { left: T, right: T }
impl<T> Pair<T> {
    fn zip<U>(self, other: Pair<U>) -> Pair<(T, U)> {
        let Pair { left, right } = self
        let Pair { left: other_left, right: other_right } = other
        Pair { left: (left, other_left), right: (right, other_right) }
    }
    fn combine<U>(value: T, other: U) -> (T, U) { (value, other) }
}
fn example() {
    let p = Pair { left: 42, right: 7 }
    let q = Pair { left: true, right: false }
    let zipped = p.zip(q)
    let explicit = p.zip::<Bool>(q)
    let bound = Pair::<Int64>.combine::<Bool>(4294967295, true)
    let unbound = Pair.combine::<Int64, Bool>(4294967295, true)
}
```

- `value.method::<U>(...)`：impl 实参来自接收者，显式列表只对应方法参数。
- `Pair::<T>.method::<U>(...)`：两层参数各自绑定；有 self 的方法仍需传入显式接收者。
- `Pair.method::<T, U>(...)`：未绑定命名空间的显式列表依次包含全部 impl 参数，再包含全部方法参数。
  `Self.method::<U>(...)` 中的 impl 参数已经绑定，规则与第二种相同。
- 省略方法的显式列表时，从普通实参推导剩余参数；已有的 `Pair.make::<T>(...)` 保持兼容。
  显式列表可省略末尾 callable 参数，其余部分必须完整，不支持 `_` 占位；返回注解不反向推导方法参数。
  例如 `Some(1).map::<Int32>(|x| { x + 1 })` 的 F 仍从回调推导。
- 两层类型参数合计最多 64 个，不得重名，也不能把方法参数命名为 `Self` 或 impl 目标的名称。
  不同方法可以各自声明同名 U，身份互不混淆。即使未使用的参数也必须满足 Copy 限制。
- 所有模板体都先按刚性参数检查，再按完整的两层实参缓存实例；继续遵守每次模块编译 4096 个实例与
  隐藏闭包入口的合计限额，以及 32 层依赖限额。
  定义作用域、成员可见性、同步 / CPS、失败、取消和重载后的旧任务保活规则不变。
  固有方法可声明带约束的参数；trait 要求的方法本身不接受独立类型参数。拥有型记录实参可用；闭包实参须具有 callable bound。

### Option 与 Result 后缀解包

标准库 `Option<T>` 和 `Result<T, E>` 支持后缀 `!` 与 `?`，同步与 `@suspends` 函数均可使用：

| 表达式 | 成功分支 | 失败分支 |
| --- | --- | --- |
| `option!` | Some 取出 T | None 触发当前任务失败 |
| `result!` | Ok 取出 T | Err 触发当前任务失败并清理错误载荷 |
| `option?` | Some 取出 T | 从当前函数返回 None |
| `result?` | Ok 取出 T | 从当前函数返回 Err，转移原错误载荷 |

`?` 要求当前函数返回同类标准枚举；成功载荷类型可以不同，Result 的错误类型必须一致，
不做隐式转换，也不在 Option 与 Result 之间转换。闭包内的 `?` 返回该闭包；跨 `scope`
的提前返回与显式 return 一样，先汇合子任务再退出。Err / None 是普通返回值，不触发任务失败。

```joky
fn increment(value: Option<Int32>) -> Option<Int64> {
    Some(int64(value?) + 1)
}
fn main() {
    println(increment(Some(41))!)
}
```

操作数只求值一次。拥有型实例按值消费，成功时转移载荷，失败或提前返回时沿普通清理路径释放局部资源；
Copy 实例仍可再次使用。运算符只识别标准库类型的声明身份，支持导入别名和关闭 prelude 后的显式导入，
不适用于同名自定义枚举或 `&Option` / `&Result`；需要借用时用 match。

后缀与成员、下标访问可连续使用，例如 `nested!!`、`read()?.field`；优先级高于前缀一元运算和二元运算。
`!flag` 仍是 Bool 逻辑取反，`!=` 仍是不等比较。在换行会结束表达式的位置，后缀不能跨行：
下一行的 `!flag` 开始新表达式，下一行的 `?` 则报错。括号内仍遵循允许跨行表达式的规则，
以点开头的跨行方法链也仍可使用。

### Option 与 Result 基础方法

默认 prelude 提供下列普通源码方法，载荷和默认值支持 Copy 类型及拥有型数组、记录、枚举和元组。
四个状态查询方法通过 `&self` 共享借用，查询后仍可使用原值；`take(&mut self)` 修改原值。
其余方法按值接收，消费拥有型实例；未选中的拥有型默认值、备选值和载荷在退出时清理。
`Option.zip` 的载荷元组支持 Copy 和拥有型元素；任一侧为 None 时清理另一侧的拥有型值。

`Option<T>`：

| 方法 | 语义 |
| --- | --- |
| `take(&mut self) -> Option<T>` | 用 None 替换原值并返回旧 Option，支持 Copy 与拥有型载荷 |
| `is_some(&self) -> Bool` / `is_none(&self) -> Bool` | 共享借用，判断当前变体，不读取或比较载荷 |
| `unwrap_or(fallback: T) -> T` | Some 返回原载荷，None 返回 fallback |
| `unwrap_or_else<F: FnOnce() -> T>(fallback: F) -> T` | Some 返回原载荷；仅 None 调用一次 fallback |
| `and<U>(next: Option<U>) -> Option<U>` | Some 返回 next，None 返回 None，可改变载荷类型 |
| `or(fallback: Option<T>) -> Option<T>` | 保留原 Some；原值为 None 时返回 fallback，载荷类型保持 T |
| `zip<U>(other: Option<U>) -> Option<(T, U)>` | 两边均为 Some 时返回载荷元组，否则返回 None |
| `map<U, F: FnOnce(T) -> U>(transform: F) -> Option<U>` | Some 调用一次 transform，None 不调用 |
| `and_then<U, F: FnOnce(T) -> Option<U>>(transform: F) -> Option<U>` | Some 返回回调结果，None 不调用 |

`Result<T, E>`：

`unwrap() -> T` 消费 Result 并取出 Ok 载荷；Err 分支触发任务失败并清理错误载荷。
需要处理预期错误时使用 match 或 `unwrap_or_else`，不要用 unwrap 代替错误分支。

| 方法 | 语义 |
| --- | --- |
| `is_ok(&self) -> Bool` / `is_err(&self) -> Bool` | 共享借用，判断当前变体 |
| `unwrap_or(fallback: T) -> T` | Ok 返回原成功载荷，Err 返回 fallback |
| `unwrap_or_else<F: FnOnce(E) -> T>(fallback: F) -> T` | Ok 返回原载荷；仅 Err 将错误载荷交给 fallback，调用一次 |
| `ok() -> Option<T>` | Ok 转为 Some，Err 转为 None，舍弃错误载荷 |
| `err() -> Option<E>` | Err 转为 Some，Ok 转为 None，舍弃成功载荷 |
| `and<U>(next: Result<U, E>) -> Result<U, E>` | Ok 返回 next，Err 保留原错误；成功类型可变，错误类型 E 必须相同 |
| `or<F>(fallback: Result<T, F>) -> Result<T, F>` | 保留原 Ok；原值为 Err 时返回 fallback，舍弃原错误；成功类型 T 必须相同，错误类型可变 |

回调方法另提供 `Result.map<U, F: FnOnce(T) -> U>(F) -> Result<U, E>`、
`Result.map_err<U, F: FnOnce(E) -> U>(F) -> Result<T, U>` 和
`Result.and_then<U, F: FnOnce(T) -> Result<U, E>>(F) -> Result<U, E>`。
map / and_then 仅在 Ok 时执行回调，map_err 仅在 Err 时执行；其余分支保持原载荷。
这些方法的回调环境仍先求值，但回调函数体仅在选中分支执行一次，失败正常传播。

带实参的方法均按普通按值调用规则，先计算接收者，再从左到右计算实参。**普通默认值和备选值总会求值**，
即使它们不会被选中；实参的副作用、失败或挂起仍会发生。例如 `Some(42).unwrap_or(expensive())`
仍执行 expensive，`Ok(42).or(other())` 仍执行 other。这些接收普通默认值或备选值的方法没有惰性回调语义。
按值调用中的 Copy 实例遵循快照规则；拥有型实例的取值会消费原绑定，四个借用式状态查询则不会。

`unwrap_or_else` 延迟的是回调函数体；创建闭包、计算捕获值仍在调用前完成。Some / Ok 分支不会执行回调，
但仍清理其拥有型捕获；None / Err 分支执行一次，Result 的错误载荷按值传入回调。
该方法仍消费拥有型接收者，回调失败正常传播。例如：

```joky
fn fallback() -> String { "default" }
fn example() -> String {
    let value = Some("saved")
    if value.is_some() {
        value.unwrap_or_else(fallback) // Keep saved without calling fallback.
    } else {
        value.unwrap_or_else(|| { "missing" })
    }
}
```

关闭 prelude 后可显式导入 `joky/option`、`joky/result`；选择性类型导入、重命名及模块限定调用保持相同的名义身份。
`joky/result` 单向依赖 `joky/option`，但不重导出 Option / Some / None；单独导入 Result 后可以链式调用
`result.ok().unwrap_or(default)`，需要直接书写 Option 类型名或构造器时再显式导入它们。
不含回调的方法不依赖闭包或 trait；map / map_err / and_then / unwrap_or_else 接收满足 FnOnce 的同步闭包或语言函数指针，
未选中回调的分支也会清理其拥有捕获。这些方法仍由普通 Joky 源码实现。
完整示例见 [option_result.jk](../examples/option_result.jk)；显式方法类型实参见 [generic_methods.jk](../examples/generic_methods.jk)。

### 受检整数方法

默认 prelude 提供标准 `Option<T>`、五种整数的 `MIN` / `MAX`，
以及 `to_i32()`、`to_i64()`、`to_u8()`、`to_u32()`、`to_u64()`：

```joky
import joky/integer

fn example() -> Option<UInt64> {
    let n = -1
    n.to_u64()                   // None
}
fn forced() -> UInt64 { uint64(-1) } // 18446744073709551615
fn maximum() -> Int32 { Int32.MAX }  // 2147483647
```

25 种源/目标组合一律返回 `Option<目标整数>`：数值可表示则 Some，否则 None，即使同型或保证不丢失的宽化也如此。
`uint64()` 等五个全局函数保留原来的 wrapping 强制转换语义；没有另一套 `try_to_*()` 或 `try_int*()` 内建。
接收者的错误仍正常传播，不能把除零等运行时错误转换成 None。

这些方法和边界定义在 [integer.jk](../src/stdlib/integer.jk)，Option 定义在 [option.jk](../src/stdlib/option.jk)，范围检查用普通 Joky 比较、分支和内建转换实现。
关闭 prelude 后，`import joky/integer` 仍可显式启用这些成员及 Option 类型。
`joky/integer` 和 `joky/option` 共用同一个 Option 声明，可以同时导入且不依赖顺序。
重复模块路径或重复整数成员仍报错。示例见 [integer_methods.jk](../examples/integer_methods.jk)。

## 静态 trait 与泛型约束

```joky
pub trait Eq {
    fn eq(&self, other: &Self) -> Bool
}
struct UserId { value: Int32 }
impl Eq for UserId {
    fn eq(&self, other: &Self) -> Bool { self.value == other.value }
}
fn same<T: Eq>(left: &T, right: &T) -> Bool { left.eq(right) }
fn example() -> Bool {
    let left = UserId { value: 42 }
    let right = UserId { value: 42 }
    same(&left, &right) && Eq.eq(&left, &right)
}
```

- trait 方法可只有签名，也可带默认体；`Self` 表示实现目标。支持 `self`、`&self`、`&mut self`、
  无接收者的关联函数、空标记 trait 和 `@suspends`。无默认体的方法必须实现，参数、返回值、接收者形式与挂起标记逐项匹配。
  impl 方法不写 `pub`，可见性继承 trait；trait 默认模块私有，`pub trait` 可以导出。
- 函数、固有方法和 impl 类型参数支持 `T: Eq + Hash`，每次泛型调用必须满足全部约束。
  不同导入别名仍指向同一 trait；重复别名约束也报错。记录、枚举及透明别名声明暂不接受参数约束。
  泛型体只使用已声明的能力，包括未实例化的模板；不会因某次类型实参恰好拥有某个方法而放行。
- 支持条件实现，如 `impl<T: Value> Value for Pair<T>`。目标仍限具体 Copy 基元，
  或按声明顺序完整应用 impl 参数的记录 / 枚举；也支持单参数 blanket impl，如 `impl<T: A> B for T`。
  blanket impl 只能在 trait 定义模块声明；每个 trait 最多一个 blanket impl。
  具体调用/约束证明时若 blanket 与名义实现同时满足条件则报错，没有特化优先级。
  不支持别名目标、特化和部分具体化。
  同一 trait / 名义类型只能有一个实现，即使条件不同也拒绝重叠；trait 或被实现的名义类型必须定义于实现模块。
  固有 impl 的条件同样约束其方法和关联常量；受约束常量需显式绑定目标，如 `Pair::<UserId>.SIZE`。
- 具体类型的点调用优先选择可见的固有方法，否则在当前作用域可见的 trait 中查找。
  多个 trait 提供同名方法时报告歧义，用 `Eq.eq(left, right)` 指定 trait。
  显式 trait 调用不自动取借用，接收者作为普通第一个参数传入。
  无法从实参推导 Self 时可写 `Factory.make::<UserId>(42)`；该类型实参是 Self，不是方法级泛型参数。
- 模板检查时选择的 trait 会保留到单态化阶段。实例类型上额外的固有方法或另一个同名 trait 方法不会改变选择。
  约束证明复用相同义务，单次证明最多 256 个义务、32 层依赖；函数实例与隐藏闭包入口仍受 4096 / 32 限额约束。
- `import module.{type Eq as Equal}` 导入 trait 别名；`import module` 支持 `module.Eq.eq(...)`，
  也让该模块公开的 trait 参与点调用。库模板保留自己的定义作用域，私有 trait 方法不会暴露给消费者。
  trait 和 impl 只影响编译期，生成普通静态调用；JIT、解释器和 AOT 无 vtable、额外类型标签或新增运行时 ABI。

借用、返回引用来源、参数求值顺序、挂起与取消继续沿用普通函数规则。
未声明效果的抽象 trait 调用保守地视为可能分配、失败和保留普通引用。
有函数体的语言函数按参数自动推导借用是否逃逸，包含挂起函数及结构化子任务；
普通 helper 和实例方法无需标注。省略显式 `&` 的实参自动借用仍只用于同步调用。
分析覆盖直接调用、递归、导入函数、泛型模板及具体实例，也检查按值聚合内携带的引用。
返回借用和保存借用分别分析：仅返回视图的 helper 可在调用方继续使用该视图，
不会因此被当成保存了输入。仅在证明参数既不被返回、也不被保存时，才获得完整不逃逸保证；
无法证明时保持保守，仍可按普通来源规则显式传引用。返回输入引用的函数继续合法，
返回值仍依赖未被排除的输入来源，不会因为省略属性而改变成禁止返回引用。

语言函数和 trait 方法仍可在参数列表后写可选的 `@noescape`，保证全部借用实参不被返回或保留；
`@noescape(key, other)` 只约束指定参数。属性可与 `@suspends` 组合，不能用于 extern。
它不承诺无分配、无失败或无副作用，也不放宽借用跨挂起的存储要求。
显式契约始终检查，不会在推导失败后静默降级。
无函数体的 trait 声明不能推导，需用契约向泛型调用者提供保证；公共库可用契约固定接口要求。
实现方法继承 trait 声明，允许实现参数改名；递归调用也必须通过函数体检查。
函数类型可用 `fn(&T) @noescape -> R` 保留全部参数不逃逸的契约，详见函数值章节。
因此 `fn inspect<T: Eq>(x: T) { x.eq(&x); }` 不能把局部借用交给未知实现；可改为接收引用参数，
由具体调用处检查存储条件，或使用 arena 对象。单态化后根据实际方法体计算效果，纯实现仍可接受局部值借用，
释放操作仍区分 `needs_arena` 与真正分配的 `uses_arena`。
点调用也保留现有局部借用限制：不借用临时值、不直接借用经 arena 引用解引用后的内联字段。

首版不支持 trait 自身的类型参数、trait 方法独立泛型参数、关联类型默认值、GAT、trait 关联常量、
继承、特化、拥有型 `dyn Trait` 或运算符重载。标准 Eq / Hash 也是普通源码 trait，不是内建运算符协议；
编译器检查签名与约束，不证明相等关系、哈希一致性等代数规律。
泛型类型实参支持 Copy 与拥有者；自动清理及 `Drop` 规则见下一节。

借用型 `&dyn Trait` / `&mut dyn Trait` 已支持 JIT、解释器和 AOT 动态分派。
带类型局部初始化、赋值和实参的期望类型触发显式引用擦除；全部关联类型必须绑定。
接口中所有方法须同步、以 `&self` / `&mut self` 接收，并声明所有借用参数不逃逸；
除接收者外不能使用 Self，不能返回引用或含引用结果。未使用的接口也检查这些条件。
`view.method(args)` 与 `Trait.method(view, args)` 按接口声明顺序的槽调用最终默认/覆盖实例；
默认体内部仍按具体 Self 静态分派，同名固有方法和其他 trait 不改变动态槽。
共享视图可重复调用；独占视图调用共享/独占方法或传给同步 helper 时短期重借用。
视图不拥有或销毁原对象，方法的拥有参数/结果沿普通清理路径处理；结果可为 String、记录、枚举或元组。

`&*view` 建立共享重借用；`&mut *view` 建立独占重借用，要求原视图已有独占权限。
这些组合也支持普通受检引用；不会按值读取 dyn 目标。子借用期间不能冲突使用原视图；
共享副本、helper 转发、赋值与多层分支保留全部原对象来源和字段路径，零大小对象也分别检查。
原对象在视图最后使用之后恢复访问；结束视图不会运行 Drop。

只允许同步局部/参数视图，不允许 dyn 返回、聚合/容器存储、闭包捕获、宿主 ABI，
也不能取得绑定动态方法的函数值。`@suspends` 形参不接受 dyn；活跃视图不能跨语言挂起
或进入并发分支。挂起函数可以调用同步 helper，也可以在挂起点之间短期借用合法的 Copy
局部对象；无引用拥有对象适用下文的拥有值借用规则，dyn 视图本身仍不能跨挂起。
关联绑定描述方法参数/结果，绑定拥有类型本身不等于视图持有该资源；同步方法可以返回拥有值。
解释器时间片暂停保留同步调用的视图与原对象，取消、失败或预算耗尽按普通所有者路径清理。
短期实例方法可以借用既有 State 创建视图，视图不能写回 State；重载后的旧实例继续使用旧方法表。
完整示例见 [borrowed_dyn.jk](../examples/borrowed_dyn.jk)，验收见[实施记录](borrowed-dyn-trait.md)。

### 默认方法与关联类型

方法后的 `{ ... }` 是默认实现，空 `{}` 也是实际方法体。impl 可以省略默认方法，
也可以用同名且签名匹配的方法覆盖，不需要 `override`。默认体在 trait 定义作用域以刚性
Self 检查：即使没有 impl 或所有 impl 都覆盖它，也不能访问实现类型的字段或未声明能力。
默认体选定的 trait 调用在实例化后调用最终覆盖版本，同名固有方法不会改变这一选择。
`Trait.method(...)` 也选择最终实现，不提供强制调用原默认体的语法。直接和相互递归使用普通静态入口。

```joky
trait Source {
    type Item
    fn next(&mut self) @noescape -> Option<Self.Item>
    fn skip(&mut self) @noescape { let ignored = self.next() }
}
fn take<S: Source>(source: &mut S) @noescape -> Option<S.Item> { source.next() }
fn first<S: Source<Item = Int32>>(source: &mut S) @noescape -> Int32 {
    source.next().unwrap_or(0)
}
```

完整可运行输入及显式导入见 [trait_sources.jk](../examples/trait_sources.jk)。
impl 用 `type Item = Int32` 指定关联类型；要求必须且只能定义一次，不能在固有 impl 中声明，
不能加 `pub`，也不能通过不同 Item 选择同一类型的多个 impl。关联类型与方法可以交错、前向引用；
关联项后若还有成员，必须用换行或分号分隔，同一行空白或逗号不算分隔。

`Self.Item` 绑定当前 trait 的项。`T.Item` 必须从可用能力唯一确定所属 trait；多个 trait
都有 Item 时，即使结果相同也报歧义。限定写法 `<T as Source>::Item` 可用于参数或具体类型，
trait 路径支持模块限定及导入别名。具体类型的简写只搜索当前作用域可见的 trait。
`type Item: Show + Other` 声明关联项能力；`S: Source<Item = U>` 同时声明能力与类型等式，
尖括号只接受具名等式，允许多项及尾逗号，拒绝重复、未知项与 `_`。关联项能力列表不接受 callable 或嵌套等式。

投影按基础类型、trait 身份和关联项序号绑定，透明别名不改变身份。刚性模板保留未确定的投影；
只有声明等式或唯一适用 impl 才能确定其类型。推导先从普通参数位置确定 Self，不通过 Item 的结果反推实现类型。
关联能力和等式在定义期与实例期检查；投影、约束证明及其进入的别名展开共用每次请求的
32 层依赖 / 256 义务限额，重复子图复用结果，环报告依赖路径。

默认方法的抽象调用仍只依赖声明的效果契约；当前默认体可推导为纯净，不代表覆盖体自动获得同样保证。
`@noescape` 按参数位置继承，`@suspends` 逐项匹配。具体默认实例按真实 Item 重新检查移动、借用、
存储、挂起与清理，投影不会把引用变成无引用 Copy 值；拥有型聚合跨挂起仍须满足完整存储树无引用的要求。
丢弃拥有型结果、失败和取消均走原有清理路径。内建 Drop 仍要求显式同步 drop，不补空默认实现。

Show 采用默认 `to_text(&self) @noescape -> String` 复用 builder 包装；
`Show.to_text(&value)` 选择最终方法，原 `joky/show.to_text` 自由函数保持可用，`print` 复用同一包装。

### Drop 与自动清理

Array、拥有记录及拥有枚举在离开作用域时按逆声明顺序清理。移动转移清理责任，
赋值先求出右侧新值，再析构旧值；提前 return、break、continue 和运行时失败也会清理。
独立表达式产生的拥有临时值最迟在所在词法作用域结束时清理。

需要自定义逻辑时显式导入编译器认可的 `joky/drop.Drop`：

```joky
import joky/array.{type Array}
import joky/drop.{type Drop}

extern fn report(id: Int32)
struct Resource { id: Int32, values: Array<Int32> }
impl Drop for Resource {
    fn drop(&mut self) { report(self.id) }
}
```

- 先运行自身 drop，再按字段声明顺序递归清理拥有字段；每个字段的 drop 完成后释放其缓冲区的载荷与控制块。
  未实现 Drop 的 Array 和包装记录仍自动释放存储，不需要手动调用 release。
- Drop 仅适用于拥有记录：一个或多个拥有字段加无引用 Copy 元数据；可写无条件泛型 impl，
  枚举本身暂不能实现 Drop，但其当前变体的拥有载荷按原规则清理。
  不接受 Copy 类型或附加 trait 约束的条件 Drop。用户自定义的同名 trait 仍是普通 trait。
- drop 必须是同步 `fn drop(&mut self)`，不能显式调用或取得其函数值。
  实现 Drop 的记录允许整体移动；拥有绑定的字段不能直接移出或通过字段赋值替换，但可通过独占重借用后 `*field = next` 或 `mem.replace` 替换，始终保持父记录完整初始化。
- 字段按源码顺序初始化；构造失败或中途退出时，按逆初始化顺序清理已完成字段，尚未完整构造的外层记录不运行自身 drop。
  析构失败不阻止其他清理，保留最早的运行时错误。取消时执行清理，但不发布清理错误。
- 解释器内部时间片、取消、模块重载及 VM 销毁保持清理责任；登记与管理权转移之间不会暂停。
  析构中创建资源导致的递归清理最多嵌套 32 层，超限报告资源错误并继续释放缓冲区。
  解释器为每个同步析构提供独立的 1,000,000 指令清理额度，任务耗尽预算后仍尝试析构，
  析构死循环则报资源错误并继续释放存储；内部时间片不能打断正在运行的析构。
- 没有 Drop 祖先的拥有字段可以移出或替换；所选字段自身可以实现 Drop，其 hook 随拥有权转移。
  移出消费整个原记录，未选字段继续等待原作用域清理；不支持保留原记录继续使用的部分移出。
  替换只析构旧字段并保持记录完整；旧字段析构失败时，其他字段和右侧新值仍各自清理。
  元数据更新与字段重借用仍可用，借用检查保守地覆盖整个记录。
- 拥有型数组元素的整体移动与 Drop 已开放；无引用拥有记录、枚举、元组及容器可跨普通语言挂起，
  参数、返回值和分支捕获均保留恰好一次清理；容器借用操作可直接写在挂起函数中；整个无引用拥有对象及其元素/Slice 视图可跨挂起借用。Ptr 的显式释放协议不变；`vm.Vm` 使用同一资源所有权与自动清理机制。
  普通共享引用的任务期保留规则不因数组提前析构而缩短。Drop 不由 prelude 隐式导入。
完整可运行示例见 [static_traits.jk](../examples/static_traits.jk)。

## 导入与 prelude

默认 prelude 提供 `assert(condition: Bool, message: &String) -> Unit`；关闭 prelude 时
显式导入 `joky/check.{assert}`。原来的 `require` 已改名为 `assert`，不保留旧别名。
消息参数具有不逃逸契约，可直接写 `assert(ok, "错误信息")`、`assert(ok, message)` 或
`assert(ok, &message)`。自动借用不移动已有字符串，临时字符串保活到调用结束。
参数按从左到右的普通调用规则求值（condition 为 true 时仍求值 message）；true 正常返回，
false 产生终止性 RuntimeError，跳过后续普通执行并完成资源清理。错误拥有消息文本，
直接调用定位到 assert 的真实源码调用处，别名与跨模块调用保持来源；经函数值调用时定位到
被调用函数定义。解释器保留已有调用轨迹；原生同步调用在失败传播时保留源码调用点，
通过 `Error::native_call_sites()` 查询，CLI 渲染文件与行号；不提供字节码 PC 或原生栈。
析构中失败继续必要清理并保留首个错误。消息构造先失败时报告 ResourceLimit，assert 尚未执行。
诊断消息独立于任务 arena 保活。该接口不依赖宿主自定义注册，Debug 和 Release 均执行检查。

同模块的 `panic(message: &String)` 默认由 prelude 导入，可直接写 `panic("错误信息")`，
也支持插值字符串与已有字符串的自动借用。关闭 prelude 时显式导入 `joky/check.{panic}`。
它无条件产生终止性 RuntimeError，沿用 assert 的消息保活、调用位置与资源清理规则，不负责 IO。
编译器将绑定到该内置函数的直接调用视为发散表达式，别名调用同样有效：

```joky
fn unwrap(value: Option<Int32>) -> Int32 {
    match value {
        Some(n) => n,
        None => panic("Missing value"),
    }
}
```

发散分支不参与结果类型合并，也不需要补返回值；后续必定不可达的表达式仍报告编译错误。
当前没有用户可声明的 `Never` 类型；普通包装函数及函数值调用仍按声明的返回类型检查。
panic 的函数值签名为 `fn(&String) -> Unit`，间接调用需显式传引用。
显式 `&"..."` 仍不支持：自动借用仅在受检查的调用边界创建临时绑定，不开放可逃逸的临时引用。

内嵌标准库用 `#intrinsic("标识符")` 声明编译器提供的操作。例如字符串复制桥接声明为：

```joky
#intrinsic("text.clone")
fn copy_text(text: &String) @noescape -> String
```

这类声明没有占位函数体，尾随分号可省略，与 `extern fn` 一致；函数名可改变，操作身份由注解标识符决定。
部分 domain 调用适配函数带同类注解并保留实际源码函数体，编译器仍在调用点检查及捕获操作身份。
该属性只允许受信任的内嵌标准库使用；普通文件或用户模块中的同名路径和属性不授予权限。
未知标识符、跨模块权限、错误声明形式和不符合操作要求的签名均会被拒绝。

源码库包括 `joky/option`、`joky/result`、`joky/integer`、`joky/array`、`joky/list`、`joky/set`、`joky/map`、`joky/string`、`joky/json`、`joky/json/typed` 和 `joky/prelude`；
`joky/vm` 是宿主绑定模块。路径使用 `/`，模块成员与枚举构造器使用 `.`。
导入必须位于全部声明之前，不执行用户代码；支持用户文件模块，尚无通配导入。

```joky
import joky/option as opt.{type Option as Maybe, Some as Present, None as Missing}

fn make() -> Maybe<Int32> { Present(42) }
fn empty() -> opt.Option<Int32> { Missing::<Int32> }
fn read(value: Maybe<Int32>) -> Int32 {
    match value { Maybe.Some(n) => n, opt.Option.None => 0 }
}
```

- 默认模块名为路径末段，例如 `option`；通常直接写 `import joky/option`，仅在需要规避重名时使用 `as opt` 替换该模块名。
  `import joky/option.{type Option as Maybe} as opt` 也合法。
  模块别名与成员选择可同时使用，`opt.Option`、`opt.Some`、`opt.Option.Some` 指向同一枚举声明。
- `import joky/option.{type Option}` 引入类型名及其构造器命名空间，因而允许 `Option.Some(42)`。
  类型别名导入 `type Option as Maybe` 同样允许 `Maybe.Some(42)`；这与透明类型声明 `type X = ...` 不同。
  裸 `Some(42)` 由单独选择的构造器或 prelude 提供；仅选择 `Some` 不会顺带引入裸类型名 Option。
  `None::<Int32>`、`Missing::<Int32>` 可显式指定无载荷构造器的类型参数，不加调用括号。
- 宿主模块也支持别名及选择性导入，例如
  `import joky/vm as engine.{type Vm as Engine, call_int32_0 as invoke}`。
  `engine.Handle` / `Engine` 保留原 opaque 身份，`engine.create()` / `new()` 绑定原宿主入口。
  `call_int32_0/1/2` / `register_int32_0/1/2` 的别名保留原有参数及 ABI 限制。
- 默认 prelude 引入 `Option`、`Some`、`None`、`Result`、`Ok`、`Err` 及上述基础方法，以及整数的 16 个 `to_*()` 方法与 `MIN` / `MAX`。
  四个 wrapping 转换函数 `int32` / `int64` / `uint32` / `uint64` 属于语言内建，不受 prelude 开关影响。
  prelude 还引入 `print` / `println`，不引入数组或 VM。显式 `import joky/prelude` 可在关闭默认 prelude 后恢复这些名称；
  `import joky/prelude as p` 提供限定名称及相应类型的成员方法。
- CLI 的 `run` / `build` 支持 `--no-prelude`；Rust 中用
  `TypeRegistry::with_prelude(false)` 或 `set_prelude_enabled(false)`，
  `VmEngine::set_prelude_enabled(false)`，以及编译开始前的 `AotCompiler::set_prelude_enabled(false)`。
  已规范化 AST、已加载模块和挂起任务保留其绑定；开关作用于后续源码编译。
- 显式选择性导入的重复名称、重复模块路径、模块别名与同级声明冲突均报错；`_` 不能用作导入名称。
  局部值遮蔽导入的构造器、函数和模块前缀；局部字段或方法仍按原规则查找。
  同级函数可以遮蔽 prelude 的裸构造器或打印函数，限定名称仍可用。
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

- 函数、记录、枚举、透明类型别名、模块常量、impl 内的方法和常量默认私有；`pub` 标记对其他模块公开的声明。
  `pub enum` 同时公开全部构造器；不同枚举的同名成员可以共存，裸导出名有歧义时须使用类型限定名。
  `pub struct` 只公开类型本身，字段默认私有，可按字段写 `pub` 跨模块读写与构造。
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

## 模块常量

模块顶层可声明标量常量，与关联常量共用同一套求值规则；`pub const` 对其他模块公开，
默认模块私有。`const` 计入声明边界，`import` 仍须位于所有声明之前：

```joky
const MAX_ENTITIES: UInt32 = 4096
const DOUBLE: Float64 = float64(MAX_ENTITIES) * 2.0
pub const DEFAULT_SPEED: Float64 = 120.0
```

`app/config.jk`：

```joky
pub const LIMIT: Int32 = 100
const SECRET: Int32 = 7
pub fn reveal() -> Int32 { LIMIT + SECRET }
```

`main.jk`：

```joky
import app/config.{LIMIT}
const SCALED: Int32 = LIMIT * 2
fn main() -> Int32 { SCALED + config.LIMIT + config.reveal() }
```

- 类型限八种整数、`Float32` / `Float64`、Rune 和 Bool，以及解析到这些类型的透明别名；
  注解在声明模块的类型作用域解析。不接受 String、引用、记录或容器。
  表达式白名单与关联常量一致：标量字面量、可见的模块常量与关联常量、整数和浮点内建转换、
  一元及二元运算；每表达式 256 个语法节点、依赖链 32 层，模块常量与关联常量共用依赖图并检测环。
- 定义模块内裸名可前向引用；消费者通过 `alias.NAME`、完整路径或选择导入（含重命名）读取，
  绑定保持声明身份。私有常量不进入导出，跨模块限定读取未命中时报
  `Undeclared constant`。公开常量可依赖私有常量，只导出计算后的值。
- 同一值作用域内常量与函数、裸枚举构造器及导入名冲突报 `duplicate_definition`；
  局部绑定可遮蔽裸常量，遮蔽模块别名后限定名按字段访问解析，不回退模块查找。
- 常量在编译期按对应类型的普通规则求值（整数环绕、除零和非法移位为编译错误、浮点按 IEEE 折叠），
  未使用的声明同样检查；读取内联为标量立即值，不产生模块初始化函数或运行时全局数据。

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

别名可前向引用其他别名，最终目标必须是支持的基础类型、共享/独占引用、模块内记录、枚举、Copy 或拥有型元组或已注册的
宿主 opaque 类型，例如 `type Function = vm.Function`。别名不创建新类型、构造函数或运行时值，
`Count` 和 `Int32` 完全等价；不同 opaque 类型的身份仍然不同。
参数、返回值、局部注解、CLI 入口及 extern 签名均按展开后的类型检查，挂起约定不受影响。

别名名是单个标识符，与函数及局部变量使用独立命名空间；同一模块中别名名不能重复，
也不能覆盖基础类型、记录、枚举或已注册的同名 opaque 类型。所有别名都会检查，包括未使用的声明；
未知目标和循环引用报错，循环错误指出引用链及闭合环的位置。
当前不支持局部类型声明、限定别名声明名或独立的 newtype 声明；`struct` 可以表达名义包装类型。

别名在声明模块中解析；`pub type` 可通过模块限定名或 `import path.{type Name as Alias}` 导入。
别名不进入宿主注册器；成功重载可重新定义别名，失败加载保留旧模块。
旧挂起任务继续使用编译时的类型和代码。Rust `function_signature` 返回展开后的签名，
注册宿主函数时仍使用实际基础或 opaque 类型。完整示例见 [aliases.jk](../examples/aliases.jk)。

### 泛型类型别名

```joky
type PairOf<T> = (T, T)
type MaybePair<T> = Option<Pair<T>>
struct Pair<T> { left: T, right: T }

fn first<T>(values: PairOf<T>) -> T {
    let (a, b) = values
    a
}
```

- 类型参数只在该别名目标内有效，遮蔽同名模块类型，不泄漏到后续声明。最多 64 个参数，
  允许尾随逗号，不接受空列表、重复参数、`_` 或 trait 约束。
- 每次引用都必须给出完整类型实参，如 `PairOf<Int64>`；别名可以嵌套应用，也可用于函数、
  记录字段、枚举载荷、元组和显式泛型调用实参。函数推导按展开后的结构进行。
- 实参遵循现有 Copy / 拥有者规则，包括 String；拥有者仍按值移动。
  即使 `type Ignore<T> = Int32` 不使用 T，`Ignore<vm.Function>` 也会报错。
  固定目标仍可为 String 或已注册的 opaque 类型；限制针对类型实参。
- 别名完全透明：`PairOf<Int32>` 与 `(Int32, Int32)` 相同，不增加名义身份、构造函数或成员命名空间。
  `MaybePair<Int64>` 的值仍用 `Some(Pair { ... })` 构造；别名不能用于声明 impl。
  泛型函数中只出现在被擦除位置的参数无法由值推导，需要显式提供。
- 公开模板随导入绑定保留定义作用域及原有名义类型身份；导入重命名、消费者的同名声明均不改变目标。
  未使用的模板也检查目标和环。经实参形成的依赖环保守拒绝，包括经未用于目标的参数形成的环。
- 规范化阶段缓存最多 256 个不同别名应用（包括刚性参数应用），重复应用复用结果；该预算与函数、
  记录及枚举实例分别计算。目标沿用 65,536 标量槽、256 层布局上限，共享零大小类型图不会指数展开。
  别名在 IR / ABI 检查前擦除，三后端同步与 CPS 沿用目标类型的布局和挂起行为。

完整示例见 [generic_aliases.jk](../examples/generic_aliases.jk)。

## 记录

`struct` 在模块顶层声明名义记录，允许与别名、函数交错以及前向类型引用：

```joky
struct Point { x: Int32, y: Int32 }
type Position = Point

fn translate(p: Position, dx: Int32) -> Point {
    Point { x: p.x + dx, y: p.y }
}

fn example() -> Int32 {
    var p = Point { x: 1, y: 2 }
    let saved = p
    p.x = 10
    saved.x  // 1: copying a record preserves its value.
}
```

- 字段类型支持 `Int32`、`Int64`、`UInt32`、`UInt64`、`Bool`、`Unit`、`Ptr`、共享引用、嵌套记录、有限枚举及 Copy 元组，允许空记录 `struct Empty {}`。
  仅含这些 Copy 字段的记录按值复制，空记录仍保留名义类型身份，不等于 Unit。
  另支持[拥有记录与 Copy 元数据](#拥有记录与-copy-元数据)，此类记录非 Copy，暂限同步使用。
  String 作为拥有叶参与同步聚合布局和清理；宿主 opaque 字段仍不接受。
- 圆括号外，记录构造名称与开头的 `{` 须在同一行；字段内容允许跨行。
- 相邻字段声明及相邻构造项之间必须有 `,`，或源码在两项之间存在真实换行；关闭 `}` 前无需分隔，
  允许尾随逗号。缺失分隔在同一行时报错，`;` 不是字段分隔符。
  构造必须提供全部字段，不能重复、缺失或使用未知字段，不提供隐式默认值。
  支持同名字段简写：`User { name, age, nickname }` 等价于
  `User { name: name, age: age, nickname: nickname }`，可与显式初始化混用，如 `User { name, age: 18, nickname }`。
  简写按当前位置的普通名称查找解析右侧值，Copy 值复制、拥有值移动，字段可见性和类型检查不变。
  初始化按源码书写顺序求值，布局按声明顺序；Unit、空记录的初始化副作用也会执行。
- 字段访问支持 `p.x`、`shape.point.x`、`translate(p, 1).x` 和 `(Point { x: 1, y: 2 }).x`。
  `var` 根绑定允许字段赋值、嵌套字段赋值以及整个记录的替换；`let` 和函数参数不可修改。
  读取可变值时保留快照，后续初始化项或参数中的修改不影响已经求出的值。
- 不同声明的记录不能互换，即使名字或布局相同。重新加载源码创建新身份，
  原任务继续持有旧代码和类型。透明别名可用于注解，但构造仍使用原始记录名。
- 同步和 `@suspends` 函数都可传递和返回 Copy 记录；分支、提前返回及 parallel 参数遵循同样规则。
  `extern fn`、宿主注册和公开标量调用入口不接受记录签名，CLI main 仍限 Unit / Int32。
- 所有声明均检查，包括未使用的声明。直接或经别名形成的纯记录无限内联布局会报
  `recursive_record`；含枚举的环报 `recursive_type`，纯别名环仍报 `cyclic_type_alias`。当前每个记录最多展开为 65,536
  个标量槽，嵌套最多 256 层，超过限制报 `resource_limit`。
- 记录解构见下一节；记录仍不支持默认值、整体相等比较或用户定义析构（Drop 通过 trait 定义，不是普通析构函数）；方法通过独立的 `impl` 声明。

解析只依赖 token：名称之后的 `{}`、以 `字段名:` 开始的花括号，
以及首个字段名后接逗号、换行或 `}` 的花括号解析为构造。
若意在先读变量再求独立块，应明确分隔，如 `value; { name }` 或将 `{` 放到下一行。
`if` / `while` 条件及 `match` 被匹配表达式中未被括号、实参列表或内层块包住的构造受限，避免吞掉条件后的块；
使用 `if (Point { x: 1, y: 2 }).x > 0 { ... }` 明确边界。
`.` 字段访问优先于一元及二元运算；模块限定调用仍有效，局部值遮蔽模块名前缀时按该值的方法解析，
找不到方法即报错，不回退为模块调用。完整示例见 [records.jk](../examples/records.jk)。

### 记录解构

`let` / `var` 和 match 载荷支持名义记录模式；字段按名字匹配，顺序可与声明不同：

```joky
struct Point { x: Int32, y: Int32 }
struct Segment { start: Point, end: Point }

fn norms(p: Point, s: Segment) -> Int32 {
    let Point { x, y } = p                 // 全部字段绑定
    let Point { x: left, .. } = p          // 重命名并忽略其余
    let Segment { start: Point { x: sx, .. }, end: _ } = s   // 嵌套
    x + y + left + sx
}
```

- `field` 是 `field: field` 的简写；`field: pattern` 重命名或递归匹配。
  `..` 忽略所有未列字段，最多一次且必须位于末位，其后允许尾随逗号；
  不写 `..` 必须列全字段。字段分隔沿用构造的逗号或换行规则。
  `Type {}` 只匹配空记录；模式不可包含默认值、范围或表达式。
- 不存在的字段、重复字段、重复绑定名及错配的名义类型均为编译错误。
  头部支持模块限定名、透明别名和泛型实例：无显式参数时从右侧名义类型推导，
  显式写法沿用 `Pair::<Int32> { ... }` 风格。整个模式可在右花括号后加类型注解，
  如 `let Point { x, y }: Point = point`。
- 右侧只求值一次；全部绑定在整个解构成功后才进入作用域，右侧不能引用新名字。
  `let` 产生不可变绑定，`var` 每个叶子获得独立可变存储；不在同一模式内混用单字段 mut 标记。
- Copy 记录复制字段，原值仍可用；拥有记录整体被消费，拥有字段转移到新绑定，
  Copy 字段产生快照，之后原记录不可再读或修改。忽略的拥有字段在解构结束时
  按记录自身的析构顺序清理（与该记录直接离开作用域一致），先于下一用户表达式。
- 有 Drop 祖先的拥有字段不能被移出；字段自身实现 Drop 时整体绑定合法
  （hook 随拥有权一起转移），继续展开该字段并移出其内部拥有者则拒绝。
  只复制 Copy 字段、其余全部忽略时不拆有 Drop 的记录：复制完成后对完整值执行正常 Drop。
- `let User { name, score } = &user` 沿用共享借用 match：Copy 字段绑定为值快照，
  拥有字段绑定为 `&Field` 视图，嵌套继续投影；原记录保持完整，不因 `_` / `..` 清理。
  视图存续期间，原记录的替换、移动和字段写入按既有借用冲突规则拒绝。
  `&mut` 解构、独占借用模式和借用元组解构仍拒绝。
- Copy 记录解构可用于同步与挂起函数（含 CPS 恢复）；拥有记录解构与借用解构限同步函数，
  在解释器时间片、取消、失败和 VM 销毁时保持恰好一次清理。
- 显式列出的字段（包括 `field: _`）必须可见；`..` 可省略不可见字段，但不能绑定或借用它们。
  无 `..` 且存在不可见字段时拒绝解构，诊断不泄露私有字段名单。
  模块限定名、透明别名、泛型实例和公开泛型函数的定义作用域遵循同一可见性规则。
- match 载荷中的记录模式（如 `Circle(Point { x, y }) => ...`）复用既有覆盖检查；
  记录模式本身不可失败。`if let`、顶层记录 match、守卫与字面量模式仍不开放。

完整示例见 [record_patterns.jk](../examples/record_patterns.jk)。

### 泛型记录

记录可声明 Copy 或拥有型记录实参的类型参数，构造使用字段推导、上下文注解或显式 `::<...>`：

```joky
struct Pair<T> { left: T, right: T }
struct Marker<T> {}
fn first<T>(p: Pair<T>) -> T { p.left }
fn make<T>(value: T) -> Pair<T> { Pair { left: value, right: value } }
fn example() -> Int64 {
    let inferred = Pair { left: 4294967295, right: int64(7) }
    let annotated: Pair<Int64> = Pair { left: 4294967295, right: 0 }
    let explicit = Pair::<Int64> { left: 42, right: 0 }
    let phantom: Marker<Bool> = Marker {}
    first(inferred)
}
```

- `struct Pair<T, U>` 的参数具有声明局部身份，最多 64 个，不能重复或使用 `_`。
  参数可实例化为支持的 Copy 数据或拥有者，包括 String；即使参数未出现在字段中，也不接受 opaque 类型。
- 类型位置写 `Pair<Int64>`，构造位置写 `Pair { ... }` 或 `Pair::<Int64> { ... }`。
  支持嵌套应用及模块限定构造，如 `model.Pair::<Int64> { ... }`。非泛型透明别名可指向具体实例，但不新增构造器。
- 推导先收集已知字段类型，再为仍未确定的整数字面量默认 Int32；声明顺序不改变初始化的源码求值顺序。
  返回类型、绑定注解、调用形参、外层字段和载荷的预期类型可以向内传递。
  `var` 字段赋值、整个记录替换、分支及 match 沿用普通记录的类型与快照规则。
- 未出现在字段中的 phantom 参数需要注解或显式实参；不会按后续语句反向推断已经完成的绑定。
  不同实例（包括零字段实例）按声明身份及全部类型实参区分；跨模块重命名导入保留身份，重新解析 / 重载产生新身份。
- 泛型函数可以构造、传递、返回 `Pair<T>` 并读写其已声明字段；模板即使未调用也接受刚性检查。
  `T` 本身不获得字段或整数运算能力；方法使用 `impl<T> Pair<T>`，不支持 `impl Pair` 或 `impl Pair<Int64>`。
- 记录实例在类型规范化与 IR 规划阶段分别缓存，每阶段最多 256 个；仍限 65,536 标量槽和 256 层嵌套。
  phantom 参数也计入嵌套深度；未经过引用的声明依赖环保守拒绝；共享引用允许有限布局的递归对象类型。
  JIT、解释器和 AOT 共用实例布局，同步 / CPS 调用、Pending、失败、取消和重载沿用现有聚合值协议，宿主 ABI 不接收记录。

完整可运行示例见 [generic_records.jk](../examples/generic_records.jk)。

## 枚举与模式匹配

`enum` 声明模块内名义值类型，每个变体有零个或一个载荷；多个数据通过记录或元组组合：

```joky
struct Point { x: Int32, y: Int32 }
enum Lookup { Missing, Found(Point) }
type LookupResult = Lookup

fn value(result: LookupResult) -> Int32 {
    match result {
        Lookup.Missing => 0
        Found(point) => point.x + point.y
    }
}
fn answer() -> Int32 { value(Lookup.Found(Point { x: 19, y: 23 })) }
```

- 本地枚举构造可使用声明名：无载荷为 `Lookup.Missing`，有载荷为 `Lookup.Found(value)`；
  裸成员名在本地候选唯一时也可使用，入口文件与依赖模块规则相同。
  导入的枚举可用模块或类型导入别名限定，也可用显式导入 / prelude 提供的裸构造器。
  不接受无载荷构造器的 `Missing()` 或透明类型别名构造；`Done(Unit)` 仍须显式传入 `{}`。
  Copy 载荷支持 Int32 / Int64 / UInt32 / UInt64 / Bool / Unit / Ptr / 共享引用 / Copy 记录 / 有限嵌套枚举 / Copy 元组。
  载荷也可为 Array 或拥有型记录、枚举；此时整个枚举非 Copy，按值构造、绑定、传参、返回及赋值均转移所有权。
  同一拥有型枚举的其他载荷必须是拥有者或无引用 Copy 数据，不能混入共享引用及数组视图。
  记录、枚举和元组可以相互组合，但不允许无限内联循环，String 使用拥有载荷规则；opaque 载荷仍不支持。
- 枚举至少一个变体，名称不能重复，`_` 保留给通配模式。相邻变体之间必须有 `,`，
  或源码在两项之间存在真实换行；载荷类型和单个模式绑定允许尾随逗号。类型声明支持前向引用及透明别名。
  不同用户声明或不同次解析产生不同类型身份，即使名字、变体及布局相同；标准库身份按编译环境共享。
- 分支以 `pattern => expression` 书写，不带前导 `|`。相邻分支用逗号或真实换行分隔，
  块分支也需要分隔；允许尾逗号，不允许空分支。
- `match value { pattern => expression ... }` 是表达式，被匹配值只求值一次，本节描述枚举值或枚举引用 `&Enum` / `&mut Enum`；
  数组、切片及标量字面量匹配见[序列与字面量模式](#序列与字面量模式)。
  模式支持 `Variant`、`Enum.Variant`、`Variant(binding)`、`Variant(_)` 和整个分支的 `_`。
  有载荷模式可递归写元组及嵌套构造，例如 `Some((left, right))`、`Some(None)`、
  `Some(Some(value))`；元组形状和元素数量必须与载荷类型一致。模式中的导入名称按声明身份检查；
  其余无限定模式由被匹配类型确定。裸的无载荷构造器按当前载荷的枚举类型解析，其他裸名称创建绑定。
  有限定模式可使用声明名或导入别名，载荷模式必须匹配声明的有无。
  例如本地 `Choice.None` 遮蔽 prelude 的裸 `None`；匹配标准 Option 时可写 `Option.None` 明确身份。
  载荷绑定不可变，分支及被匹配表达式中的局部声明不会泄漏；按值或共享匹配的 Copy 绑定是独立快照。
- 必须穷尽所有变体，否则报 `non_exhaustive_match` 并列出缺失项；`_` 覆盖剩余变体。
  重复变体、完整覆盖后或 `_` 后继续写分支均报 `unreachable_code`。
  未知变体、错误限定名及模式载荷数量错误报 `invalid_pattern`。
- 所有正常完成的分支必须同型，也可返回 Unit、记录或枚举。`return` / `break` / `continue`
  等无正常值的路径不参与类型合并；不隐式贯穿，不根据常量被匹配值省略其他分支的静态检查。
  分支体是一个表达式，多步操作用块或分号序列；`;` 仍是可选的连接运算符，
  `A => first(); value, B => fallback` 合法，分支末尾的分号将其正常值变为 Unit；分号后的换行结束该分支，
  跨行的多步分支应使用块或括号序列。
- 构造载荷、被匹配表达式和选中分支均可在 `@suspends` 内挂起；恢复不重放此前副作用。
  同步/CPS 参数、返回值、递归和 parallel 参数支持枚举；取消、失败及重载继续遵循原任务生命周期。
  extern、宿主注册及公开标量调用入口不接受枚举签名，CLI main 仍限 Unit / Int32。
- 第一版布局为一个 Int32 标签，加上**所有变体载荷各自独立的扁平标量区**，
  大小为 `1 + sum(payload slots)`，不是最大载荷大小；构造会初始化全部标量区。
  记录、枚举及元组共用 65,536 标量槽、256 层布局上限；含枚举的内联循环报 `recursive_type`，
  纯记录环和纯别名环分别保持 `recursive_record` / `cyclic_type_alias`。
  该布局是内部实现，紧凑 union 布局留待后续优化，不承诺宿主 ABI。

`enum` / `match` 现在是保留字；局部值遮蔽枚举构造名前缀时按局部值处理。
枚举声明同时提供类型名和构造器命名空间，例如 `Option`、`Option.Some`、`Option.None`。
`type Option` 的选择性导入也能使用限定构造器；外部枚举的裸构造器必须单独导入或由 prelude 提供。
同一模块内不同枚举可声明同名成员，`First.Empty` 与 `Second.Empty` 按各自类型解析；
本地裸构造器只有唯一候选时才可用于表达式。同名候选不会按声明顺序或表达式的预期类型自动选择。
本地构造器遮蔽 prelude 的同名构造器，与显式导入、模块别名或同级函数/常量重名仍报错。
`module.Empty` 若对应多个公开枚举，也报告歧义，应改写为 `module.First.Empty`；
`import module.{Empty}`（包括重命名）会在导入处报告歧义，可改为 `import module.{type First}`。
公开导出只考虑公开枚举，私有枚举的同名成员不会使外部导出产生歧义。
`match` 的无限定成员模式仍可根据被匹配枚举确定含义，包括嵌套载荷模式；
显式限定或导入重命名的模式继续校验枚举身份。同一枚举内部的重复成员仍报错。
支持嵌套的序列、整数、Bool 和 Rune 字面量模式；不支持 guard、字符串/浮点字面量模式或枚举整体相等比较。模式递归深度与类型布局同样限制为 256 层。
拥有型 enum 的按值 `match` 消费整个值，将当前变体的载荷转交给模式绑定；`Variant(_)` 和 `_` 清理未绑定的拥有载荷。
分支退出、提前返回和失败均保持恰好一次清理，仅当前标签对应的载荷运行 Drop；未激活的拥有者槽为空。
这些规则适用于任意用户枚举，与 Option / Result 的名称无关。示例见 [owning_enums.jk](../examples/owning_enums.jk)。
示例见 [enums.jk](../examples/enums.jk)。

共享借用匹配可写 `match &value`，也可直接匹配已有的 `&Enum` 参数或 `Array.borrow_ref` 返回值。
它沿用上述递归模式与穷尽检查，适用于任意用户枚举及 Option / Result：

```joky
import joky/io.{println}

let zipped = Some((resource(20), resource(22)))
match &zipped {
    Some((left, right)) => println(left.id + right.id)
    None => println(0)
}
let moved = zipped
```

- 模式绑定的类型由对应子载荷决定：拥有者 `T` 绑定为 `&T`，Copy 数据绑定为复制值。
  因此整个拥有型元组/嵌套枚举绑定为共享引用，继续展开模式则逐个决定各绑定类型。
- 匹配只读取原存储，不移动载荷或转移清理责任；`_` 不触发 Drop，原枚举按原作用域规则清理。
  活跃的资源引用依赖整个原枚举；来自数组的引用继续依赖整个原数组。
  引用最后使用前拒绝冲突的修改、移动或释放，最后使用后可再次修改或消费原值。
- 共享资源引用可以在同步 helper 间传递和返回，不能从局部或按值拥有者逃逸，
  不能存入聚合、arena 或传给宿主，也不能通过共享引用移动资源或取得独占权限。
  已有独占引用也可显式 `&*p` 后使用上述共享匹配规则。
- 无引用局部值和拥有型资源的共享借用，以及符合 arena 存储规则的 Copy 枚举共享引用，
  可在 `@suspends` 中匹配并跨挂起保留，恢复后不重复执行分支副作用。
  从共享匹配中返回按值 `T` 的泛型 helper 必须声明 `T: Copy`。

完整示例见 [borrowed_matches.jk](../examples/borrowed_matches.jk)。

独占借用匹配可写 `match &mut value`，也可直接匹配已有的 `&mut Enum`。
每个绑定均为对应载荷的 `&mut T`，包含 Copy 标量、记录、元组及拥有者：

```joky
var value = Some((resource(1), 0))
match &mut value {
    Some((item, count)) => {
        item.id = 42
        item.values.push(42);
        *count = item.id
    }
    None => {}
}
```

- 标量引用通过 `*p / *p = next` 访问，记录字段与方法沿用自动解引用。
  绑定名称本身不可重新赋值；修改的是原存储，后续匹配可观察更新。
- 递归模式中不同元组成员可同时独占借用；编译器验证它们来自不重叠的载荷路径。
  原枚举及来源数组仍受借用约束，不能借此替换父值、切换其变体或修改其他数组索引。
  匹配会重借用已有独占引用，载荷引用最后使用后可继续使用原引用。
- Copy 载荷即使取成 `&mut Int32` 等普通引用，仍保留拥有型来源的存储依赖；
  同步 helper 返回、`&*p` 和分支合并均不会丢失该依赖，不能存入任务期对象或引用数组。
- 借用不移动资源或改变清理责任，更新后的资源在正常退出、失败及取消时仍恰好清理一次。
  `*p = next` 可替换 Copy 或拥有型数据并清理旧值；`mem.replace` 可在独占借用结束后替换拥有者并返回旧值。
- 语言仍不支持多层引用，因此独占模式不能绑定本身为引用的载荷；可忽略它或改用共享匹配。
  泛型实例在特化后检查此限制。无引用局部值、拥有对象及投影出的 Copy 字段、容器元素和 Slice
  均可在保持原存储存活且无冲突时跨挂起。arena 中的 Copy 枚举也可独占匹配并跨挂起恢复。

完整示例见 [mutable_borrows.jk](../examples/mutable_borrows.jk)。

### 泛型枚举

枚举可声明 Copy 或拥有型类型实参的类型参数，用 `Option<Int32>` 等类型应用指定实例。Option / Result
是普通枚举名，没有特殊运行时表示；`joky/option` 和 `joky/result` 提供标准声明，
默认 prelude 已引入这两个类型及其构造器，以下示例可直接使用：

```joky
fn unwrap_or<T>(value: Option<T>, fallback: T) -> T {
    match value {
        None => fallback
        Some(value) => value
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
  也可把类型实参绑定到命名空间：`Option::<Int32>.Some(42)`、`Option::<Int32>.None`；
  两处不能重复提供。不接受省略 `::` 的 `Option<Int32>.Some` 或部分 `_` 推导。
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
- 声明时检查所有变体，包括未实例化的模板；类型实参及载荷支持 Copy 或拥有者，String 按拥有者处理，opaque 不接受。
  每个实例按特化后的实际载荷决定 Copy / 移动规则，元组载荷也遵循同一规则。
  递归值可以通过拥有容器的间接存储形成有限布局，例如 `enum Tree { Nil, Items(Array<Tree>) }`，
  也支持 Map、透明别名、容器包装、记录/枚举互递归与有限泛型实例图。
  类型依赖按字段及载荷的实际存储路径检查，遇到内部 Buffer 控制块便结束内联布局展开。
  `enum Bad { Next(Bad) }`、经 Option/元组/记录/别名形成的无限内联布局仍报递归类型错误；
  每轮改变实参而产生无限实例的泛型递归受实例数与类型深度限额约束，不会无限展开。
  未存储的 phantom 实参仍参与依赖、身份、类型有效性及深度检查，不能隐藏声明或别名循环。
- 编译期间缓存实例，规范化和 IR 规划各自最多 256 个不同枚举实例（含刚性参数实例）；
  函数实例限额独立计算。布局最多 65,536 标量槽，类型嵌套最多 256 层，未用于载荷的类型实参也计入嵌套深度。
  共享类型图的比较、替换及身份检查避免重复展开。
- 枚举实例沿用标签加独立载荷区的内部布局，支持三后端同步 / CPS、值快照、Pending、取消和旧任务保活。
  没有新增运行时泛型值、extern 聚合 ABI 或公开聚合调用入口。

方法可使用[泛型固有 impl](#泛型固有-impl)；静态 trait 已实现，泛型记录见[泛型记录](#泛型记录)。
完整可运行示例见 [generic_enums.jk](../examples/generic_enums.jk)。

递归拥有值仍按移动规则使用：Array/Map 可借用子树、替换和移出子树；存活的内部借用会阻止
移动、替换或扩容所属容器。拥有树可跨普通 CPS 挂起，取消与失败均释放完整初始化前缀。
清理按迭代遍历释放子缓冲区，不因树深度增长原生调用栈，也不需要分配清理工作栈；
其状态计入缓冲区控制块预算。用户 Drop 主动创建或释放其他拥有者造成的析构重入仍受既有 32 层限制。
实际行为及拒绝边界见 [JSON1a 递归拥有树探针](../tests/fixtures/json_recursive_owners.jk)。


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
- 元素支持 Int32 / Int64 / UInt32 / UInt64 / Bool / Unit / Ptr、Copy 聚合和拥有型数组、记录、枚举、元组，
  不接受宿主 opaque 元素。支持透明别名、任意有限嵌套、记录字段和枚举的单个载荷。
  `(Int32, Bool)` 与同结构别名等价，与 `(Bool, Int32)`、`(Int32,)` 不同；
  元组中的名义类型仍按原身份比较，重载产生的新记录或枚举不会因外层元组而变得兼容。
- 全部元素为 Copy 时，元组为 Copy；包含拥有者时，整个元组按值移动。构造、传参、返回、赋值
  都转移所有权，移动后不能再次使用原值。拥有型元组的其他元素必须是不含引用的 Copy 数据。
  解构消费整个元组，将每个名称对应的拥有型子树转移到独立绑定；忽略项在解构结束时清理。
  元组本身没有自定义 Drop；成员记录的 Drop 及嵌套拥有者按元素声明顺序清理。
  引用不允许从借用元组中解构出拥有者。无引用拥有型元组及其完整对象视图支持挂起调用，
  仍须满足[拥有值借用](#跨挂起借用)规则；可作为引用自由的 Array 元素。
  消费式解构后的独立拥有绑定可以分别参与并行可变借用，包括函数或 Result 返回的元组。
  解构前必须结束源的活跃借用；同一拥有者的别名冲突、视图逃逸及容器内容引用的约束保持有效。
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
  不同表达式之间须用分号或换行分隔；圆括号外，被调用表达式与 `(` 必须在同一行，换行后的 `(value)` 是下一表达式。
  若返回元组前一项以名称结束，写 `let (x, flag) = pair; (x + 1, flag)`，
  或用 `return (x + 1, flag)` 明确边界，不把分号规定为通用终止符。
- Copy 元组支持同步/CPS 参数、返回值、if/match 合并、递归及 parallel 参数，挂起恢复不重放前面的元素。
  拥有型元组支持同步调用和分支合并，并在返回、失败或取消时清理剩余成员。
  `(Unit, Empty)` 这类零存储元组仍有结构类型，构造和调用的副作用不会被省略。
  元组沿用聚合值的 65,536 标量槽、256 层布局上限；类型/绑定模式语法嵌套也有限额。
  经过元组的循环依赖继续按参与声明报别名环、记录环或枚举环错误。
- 用从零开始的十进制成员索引访问元素，如 `pair.0`、`nested.1.0`、`record.pair.0`；
  `nested.1.0` 是连续两次访问，不是浮点字面量。索引不接受进制前缀、下划线或负号，越界在编译时报错。
  临时元组也可读取成员，如 `make_pair().0`；成员支持赋值、复合赋值和 `&pair.0` / `&mut pair.0`，
  引用上的成员访问沿用记录字段的自动解引用、可变性和借用冲突检查。
  拥有型元素沿用字段移动、替换及借用规则：不能通过借用移出拥有者，不能移出或替换实现 Drop 的记录的拥有字段。
  移出成员后，原聚合尚未完整初始化时不能再次使用；需要分别使用剩余元素时用消费式解构。
  不提供动态索引、解构赋值、参数模式或整体比较；match 的枚举载荷可使用嵌套元组模式。
  extern、宿主注册和公开标量入口暂不接受元组，CLI main 仍限 Unit / Int32。

完整示例见 [tuples.jk](../examples/tuples.jk) 和 [owning_tuples.jk](../examples/owning_tuples.jk)。

## 字符串字面量

双引号字符串支持 UTF-8 文本，以及 `\n`、`\r`、`\t`、`\0`、`\"`、`\\` 六种简单转义，
另支持 Unicode 转义 `\u{H...}`：花括号内为 1–6 个十六进制数字，允许前导零，
不允许空内容或分隔符；数值必须为 `0..0x10FFFF` 中排除 `0xD800..0xDFFF` 的 Unicode 标量。
例如 `"\u{4E2D}\u{1F642}"` 与 `"中🙂"` 内容相同；`\u{0}` 同样允许内部 NUL。
花括号默认触发 `{expr}` 插值，`{{` / `}}` 输出字面花括号；转义只解码一次，`"\\n"` 的内容是反斜杠和字母 n。
`\0` 表示文本中的 NUL 字节，String 按字节长度传递，NUL 不截断后续内容。
普通单行字符串中的未知转义、裸 LF / CR、反斜杠续行和未闭合字符串均为解析错误，
定位使用原始源码字节范围。`r` 前缀与三引号是可组合的两个维度，四种写法都产生 String：

| 写法 | 解码转义 | 插值 | 文本跨行 |
| --- | --- | --- | --- |
| `"..."` | 是 | 是 | 否 |
| `r"..."`、`r#"..."#` | 否 | 否 | 否 |
| `"""..."""` | 是 | 是 | 是 |
| `r"""..."""`、`r#"""..."""#` | 否 | 否 | 是 |

原始字符串中反斜杠与花括号都是普通字符，`r"\d+"` 保留反斜杠，`r"{value}"` 不插值。
内容需要双引号时，在 `r` 后添加 N 个 `#`，结束引号后跟相同数量的 `#`；
例如 `r#"{"k": "v"}"#`、`r##"ends with "#"##`。单个双引号定界的原始字符串仍不能跨行。
`r` 仅在紧跟零个或多个 `#` 再跟双引号时是前缀，其余情况仍可用作标识符。

三引号字符串的开定界符后同一行只能有空格或 tab，随后必须换行；内容从下一行开始。
结束定界符前的同一行也只能有空格或 tab，这段前导空白是去缩进基准。
每个非空白内容行必须具有逐字符相同的前缀，去掉该前缀后保留额外缩进；
tab 不展开成空格，前缀不匹配时报错。纯空白行输出为空行。
开定界符后的换行、结束定界符前的换行不计入内容；要保留末尾换行，在结束定界符前再空一行。
源码中的 CRLF 与 CR 统一为 LF；`\n`、`\r` 等转义生成的字符不作为去缩进的物理行边界。

```joky
let sql = """
    SELECT *
      FROM users
    """
// Equivalent to "SELECT *\n  FROM users".
let id = 7
let rendered = """
    id = {id}\tok
    """
// Equivalent to "id = 7\tok".
let pattern = r"""
    path ~ '\d+' {id}
    """
// Backslashes and braces remain literal; indentation is still stripped.
```

`r"""` 同样去缩进并归一化物理换行，只关闭转义和插值；它也支持 `#` 定界符，
例如用 `r#"""` 与 `"""#` 包围含有 `"""` 的内容。三引号普通字符串保留全部现有转义，
包括 NUL 与 Unicode 标量转义，且支持完整插值表达式；表达式内部的源码按普通代码解析，
其中的换行、注释和嵌套字符串不参与外层文本去缩进。结束定界符之后可继续普通表达式语法。
这些写法沿用相同的 String 所有权与三后端语义；文本拼接仍使用 `concat` 等接口，尚无字符串 `+`。

字符字面量使用单引号，必须恰好包含一个 Unicode 标量；支持与字符串相同的简单转义和
`\u{H...}` Unicode 转义。例如 `'a'`、`'中'`、`'\n'` 和 `'\u{1F642}'`。
字符值的类型是 `Rune`，对象布局为 4 字节，支持同型 `==` / `!=`，以及 Array/Slice、聚合、泛型和语言 CPS 参数/结果。
`Rune.from_u32(value)` 返回 `Option<Rune>`，代理项或超过 `0x10FFFF` 时返回 None；
`to_u32()` 显式取得 UInt32 数值。整数不能隐式定型为 Rune，也没有 wrapping `rune()` 构造。
Rune 不支持整数算术、位运算或排序比较；非法字符转义在词法阶段拒绝。
Rune 尚不支持直接宿主注册、extern 或 C 字段/借用 ABI；宿主应传 UInt32，再由语言受检转换。

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

运算符从高到低为一元 `-` / `!` / `~`、乘除与余数、加减、移位、`&`、`^`、`|`、比较、`&&`、`||`、赋值、分号序列。
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
所有整数类型支持 `%`，与 `*`、`/` 同级左结合。余数遵循向零截断除法，
非零余数与被除数同号：`-7 % 3 == -1`、`7 % -3 == 1`；有符号 `MIN % -1 == 0`。
除数为零沿用 `Division by zero` 运行时失败，浮点不支持 `%`。
CLI 源码运行渲染出错表达式；构建的独立程序向 stderr 报告错误及源码字节范围，以状态 1 退出。
嵌入原生代码的错误协议见[架构说明](architecture.md#原生错误传播)。
`@suspends` 函数的循环条件及循环体都可以挂起，
恢复时继续当前调用，不重复执行此前的赋值或副作用；循环中的 `parallel` 每轮创建新的分支。
默认情况下循环本身不会自动挂起；Rust 嵌入接口可为解释器设置 `InterpreterLimits`，
启用指令总预算、异步执行时间片和帧数量限制，详见 [解释器文档](interpreter.md#resource-limits)。
支持 `+=`、`-=`、`*=`、`/=`、`%=`、`&=`、`|=`、`^=`、`<<=`、`>>=`，
优先级与 `=` 相同，右结合，结果为 Unit；两侧类型和运算规则与对应二元运算一致。
目标可以是 `var`、其字段或可变引用指向的值。目标地址只求值一次，先快照旧值，再求右侧，
最后计算并写回；右侧挂起不会重算地址或旧值，计算失败不会写回。
例如 `var x = 10; x += { x = 100; 2 }` 最终为 12；右侧自身的副作用遵循既有借用规则，
不会因计算失败而回滚。浮点只支持四则复合赋值，逻辑运算没有复合赋值。

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
else 可以换行，但不能用分号与 then 块分开。`||` 按最长匹配分词，不是两个 parallel 分支符。
完整可运行示例见 [examples/conditionals.jk](../examples/conditionals.jk)。

## 序列与字面量模式

`match`、`if let` 和 `while let` 接受数组/切片的序列模式，并复用枚举的求值一次、
分支作用域、分支结果同型及穷尽检查。元素模式可嵌套枚举、元组、记录、序列和标量字面量。
整数按被匹配的整数类型检查范围；Bool 和 Rune 按各自类型匹配，不调用用户相等协议。

| 模式 | 长度与绑定 |
| --- | --- |
| `[]` / `[x]` / `[x, y]` | 恰好 0 / 1 / 2 项 |
| `[head, ..]` | 至少 1 项，忽略余下部分 |
| `[head, ..tail]` | 至少 1 项，绑定余段 |
| `[first, ..middle, last]` | 至少 2 项，绑定中间余段 |
| `[..]` | 任意长度 |

每个序列最多一个 `..`，可以位于开头、中间或结尾；`.._` 等同于 `..`。
名称绑定不能重复，包括余段名。没有 `..` 时严格匹配长度，不能隐式忽略多余元素。

```joky
import joky/array.{type Array}
import joky/slice

fn inspect(values: &Array<Int32>) -> Int32 {
    match values {
        [] => 0
        [0, ..] => 100
        [only] => only
        [first, ..middle, last] => first + last + int32(slice.len(middle))
    }
}
```

共享匹配 `match &values` 的 Copy 元素绑定为独立快照，拥有元素绑定为 `&T`；
命名余段始终为 `&Slice<T>`，不复制元素。独占匹配 `match &mut values` 的元素绑定为
`&mut T`，余段为 `&mut Slice<T>`，同一模式的不同元素及余段可同时使用；含引用元素不能
再绑定为独占元素引用。来源数组和外层拥有者仍受约束，视图存活期间不能扩容、移动、
覆盖或借用整个数组进行冲突访问。无引用元素与 Slice 的引用可跨挂起，仍不能逃逸原存储的生命周期。

按值匹配 FixedArray 复制值，原值仍可用；命名余段为 `FixedArray<T, N-K>`，K 是前后缀项数，
当前要求具体长度，类型中的 `N-K` 由编译器计算，不开放源码常量算术。
按值匹配 Array 消费整个数组，元素绑定取得所有权，命名余段是保序的 `Array<T>`，可继续移动
或跨挂起持有。忽略的拥有元素在匹配作用域清理。所有长度、字面量和嵌套标签检查通过以后
才开始移出元素；失败分支不会破坏后续分支看到的值。当前消费前缀使用保序移除，未承诺常数时间。

固定长度的 `[a, b]` 可穷尽 `FixedArray<T, 2>`；不可能的长度直接报 `invalid_pattern`。
动态长度的 `[]` 与 `[head, ..]` 合起来穷尽；`[]` 与 `[0, ..]` 则不穷尽。
元素值和枚举载荷也递归参与覆盖判断，重复或被前面分支覆盖的模式报 `unreachable_code`。
分析按有限长度类别及实际测试位置进行，保留编译工作量限额，不按大数组全部元素展开。
尚未实例化的固长 N 不支持带前后缀的模式；可使用不约束长度的 `[..]` 或共享/独占的全长余段。

`let` / `var` 还支持具体 FixedArray 的不可失败解构及共享解构，例如
`let [a, b]: FixedArray<Int32, 2> = [20, 22]`。按值余段是独立固长数组，共享余段是切片。
动态数组的 let/var 序列解构不开放，使用 `match` 或 `if let`。
行为覆盖见 [array_patterns.jk](../tests/fixtures/array_patterns.jk)，失败清理见
[array_patterns_cleanup.jk](../tests/fixtures/array_patterns_cleanup.jk)。

## 条件模式与模式循环

`if let Pattern = value { ... }` 与 `while let Pattern = value { ... }` 复用 `match` 的
枚举模式：限定或未限定变体、`_`、嵌套变体、元组载荷和记录载荷解构。
匹配对象必须是枚举值或枚举引用；顶层普通变量、元组和记录模式尚不支持。

```joky
fn fallback(value: Option<Int32>) -> Int32 {
    if let Some(n) = value { n } else { 42 }
}
fn drain(values: &mut Array<Int32>) {
    while let Some(n) = values.pop() {
        println(n)
    }
}
```

`if let` 的右侧只求值一次，成功后绑定仅在 then 块中有效。支持 `else`、`else if`
和 `else if let`；分支结果规则与 `if` 一致，没有 else 时成功块必须返回 Unit 或提前跳转。
失败不会创建载荷绑定。所有分支仍接受名称、类型和挂起检查，包括不可达的 else 分支。

`while let` 每轮重新求值右侧并匹配，失败退出，成功绑定只在本轮循环体中有效。
循环结果为 Unit，正常完成的循环体结果被丢弃；`continue` 清理本轮绑定后重新求值，
`break` 清理后退出，`return` 沿用函数退出规则。右侧与成功块均可挂起，恢复时不会重放
已执行的求值、匹配或副作用。

所有权与借用规则沿用 `match`：按值匹配移动拥有者，失败的模式及未使用载荷仍会清理；
匹配 `&value` / `&mut value` 保留原拥有者，Copy 载荷按值读取，非 Copy 载荷绑定为对应引用。
第一版不支持 `&&` 模式链或模式守卫。

## for 循环与迭代器

`for pattern in iterator { body }` 支持标准库 `joky/iter.Iterator` 和下述 Array/Slice/Map 借用游标。
右侧只求值一次：拥有值移动到循环的隐藏可变绑定，Copy 值按普通规则复制。
普通 Iterator 每轮同步调用 next，Some 的载荷绑定到 pattern，第一次 None 结束循环。

```joky
import joky/iter.{range}

fn sum() -> Int32 {
    var total = 0
    for n in range(0, 10) { total += n }
    total
}
```

`joky/iter` 的普通 Iterator 接口使用 Joky 源码和静态分派：

| 接口 | 行为 |
| --- | --- |
| `Iterator` | 关联类型 Item；`fn next(&mut self) @noescape -> Option<Self.Item>` |
| `next<I: Iterator>(&mut I) @noescape -> Option<I.Item>` | 显式选择 Iterator.next，忽略同名固有方法 |
| `Range` | Copy 的 Int32 递增迭代器，字段私有 |
| `range(start: Int32, end: Int32) -> Range` | 半开区间，包含 start、不包含 end；start >= end 时为空 |

range 不在 prelude 中，需要显式导入。next 在耗尽的 Range 上始终返回 None，
整数最大/最小边界按同样规则处理，不通过 end + 1 构造区间。
其他整数类型、步长、倒序、闭区间、范围运算符与 match 范围模式尚未提供。

自定义迭代器显式导入 `type Iterator` 并实现关联 Item 和 next。泛型函数必须声明
`I: Iterator`，可用 `I: Iterator<Item = Int32>` 限定元素类型；仅有同名 next 方法不足以参与 for。
生成调用固定标准协议身份，用户同名 Iterator、next、Option 或 Some 不改变循环语义。
语法所需标准库依赖自动装载，`--no-prelude` 下仍有效。

pattern 沿用 let 的不可失败模式，支持名字、`_`、元组、记录及已有的 FixedArray 解构，
不支持可失败枚举/字面量模式或自动过滤。绑定不可变且仅本轮有效，不同轮次可重复使用同名绑定。
循环结果为 Unit，正常完成的循环体末值丢弃；continue 清理本轮后再次 next，
break 退出最近的 while/for，return 退出函数，均遵守原有自动清理规则。
输入表达式在新循环开始之前求值，其中的 break/continue 仍指向外层循环。

Item 可以是受支持的普通值或拥有值；每项的移动、忽略、失败清理沿用 Option/match。
迭代器在结束或提前退出时清理；循环体可在挂起函数中挂起，恢复不会重新求值输入或重放 next。
迭代器、元素及引用仍须满足现有跨挂起存储资格，取消先清理子调用再清理本轮元素和迭代器。
当前不自动将 Array/Map 转成迭代器。Array 显式选择 into_iter、iter 或 iter_mut；
Map 可选择 iter/iter_mut 成对遍历，也可通过 Slice 接口遍历 keys/values/values_mut；
IntoIterator 与 map/filter 等组合器仍属于[后续工作](for-iterators.md)。

### Array 消费式迭代

`joky/array.IntoIter<T>` 是不可复制的拥有型迭代器，字段私有，实现
`Iterator<Item = T>`。`array.into_iter(values)` 与 `values.into_iter()` 使用同一实现。
即使元素是 Copy 或数组为空，转换也消费原数组；转换后不能再次使用原绑定。
存在活动元素/切片借用时不能转换，迭代器本身也不能重复移动。

```joky
fn sum_values() -> Int32 {
    let values = [10, 20, 30]
    var total = 0
    for value in values.into_iter() { total += value }
    total
}
```

元素按转换前的下标顺序交付，支持 Array 原有的 Copy、String、拥有记录和嵌套数组等元素。
每次 next 移交一个值的所有权，不 clone 元素；耗尽后继续 next 返回 None。
已交付的拥有值独立于迭代器存活；共享引用元素仍依赖其原始内容来源。
未消费的元素在迭代器退出作用域、被丢弃、发生失败或任务取消时恰好清理一次，
不承诺剩余元素的析构次序。循环体可挂起，存储树仍须满足原有跨挂起资格。

当前实现转换时原地反转一次，随后从末端移出，因此初始化 O(n)、每次 next O(1)，
不另建元素缓冲区，也不逐项从头部删除搬移后缀。转换和取项过程中均保持完整的已初始化前缀，
复用 Array 的失败与取消清理。耗尽后容量仍归迭代器所有，迭代器清理时才释放缓冲区；
这些复杂度描述不代表进行过性能测量。

### Array 借用式迭代

`values.iter()` 返回 `&joky/iter.Iter<T>`，`values.iter_mut()` 返回 `&mut Iter<T>`；
模块函数 `array.iter(&values)` / `array.iter_mut(&mut values)` 等价。
它们按下标正序借用原数组，分别给出 `&T` / `&mut T`，不消费数组或移出元素。
支持与 borrow_ref 相同的无引用元素，包括普通 Copy 数据、String、拥有记录和嵌套数组；
含引用的元素仍不支持直接引用遍历。

```joky
fn increment() -> Int32 {
    var values = [1, 2, 3]
    for value in values.iter_mut() { *value += 10 }
    var total = 0
    for value in values.iter() { total += *value }
    total
}
```

Iter 是编译器识别的借用目标，只能通过 `&Iter<T>` / `&mut Iter<T>` 使用。
它不实现普通 Iterator，不能作为 `I: Iterator` 的参数；独占引用不进入 Option，
现有视图聚合、泛型实参与引用存储限制保持不变。显式类型为 Iter 的借用参数可用于泛型转发。
`for` 通过游标类型选择下面的两步操作，同名用户函数不能改变生成调用：

| joky/iter 接口 | 行为 |
| --- | --- |
| `advance<T>(&Iter<T>) -> Bool` | 同步前进一步；成功后存在当前项，空游标及耗尽后始终 false |
| `current<T>(&Iter<T>) -> &T` | 借用当前元素，不改变位置 |
| `current_mut<T>(&mut Iter<T>) -> &mut T` | 独占借用当前元素，不改变位置 |

首次 advance 前或 advance 返回 false 后调用 current/current_mut 会产生受检运行时错误。
手动推进后再进入 for，会从下一项继续；break/continue/return 沿用普通循环语义。
共享游标可在本地复制，但副本共享位置；重新调用数组的 iter 才会创建独立游标。
独占游标按现有引用规则移动进循环；若循环后还要使用同一游标，写 `for value in &mut *cursor` 显式重借用。
advance 只修改游标位置，因此先前的共享元素引用可跨越后续 advance，仍指向原来的元素。
独占元素引用仍活跃时不能再 advance 或取得冲突借用；检查保守地借用整个数组，
不根据索引证明不同元素互不重叠。引用最后一次使用后可继续推进或再次借用。

游标和元素引用的来源始终追溯到原数组；借用存活期间不能移动、释放、扩容或结构性修改数组。
共享遍历允许其他共享读取，可变遍历期间禁止通过原数组进行冲突访问。
元素引用可比游标的局部绑定活得更久，但不能逃出其数组来源的有效期。
循环绑定本身不可重新赋值；`&mut T` 仍允许通过 `*value` 或字段修改元素。

循环体可挂起，游标和元素按现有无引用存储树规则保活；恢复不重放 advance。
正常结束、提前退出、失败和取消都不由游标析构元素，元素始终归原数组清理。
构造游标 O(1)，分配一个任务 arena 内的小型描述符；每次 advance/current 为 O(1)、不分配。
描述符随任务 arena 释放，不承诺在游标最后一次使用时立即回收；未进行性能测量。

### Slice 与 Map 单列遍历

`joky/slice` 提供两个模块函数，沿用上述 Iter 的推进、重借用、耗尽和元素资格规则：

| 接口 | 行为 |
| --- | --- |
| `iter<T>(&Slice<T>) -> &Iter<T>` | 按切片下标正序借用，每项为 &T |
| `iter_mut<T>(&mut Slice<T>) -> &mut Iter<T>` | 按切片下标正序独占借用，每项为 &mut T |

子切片的游标只访问该子区间，空切片不产生元素。创建游标不复制载荷、不消费 Slice 或源容器，
每次创建分配一个 O(1) 的任务 arena 描述符；每次取项 O(1)，不分配。
引用来源经 Slice/子切片继续追溯到原 Array 或 Map，元素引用可以超过局部视图绑定的作用域，
但不能超过来源的有效期。最后一次借用使用后才恢复源容器的冲突操作权限。

```joky
import joky/map.{type Map}
import joky/slice

fn update_values() -> Int32 {
    var table = Map.new::<Int32, Int32>()
    table.insert(1, 10)
    table.insert(2, 20)
    for value in slice.iter_mut(table.values_mut()) { *value += 1 }
    var total = 0
    for key in slice.iter(table.keys()) { total += *key }
    for value in slice.iter(table.values()) { total += *value }
    total
}
```

Map 的键始终只读，values_mut 只允许修改值；活动游标/元素借用仍依赖整个 Map，
不能同时插入、删除、移动、释放，或通过独占视图的其他来源冲突访问。
遍历顺序未指定；共享 keys/values 的相同下标在借用期间对应同一项，删除重插后不承诺保持旧顺序。
这三个入口提供单列借用遍历；成对遍历使用下面的 Entry 视图，不构造 `(key, value)` 借用元组。


### Map 成对借用遍历

`table.iter()` 返回 `&map.Iter<K, V>`，`table.iter_mut()` 返回 `&mut map.Iter<K, V>`；
同名模块函数接收 `&Map` / `&mut Map`。for 每轮分别绑定 `&map.Entry<K, V>` / `&mut map.Entry<K, V>`，
顺序未指定，元素不复制、不移出 Map。K/V 均须满足直接元素引用的无引用存储资格。

```joky
import joky/map.{type Map}

fn update_entries() -> Int32 {
    var table = Map.new::<Int32, Int32>()
    table.insert(1, 10)
    table.insert(2, 20)
    for entry in table.iter_mut() {
        let delta = *map.entry_key(entry)
        *map.entry_value_mut(entry) += delta
    }
    var total = 0
    for entry in table.iter() {
        total += *map.entry_key(entry) + *map.entry_value(entry)
    }
    total
}
```

| joky/map 接口 | 行为 |
| --- | --- |
| `advance<K, V>(&Iter<K, V>) -> Bool` | 同步推进两列，耗尽后始终 false |
| `current<K, V>(&Iter<K, V>) -> &Entry<K, V>` | 取得当前项的稳定共享视图 |
| `current_mut<K, V>(&mut Iter<K, V>) -> &mut Entry<K, V>` | 取得当前项的稳定独占视图 |
| `entry_key<K, V>(&Entry<K, V>) -> &K` | 只读 key，包含可变遍历场景 |
| `entry_value<K, V>(&Entry<K, V>) -> &V` | 只读 value |
| `entry_value_mut<K, V>(&mut Entry<K, V>) -> &mut V` | 独占 value，可原地修改或 mem.replace |

首次推进前、耗尽后的 current/current_mut 受检失败。共享游标别名共享位置；每次 current
保存同一项的 key/value 地址，之后推进不改变旧 Entry 的身份。共享 Entry 及其元素引用
可以跨推进或局部游标绑定作用域存活，但始终依赖原 Map。独占游标用 `&mut *cursor` 重借用后
可在 for 提前退出后继续使用；活跃的独占 Entry/元素借用阻止推进及冲突访问。

Entry 是编译器识别的借用目标，不是拥有记录或普通元组；不能按值使用、解构成引用元组、
存入 Option/记录/数组或传给宿主。借用仍保守地覆盖整个 Map，不证明 key/value 字段不重叠：
不能在 key 引用仍活跃时取得可变 value。先读取或计算 key，再调用 entry_value_mut，如上例。
共享读取 key 和 value 可同时进行；没有可变 key 接口。借用最后一次使用后恢复原 Map 的权限。

Iter/Entry 的实际类型别名分别是 `joky/iter.Entries<K, V>` / `Entry<K, V>`。
for 使用该模块的 entries_advance、entries_current、entries_current_mut 固定入口；
它们与 map 的手动接口语义相同，不实现普通 Iterator，也不改变其 @noescape 或 Item 契约。
符合现有存储资格时，Entry 与其元素可跨语言挂起，恢复不重放取项，取消只由 Map 清理拥有元素。

当前创建游标 O(1)，分配一个任务 arena 描述符；advance O(1)、不分配。
每次 current/current_mut 为稳定 Entry 分配一个只保存两项地址的小型描述符，耗时 O(1)，
不复制 key/value；for 每轮一次。因此遍历 n 项额外使用 O(n) 描述符空间，重复手动 current 也累计计费。
这些描述符随根任务 arena 统一释放，预算不足时终止任务并正常清理 Map；不承诺提前回收。
这是当前实现成本，尚未进行性能测量。

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
同一行的独立表达式必须用分号分隔，`var x = 1 x = x + 1` 会报错。
`f(x)(y)` 连续调用两次；若要丢弃第一次调用结果再求值 y，写 `f(x); (y)`。
完整表达式后的换行分隔下一表达式，不插入丢弃值的分号。`5` 换行接 `-2` 是两个表达式，
块的结果为 -2；`let r = &mut *p` 后可直接换行写 `*r = *r - 1`。
未完整的表达式继续解析：`1 +` 换行接 `2` 仍是加法，`let value =` 后也可换行写初始化值。
圆括号、元组和调用参数列表内，换行仍是空白，`(1` 换行接 `+ 2)` 仍是加法；
实参之间仍须有逗号。嵌套的 `{ ... }` 块与记录初始化项恢复换行分隔。

下一行以 `.` 开头时明确继续字段或方法链，允许中间有注释、空行和 CRLF：

```joky
let text = "abc"
let length = text
    // Continue the receiver above.
    .byte_length()
```

`.` 之前有分号时链条已结束，不能用 `.method()` 开始下一表达式。
圆括号外，被调用表达式或方法名与调用的 `(` 必须在同一行；
`f` 换行接 `(42)` 是两个表达式，`f(` 后的参数可以跨行。
二元运算符放在前一行末尾，或把完整计算放入圆括号；行首的 `-`、`*`、`&` 开始新的独立表达式。

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
不可达并报错。块内 `return` 后换行解析为裸 return，下一行的值不属于返回值；
跨行返回值使用 `return (` 后换行写表达式，再关闭 `)`。圆括号内部换行仍为空白。
在 `;`、`}`、`)`、`,`、parallel/race 分支符 `|` 或输入末尾前也解析为裸 return。
二元或一元运算中使用跳转需加括号或块。

```joky
fn positive(value: Int32) -> Int32 {
    if value <= 0 { return 0 }
    value
}
```

`break` 退出当前函数最近一层 `while` 或 `for`，`continue` 回到该循环的条件检查或取下一项。
二者不带值或标签，循环外使用报错；不能跳到调用者的循环。循环条件也在该循环的跳转范围内。
若条件在所有路径上都提前跳转，循环体必须为空：`while (break) {}` 正常结束，
`while (return 42) {}` 返回函数，`while (continue) {}` 持续重试条件。
条件为字面量 `true`（可加圆括号），且没有退出当前循环的 `break` 时，循环没有正常结束路径，
不需要在循环后补返回值；后续表达式会报不可达错误。`return`、`?` 或 `panic` 可以退出函数或终止执行，
`continue` 只开始下一轮，内层循环的 `break` 不会让外层循环正常结束。
其他 Bool 条件仍保守地视为可能结束，不推导常量绑定或 `1 == 1`；`if false { break }` 也保守计作出口。

跳转不产生正常值；尾随分号只丢弃正常值，`return 42;` 仍返回 42，`break;` 仍退出循环。
类型检查拒绝结构上不可达的后续表达式，包括序列、运算右侧和后续实参；不按常量折叠删除路径。
例如 `(return 1) + 2` 报不可达错误，`1 + (return 2)` 返回 2。
`if` 两条分支都提前跳转时，整个 if 也不会正常结束；仍可正常结束的路径必须满足原有类型约束。

`@suspends` 返回值和跳转之前的调用可多次挂起，恢复不重放已完成的求值。
失败或取消不会转为成功返回。parallel/race 分支中的 return 仅结束该分支；调用者跳转前须等整个操作结束。
parallel 等全部分支完成，race 在首个分支完成并取消其他分支后结束。
两者的实参都先于所有分支求值，实参中的跳转可以阻止整个并发操作启动。
完整示例 [examples/jumps.jk](../examples/jumps.jk) 依次输出 `0`、`4`、`13`。

## Rust 嵌入持久实例

`joky::vm` 提供 `InstanceId`、`InstanceValue`、`InstanceStatus`、`InstanceLimits` 和 `InstanceCleanupReport`。
JIT 与 Interpreter 都支持；游戏脚本可选 Interpreter，无需为脚本生成原生代码。
这是宿主 API，不是 `joky/vm` 模块内的语言函数。
[Godot G0–G2 适配器](../integrations/godot/README.md)使用此接口提供解释器脚本实例。

```rust
use joky::vm::{InstanceValue as V, VmEngine, VmMode};

let mut vm = VmEngine::with_mode(VmMode::Interpreter);
vm.load_source(r#"
    struct State { count: Int32 elapsed: Float64 }
    fn make(count: Int32) -> State { State { count: count elapsed: 0.0 } }
    fn tick(state: &mut State, delta: Float64) -> Int32 {
        state.elapsed = state.elapsed + delta
        state.count = state.count + 1
        state.count
    }
"#)?;
let id = vm.create_instance("make", &[V::Int32(10)])?;
assert_eq!(vm.call_instance(id, "tick", &[V::Float64(0.016)])?, V::Int32(11));
vm.close_instance(id)?;
Ok::<(), joky::Error>(())
```

构造函数必须返回无引用的记录；允许所有现有数值类型、Bool、Rune、Unit，以及 String、
元素递归无引用的 Array、Map、StringBuilder、含这些字段的嵌套记录/枚举/元组、递归拥有树和 Drop。类型筛选穿透别名、
具体泛型实参、零大小字段和全部嵌套元素；任何层级的引用、视图、Ptr、宿主 opaque、
函数值均不允许。缓冲区浅层元素身份会按完整定义检查；Map 的 key/value 均须通过完整检查，空 Map 也不能隐藏不合格类型。
构造参数、方法额外参数和结果都是 `InstanceValue` 标量，
严格同型，不隐式做数值转换；Unit 参数仍需显式传入 `V::Unit`。

方法入口是顶层同步函数，首参为该实例名义类型的 `&State` 或 `&mut State`，且编译器
证明首参借用不逃逸。通常由函数体自动推导，无需写 `@noescape`；显式标注仍可作为契约。
可以在此入口内调用固有方法；宿主暂不直接查找 impl 成员。
普通 `function_signature` / 标量调用不暴露这类引用入口；`instance_type(id)` 返回根类型快照。
`instance_function_signature(name)` 只读取当前模块经编译器检查的实例入口签名，不执行构造、
不创建根、不暴露地址；不合格或不存在的入口返回 None。旧实例的方法仍由其旧模块决定。

`create_instance` 成功才发布 ID。每次 `call_instance` 使用独立临时存储，根状态持续到关闭。
签名、句柄或重入检查失败保持原状态；进入方法后的执行错误标记 Failed，停止所属任务后在安全出口释放根，
不能继续调用，外部副作用不回滚。用 `instance_status` 和 `instance_error` 查询状态/首错。
状态有 Ready、Busy、Failed；关闭后查询报错。同实例同步重入被拒绝，不同实例可交叉调用。
回调中关闭立即使 ID 失效，实际存储在当前调用及活跃任务 stop 结束后释放，该调用不发布成功结果；
方法已报错时保留该错误，否则返回关闭错误。

ID 绑定所属 VM，关闭后不复用；重复关闭和跨 VM 使用均报错。VM 销毁释放全部实例。
重载成功后新实例使用新代码，旧实例继续绑定旧布局和方法；加载失败保留原模块。
P1–P3 不支持接收者跨挂起、保状态热迁移或 C VM 实例 API。

拥有字段重借用沿用既有规则，替换使用 `mem.replace(&mut state.field, next)`，
Array 可原地 push、replace、clear；Map 可扩容、插入、替换、删除和 clear，StringBuilder 可追加、
reserve、clear，移出后可消费式 finish；直接按值读取借用字段仍被拒绝。clear 保留容器容量，
用 mem.replace 换成空容器或关闭实例才释放预留容量。调用不会深复制状态，
只迁入拥有分配的 arena 登记，并在出口保留最终可达的拥有图；临时值不延长到实例寿命。
入口转移预检失败保持 Ready，执行、Drop 或出口预算/转移失败进入 Failed，不发布结果。

`set_instance_limits` 的 `max_instances`、`max_bytes`、`max_call_bytes`、`max_peak_bytes`
默认分别为 4096 个身份、64 MiB 活根、64 MiB 调用额外分配、128 MiB 实例与活跃调用合计。
导入已有拥有分配不重复计费，出口计入新持有的载荷；释放旧值退还 arena 计费。
Failed 身份占数量，安全出口清理后不占活根字节，需关闭才能退还身份配额。计费包含分配载荷、对齐与
逻辑登记项、实例及根/Buffer 元素清理描述项；不包含 Rust 容器预留容量、解释器局部栈、编译代码、诊断
和其他普通 VM 调用/异步任务，不代表进程 RSS。普通任务的预算保持原有规则。
`instance_count` / `instance_bytes` 返回身份数量和最近持久出口的活根计费，
`instance_peak_bytes` 返回实例及其活跃调用当前的合计计费，不是历史最高值。
解释器执行限额独立；JIT 调用不受解释器指令预算抢占。

`close_instance` 先使 ID 失效并取消/stop 所属任务，再运行 Drop；Drop 报错仍继续释放其他拥有字段，返回首错。
VM 销毁也继续清理全部实例；可预先保存 `vm.instance_cleanup_report()`，销毁后通过
`report.error()` 检查首个根清理错误。`instance_cleanup_error()` 可在 VM 存活时查询同一报告。

## when 状态域操作

`when` 在具名、非泛型 `@suspends` 函数中访问 1–64 个域；需要导入 `joky/domain`。
访问列表只接受已经绑定的句柄名。外层名字表示句柄，块内同名名字表示状态引用：

```joky
import joky/domain

#state
struct Counter { value: Int32 }
fn make(value: Int32) -> Counter { Counter { value: value } }
impl Counter {
    fn add(&mut self, first: Int32, second: Int32) -> Int32 {
        self.value += first + second
        self.value
    }
}
fn main() @suspends -> Int32 {
    let owner = domain.create(make, 10)
    let counter = owner.handle()
    let amount = 2
    let result = when "increment" (mut counter) {
        counter.value += amount
        counter.value
    }
    result.unwrap() + counter.add(1, 2).unwrap()
}
```

- `when (counter) { counter.value }` 是匿名形式；`when "increment" (...) { ... }` 是具名形式。
  标签为非空普通字符串字面量，不支持插值。按解码后的内容区分大小写，同一所属函数内唯一，
  包括不同分支和嵌套作用域；循环反复执行同一个块不算重复。不同函数可以使用相同标签。
- `mut counter` 需要 `WriteHandle<S>`，块内获得 `&mut S`；不写 `mut` 时，读／写句柄都只授予 `&S`。
  访问列表最多 64 个绑定位置。不同绑定可以指向同一只读域，任何涉及写入的别名在执行前拒绝。
- 块体同步执行，复用完整可达调用链的域操作证明。不能挂起、调用宿主或网络效果、嵌套 `when`、
  捕获函数值或让状态引用逃逸。首版不支持块内闭包及 `return`；`break`、`continue` 只能作用于块内循环。
  `scope { branch { ... } }` 中的块仍归属于外层具名函数。`parallel`／`race` 的分支仍须为函数调用，
  可通过具名辅助函数包含 `when`。
- `outbox.emit(handle, &bytes)` 在当前操作内登记 `Array<UInt8>` 消息，可通过同步业务辅助函数调用。
  它复制字节到有界暂存批次，不执行 I/O；正常成功时所有目标容量验证、状态替换与消息入队
  在同一轮完成，再唤醒接收者。业务 Err、运行异常、资源不足或任一队列拒绝均丢弃整批。
  `DomainAccessError.OutboxFull(UInt64)` / `OutboxClosed(UInt64)` 携带失败队列身份。
  每次操作默认最多 1024 条、1 MiB 消息；宿主通过 `DomainLimits.max_operation_messages` /
  `max_operation_bytes` 配置，批次自身超限返回 `ResourceLimit`。描述符和字节均计入域预算。
  普通域读写、构造器和迁移不得调用登记效果；在事务外直接调用 emit 会报告运行错误。
- 其他外层局部变量按名称排序，按传递值规则封装成独立捕获包，在入队前固定；Copy 值复制，
  String／Array 等拥有值按值移动，调用方不能继续使用被消费的值。可变 Copy 捕获在块内的赋值不修改外层变量。
  捕获包及块末尾结果支持 String、Array 及合格组合，不允许引用、资源或函数值；
  队列保存独立数据，不转移活根内部引用。
  多个捕获共同计入下述 64 个结构项、64 槽、8 层聚合限制，编译器生成的外层捕获记录也计入。
  枚举允许 `Option<T>`、`Result<T,E>` 等具体实例，所有分支载荷和类型参数都须符合传递值资格。
- 表达式结果是 `Result<R, domain.DomainAccessError>`，其中 R 为块末尾表达式类型。
  关闭、域隔离、非法访问、入口不兼容及资源不足保留现有分类。参数／捕获包准备失败仍可走
  任务失败通道；候选复制、执行或结果预留遇到资源不足时返回 `ResourceLimit`，不发布候选。
  算术异常等块体错误仍终结当前调用，但不会隔离或修改参与域。
- 每次操作深复制完整访问集合，块体只读写这些独立候选。标准 `Result<T,E>` 的顶层 `Err`
  丢弃全部候选；顶层 `Ok` 或普通非 Result 返回值成功发布。`false`、`Ok(false)`、`None`
  以及自定义枚举均不隐式回滚。业务错误作为 `Ok(Err(e))` 返回，与外层运行时拒绝区分。
- 只有成功发布才推进声明可写域的修订。候选准备、同步计算及发布在同一 FIFO 执行轮次内完成，
  不跨挂起点或切换代码版本，因此此路径没有中途并发冲突，也不隐式重试。深复制计入临时峰值预算。
  取消尚未执行的申请不修改状态；发布后的取消只丢弃结果。发布后在调用方复制／解码拥有型结果
  仍可能分配失败，这类交付错误走任务失败通道，不撤销已发布状态。需跨挂起计算时继续使用显式快照与候选 API。
- 此阶段尚未支持块内消息登记。网络发送必须在块外；状态与消息共同发布仍使用候选的 `with_outbox`。

`when` 是上下文关键字：`when (...) { ... }` 或 `when "标签" (...) { ... }` 才识别为状态操作。
普通标识符／函数仍可叫 `when`；条件中的普通调用紧邻 `{` 时，使用 `(when(...))` 消除歧义。

### when 身份与热更新

具名块的稳定身份由逻辑模块名、所属函数限定名和标签组成，独立于行号、块顺序和编译进程地址。
参数捕获字段名／顺序／类型、域顺序／权限／布局、结果类型另作兼容契约检查。
移动代码或插入其他块不改变具名身份；改名、删除或不兼容契约会返回 `Incompatible`，不退回旧块体。
兼容更新后，旧流程仍使用原捕获值，由第一个参与域当时绑定的最新实现执行。

入口模块的逻辑名默认 `main`，表示当前发布入口，与物理文件名无关；Rust 宿主可在解析后设置
`parser::ast::Module.logical_name`，再交给 `load_module` 或对象编译入口。逻辑名须满足用户模块路径规则。
导入模块使用规范 import 路径。临时文件路径、源码展示名称都不参与操作身份。
匿名块仅在同一次模块编译中匹配；提供实现的模块发生替换后，旧匿名请求返回 `Incompatible`。
AOT 中两种形式均可用；AOT 中的块不会自动成为另一个 Script 模块的实现，应由 Script 自己声明业务块。

### #state 与句柄方法

`#state` 可标在非泛型 struct 上，声明时检查整个状态存储图：允许合格的 String、Array、Map、
嵌套记录等拥有存储，禁止引用、资源、函数和用户清理钩子。不自动创建域；未标注但合格的旧记录仍可使用。
属性不表示所有方法都合格，作为域操作调用的方法仍须通过效果及不逃逸证明。

`handle.method(arg1, arg2)` 可调用状态类型的非泛型固有方法，接收者必须为 `&self` 或 `&mut self`。
它在 `@suspends` 流程中等待，与 `when` 共用候选发布和业务 `Err` 回滚规则，返回同样的访问 Result。
参数在调用方按顺序求值，再合成有界传递值包；
读句柄不能调用可写方法。方法按模块／类型／方法身份匹配当前实现，旧独立函数 API 保持可用。
句柄本身已有的固有方法和可见 trait 方法优先，例如 `read_only()`；发生名称遮蔽时用 `when`
访问状态方法。首版不通过句柄转发状态 trait 方法、泛型方法或按值消耗状态的方法。
普通状态值及 `when` 内的状态引用仍使用普通同步方法调用。

### 业务流程中的选择

- 单域已有合格固有方法时，用句柄方法；多个域需要一起读取或修改时，用 `when`。
  跨热更新继续执行的业务块使用有稳定含义的标签，显式处理 `Incompatible` 等访问错误。
- 网络读取、等待和目录查询放在块外，先得到所需句柄和纯值输入，再进入同步块。
  捕获固定的是申请时的输入，块体则可能来自新版本；发布者仍须保证业务含义兼容。
- 用标准 `Result<T,E>` 表达需要回滚的业务拒绝；普通 `false` 仍表示正常完成。
  `domain.write_to` 等显式原地短操作保留旧语义，不因返回业务 `Err` 而回滚。
  若要在挂起计算后验证依赖并一次发布多个候选，使用快照与候选 API。
- 应答必须与状态提交绑定时，通过候选的 `with_outbox` 附加消息，提交成功后由宿主发送。
  单独的 `when` 写入加后续发送不具有这一保证，发送失败或调用者取消不会撤销已完成的修改。

[完整服务端示例](aot-script-hot-reload.md#111-hr7-服务器联合功能验收已实现)组合目录、具名块、句柄方法、
候选信箱、代码发布和连接关闭；[Script 源码](../tests/fixtures/server_reload_script.jk)保留两种提交路径以说明边界。

## Rust 宿主状态域

SC1 提供 Rust 状态域所有者、独立请求任务，以及下节的源码 `joky/domain` 生命周期入口。
源码短操作支持单个有界纯值输入与结果（见下节）；Rust 宿主入口保持下述标量参数列表。
`joky::vm` 导出 `DomainOwner`、`DomainHandle`、`DomainRequest`、`DomainOutcome` 和 `DomainLimits`。
JIT 与 Interpreter 可使用 Rust 接口；源码生命周期同时支持独立 AOT，尚未开放 C 状态域 API。

`VmEngine::create_domain(constructor, args)` 返回唯一 Owner；构造函数必须返回普通、非泛型的
记录；HR1 允许字段递归包含标量、String、嵌套记录、元组、无拥有载荷的枚举与
受检 Buffer 容器，包括 `Array<Record>`、`Map<K, Record>`，记录内还可拥有其他容器。
嵌套泛型记录按完整字段和实参检查，不靠容器名字获取资格；泛型最外层根仍未开放。
参数与操作结果仍使用 `InstanceValue` 的标量类型。整个存储图（含空容器的元素类型）拒绝
引用、视图、裸指针、闭包、外部资源、用户 Drop、拥有载荷枚举及递归类型。类型展开深度
最多 32、访问节点最多 65,536；超限不能成为域入口。
`owner.read_handle()` 或 `write_handle()` 产生可复制句柄；`handle.read_only()` 只能降级权限。
句柄不保活状态，跨 VM 使用、关闭后使用及不匹配的名义类型都会被拒绝。

`vm.queue_domain_call(handle, method, args)` 验证类型和权限，复制输入并返回唯一 Request，
此时不执行操作体。方法的首参必须是对应记录的受检不逃逸引用，余参和结果为标量。
编译器还必须证明操作及其直接调用链同步且没有宿主调用、间接调用、任务操作或用户 Drop。
受检 String/Buffer 操作和标量 `alloc` 临时存储可以使用；临时值也不能带用户清理。
每次同步调用结束后只保留完整根拥有图，临时分配回收；普通同步函数不自动合格。
只读句柄不能调用 `&mut` 方法；拥有字段的替换沿用 `mem.replace` / `mem.swap` 借用规则。

短操作与请求根统一由 `vm.poll_tasks(max_polls)` 驱动，旧的 `poll_domains` 入口已移除。
每次轮询只处理有限就绪快照，短操作也消耗一个 poll 配额；完成后唤醒等待它的请求，
请求在后续轮询恢复。内部短操作不出现在 `PollReport.completed` 中，也不提供时间预算或 CPU 抢占。
`request.take_result()` 返回 `Option<DomainOutcome>`：`Value`、`Failed`、`Cancelled` 或 `Closed`；
None 表示仍在排队或已经领取，结果只能领取一次。`request.cancel()` 撤销未执行操作，
Drop 自动移除登记；已经完成的结果不被取消覆盖。

`vm.domain_revision(handle)` 查询当前修订，初值为 0。正常返回的写操作推进一次，业务返回
false 也一样；只读操作不推进。写操作运行时失败隔离并释放该域根，关联排队项失败，其他域继续；
只读失败保留域。操作不提供失败回滚。`owner.close()` 或 Owner Drop 释放根并使排队项返回 Closed；
VM 销毁后尚未领取的 Request 返回 Closed，Owner 和 Request 不延长 VM 生命周期。

`set_domain_limits` 设置后续准入限制，不能缩小到排除仍存活的登记。默认最多 4096 个域、
64 MiB 状态存储峰值（活根、候选及同步调用临时存储）、4096 个请求和 4 MiB 请求逻辑存储。已完成未领取的结果仍占配额；
请求逻辑存储包括固定登记项、方法名和输入位槽，不是进程内存上限。
当前没有等待配额的隐含队列；额度不足直接返回 ResourceLimit。

`vm.publish_domain_code()` 把结构兼容的既有域切换到当前模块并返回切换数量；单纯
`load_source` 只影响之后创建的域。兼容要求同名记录的字段名、可见性、布局与类型逐一相同；
任一同名记录结构变化时返回 `TypeMismatch`，所有域保持原样。发布不改变根数据、修订号和句柄。
之后的兼容单域调用执行新代码；已挂起的请求继续运行启动时的代码，其单域操作按调用方签名
检查兼容，发布前已排队、签名已变化的单域调用以 `TypeMismatch` 失败而域保持可用。
具名 pair/access 从第一个参与域的当前模块解析操作，其他域可以绑定不同模块；参数顺序也决定代码归属。
准入及执行前都检查完整参数／返回签名、引用读写模式、同步无宿主无逃逸证明，以及全部域的当前布局与权限。
入口缺失或签名／布局不兼容时以 `TypeMismatch` 拒绝整次调用，不写入、不推进修订、不隔离域。
单域与联合快照在执行时保存数据修订、代码版本
和 schema 身份；写目标或只读依赖的数据、代码、schema 变化都会使旧候选提交返回
`Conflict`，不因参与域的读写角色改变分类。代码版本耗尽时整批发布返回 `ResourceLimit`，
不回绕。具名晚绑定不适用于已经完成的候选计算，也不透明重放流程；实现范围见[热更新调用规则](aot-script-hot-reload.md#33-调用侧规则)。

`spawn_domain_task(handle, name, args)` 在状态当前绑定的模块中创建未启动的标量请求根；它继承句柄
的读写权限，具有独立 arena 和 fail-fast 组，首次执行前登记到域所有者。普通请求运行时失败
只结束该请求及其结构化子任务；受检写操作失败仍隔离状态域并取消关联请求。
`cancel_domain_task(id)` 取消单个请求，`domain_task_count(handle)` 查询关联请求数，
`task_count()` 查询执行器总登记数（包括内部短操作）。`configure_tasks` 只允许在任务、域和
结果登记均为空时更换执行器。请求根与短操作共用执行器任务额度，满额在执行操作体前失败。

Rust 自定义挂起适配器可用 `queue_current_domain_call` 提交所属域的操作，将 `DomainRequest`
放入自己的帧清理，并调用 `register_current_wait()` 登记当前根的唤醒 token。该方法拒绝
其他执行器和第二个等待根；stop 中必须销毁 Request，撤销排队与结果额度。直接运行没有
域任务环境的 continuation 不能使用此上下文，宿主在回调中嵌套运行另一个 continuation
也不会把外层权限传给内层。

## 内置域访问 `joky/domain`

`joky/domain` 在 JIT、解释器及独立 AOT 中使用同一执行器、状态注册表和关闭协议。
S 必须是普通非泛型记录。单域、双域及集合均支持上节所述受检嵌套记录和 String、Array/Map 拥有图。
标量输入 A 与结果 R 支持 Int8/16/32/64、UInt8/16/32/64、Float32/64、Bool、Rune 和 Unit；
`create` 的输入、具名短操作及异步请求的输入／结果另支持有界嵌套传递值，具体范围见下述能力表。
构造、带参查询、写入及请求各接收一个输入，用 `Unit` 表示无数据输入；该实参表达式仍只求值一次。
拥有值支持 String 和元素满足传递值契约的 Array；不接受 Map、借用、函数或资源作为输入/结果，也不支持可变参数。
以下入口除 `handle.read_only()` 外均为 `@suspends`：

| 入口 | 行为 |
| --- | --- |
| `create<S, A>(fn(A) -> S, A) -> Owner<S>` | 调用受检构造函数，建立独立根 arena |
| `owner.handle() -> WriteHandle<S>` | 取得可复制的写句柄 |
| `owner.read_handle()` / `handle.read_only() -> ReadHandle<S>` | 取得读句柄；后者同步收窄权限，不访问执行器或活状态 |
| `owner.close()` | 消费唯一 Owner，取消关联工作并等待关闭完成 |
| `read_from<S, R>(ReadHandle<S>, fn(&S) -> R) -> Result<R, DomainAccessError>` | 排队执行同步只读短操作 |
| `query_from<S, A, R>(ReadHandle<S>, fn(&S, A) -> R, A) -> Result<R, DomainAccessError>` | 排队执行带独立输入的同步只读查询 |
| `write_to<S, A, R>(WriteHandle<S>, fn(&mut S, A) -> R, A) -> Result<R, DomainAccessError>` | 排队执行同步独占短操作 |
| `request<S, A, R>(WriteHandle<S>, fn(A) @suspends -> R, A)` | 启动独立请求并等待其结果 |
| `request_read<S, A, R>(ReadHandle<S>, fn(A) @suspends -> R, A)` | 同上，请求只获得读权限 |

Owner 不可复制，作用域退出或任务取消时自动关闭。句柄不保活状态，可传递或存入容器，
不能从整数构造，也不能提升权限。关闭后旧句柄永久失效；仍可同步调用 `read_only()`，实际访问时检查关闭、失败及执行器归属。
`request` 与 `request_read` 返回 `Result<R, RequestError>`。成功载荷支持有界传递值，保留完整标量位模式，
包括 UInt64 最大值、浮点负零和 NaN 载荷，不占用成功值作为错误哨兵。

当前入口能力如下；表中“传递值”采用下节的统一资格与上限，允许 String 和合格 Array，不允许 Map 或资源：

| 入口 | 状态根 | 输入／结果 |
| --- | --- | --- |
| `create` | 受检拥有图 | 构造输入支持传递值；返回完整状态根 |
| 单域候选构造 | 受检拥有图 | 构造输入支持传递值；返回完整状态根 |
| 单域 `read/query/write`、`read_from/query_from/write_to` | 受检拥有图 | 查询、写入的输入和所有操作的结果支持传递值；`read*` 无额外输入 |
| 双域短操作 | 受检拥有图 | 查询、写入的输入和所有操作的结果支持传递值；`read_pair` 无额外输入 |
| `snapshot_from` | 受检拥有图 | 投影结果支持传递值 |
| `snapshot_pair`、集合快照和候选 | 受检拥有图 | 投影结果和候选构造输入均支持传递值 |
| `request/request_read` | 所属域沿用单域根资格 | 异步入口输入与结果支持传递值 |
| `after/every` | 所属域沿用单域根资格 | 操作输入支持传递值，结果为 Unit |
| `lookup::<S>/lookup_write::<S>` | 受检拥有图 | 显式指定状态类型，无投影参数 |

| RequestError | 含义 |
| --- | --- |
| `Failed` | 普通请求运行时失败，所属状态仍可用 |
| `Cancelled` | 请求被取消；其清理完成后才发布 |
| `Closed` | 域已关闭、正在关闭，或句柄已失效 |
| `ResourceLimit` | 请求/任务额度、逻辑字节预算或解释器执行限制不足 |
| `InvalidAccess` | 请求访问的入口、名义状态类型或读写权限不符合要求，或签名与域当前代码不兼容 |
| `DomainFailed` | 受检写操作运行时失败，域已被隔离；后续请求仍能区别于关闭 |

域关闭或失败导致请求终止时，优先报告该域的终态；已经发布的结果不被随后关闭覆盖。
创建错误仍终止当前任务。异步请求的 RequestError 与下面的短操作访问错误独立；
这里不携带错误文本或用户自定义的业务错误载荷。

请求具有独立根 arena 和失败组。结果在请求清理完毕后才交给调用者；调用者被取消时
同步撤销请求，域关闭也会取消请求。API 不提供脱离调用者的启动；并发启动使用既有
`parallel`、`race` 或 `branch`。普通请求失败不会关闭状态；写短操作运行时失败会隔离该域，
取消关联请求，已发生的写入不回滚。请求回执和短操作共同占用有界请求数量与字节额度。

单域 `read/query/write`、`read_from/query_from/write_to` 与双域
`read_pair/query_pair/update_pair/write_pair` 均返回
`Result<R, DomainAccessError>`；成功值保留完整标量位模式或传递值。`Err` 保证操作体未执行：

| DomainAccessError | 执行前拒绝原因 |
| --- | --- |
| `Closed` | 域正在关闭、已关闭，或源码句柄在当前执行器中不可用 |
| `DomainFailed` | 域已因之前的写操作失败而隔离 |
| `InvalidAccess` | 没有所属请求上下文、权限不足或存在可写别名 |
| `Incompatible` | 入口、输入／结果契约或状态 schema 与当前代码不兼容 |
| `ResourceLimit` | 请求／任务准入额度、请求字节预算或修订余量不足 |

源码句柄不携带执行器身份；其他执行器的 token 与已关闭 token 同样返回 Closed。
Rust 宿主仍沿用 `DomainRequest` / `DomainOutcome` 错误通路，可区别错误执行器身份。
执行前拒绝的分类在确定时记录，后续关闭或发布不会改写已确定的结果。调用方实参求值、
值包准备和操作体内的运行时失败仍沿任务失败通道传播；操作体失败不会变成可捕获的访问 Err，
失败写操作仍隔离全部声明可写的域。可显式匹配 Err；若拒绝应视为任务失败，可调用
`Result.unwrap()`。取消等待撤销排队项或丢弃已完成结果，不自动重试操作。

请求内部可用 `read<S, R>(fn(&S) -> R)`、
`query<S, A, R>(fn(&S, A) -> R, A)` 和 `write<S, A, R>(fn(&mut S, A) -> R, A)` 访问启动时授予的域；
Rust `spawn_domain_task` 也提供同一上下文。下面是无需 Rust 宿主的完整程序：

```jk
import joky/domain
import joky/task
struct Room { count: Int32 }
fn make(count: Int32) -> Room { Room { count: count } }
fn add(room: &mut Room, delta: Int32) -> Int32 {
    room.count = room.count + delta
    room.count
}
fn count(room: &Room) -> Int32 { room.count }
fn request(delta: Int32) @suspends -> Int32 {
    domain.write(add, delta).unwrap()
    task.yield_now()
    domain.read(count).unwrap()
}
fn main() @suspends {
    let room = domain.create(make, 10)
    let handle = room.handle()
    domain.request(handle, request, 2).unwrap_or(-1)
    domain.read_from(handle.read_only(), count).unwrap()
    room.close()
}
```

构造函数与读写操作必须是直接指定的命名函数，编译器检查同步、不逃逸和无宿主调用；
请求入口也必须是命名函数，但允许挂起。支持导入的命名入口，不接受闭包或动态函数值，
这些 intrinsic API 自身也不能保存为函数值。排队环境仅包含模块与操作身份、句柄和独立
传递值输入，不包含状态借用。泛型调用在具体实例化时再次检查载荷资格；String、Array 及其嵌套拥有捕获按值移动。
请求的环境权限与操作名义类型在运行时再次检查；重载后，旧域和请求继续使用旧代码与类型，
直到宿主发布结构兼容的新代码（见 Rust 宿主状态域一节）。

### 构造与短操作的传递值载荷

`read_from/query_from/write_to`、请求内的 `read/query/write` 与
`read_pair/query_pair/update_pair/write_pair` 支持标量，
以及由标量、String、Array、元组、具体记录与枚举嵌套组成的输入 A、结果 R。记录允许普通记录和已实例化的
泛型记录，例如 `Pair<Int32>`；元组按元素顺序传递，包括 `(Unit, Empty)` 等零槽组合。枚举可以是无载荷枚举，
或所有分支载荷均为传递值的具体实例，包括 `Option<Int32>`、`Result<Reply, BusinessError>` 和自定义泛型枚举。
所有具体类型参数也须满足资格，即使参数没有出现在载荷中；不能用 `Phantom<Ptr>` 绕过限制。

每个包分别最多 64 个结构项、64 个标量槽、8 层聚合（记录／枚举／元组／Array，最外层为第 1 层）。
记录的每个字段和具体类型参数计一项，元组每个元素计一项；枚举的标签、每个分支和每个具体
类型参数各计一项，再递归累计参数契约、字段、元素和分支载荷中的结构项。同一类型在参数与
字段／载荷处出现时分别计数；Unit／空记录字段、零槽元组元素和无载荷分支也计数。
只使用某一个分支不会免除其他分支的检查或计费，普通记录原有 64 字段边界不变。
允许 String 作为拥有叶，包括 `Option<String>`、`Result<String, BusinessError>` 及嵌套记录／元组。
Array 元素递归满足同一契约，支持 `Array<String>`、`Array<Option<String>>` 和嵌套 Array；
空数组同样检查元素类型。传递保留元素和长度，不保留额外预留容量；接收数组的容量等于长度。
数组类型本身计一个结构项和一层聚合，再递归计算元素契约；
动态元素个数不展开到 64 项／64 槽上限，数组数据和字符串字节按实际存储计入既有预算。
String 和 Array 按值移动；需保留原值时显式构造副本，String 可用 `.clone()`。
不接受 Map、原始 Buffer、引用、指针、函数、资源、用户 Drop 或未实例化类型参数。
单域与双域根均可持有受检拥有图。可执行示例见
[state_domain_values.jk](../tests/fixtures/state_domain_values.jk) 和
[state_domain_queries.jk](../tests/fixtures/state_domain_queries.jk)；枚举覆盖见
[state_domain_enum_values.jk](../tests/fixtures/state_domain_enum_values.jk)，组合类型覆盖见
[state_domain_composite_values.jk](../tests/fixtures/state_domain_composite_values.jk)；String 生命周期覆盖见
[state_domain_text_values.jk](../tests/fixtures/state_domain_text_values.jk)，数组与嵌套拥有值覆盖见
[state_domain_array_values.jk](../tests/fixtures/state_domain_array_values.jk)。

`query_from` 与请求内 `query` 接收 `fn(&S, A) -> R`，`query_pair` 接收
`fn(&S, &T, A) -> R`。带输入不授予写权限，`request_read` 内也能使用 `query`；
查询不推进任何参与域的数据修订。无输入查询继续使用 `read`、`read_from`、`read_pair`。

输入在入队前封装为独立数据，操作执行时建立域内拥有值；队列不保留调用方的 String 或 Array 地址。
结果在操作 arena 清理前封装，返回调用方时建立新的拥有值。排队输入和未收取结果按实际字节占用请求额度。
结果交付失败发生在操作执行之后，不作为“操作体未执行”的 DomainAccessError 返回。
晚绑定比较完整输入与结果契约，包括嵌套名义类型、字段名、顺序、可见性和类型，
以及记录／枚举的具体类型参数、枚举分支名／顺序和每个分支的载荷类型、元组元素顺序与类型；
算法变化且契约兼容时，旧流程及排队项执行当前发布的实现，不兼容则在操作体运行前拒绝。
执行前取消不写状态；执行完成后取消只丢弃结果，已完成的写入不会回滚。

`create(fn(A) -> S, A)` 的输入 A 也支持上述传递值，完整状态根 S 仍可包含受检拥有图。
输入表达式只求值一次，构造前复制到调用方拥有的值包；构造在一次 poll 内同步完成，成功后
才登记新域并交付 Owner。值包占用调用方 arena，根与构造临时分配占用状态预算；构造、预算
或 Owner 交接失败均清理已分配资源，沿任务错误通道传播，不返回 DomainAccessError。
构造器绑定调用流程的模块版本；热更新不会把旧流程中的构造调用重定向到新模块。
可执行示例见 [state_domain_create_values.jk](../tests/fixtures/state_domain_create_values.jk)。

`request` / `request_read` 的输入 A 和结果 R 支持同一传递值契约，可分别与标量组合，
包括空记录与 Unit。输入表达式只求值一次，在启动子任务前逐值复制；跨挂起期间不借用调用方记录。
结果存储在准入时预留并计入请求字节额度，子任务及其 arena 清理完成后才交付 `Result`。
取消等待者会取消子任务并释放输入、结果和回执；已经完成的域写入不会回滚。
`request_read` 仍只授予读权限，记录载荷不能提升权限；请求失败与状态域写失败继续分别处理。

新请求启动前检查调用方与域当前代码的完整输入／结果契约，不兼容返回 `RequestError.InvalidAccess`，
不执行请求体。兼容时以域当前代码启动，已经启动的请求始终保留其模块；请求内的具名短操作
仍按执行时的兼容代码晚绑定。调用方值包分配失败沿任务错误通道传播；请求准入或子任务预算不足
返回 `RequestError.ResourceLimit`。可执行示例见
[state_domain_request_values.jk](../tests/fixtures/state_domain_request_values.jk)。

业务枚举不改变访问协议。块返回 `Result<T,E>` 时，`when` 的类型是
`Result<Result<T,E>, domain.DomainAccessError>`：外层 `Err` 表示本次操作没有发布状态，
其中 `ResourceLimit` 也可能在候选计算后返回。`Ok(Err(e))` 表示业务拒绝，丢弃全部候选；
`Ok(Ok(value))` 表示业务成功并已发布。显式原地短操作不采用这一回滚规则。
块返回 `Option<T>` 时同样保留外层访问 Result。只构造 `None` 或 `Err` 等无法推导全部参数的分支时，
使用显式类型参数、带类型的局部变量或有完整返回类型的辅助函数。

Timer 的输入也支持上述传递值契约；重复触发时每次建立独立拥有输入，具体生命周期见[定时域调用](#定时域调用-jokydomain)。
`when` 使用同一传递值载荷资格；Rust `InstanceValue` 公共入口保持标量。

### 传递值投影快照

单域投影快照支持有界传递值结果，并携带不透明版本凭据；JIT、解释器与独立 AOT 均支持。
以下入口均为 `@suspends`：

| 入口 | 行为 |
| --- | --- |
| `snapshot_from<S, R>(ReadHandle<S>, fn(&S) -> R) -> Snapshot<S, R>` | 排队执行受检只读投影，在同一次访问中保存传递值结果、域修订、代码版本和 schema 身份 |
| `snapshot.value() -> R` | 从快照复制标量或记录结果，不重新读取状态 |
| `snapshot.check() -> SnapshotStatus` | 检查所属执行器、域身份、生命周期、数据修订、代码版本与 schema 身份 |

投影沿用同步、不逃逸和效果检查，R 支持上述有界传递值。快照不是整个状态的隐式副本，
也不携带状态借用；它可跨挂起保存，关闭原域后仍可读取保存的值。凭据封装在不可复制的
快照资源中，不能从整数或用户记录构造、修改或拆出版本。快照不保活原状态。
快照退出作用域或任务取消时释放自身存储，其存储计入调用任务的 arena 预算。

`SnapshotStatus` 为 `Current`、`Conflict`、`Closed`、`DomainFailed` 或 `InvalidAccess`。
只读操作不改变修订；正常完成的写操作即使写入相同值也使旧快照变为 `Conflict`。
关闭后重新创建的域不能恢复旧快照的有效性；写失败隔离的域返回 `DomainFailed`。
跨执行器检查返回 `InvalidAccess`。获取快照时的准入、投影或分配失败仍终止当前任务。

`Current` 只表示检查发生时版本仍一致，不预留权限，也不能与后续普通写入组合成原子提交。
单域提交使用下述候选入口消费完整快照，并在发布时重新验证版本。
可执行行为示例见 [state_domain_snapshots.jk](../tests/fixtures/state_domain_snapshots.jk) 与
[state_domain_snapshot_values.jk](../tests/fixtures/state_domain_snapshot_values.jk)。

每次 `value()` 都独立复制字段并建立新的 String／Array 拥有值，修改返回记录不会改变快照。传递值结果与完整
读取凭据在同一次执行中捕获，领取时不刷新版本；记录结果在执行前预留并计入请求额度，
交付快照后计入调用任务预算。构造候选可消费记录快照并使用其投影结果，仍保留全部依赖。
捕获前的兼容代码发布可改变实际投影实现；捕获后的值固定，不因后续发布重新计算。

### 候选根与单域原子提交

下列 `joky/domain` 入口均为 `@suspends`，JIT、解释器及独立 AOT 语义一致：

| 入口 | 行为 |
| --- | --- |
| `candidate_from<B, R, S, A>(Snapshot<B, R>, WriteHandle<S>, fn(A) -> S, A) -> Candidate<S>` | 消费完整快照及全部读取凭据，以命名、同步、受检的构造器在独立 arena 中创建目标域的完整候选根 |
| `commit_to<S>(WriteHandle<S>, Candidate<S>) -> CommitReceipt` | 消费候选，预留回执并申请 FIFO 提交；返回时不保证已经发布 |
| `deadline_after(Int32) -> CommitDeadline` | 从当前执行器时钟创建不可伪造的截止凭据，参数为非负毫秒数，0 表示立即到期 |
| `commit_before<S>(WriteHandle<S>, Candidate<S>, &CommitDeadline) -> CommitReceipt` | 消费候选，按同一 FIFO 提交；发布前检查截止凭据 |
| `receipt.wait(&mut self) -> Result<UInt64,CommitError>` | 等待终态，可重复读取；不消费回执 |
| `receipt.cancel(&self)` | 请求取消尚未发布的提交；已确定的终态保持不变 |
| `receipt.identity(&self) -> UInt64` | 返回执行器内唯一的受理序号；未能准入时为 0，不是域句柄 |

`Candidate<S>` 与 `CommitReceipt` 都是不可复制的拥有值，内部存储不向用户开放。
S 支持上述 HR1 无引用、无用户 Drop 的拥有记录图，A 支持标量与上述有界传递值；候选构造器
遵守与 `create` 相同的无挂起、无宿主调用和无用户 Drop 资格。
投影可以只包含业务所需的传递值字段，构造器必须显式生成完整 S；运行时不会从投影隐式补齐其余字段。
候选构造时必须显式提供写句柄，目标须在原快照的完整读取集合中。候选绑定所选域的身份、类型、代码版本与修订；提交还必须提供该域的写句柄。
目标缺失、重复写目标、错误执行器或类型／模块不匹配均在构造器执行前拒绝；这些构造错误沿任务错误通道传播。

`candidate_from`、`candidate_pair_from`、`candidate_set_from` 与 `candidate_set_add` 的输入
沿用上述有界传递值限制，包含空记录和嵌套 Unit。输入表达式只求值一次，记录逐值复制
到调用方值包，构造同步执行且保留完整原始读取凭据。构造器仍绑定调用流程模块，须与
目标域的模块和状态类型精确匹配；不会按新代码重新解释旧输入或刷新原快照版本。
示例见 [state_domain_candidate_values.jk](../tests/fixtures/state_domain_candidate_values.jk)。

候选构造不传入活状态地址；异常、预算失败和取消只释放候选存储，不修改旧根或隔离原域。
候选使用独立 arena，和旧根、构造临时存储共同计入 `max_state_bytes` 峰值；根的拥有槽
描述也计费。候选资源与回执资源的元数据计入调用任务预算。构造返回后筛选根、String 与
递归容器控制块及载荷，回收其余临时分配；发布整体移动候选 arena，不保留请求 arena，
不复制载荷，也不重复计费。拒绝、关闭及替换后的旧根只执行受信任的存储释放。

`wait()` 成功返回 `Ok(UInt64)`（本次提交的新修订），失败返回 `Err(CommitError)`。
单域、双域和集合共用 `CommitError`：`Conflict`、`Closed`、`DomainFailed`、`InvalidAccess`、
`Cancelled`、`ResourceLimit`、`Expired`、`OutboxFull` 或 `OutboxClosed`。
后两者用于附带出站消息的提交，详见出站信箱一节。提交和短操作共享受理队列；发布前验证
完整单域凭据、生命周期与修订余量，再于无挂起、无分配、无用户代码的区间替换根并记录结果。
拒绝会销毁候选，旧状态保持不变；没有自动重试。构造候选或分配回执资源失败仍终止当前任务，
发生在提交准入之前。排队额度或执行器任务额度不足通过回执返回 `ResourceLimit`。

回执及其已完成请求记录持续占用额度，直到回执释放。丢弃待执行回执会移除排队项并回收候选；
丢弃已提交回执只释放结果记录，不撤销提交。仅取消借用回执的等待分支，不等于取消回执拥有的
提交；可显式调用 `cancel()`。取消和关闭按执行器处理顺序与发布排序，发布后的取消、后续写入
或域关闭均不改写既有回执。回执不保活状态，也不提供执行器关闭后的结果查询或持久化历史。

`CommitDeadline` 是不可复制的拥有值，绑定创建它的执行器及其单调时钟，不保活执行器或状态。
可在计算、网络等待或候选构造之前创建，随后多次借用；借用或提交不会延长截止时间。
`commit_before` 将凭据内容复制进候选包，返回后无需保留原凭据。跨执行器使用返回
`InvalidAccess`；`deadline_after` 的负数参数或时间加法溢出会终止当前任务。
凭据资源计入调用任务预算；分配失败发生在创建阶段，不产生提交。

发布前重新读取该执行器的 `Clock`，`now >= deadline` 时返回 `Expired` 并回收候选；
旧状态与修订不变，不依赖到期定时器已触发。检查不自动取消计算或唤醒尚未提交的任务。
默认时钟为真实单调时间；嵌入宿主配置模拟时钟后，有效期随该时钟推进，暂停时也暂停。
当前不支持同时选择多个时钟或业务阶段凭据。该毫秒参数仅相对绑定时钟，不是墙钟时间戳。
截止时间只约束最后一次检查，不保证发布及后续清理在截止之前完成。

关闭、域失败或取消若已先成为提交终态，不改写成 `Expired`；有效凭据的发布检查在修订冲突
判断之前检查到期。已经成功或返回 `Err(Expired)` 的回执不因随后到期、取消或关闭而改变。
`commit_to` 保持无有效期限制；它仍检查版本、权限与生命周期。

可执行示例见 [state_domain_commits.jk](../tests/fixtures/state_domain_commits.jk) 与
[String/Array 示例](../tests/fixtures/state_domain_owned.jk)、
[嵌套记录与 Map 示例](../tests/fixtures/state_domain_nested.jk)、[有效期示例](../tests/fixtures/state_domain_deadlines.jk)。
候选构造输入和快照投影结果均支持有界传递值；`snapshot_from` 只记录单域依赖，`snapshot_pair` 可记录完整双域依赖（见下节）；尚不支持自定义提交期谓词。
当前私有 AOT 模块描述格式为 v3，保留记录字段可见性；旧 v1/v2 描述对象须重新编译。拥有值桥扩展了私有 CPS 帧，AOT 对象须与匹配的运行时一起构建。

### 双域短操作

SC3 的首个切片提供同一执行器内的两个域访问，JIT、解释器、独立 AOT 均可使用。
当前两个根都必须是普通非泛型、满足受检拥有图资格的记录；两者可以是不同名义类型，也可以绑定
不同模块版本。操作由第一个域的当前模块解析，其余域须与对应参数的状态布局兼容；
参数顺序决定实现归属，调度不能改变该顺序。受检拥有图可参与双域短操作，权限与所有权检查保持一致。

| 入口（均为 `@suspends`） | 权限与输入 |
| --- | --- |
| `read_pair<S,T,R>(ReadHandle<S>, ReadHandle<T>, fn(&S,&T)->R) -> Result<R, DomainAccessError>` | 两个只读参数，无额外输入 |
| `query_pair<S,T,A,R>(ReadHandle<S>, ReadHandle<T>, fn(&S,&T,A)->R, A) -> Result<R, DomainAccessError>` | 两个只读参数，接收独立查询输入 |
| `update_pair<S,T,A,R>(ReadHandle<S>, WriteHandle<T>, fn(&S,&mut T,A)->R, A) -> Result<R, DomainAccessError>` | 第一个只读、第二个可写 |
| `write_pair<S,T,A,R>(WriteHandle<S>, WriteHandle<T>, fn(&mut S,&mut T,A)->R, A) -> Result<R, DomainAccessError>` | 两个可写参数 |

A、R 支持上述传递值；操作必须是命名、同步、无宿主调用、状态借用不逃逸的受检入口，不能用
普通闭包、函数变量或 `@noescape` 声明绕过证明。输入在排队前独立保存，状态地址到执行时
才解析，操作中不能挂起或追加域访问。可使用受检临时存储，计入状态峰值并在调用后回收。

两个域共享一份有界 Request、一个受理序号和现有执行器 FIFO，不在各域分别排队，也不
占有第一个域等待第二个。一次 poll 同步执行整个操作，没有可见的半次运行；这个切片不
提供并行读执行或针对等待访问集合的绕行调度。同域双读合法，保留两个只读参数绑定；
任何涉及可写参数的同域别名都在执行前拒绝，包括一个只读加一个可写的情况。

执行前检查全部参与域的身份、权限、类型、代码版本、状态及可写域修订余量；任一预检失败
不执行操作体。正常返回后每个可写域推进一次修订，包括返回 false 或没有实际修改；只读域
修订不变。短操作没有失败回滚：运行时失败隔离全部声明可写的域并释放其根，终结涉及它们
的排队申请和所属请求任务；只读参与域和无关域继续可用。任一域在执行前关闭会终结整份
申请，取消/丢弃申请也一次移除全部依赖。执行前拒绝返回 DomainAccessError；操作体失败
沿任务失败通道传播，不返回提交回执。

Rust 宿主使用 `vm.queue_domain_pair([first, second], method, args)`，返回已有 `DomainRequest`。
方法前两个引用参数决定读写模式，余参和返回值为标量；宿主允许标量参数列表。
生命周期和限额沿用单域 Request，尚未收取的结果继续占用请求额度。

可执行示例见 [state_domain_pairs.jk](../tests/fixtures/state_domain_pairs.jk)。这些入口的成功分支返回
传递值结果，不产生版本凭据或候选提交；需要凭据时使用下述联合快照。三域及以上使用后述有界访问集合；
双候选原子发布见下节。

### 联合快照与完整读取凭据

`domain.snapshot_pair<S,T,R>(first: ReadHandle<S>, second: ReadHandle<T>, projection: fn(&S,&T)->R)`
为 `@suspends` 入口，返回 `Snapshot<S,R>`。根类型和实现归属与 `read_pair` 相同：
两个普通受检状态记录、同一执行器、首域当前模块提供受检同步只读投影，其余域布局须兼容；
投影结果支持有界传递值。允许同域重复读。捕获完成后保存原投影结果与版本凭据，不晚绑定重算。

投影值与两个域的修订在同一次 FIFO 短操作中捕获；稍后收取结果时不重新读取修订。
类型参数 S 保留首域的类型信息，不限定候选写目标；构造时可显式选择任一参与域，
包括与首域类型不同的第二域。重复域只保留一份版本依赖。

`value()` 返回独立保存的值，关闭后也可读取；`check()` 检查整个依赖集合。身份和生命周期
检查先于修订检查；任一域修订变化均可返回 Conflict，相同值写入也会推进修订。
`candidate_from(snapshot, target, constructor, input)` 消费整个快照并携带全部读取凭据，不能从快照
中取出部分凭据构造候选。现有 `commit_to` / `commit_before` 在发布前验证全部读取依赖，
只读依赖变化也会拒绝发布；只读依赖在排队期间关闭或失败会终结整份申请。
提交成功后只替换显式所选域的根并推进它的修订；之后依赖关闭不会改写已提交回执。

读取集合为有界的不透明运行时数据，保存在快照、候选及排队结果中，随相应资源计费和释放；
它不保活状态根，也不阻止域关闭。当前没有合并分时快照或拆分依赖的 API，
更多域使用后述有界访问集合，也不自动追踪快照之外的业务数据。
单目标发布与下述双候选原子发布使用同一完整读取验证。
三后端可执行流程见 [state_domain_joint_snapshots.jk](../tests/fixtures/state_domain_joint_snapshots.jk)。

### 双候选原子发布

联合快照的两个域必须为不同身份。先以 `candidate_from` 消费联合快照，显式指定首个写目标；
再以 `candidate_pair_from` 消费该候选，显式指定另一个写目标，得到唯一拥有的
`CandidatePair<S,T>`。写目标顺序可以与快照读取顺序不同。第二个构造器仍须命名、同步、
无宿主调用且受检；其返回类型和模块版本在运行时与显式目标精确匹配。两份完整候选根各有独立存储，构造中可挂起
的只有外围流程，构造器本身不能挂起。第二次构造失败会释放第一份候选，旧状态保持不变。

| 入口（均为 `@suspends`） | 结果与约束 |
| --- | --- |
| `candidate_pair_from<S,T,A>(Candidate<S>, WriteHandle<T>, fn(A)->T, A)` | 返回 `CandidatePair<S,T>`；A 为有界传递值，显式选择原读取集合中尚未写入的域，保留完整读取凭据 |
| `commit_pair_to<S,T>(WriteHandle<S>, WriteHandle<T>, CandidatePair<S,T>)` | 消费整包，返回 `PairCommitReceipt`；两个句柄须依次对应两个确切域身份 |
| `commit_pair_before<S,T>(WriteHandle<S>, WriteHandle<T>, CandidatePair<S,T>, &CommitDeadline)` | 同上，发布前检查执行器时钟截止时间 |

不接受单域快照、同域重复读快照扩展出的双写包；不提供拼接独立候选、拆包、提交时交换已绑定目标或删除
读取依赖的接口。此双域接口沿用 `snapshot_pair` 的根资格；更多域使用下述有界访问集合。
构造与提交之间可以任意等待，任一域修订变化均使整个提交 Conflict，不自动重试。

一包只占一份有界队列申请。发布前完成全部身份、权限、模块、类型、生命周期、读取修订、
截止时间和修订溢出检查；任一失败都不替换任何根、不推进任何修订。全部通过后，同一次
同步操作替换两个根并各推进一次修订，中间不分配、不运行用户代码、不挂起；两份旧根在
完整提交事实记录后才释放。排队前及排队中取消/丢弃释放整包；任一参与域关闭或失败终结
整份申请。联合读取只能观察提交前或提交后的状态。

`PairCommitReceipt.wait()` 返回 `Result<(UInt64,UInt64),CommitError>`：成功的两个新修订
按候选构造顺序对应确切写目标，失败使用公共 `CommitError`。`identity()` 和 `cancel()` 与单域回执一致；成功后再次等待、取消或
关闭任一域都不会改写原有修订对。回执仍存活时继续占用请求配额。

三后端完整示例见 [state_domain_pair_commits.jk](../tests/fixtures/state_domain_pair_commits.jk)。

### 有界多域访问集合

`joky/domain` 提供动态、异构访问集合，最多包含 **64 个不同域**，域根均须为普通非泛型的受检状态记录。
`AccessSet.include` 保存域身份，重复读归一化；空集合拒绝捕获，第 65 个不同成员受控拒绝。
集合不能拼接不同时刻的快照，也不能删除读取依赖。单域、双域接口继续可用。

| API | 行为 |
| --- | --- |
| `access_set() -> AccessSet` | 创建独占的有界访问集合 |
| `AccessSet.include<S>(&mut self, ReadHandle<S>)` | 登记只读成员；不读取状态、不持有借用 |
| `snapshot_set(AccessSet) -> SnapshotSet` | 消费访问集合，一次 FIFO 操作深复制全部拥有根并捕获完整修订集合 |
| `read_snapshot<S,R>(&SnapshotSet, ReadHandle<S>, fn(&S)->R) -> R` | 对指定成员的冻结副本运行受检同步只读投影；R 为有界传递值 |
| `query_snapshot<S,A,R>(&SnapshotSet, ReadHandle<S>, fn(&S,A)->R, A) -> R` | 在同一冻结副本上运行带参投影；A、R 均为传递值，输入求值一次并按值传入 |
| `SnapshotSet.check() -> SnapshotStatus` | 检查全部成员的当前身份、生命周期与修订；不授予提交权限 |
| `candidate_set_from<S,A>(SnapshotSet, WriteHandle<S>, fn(A)->S, A) -> CandidateSet` | 消费完整快照，构造第一个独立候选；A 为有界传递值 |
| `candidate_set_add<S,A>(CandidateSet, WriteHandle<S>, fn(A)->S, A) -> CandidateSet` | 消费整包，添加另一个成员的独立候选；A 为有界传递值，失败释放整包 |
| `edit_snapshot<S,A>(SnapshotSet, WriteHandle<S>, fn(&mut S,A)->Unit, A) -> CandidateSet` | 消费冻结快照，移动指定冻结根到候选并局部编辑；保留其余冻结根与完整依赖 |
| `edit_candidate<S,A>(CandidateSet, WriteHandle<S>, fn(&mut S,A)->Unit, A) -> CandidateSet` | 消费冻结派生的整包，继续编辑已选目标或加入另一个冻结目标；A 为标量或传递值 |
| `commit_set(CandidateSet) -> SetCommitReceipt` | 消费整包并申请原子发布 |
| `commit_set_before(CandidateSet, &CommitDeadline) -> SetCommitReceipt` | 同上，发布前检查执行器截止时间 |

上述入口均为 `@suspends`，便于在宿主驱动任务内使用；冻结复制和实际发布各自同步完成，
不跨挂起点持有活根引用。命名投影与构造器须通过编译器的 domain_entry 证明，不能使用
函数变量、宿主回调、可挂起操作或用户析构。投影使用冻结模块中精确匹配状态类型的入口；
调用方可来自兼容的另一版本，运行前按名称校验完整接收者、输入与结果契约，不能只凭相同槽数重解释值。
候选构造器仍须匹配目标域。冻结副本保留原模块，原域关闭后仍可读取旧值。
副本与动态元数据计入状态字节预算，不能靠快照绕过存储上限。
`read_snapshot`／`query_snapshot` 的结果独立封装到调用方值包，不引用冻结根；结果包计入调用
任务预算。投影始终使用冻结副本保留的模块，原域写入、代码发布或关闭不会改变投影来源；
投影失败不修改冻结根或在线状态，错误仍沿任务失败通道传播。

写目标可以是快照中的任意成员，与 include 顺序无关；必须提供该域的写能力。构造入口不能重复添加
同一个域；编辑入口允许多次编辑同一个候选，但只登记一次写目标。两者均不能扩展到快照之外。每个候选保留原始完整读取集合，未写入的第四、第五域
同样参与冲突、关闭与失败检查。构造及等待期间不改动活根；发布前预检全体候选，任何失败
都保持所有旧根和修订。成功后在同一次操作中替换全部写根，各推进一次修订，记录完整回执
后释放旧根；只读成员的修订不变。不自动重试，也不自动发现未登记的业务依赖。

冻结采用保留容器容量和清理描述的深复制，不调用用户 Clone／Drop，也不使用写时复制。
状态预算覆盖活根、所有冻结根、候选、复制与调用临时存储的同时峰值；复制失败清理所有部分副本。
投影快照 `snapshot_from`／`snapshot_pair` 只保存结果与凭据，不复制根。

`edit_snapshot`／`edit_candidate` 使用捕获时的代码、精确类型及受检同步可写入口，编辑结果须为 Unit。
选中冻结根的所有权直接移动，派生和后续编辑不再复制整个根；其余冻结根保留到包被丢弃或提交，
用于后续选择目标。提交时释放未选中的冻结存储，但其读取版本仍参与验证。编辑可跨任务挂起分步调用，
每次编辑本身同步完成，不开放 HR5 的迁移能力。跨模块版本的调用须与冻结模块中的命名编辑入口契约完整兼容；
实际仍用冻结模块的代码修改候选，不改动读取凭据。旧任务因此可在冲突后重新捕获并编辑兼容的新快照；
发布前捕获的候选在发布后提交仍会冲突。目标关闭、权限、类型或输入契约错误通过任务失败通道报告；
编辑运行时失败或预算耗尽清理整包，均不改变活根。编辑 API 用于冻结派生的包，构造候选继续使用
`candidate_set_from`／`candidate_set_add`。投影结果支持上述传递值，包括 String、Array 及其嵌套组合。

`Snapshot.value()` 等保存值读取继续为 `@suspends`：需要检查托管资源租借并按任务预算分配结果包，
不因数据已保存而绕过资源访问协议。拥有型冻结、跨根移动及分步编辑示例见
[state_domain_storage.jk](../tests/fixtures/state_domain_storage.jk)。

`SetCommitReceipt.wait()` 返回 `Result<Unit,CommitError>`。
`revision(ReadHandle<S>) -> Result<Option<UInt64>,RevisionError>` 查询本次提交保存的修订：
成功且属于写目标返回 `Ok(Some(revision))`，成功但不是写目标（含只读依赖）返回 `Ok(None)`；
未完成返回 `Err(Pending)`，提交失败返回 `Err(Failed(CommitError))`，跨执行器查询回执返回
`Err(InvalidAccess)`。状态与修订分开传递，完整 UInt64 范围可用，不再以 0 代替查询状态。`identity()` / `cancel()` 沿用已有回执规则；
发布后的取消、域关闭不会改写成功事实或保存的修订，回执存活期间仍占请求配额。

Rust 宿主另可使用 `queue_domain_access(&[DomainHandle], method, args)` 对 2–64 个参数执行一次
受检短操作。入口的连续引用参数声明读写模式，后续参数与结果为标量；由第一个域解析当前实现，
其余参与域须与该入口的对应引用类型结构兼容，
拒绝任意位置的可写别名。读取、候选发布与单域工作共用现有单线程 FIFO，没有部分持权等待、
并行读或绕行调度。源语言使用集合快照与候选 API 表达多域协调，不提供可变长度引用回调。
直接联合操作允许在可写根之间用受检移动／交换转移拥有值，调用结束后按实际拥有图归还各根。
只读图不可转出拥有值；失败恢复只读域，隔离并清理全部可写域，不提供写操作回滚。

三后端可执行换房示例见 [state_domain_many.jk](../tests/fixtures/state_domain_many.jk)：
同时写玩家、旧房间、新房间，房间规则作为只读第四域；规则变化使三份候选整体冲突。

## 有界请求准入

`joky/admission` 提供同一执行器内的活动请求名额。创建昂贵请求、捕获快照或构造候选之前
先取得许可，之后可跨挂起保留；许可不授予状态读写权限，也不持有活根引用。

| API | 契约 |
| --- | --- |
| `create(capacity: UInt64, max_waiters: UInt64) -> Pool` | 创建拥有者；capacity 必须大于 0，max_waiters 可以为 0 |
| `Pool.handle(&self) -> Handle` | 返回可复制的非拥有申请句柄；句柄不保持池开放 |
| `Pool.close(&self)` | 幂等关闭，拒绝新申请并唤醒全部等待者 |
| `Handle.try_acquire(self) -> Result<Permit, AcquireError>` | 不排队；满时返回 Full，关闭或句柄不属于当前执行器时返回 Closed |
| `Handle.acquire(self) -> Result<Permit, AcquireError>` | 有空位立即取得，否则 FIFO 等待；等待名额耗尽返回 WaitLimit，关闭返回 Closed |
| `Permit.release(self)` | 消费许可并同步归还一个名额 |

除 `Permit.release` 外，上表入口均为 `@suspends`，在宿主驱动任务内使用；`try_acquire`
不会因为容量不足而等待。Pool 和 Permit 为不可复制的拥有值。Pool 离开作用域或其任务
取消时自动关闭；Permit 离开作用域、任务失败或取消时自动归还，不要求显式 release。
AcquireError 包含 Full、Closed、WaitLimit；容量非法、整数溢出或内存预算不足是运行时错误。

等待只登记当前任务的唤醒信息，不为每个申请创建额外任务，也不轮询空队列。释放名额时
先为最早等待者预留，再通知其恢复，新申请不能抢走已预留名额。等待者取消后立即移除登记；
若已预留但尚未交付许可，取消会将名额转交下一位。预留尚未交付的申请仍占等待名额，直到
恢复收取或取消；`max_waiters=0` 表示容量不足时 acquire 立即返回 WaitLimit。

关闭会使尚未交付的等待申请返回 Closed，包括已经预留名额的申请；已经交付的 Permit
保持唯一释放责任，可以继续保留到原工作结束并安全释放。关闭不取消已准入业务，也不回滚
已经提交的状态；需要取消业务时由上层请求组负责。句柄身份不复用，旧句柄不能申请新池。

池元数据、预分配等待槽及每槽挂起上下文计入执行器共享状态字节预算
（Rust 宿主通过 `DomainLimits.max_state_bytes` 配置）；池和许可的语言资源包装计入所属
任务预算。所有等待槽在创建池时预留，配额不足在发布池之前拒绝；许可交付分配失败会归还
名额并继续唤醒队列，不留下悬空预留。

首版只限制活动名额；候选字节和域请求记录仍由原有配额独立检查，不预留它们。
不提供跨执行器申请、多池原子预约、Channel、单次应答或业务世代自动联动。
完整三后端示例见 [admission.jk](../tests/fixtures/admission.jk)。

## 域目录 `joky/domain`

域目录按 `UInt64` 业务 ID 查找状态域。`Directory` 唯一拥有目录存储，`DirectoryHandle`
是可复制、可通过 `Vm.call` 平面记录契约传递的访问能力。模块不因此获得可变全局状态；
AOT 服务作用域持有 Directory，把句柄传给 Script 或业务上下文即可。
目录可以包含不同状态类型，查找通过显式状态类型参数检查布局和访问权限。
普通 `insert` / `insert_read` 仅保存弱路由；`insert_owned` 消费域 Owner，将关闭责任交给目录。
拥有条目使状态保留到条目移除或目录关闭，无需挂起一个初始化任务来持有状态。

以下操作均为 `@suspends`，每次目录访问在一个运行时调用内完成，不等待容量。
删除、替换或关闭拥有条目时还会按域关闭协议取消关联工作并释放状态。

| API | 语义 |
| --- | --- |
| `domain.directory(capacity: UInt64) -> Directory` | 创建正容量的有界目录，预分配条目存储并计入执行器共享状态预算 |
| `Directory.handle(&self) -> DirectoryHandle` | 返回当前执行器内的非拥有能力，身份不复用 |
| `Directory.close(self)` | 关闭并释放目录及其拥有的域；离开所有者作用域也会关闭，普通弱条目的目标不受影响 |
| `DirectoryHandle.insert(key, WriteHandle<S>)` | 插入可写域；存活的同名键返回 Occupied，不覆盖 |
| `DirectoryHandle.insert_read(key, ReadHandle<S>)` | 插入只读域，不可通过查找提升为写权限 |
| `DirectoryHandle.insert_owned(key, Owner<S>)` | 消费唯一所有者并插入拥有型可写条目；成功后由目录关闭域，失败时关闭传入域 |
| `DirectoryHandle.replace(key, expected: ReadHandle<S>, target: WriteHandle<T>)` | 仅在当前条目指向 expected 域时替换，否则 Conflict |
| `DirectoryHandle.replace_read(key, expected: ReadHandle<S>, target: ReadHandle<T>)` | 条件替换为只读域 |
| `DirectoryHandle.remove(key, expected: ReadHandle<S>) -> Bool` | 仅删除仍指向 expected 域的条目；拥有条目同时关闭域，避免旧路由清理误删另一个域的新路由 |
| `domain.lookup::<S>(directory, key) -> Option<ReadHandle<S>>` | 检查存活与状态布局兼容性，返回只读句柄 |
| `domain.lookup_write::<S>(directory, key) -> Option<WriteHandle<S>>` | 同上，且要求注册的目标具有写权限 |

插入和替换返回 `Result<Unit, DirectoryError>`，错误有 `Closed`、`Full`、`Occupied`、
`InvalidTarget`、`Conflict`。关闭或来自其他执行器的目录视为 Closed；目标必须属于当前执行器且
存活、健康。关闭/失效域的旧条目在查找时移除，插入前也会统一清理，以回收未再查找条目的容量。

`insert_owned` 不返还失败的 Owner；无论结果如何，调用者都不能再次使用已移入的所有者。
同一个域只能有一个所有者，但可以另建弱路由。替换拥有条目为另一个域会关闭旧域，
新目标仍是弱路由；替换为同一域（包括收窄为只读）保留原关闭责任。
拥有条目的关闭会使所有指向该域的句柄失效，并取消所属请求，不只影响本目录内的路由。
目录本身仍是独占资源；`DirectoryHandle` 不延长目录寿命。

拥有条目不保存初始化 continuation 的代码。兼容发布可更新域的当前实现，
已结束初始化流程的旧模块可在其他持有者释放后退役。交接本身不执行代码发布：
旧流程在发布期间创建状态的情况仍需在对业务开放前完成相应的发布校验。

查找在目录关闭、跨执行器、键不存在、域关闭/隔离或缺少写权限时返回 None；目标存在但与
显式指定的 S 不兼容时产生 `TypeMismatch` 运行时错误，不返回错误类型的句柄。
S 必须满足单域状态根资格；查找不执行用户操作，不读取字段，也不要求声明投影函数。
兼容判断沿用域代码发布的记录名称、字段、布局和类型规则；代码热更新不改变目录身份和路由。
返回的域句柄仍受后续域操作的存活、类型、权限及代码版本检查，查找不延长其有效期。

```joky
import joky/domain
struct Room { members: Int32 }
fn open(n: Int32) -> Room { Room { members: n } }
fn count(room: &Room) -> Int32 { room.members }
fn install(directory: domain.DirectoryHandle) @suspends {
    directory.insert_owned(1001, domain.create(open, 3))!
}
fn main() @suspends {
    let rooms = domain.directory(1024)
    let directory = rooms.handle()
    install(directory)
    match domain.lookup::<Room>(directory, 1001) {
        Some(handle) => { println(domain.read_from(handle, count).unwrap()) }
        None => { println("room unavailable") }
    }
}
```

DirectoryHandle 允许持有者修改整个目录，宜按业务权限拆分目录。条件替换/删除比较的是域身份，
不是连接会话或每次绑定的世代；同一域 A→B→A 后，旧 A 句柄仍满足身份比较。需要会话世代隔离时
应在业务路由状态中记录并验证会话身份。重绑定不会撤销此前查到的、仍然存活的域句柄。

目录变更即时生效，不随候选提交成功或失败回滚，也不自动成为候选读取凭据。如果房间成员变更与
路由变化必须作为同一事务提交，应把权威路由关系放在状态域中并参与联合验证；目录仅作为查找入口。
当前不提供泛型键、枚举、自动创建域、隐式模块目录或 Context/配置快照 API。
完整路由、权限、销毁与替换示例见[目录测试](../tests/fixtures/directory.jk)。

## 定时域调用 `joky/domain`

`Timer` 唯一拥有一个执行器内定时器，`TimerHandle` 是可复制、不保活所有者的平面记录。
AOT 可以持有 Timer，把句柄交给 Script 登记业务操作。登记只捕获目标域、具名入口、受检
签名及有界传递值输入，不捕获闭包、域根或 Script 模块。所有者退出作用域即取消。

| API（均为 `@suspends`） | 行为 |
| --- | --- |
| `domain.timer() -> Timer` | 创建 Idle 定时器，元数据计入执行器共享状态预算 |
| `Timer.handle(&self) -> TimerHandle` | 取得当前执行器的弱能力 |
| `Timer.close(self)` | 关闭、取消排队调用并释放所有者 |
| `domain.after<S,A>(timer: TimerHandle, target: WriteHandle<S>, milliseconds: Int32, operation: fn(&mut S,A)->Unit, input: A) -> Result<Unit,TimerError>` | 登记一次调用，延迟非负；0 表示排队执行，不在登记期间调用 |
| `domain.every<S,A>(timer: TimerHandle, target: WriteHandle<S>, milliseconds: Int32, operation: fn(&mut S,A)->Unit, input: A) -> Result<Unit,TimerError>` | 登记周期调用，间隔必须大于 0，第一次在一个间隔后到期 |
| `TimerHandle.cancel(self)` | 取消尚未开始的调用，Active 变为 Cancelled；其他终态不变 |
| `TimerHandle.status(self) -> TimerStatus` | 观察 Idle / Active / Completed / Cancelled / Closed / InvalidAccess / ResourceLimit / Failed |

操作必须是编译器受检的同步域写入口，A 支持上述有界传递值，结果为 Unit；
不允许宿主调用、闭包或挂起。空记录与嵌套 Unit 也遵循同一资格及计数限制。
输入表达式只求值一次。Copy 输入保存独立副本；String／Array 拥有值按值移动，需保留原值时显式构造副本。
每次调用使用同一份保存值，不保留操作内对输入副本的修改，也不自动计算实际经过时间。业务需要 dt 时自行选择固定步长或显式建模时间。
时钟是登记所在执行器的单调 Clock：普通运行使用真实单调时间，嵌入宿主可用模拟时钟控制暂停。
不提供每个定时器单独选择时钟、持久化或重启恢复。

每个 Active 定时器最多有一个待处理短调用，完成后才安排下次到期。周期以本次登记的节拍为准，
下一截止时间严格晚于完成处理时的当前时刻；跳过错过的节拍，不补发积压。协作式调度不能保证
准时或抢占长操作。状态轮询不会自行等待，确需循环观察时应使用 `task.yield_now()` 让出执行权。

TimerError 包含 Closed、Busy、InvalidDelay、InvalidAccess、ResourceLimit。正在使用的定时器
返回 Busy，不隐式替换原登记。Completed、Cancelled、InvalidAccess、ResourceLimit、Failed
可在旧调度任务清理后重新登记；关闭不可重开。取消若发生在当前调度栈中，清理完成前仍返回 Busy。
未知、跨执行器或已销毁的句柄查询为 Closed。登记失败不改动原定时器状态。

每次到期及排队调用开始前都核对当前代码的签名；兼容发布后的下一次调用执行新代码。
入口删除或签名不兼容令定时器终止为 InvalidAccess，不修改域；配额不足终止为 ResourceLimit，
不自动重试。域关闭取消其定时器并产生 Closed；写操作失败沿用域隔离规则，定时器终止为 Failed。
已经开始的同步调用运行到返回或失败，取消不回滚其状态变更；一次调用成功后取消也不撤销修订。

Timer 登记/取消即时生效，不参与候选事务。tick 是普通短域写操作，不自动附带出站消息；
需要状态与消息一起发布时，业务仍须通过候选与出站信箱提交。执行器 task 数、共享状态预算和
每次到期的请求数/字节额度共同约束资源。保存的记录位槽计入共享状态预算，排队副本另计入请求字节额度；
完成、取消、关闭和失败均释放保存值与排队副本。调用方值包分配失败沿任务错误通道传播，
登记预算不足返回 `TimerError.ResourceLimit`，到期时请求准入不足则终止为 `TimerStatus.ResourceLimit`。
完整示例见[标量定时器测试](../tests/fixtures/domain_timers.jk)和
[记录定时器测试](../tests/fixtures/domain_timer_values.jk)。

### 周期声明 `#every`

`#every(1000)` 标注 `#state` 类型的固有方法，声明 1000 毫秒周期。方法必须是非泛型、
同步、无宿主调用的受检域写操作，只接受 `&mut self`，返回 Unit。尚不支持额外输入、dt、
泛型 impl、trait 方法或顶层函数。配置从状态字段读取；普通方法调用仍正常执行一次方法体。
属性参数须为一个正 Int32 整数字面量（1–2147483647），不接受常量名或运行时表达式，
不得重复或与其他方法属性组合。即使没有创建实例，也会检查声明资格。

每个状态域实例创建成功时自动登记该类型的全部周期方法，由状态域拥有登记；
无需创建 Timer 或手动调用 schedule。普通结构体值、快照、候选及迁移中间状态不启动任务。
模块加载本身不启动任务。首次在一个周期后到期，沿用上文的节拍跳过、同域串行执行和写失败隔离规则。
Owner 销毁、显式关闭或域隔离会取消相应任务；自动调度仍需宿主驱动执行器，没有独立后台线程。
实际写法及可执行示例见[周期声明测试](../tests/fixtures/domain_every.jk)。

兼容代码发布会同时更新周期声明：只改方法体时保留原节拍，排队 tick 兼容晚绑定到新版实现；
新增方法自动启动；删除属性或方法会取消其登记及排队 tick；改名视作删除旧方法并新增方法。
改变周期会取消旧登记和排队 tick，按发布时钟重新计时。迁移候选在成功发布前没有周期任务，
发布时用新状态类型的声明重新建立调度；批量迁移和不改 schema 的代码发布也整批处理。

创建和发布均先准备全部新任务，再安装状态与调度。失败时创建不留下实例，发布则保留原代码、
状态和周期登记。准备阶段的新旧任务同时计入峰值资源预算，预算不足不会先取消旧任务腾位置。
已经终止的登记不会因只改方法体而自动重试；改变周期或删除后重新添加声明会建立新登记。

`domain.periodic_status<S>(target: ReadHandle<S>, method: String) @suspends -> TimerStatus`
按方法短名查询，例如 `domain.periodic_status(room.read_handle(), "tick")`。
Active 表示正在调度；到期准入不足报告 ResourceLimit，写失败报告 Failed；关闭报告 Closed，
不存在该周期方法报告 InvalidAccess。查询不推进 tick、不授予写权限，也不重新启动任务。
Rust 宿主使用 `VmEngine::domain_periodic_status(handle, method)` 和 `joky::vm::PeriodicStatus`。
需要动态间隔、输入或独立取消控制时继续使用显式 Timer 与 `domain.after/every`。

## 提交式出站信箱 `joky/outbox`

信箱保存独立的 `Array<UInt8>` 消息，用于连接写出任务。长期所有者 `Mailbox` 由连接作用域
持有，`Handle` 是不保活信箱的复制句柄；它可以作为 HR2 平面记录参数传给 Script。句柄身份
在进程内不复用，操作只查当前执行器的注册表；旧句柄不会指向重建的连接。句柄也具备显式
关闭能力，不能把它当作跨进程连接 ID。

| API（均为 `@suspends`） | 行为 |
| --- | --- |
| `outbox.create(messages: UInt64, bytes: UInt64) -> Mailbox` | 创建数量与有效载荷字节双限额的 FIFO；数量必须大于 0，允许零字节容量 |
| `Mailbox.handle(&self) -> Handle` | 返回弱句柄 |
| `Handle.identity(&self) -> UInt64` | 返回不复用的身份，用于匹配提交失败的目标 |
| `Mailbox.receive(&mut self) -> Option<Array<UInt8>>` | 取出一条消息并立即归还数量与字节容量；空队列挂起并注册唤醒；关闭返回 None，空消息返回 Some(empty) |
| `outbox.close(handle: Handle)` | 幂等关闭，丢弃队列中的消息并唤醒接收方；Mailbox 析构也会关闭 |
| `outbox.batch(messages: UInt64, bytes: UInt64) -> Batch` | 创建暂存批次；按给定消息数预分配，无固定条数上限，允许零条空批次；字节上限独立于信箱上限 |
| `outbox.stage(batch: &mut Batch, target: Handle, bytes: Array<UInt8>) -> Result<Unit, StageError>` | 消费输入数组，复制到批次独立存储；不发送、不等待队列容量 |

`StageError` 为 Full 或 Closed。一次暂存失败会使整个批次失效，之后不能提交成功；即使忽略
返回错误，也不会只发布此前暂存的消息。无效或属于另一执行器的目标视为 Closed。批次资源
本身跨执行器使用则报运行时错误。分配失败也走运行时错误通道。所有暂存字节、排队消息和
预分配的队列/批次元数据纳入当前执行器的 `DomainLimits.max_state_bytes`，任务侧资源包装
另计 arena 配额；不把消息挂在永久任务 arena 上。

只有候选提交可以附带消息：

```joky
import joky/domain
import joky/outbox
import joky/array.{type Array}

struct State { count: Int32 }
fn make(n: Int32) -> State { State { count: n } }
fn read(s: &State) -> Int32 { s.count }

fn update(h: domain.WriteHandle<State>, target: outbox.Handle) @suspends -> domain.CommitReceipt {
    let snapshot = domain.snapshot_from(h.read_only(), read)
    let next = domain.candidate_from(snapshot, h, make, 42)
    var payload = Array.new::<UInt8>()
    payload.push(42)
    var messages = outbox.batch(1, 4096)
    outbox.stage(&mut messages, target, payload);
    domain.commit_to(h, next.with_outbox(messages))
}
```

`Candidate<S>`、`CandidatePair<S,T>` 与 `CandidateSet` 均提供 `.with_outbox(batch)`，
消费候选与 Batch，返回附带消息的同类型候选，然后调用对应的 commit/commit_before API。
每个候选或候选集合只接受一个批次；给单候选附加后再扩展为双候选时，批次随整个提交保留。
批次可含多个目标，同一目标的消息按暂存顺序发布。

提交先验证完整读取凭据、代码版本、截止时间和状态生命周期，再一次检查所有目标的容量
（包括重复目标的合计消息与字节数）。任一目标已关闭返回新增的 `OutboxClosed`，容量不足或
批次曾暂存溢出返回 `OutboxFull`；三种回执共用的 CommitError 提供这两个结果。拒绝时所有根和
消息均不发布；成功时根、消息和提交回执在同一无挂起区间发布，然后唤醒接收方。普通
`domain.write_to` 和可失败的原地写入不隐式获得消息回滚能力。

三种回执都提供 `failed_outbox(&self) @suspends -> Option<UInt64>`：在 `wait()` 返回
OutboxFull／OutboxClosed 后，Some(identity) 指出本次检查遇到的一个失败目标，可与
`Handle.identity()` 比较。它是该次拒绝的历史事实，接收释放容量、关闭邮箱或迟到取消不会
改变它；不会重新检查邮箱，也不保活邮箱。批次自身的消息数／字节上限溢出没有失败邮箱，
返回 None；尚未完成、成功提交或因非邮箱原因拒绝也返回 None。多个目标失败时不保证一次
列出全部，调用方处理目标后应重新准备并提交。暂存阶段的 Closed 同样保留失败目标。

批次元数据大小计算检查整数溢出，预分配或预算不足仍报告运行时错误；取消固定条数上限
不取消存储预算。容量只由信箱在入队、出队和关闭时管理，业务状态无需维护另一份发送额度。

等待提交前丢弃候选或取消未完成回执，会清理消息批次；取消已经提交的回执不会撤销消息。
接收只出队一次；取消可能丢弃已经出队、尚未写完的消息。出队后字节所有权归接收任务，
信箱关闭不会回滚在途网络发送。这里保证内存中的联合发布，不保证网络送达或 exactly-once。
写出任务可直接把收到的数组交给 `TcpWriter.write_all`，完整示例见
[出站信箱 TCP 测试](../tests/fixtures/outbox_tcp.jk)。

## Rust 宿主驱动任务

`runtime::executor::HostExecutor` 提供非阻塞推进；完整实例示例见
[host_tasks.rs](../examples/host_tasks.rs)，运行 `cargo run --example host_tasks`。
不增加脚本关键字，任务内部继续使用 `parallel`、`race`、`scope` 和 `branch`。

- `VmEngine::prepare_async(name, &[InstanceValue])` 验证精确标量签名并准备根任务，不执行用户代码。
  支持混合数值、Bool、Rune、Unit 参数与标量结果；Unit 参数仍显式传入，并保留既有 CPS 占位槽。
  不接受引用、拥有输入或引用结果。原有 `call_async*` 保持首次调用即推进的行为。
- `HostExecutor::new(clock, max_tasks)` 接受 `Rc<dyn Clock>`；`Clock::now()` 返回单调前进的 Instant；只有声明
  `allows_blocking_wait()` 的真实单调时钟能使用阻塞等待，SystemClock 已声明，虚拟时钟默认拒绝。
  执行器可 Clone，副本共享同一任务表、状态域和额度；最后一个副本释放时停止剩余任务。
  默认执行器使用系统时钟、4096 个活根限额。自定义时钟可暂停；`sleep_ms` 使用当前执行器时钟。
  用 `TaskGroup::new()` 建组，`spawn(&group, task)` 登记任务。组首次使用后绑定一个执行器。
- `poll(max_polls)` 只推进本次就绪快照，每个根至多一次，按 FIFO 就绪顺序执行。
  返回 `PollReport`：实际轮询数、已释放任务资源的完成结果、是否仍有就绪工作、最近截止时间。
  结果是标量原始位；失败保留结构化 Error。终态自动移出，执行器不积累历史结果。
- 宿主挂起回调中用 `current_waker()` 取得 `TaskWaker`；事件结果发布后调用 `wake()`。
  令牌可跨线程，只登记就绪，不执行脚本；重复通知合并，根任务取消/完成后的迟到通知无效。
  宿主仍须履行同步 stop 契约。一个根中任意分支的事件可使同根其他 Pending host 再次被询问，
  宿主轮询必须幂等，不能重放已经完成的操作。
- `cancel(id)` 取消一个根及其结构化后代；`cancel_group(&group)` 永久关闭组并取消全部根。
  同组首次失败也关闭组、取消尚未推进的兄弟根。不同组互不影响。
  执行中重入取消延迟到当前 poll 返回；组取消返回 false 表示尚有活跃 poll/stop，或组属于其他执行器。
  外部取消通常同步完成，销毁执行器会取消全部任务。

宿主自己决定每帧、事件到达或截止时间到达时调用 poll；poll 不休眠或启动后台线程，
它使用内置 Mio Reactor 有界收取 I/O 就绪。`wait()` 可阻塞等待最近截止时间、I/O 或
保留的 TaskWaker，返回后再 poll；虚拟时钟继续由宿主非阻塞推进。
未知外部等待安静保持 Pending，只有通知才再次就绪。`max_polls` 是根轮询额度，不是毫秒限制；
一个根可执行多个分支或长 native 调用。解释器的指令切片可补充限制，不能抢占 native 回调。
已启动任务可被接管，但注入时钟及捕获首次唤醒应使用 `prepare_async`，避免首次 poll 发生在执行器之外。

实例所属任务使用以下 VM API：

| API | 契约 |
| --- | --- |
| `configure_tasks(clock, max_tasks)` | 无活任务时配置 VM 执行器；max_tasks 必须大于零 |
| `spawn_instance_task(id, name, args)` | 从该实例绑定的模块准备标量根，检查 Ready 状态；不执行脚本 |
| `spawn_instance_task_with_limits(id, name, args, limits)` | 解释器专用：覆盖该任务的生命周期/切片限额，短 State 访问仍使用 VM 同步限额 |
| `current_instance_id()` | 任务推进、任务析构和 State 清理期间的所属身份；不持有 State 借用，其他上下文返回 None |
| `poll_tasks(max_polls)` | 零等待收取有界 I/O 事件并推进就绪实例任务；禁止递归推进或在同步实例调用中推进 |
| `wait_tasks()` | 等待 I/O、外部唤醒或最近截止时间；不运行脚本，返回后再 poll_tasks；逐帧宿主只用 poll |
| `cancel_instance_task(task_id)` | 取消一个根，保留实例可用状态 |
| `instance_task_count()` | 当前登记的根数，不含结构化子任务 |
| `call_current_instance(method, args)` | 仅在实例任务的同步 host 适配器中调用所属实例的受检方法 |

`call_current_instance` 的身份由执行上下文提供，脚本不能构造 State 指针；只调用同步、
已证明接收者借用不逃逸的同名义类型入口。返回标量，不保留借用；等待期间实例为 Ready，可通过
正常 `call_instance` 更新。适配器须用 `runtime::error::record_error` 把调用错误传回运行时，并立即返回；示例展示了这个约定。
同实例重入仍拒绝；旧实例的新旧任务均使用它绑定的旧代码版本。

实例任务首次失败使该实例 Failed，取消其其他根；State 在任务 stop 后释放。
`close_instance` 立即使身份失效并禁止新工作，先取消/stop 所属任务，清理任务临时资源，再析构 State。
执行中关闭会保留状态至当前 poll 或同步方法退出；迟到唤醒不能触达新实例。VM 销毁遵守相同顺序。
异步任务的临时 arena 与实例额度仍分开，执行器根数限额不代表 RSS 或所有后代的总内存限额。

JIT、解释器和 AOT 共用执行器。Rust AOT 宿主可用 unsafe `Continuation::from_native` 包装
未运行的生成入口及完整 CpsFrame，必须保持代码存活，并让生成对象与宿主链接同一份运行时。
目前未提供 C 执行器封装；Godot 的帧、信号和游戏/真实时间适配见
[Godot G2 用法](../integrations/godot/README.md#g2-异步任务)。

## Script 入口预检与共享模块

`Vm.require<A,R>(name: String) @suspends -> Result<Unit, LoadError>` 登记必需的完整入口契约。
建议在首次加载前登记所有宿主入口；已加载时还会先检查当前版本，失败不改变已登记契约。
后续 load／publish 必须同时满足全部登记项：入口存在、一个输入、挂起约定、完整输入和结果 schema。
不兼容版本返回 `LoadErrorKind.Incompatible`，当前入口和域代码均不改变；重复同一契约允许，
同名但不同的契约拒绝。普通调用仍检查实际契约，未登记的入口也可以按原有规则调用。

`Vm.module_source(name: String, source: String)` 和 `Vm.module_file(name: String, path: String)`
均为挂起方法，返回 `Result<Unit, LoadError>`，为后续编译提供显式依赖快照。
前者不执行文件 I/O；后者沿现有文件能力读取并关闭文件。模块名使用用户导入路径规则，
不允许覆盖 `joky`。更换依赖快照不改变运行中的代码，只有下次成功 load／publish 才使用它；
依赖文件也不会在每次调用或发布时自动重读。源 VM 的 load_source 不搜索磁盘。

宿主与脚本可导入同一个契约文件，见 `examples/chat_contract.jk`：宿主先用 module_file
登记该源文件，再用 require_contract 登记六个入口。每次操作固定选中的实现；源 Script 的
publish 还检查上一版具名 when／句柄方法的身份与完整捕获契约，以及全部仍存活的域 schema。
删掉具名操作、改变其访问／捕获／返回契约或遗漏活域类型都拒绝整个发布。匿名 when 仍只属于
创建它的版本，不提供跨版本晚绑定。底层 Rust publish_domain_code 仍保留按匹配类型选择的规则。

## 类型化 Script 调用 `Vm.call<A, R>`

`Vm.call<A, R>(&self, name: String, input: A) @suspends -> R`
在当前执行器上调用最近成功加载模块中的 `fn(A) @suspends -> R`。加载、兼容域代码发布和
显式关闭仍使用 `joky/vm`；不需要在业务侧编写 Rust 宿主。

```joky
import joky/vm

struct Request { player: UInt64, action: Int32 }
struct Reply { accepted: Bool, revision: UInt64 }

fn dispatch(h: &vm.Vm, request: Request) @suspends -> Reply {
    h.call::<Request, Reply>("handle_message", request)
}
```

`Vm.try_call<A, R>(&self, name: String, input: A) @suspends -> Result<R, vm.CallError>`
使用相同契约，显式处理调用边界内的失败：`Unavailable` 表示 VM／执行器不可用，
`Incompatible` 表示入口缺失或类型不符，`Failed` 表示执行、分配或结果传递失败。
失败先清理子 continuation、局部资源和错误通道，再返回 Err；之后可继续调用同一 VM。
子调用已经完成的外部效果不会回滚。业务返回 Result 时，成功调用仍得到 `Ok(业务结果)`。
输入打包和返回值解包在此边界之外，其失败仍传播给调用任务；父任务取消仍取消整个子调用，
不转换为业务 Err。原有 `call` 继续沿运行时错误通道传播失败。

Script 应声明同名、同字段契约的 Request/Reply，以及 `handle_message` 入口。A/R 分别支持
全部整数宽度、Float32/Float64、Bool、Rune、Unit、String、Array，以及由它们和元组、具体记录、枚举
嵌套组成的值，包括 `Option<String>`、`Array<Pair<String>>` 和业务 `Result<T,E>`。64 结构项／64 头部槽／8 层限制与
[域操作传递值载荷](#构造与短操作的传递值载荷)相同；所有分支和具体类型参数都要检查。
这些类型结构限制不限制字符串字节数或数组元素数，实际存储仍受现有运行时预算约束。
Map、引用、原始指针、资源所有者及含用户 Drop 的类型不能传递；双方递归检查 Drop，包括数组元素。
普通 `Context` 记录可组合目录、信箱与定时器
的复制句柄；这些句柄仍不保活对应资源，资源由宿主持有。

调用方编译时检查可桥接类型，运行时在执行目标代码前比较完整入口契约：一个输入、
`@suspends`、输入/输出类型；记录还比较类型名、字段名及顺序、每个字段的类型、可见性与
repr(C) 属性与具体类型参数；枚举比较名义名称、具体参数、分支名称／顺序和载荷契约，
元组比较元素数量、顺序和类型，嵌套逐层比较。
跨加载的内部名义 ID 不参与比较，布局相同但名称、分支顺序或载荷类型不同仍拒绝。
纯值逐标量复制；String 与 Array 按值序列化，在接收方建立独立拥有值，不传递 Buffer 或记录地址。
输入按值消费，需要保留时先 clone；数组只传递有效元素，容量不保留。结果在 VM 关闭后仍可使用。
含拥有型聚合值的源文件入口可通过该方法调用，不因此开放普通 C/Rust 宿主 ABI。
签名不符为 InvalidCall、入口缺失为 NotFound，
通过当前任务的运行时错误通道传播。

挂起调用
继续使用启动模块，新调用使用当前模块。结果在子流程清理后交付，取消同时回收桥接包；
取消借用它的子调用不关闭外层拥有者；拥有者所在任务被取消时自动关闭 VM。
加载编译仍为同步操作。语言侧调用统一使用该方法，原 `joky/script` 模块和
`vm.call_async_int32` 自由函数已移除。

## 内置标准库 `joky/vm`

直接在 Joky 程序中创建 VM、加载源码并调用函数：

```joky
import joky/io.{println}

import joky/vm

fn main() -> Int32 {
    let engine: vm.Vm = vm.Vm.new()
    let source = "fn add(x: Int32, y: Int32) -> Int32 {{ x + y }}"
    let name = "add"
    vm.load_source(engine, &source)
    println(vm.call_int32_2(engine, &name, 20, 22))
    vm.close(engine)
}
```

`vm.load_source` / `reload_source` 的源码、`register_int32_N` / `call_int32_N` 的名称均使用 `&String`。
`vm.call_int32_0/1/2` 分别接收 VM 借用、借用函数名和零到两个 Int32 参数。目标函数必须同步返回 Int32，
调用前检查完整签名。源码字符串中的字面花括号使用 `{{` / `}}` 转义。
`vm.Vm.interpreter()` 创建不生成机器码的解释器 VM；构建时的依赖仍见架构说明。
当前原生模块中的同步 Int32 函数可以作为宿主函数注册：

```joky
fn host_add(a: Int32, b: Int32) -> Int32 { a + b }
let name = "host_add"
let status = vm.register_int32_2(engine, &name, host_add)
```

`vm.register_int32_0/1/2` 分别支持零到两个 `Int32` 参数；被加载的源码用同名 `extern fn` 声明即可调用。
此特殊注册语法的函数名必须来自 JIT/AOT 原生代码；解释器函数不能转换成原生指针。
它与语言内函数值分开：后者支持三个后端，但不能传给此原生注册接口。
目标 VM 可以使用 JIT 或解释器，Rust 宿主也可以向两者注册受支持的原生函数。
注册和已加载的调用模块都会保留 JIT 代码所有权；替换注册只影响后续加载，
已有模块及 continuation 仍可调用旧入口。

| 接口 | 返回值与行为 |
|------|--------------|
| `vm.Vm.new()` | 创建默认 JIT `vm.Vm` |
| `vm.Vm.interpreter()` | 创建解释器 `vm.Vm` |
| `vm.close(value)` / `value.close()` | 消费 `Vm` 并提前关闭，正常返回 0；关闭后使用在编译期拒绝 |
| `vm.load_source(handle, source)` | 加载源码，成功返回 0，失败返回 1 |
| `vm.reload_source(handle, source)` | 替换模块，失败保留旧代码，状态码同上 |
| `vm.register_int32_0/1/2(handle, name, function)` | 注册当前模块中的同步 Int32 函数，成功返回 0 |
| `vm.call_int32_0/1/2(handle, name, ...)` | 返回函数结果；失败返回 0，并设置错误 |
| `handle.call::<A, R>(name: String, input: A) @suspends -> R` | 以当前执行器推进 `fn(A) @suspends -> R` 子流程，支持上述纯值契约；失败走当前任务的运行时错误通道 |
| `vm.module_count(handle) -> Int32` | 当前入口及仍被持有的历史已加载模块数量；未加载为 0，失败 -1 并设置错误 |
| `vm.print_modules(handle) -> Int32` | 向 stderr 按版本输出模块及已识别持有者，返回模块数；失败 -1 并设置错误 |
| `vm.publish_domain_code(handle) -> Int32` | 在该句柄绑定的当前执行器上发布兼容域代码，返回切换数量；失败返回 -1 并设置错误 |
| `vm.domain_schema(handle) -> UInt64` | 返回该 Script 唯一存活状态域的 schema 身份；失败返回 0 |
| `vm.prepare_migration(handle, schema: UInt64, source: &String, method: &String) -> Int32` | 暂存新模块并构造独立迁移候选；成功 0，失败 1 |
| `vm.begin_migration(handle, schema: UInt64, source: &String, start: &String, advance: &String) -> Int32` | 创建分步迁移候选；成功 0，失败 1 |
| `vm.step_migration(handle, budget: UInt64) -> Int32` | 推进一个同步批次；0 已完成，1 仍需推进，-1 失败；budget 必须大于 0 |
| `vm.begin_migration_batch(handle, source: &String) -> Int32` | 捕获该 Script 的完整域集合并暂存新模块；成功 0，失败 1 |
| `vm.stage_migration(handle, start: &String, advance: &String) -> Int32` | 为 start 返回类型的全部域实例构造候选；advance 为空表示同步完成，否则指定分步函数；成功 0，失败 1 |
| `vm.publish_migration(handle) -> Int32` | 校验后一起切换域根、类型和 Script 入口；成功 0，失败 1 |
| `vm.cancel_migration(handle) -> Int32` | 释放尚未发布的候选；有效句柄即使没有候选也返回 0，非法句柄返回 1 |
| `vm.error_code()` | 当前线程最近一次同步 VM 操作的状态：0 成功，1 失败 |
| `vm.print_error()` | 将最近一次错误写到 stderr |

同步操作开始前清除上次错误，两个错误查询接口不清除错误；异步调用的失败沿 CPS 传播，
不通过线程级 `vm.error_code()` 交付结果。
同步调用结果为 0 时，通过 `vm.error_code()` 区分正常结果与失败；应在下一次同步操作前查询。
`vm.Vm` 是源码库声明的拥有型记录，内部 `vm.Owner` 与 TCP owner 共用 Resource 存储。
它不能复制，移动转移所有权；作用域退出、提前返回、运行失败与任务取消都会自动关闭。
除消费型 close 外，各操作接受 `&vm.Vm`；同步函数允许隐式借用，`Vm.call` 使用共享方法接收者。
多个共享借用允许挂起调用与加载/重载并行；借用存活期间不能移动或关闭拥有者。
底层 VM 仍绑定创建线程，Rust/C 的数值句柄接口保持原有显式关闭协议。
模块与成员别名见[导入与 prelude](#导入与-prelude)。Joky 内的 `vm.load_source` 不隐式搜索文件，
显式依赖快照使用 `Vm.module_source`／`module_file` 或 Rust API；同步 `Int32` 宿主函数可通过 `vm.register_int32_0/1/2` 注册。

`Vm.call` 的名称是拥有型 String，不跨模块保留业务借用。首次异步调用或发布将句柄
绑定当前执行器，此后从另一执行器或原执行器销毁后调用均拒绝；不创建额外事件循环。
Script 使用宿主的时钟、唤醒和域注册表，调用帧在完成、失败或取消时清理子 continuation。
关闭 VM 后挂起调用在下一次推进或取消时清理；取消某次借用调用不会关闭外层 VM 拥有者。
已有流程保留启动模块，新调用使用最近成功加载的模块。加载和编译仍是同步操作。

模块诊断不需要正在执行的任务，也不会首次绑定执行器。`module_count` 与 `print_modules`
只观察该句柄成功安装过的模块，不包括尚未发布的迁移暂存模块。输出字段为句柄内的 `module`
版本、`current`、`references`（观察时强引用数）、`script_calls`、`domains`、`domain_tasks`、
`queued`、`results`（已完成未领取）、`migrations`。分类计数不等于 Rc 引用数；已交给流程的
快照／候选、宿主函数注册等额外引用当前不逐对象归因。模块历史、调用观察均不保活代码，
旧版本最后一个持有者释放后从下一次查询消失；字段不表示字节数或 RSS。

语言侧 `publish_domain_code` 只选择该 VM 句柄曾加载且仍存活的模块所拥有的域，其他 Script
或 AOT 宿主中同名状态不受影响；仅加载不发布不会替换域代码。跨版本布局与调用兼容规则同
前述状态域接口。标量范围与验收见[热更新计划](aot-script-hot-reload.md#61-hr2-标量异步探针已实现)。

### 文件加载与原子兼容发布

`Vm` 提供返回 `Result` 的便利方法；以下方法均为 `@suspends`，接受拥有型 `String`：

| 方法 | 返回值与行为 |
| --- | --- |
| `engine.load_source(source)` | `Result<Unit, vm.LoadError>`；成功替换当前 Script 入口，已有状态域继续使用其原代码 |
| `engine.load_file(path)` | 同上；使用调用任务的文件服务读取完整 UTF-8 源码，并在编译前关闭文件 |
| `engine.publish_source(source)` | `Result<UInt64, vm.LoadError>`；编译暂存模块，校验并一起切换当前 Script 入口与所属兼容域的代码，返回切换域数 |
| `engine.publish_file(path)` | 同上；先读取并关闭文件，再执行同一原子发布路径 |

加载失败保留当前入口。原子发布的编译、兼容性检查或周期任务预备失败时，当前入口与
目标域代码均保持原版本；预备任务被取消。成功时即使没有待切换域，也安装新入口并返回 0。
域的选择沿用 `publish_domain_code`：只处理该 Script 历史模块所属、仍存活且新版声明了同名
状态类型的域；其他 Script 的域不受影响。布局变化必须显式迁移。数据根和数据修订不因
兼容代码发布改变；已启动流程仍使用启动版本，之后的域操作按已有晚绑定规则执行。

文件入口沿用 `fs.File.read_text()`，不提供额外的 `_limited` 方法；任务存储与文件服务
预算仍生效。路径按宿主文件服务解析，CLI 本机服务以启动工作目录为基准。没有文件能力时
返回 `Io(Unsupported)`；读取脚本不会给子 VM 自动授予文件能力。读取或 UTF-8 校验失败后
仍关闭文件；读取与关闭同时失败时报告读取错误，读取成功而关闭失败时不编译、不发布。
文件内容读取完毕后固定为本次编译输入，但不保证并发写入期间的文件系统快照一致性。

第一版保持单源码导入范围，不自动搜索文件依赖。错误的 `path` 保存读取路径；源码方法
使用 `<source>`。诊断文件名不改变根模块默认逻辑身份 `main`，因此同一 Script 从另一条
文件路径加载兼容实现，不会仅因路径不同改变 `when` 操作身份。

`vm.LoadError` 包含拥有型 `path: String` 与 `kind: vm.LoadErrorKind`：

- `Io(fs.IoError)`：打开、读取或关闭失败；保留文件错误分类和消息。
- `Utf8(Utf8Error)`：文件内容不是合法 UTF-8。
- `Compile(vm.Diagnostic)`：解析、导入解析、类型检查或代码生成失败。
- `Incompatible(vm.Diagnostic)`：活跃目标域的状态 schema 不兼容。
- `Conflict(vm.Diagnostic)`：执行器／操作状态不匹配，或发布准备期间域凭据发生变化。
- `Runtime(vm.Diagnostic)`：其他运行时失败，包括发布的资源不足。

`Diagnostic` 含 `message: String`、`has_span: Bool`、`start/end: UInt64`；有位置时为源码
UTF-8 字节的半开区间，无位置时两端为 0。错误值独立保存，可跨挂起保留，不依赖
`vm.error_code()` 或 `vm.print_error()`。任务取消、任务预算不足以构造结果以及内部 ABI
违约仍走任务失败／清理通道，不保证都能分配出业务 `Err`。提交完成后、调用方领取结果前
发生取消或任务失败，不撤销已经提交的代码更新。

旧的自由函数 `vm.load_source(engine, &source)`／`vm.reload_source` 保留状态码协议；
`engine.load_source(source)` 是新的拥有型、可挂起、`Result` 方法。文件 I/O 可挂起，
编译和最终发布仍同步执行，不提供编译线程池、文件监视或自动重载。

Rust 嵌入方可使用 `VmEngine::load_named_source(name, source)` 保留诊断文件名；
`VmEngine::publish_source(source)` 对该引擎域注册表执行同样的原子兼容发布。
行为示例见 [文件加载／发布 fixture](../tests/fixtures/vm_source.jk)。

### 显式状态迁移

迁移入口是 `fn upgrade(old: &PreviousState) -> Room`。`PreviousState` 由运行时根据
实际旧根的类型生成，再与新源码一起编译；业务源码不能重复声明该名字。新根必须与旧根同名，
且 schema 确实变化；根仍限非泛型记录。HR5-B1 支持无用户 Drop 的有限拥有图：嵌套记录、
String、Array、Map、元组及无拥有载荷的枚举。只改变容器元素字段也算 schema 变化。
编译器证明迁移同步、旧引用不逃逸、不修改旧图且不调用宿主；旧容器必须借用读取并逐项
构造新容器，String 可显式 clone，不能从旧借用中搬走拥有者。借用、外部资源、用户 Drop、
递归拥有图、拥有载荷枚举仍不支持；多域批次入口见下文。

旧根名固定为 `PreviousState`。嵌套用户类型按声明名生成 `PreviousPlayer` 等旧视图；同名
冲突时使用按遍历顺序分配的 `PreviousTypeN`。它们只保留旧字段/变体，不继承旧业务方法，
生成名不能由业务重复声明。Array/Map 视图使用当前标准库的受检借用方法；运行前递归验证
实际旧图与生成视图的布局，包括容器元素，不能把新版 Player 当作旧元素读取。
实际示例见[拥有图迁移 fixture](../tests/fixtures/migration_owned_new.jk)：
`upgrade_player(old: &PreviousPlayer)` 为旧元素构造新版 Player，然后填入独立的新容器。
新源码沿用 `load_source` 的单源码导入范围，不隐式搜索本地文件。

使用顺序是取得 `domain_schema`，调用 `prepare_migration`，检查成功后再调用
`publish_migration`。准备期间当前入口和旧根不变；最终发布检查域身份、存活状态、修订、代码
版本、schema 身份及 Script 当前模块，任何冲突都拒绝发布，保留已完成的正常业务写入。
进入发布校验后无论成功或失败都会消费候选；进入前的句柄或执行器检查失败可能保留候选，
可显式取消。每个 VM 最多保存一个候选，再次准备会失败。准备后 `load_source` 改变当前模块
会使原候选发布失败。

HR5-B2 可改用 `begin_migration`，新源码提供两个受证明的同步入口：
`fn start(old: &PreviousState) -> Room` 构造初始候选，
`fn advance(next: &mut Room, old: &PreviousState, budget: UInt64) -> Bool` 逐批修改候选，
返回 true 表示可以发布。进度由候选字段或容器长度表达。两个引用均不得逃逸，旧引用只读；
拥有图与宿主效果限制和一次性迁移相同。完整示例见
[分步迁移 fixture](../tests/fixtures/migration_incremental_new.jk) 及
[三后端宿主测试](../tests/cli.rs) 的 `script_incremental_migration_yields_conflicts_cancels_and_publishes`。

宿主检查 begin 成功后，循环调用 step；返回 1 时可 `task.yield_now()`，返回 0 后才 publish，
返回 -1 时停止。1 是正常进度，`error_code()` 仍为 0。已经完成的候选再次 step 只做凭据检查，
不会重跑回调；一次性 prepare 的候选不接受 step。提前 publish 会失败并消费未完成候选。
进入 step 候选校验后的任何错误（包括零预算）都会丢弃候选；进入前的句柄、执行器或重入检查
失败可能保留候选。begin 和 prepare 共享“至多一个候选”的限制。

每批开始和最终发布都验证最初捕获的完整读取凭据、当前入口及唯一域选择。每批同步借用
当前旧根，返回后不保留旧指针；批次间旧根写入、关闭或代码/schema 变化会使下一步失败。
这里不复制或保活旧版本快照，不会在不同修订间拼接新图，也不自动重试；持续写入的域可能
需要业务暂缓写入再重建。budget 是业务解释的工作量提示，不是时间片；编译、start、每次
advance 及候选图扫描/回收仍同步执行。解释器指令限额按回调生效；JIT 不提供强制抢占。

除 `cancel_migration` 外，这些接口要求当前执行器。单域接口按 Script 模块归属选中恰好一个存活域；
零个或多个都拒绝，不影响其他 Script 的域。schema 身份是进程内不复用的非零令牌，不能作为
持久化 schema 版本使用；迁回旧布局也获得新令牌。成功后域身份与目录注册保持不变，新代码
可重新按新类型查找；旧布局调用被拒绝，旧快照/候选凭据失效，既有目标定时器终止为
`InvalidAccess`，需显式重新登记。已启动的流程仍保留旧代码，状态访问继续接受契约检查。

HR5-C 的批次接口与单域候选共享 VM 的唯一迁移槽。begin_migration_batch 在同一同步区间捕获该
Script 所有存活域（1–256 个）的身份、修订、代码与 schema 凭据；顺序按域创建序号固定。
它编译暂存新模块，尚不执行迁移构造。旧根按名生成 `PreviousRoom`、`PreviousLedger` 等
只读视图，嵌套类型共用去重视图。根类型短名冲突、同名旧根存在不兼容布局，或业务重复声明
生成名时拒绝整个批次；单域入口继续使用 `PreviousState`。

新模块通过已证明的构造/域操作声明了同名同布局类型时，这类域自动作为保留原根的读取依赖；
发布时更新其代码与 Type，修订、schema 与根地址不变。其余类型必须逐类 stage：
`fn start(old: &PreviousRoom) -> Room` 的返回名选中批次内全部 Room，不能只遗漏其中某个实例。
提供 advance 时要求 `fn(&mut Room, &PreviousRoom, UInt64) -> Bool`；空字符串表示 start
已完成整个候选。同一类型重复 stage、目标不在批次内、旧视图不匹配或构造中途失败均拒绝。
业务名称作为字符串传入，不需要命名约定或新关键字。

所有类型 stage 后可用 `step_migration` 推进，每次最多执行一个未完成根的回调，按轮转顺序
选择；0 表示整个批次就绪，1 表示仍有工作。未 stage 完就 step 或 publish 会失败。每次 stage、
step 和最终 publish 都校验完整集合（包括保留原根的域）、集合成员及当前 Script 绑定。
阶段校验或用户迁移失败后消费整批，之前成功构造的候选也一起释放；不会撤销期间正常业务
写入。进入前的句柄/执行器/文本参数检查失败可能保留批次，可显式 cancel。

最终发布先检查所有域的计数余量，再在不交错域操作的区间一起切换全部目标根、域代码和
Script 当前入口；清理旧根、模块与失效定时器在区间外执行。变更布局的域获得新 schema 与
递增修订，保留原根的域也递增代码版本。域身份/目录项保持不变，只有变更布局域的既有
定时器失效；兼容域的定时器继续按新代码执行。用例见
[批次旧源码](../tests/fixtures/migration_batch_old.jk) 与
[批次新源码](../tests/fixtures/migration_batch_new.jk)：两个 Room、一个 Ledger 同批迁移，
Rules 保留原根且参加冲突校验。

批次目前限同一 VM 句柄拥有的完整域集合；不包含其他 Script 的域，不提供跨 Script 原子发布、
显式子集选择、跨域聚合迁移函数、冻结/自动重试或旧版本快照。构造函数只读自己的旧根，所有
候选基于同一次捕获且始终重新验证的域版本集合。按类型 stage 会同步构造该类型的所有骨架，
step 中的单次回调、图扫描及回收也同步执行；不保证硬时间片。

候选归 `vm.Vm` 所有。取消借用 VM 的子任务时，外层拥有者仍保留候选；可用 `cancel_migration` 放弃。
拥有者退出或其任务取消时，自动关闭 VM 并释放未发布候选；也可消费型 `close` 提前关闭。
旧根、候选及迁移中的临时分配同时计入状态预算。分步接口允许宿主在批次间让出执行权，
编译及单批计算仍同步占用执行器，不提供后台编译或强制超时；
解释器沿用指令预算，JIT 计算不抢占。准备失败自动清理部分候选。三种宿主与两种 Script 后端的
可执行验收见 `migration_script_staging_and_atomic_publication_run_in_all_backends`；
[新版 fixture](../tests/fixtures/migration_new.jk) 依赖准备时注入的旧类型，不能单独加载。

`@suspends` 函数可以用 `parallel` 并发等待多个分支；下面是分支均返回 Unit 的写法：

```joky
fn main() @suspends {
    parallel {
        | fetch_left()
        | fetch_right()
    }
}
```

每个分支必须是 `@suspends` 的函数调用；parallel 轮询所有未完成分支，全部完成后继续执行。
分支可返回不同的无引用类型，结果按源码顺序组成元组，与完成先后无关。
例如 `let (left, right) = parallel { | read_left() | read_right() }`。
单个非 Unit 分支返回 `(T,)`；混合结果保留 Unit 位置，例如 `(Int32, Unit, String)`；
全部分支均为 Unit 时，整个表达式仍返回 Unit。结果范围与下文 race 相同，沿用分支函数签名。
每个已完成分支的拥有结果立即由父任务接管；等待其余分支期间发生失败或取消时也会清理。
尾随分号可以丢弃结果，拥有结果仍按正常作用域规则释放。
可运行的 scope / parallel 结果与清理用例见 [structured_values.jk](../tests/fixtures/structured_values.jk)。
分支失败会停止后续轮询并释放其他分支；解释器还立即释放整个执行状态。
分支可分配对象及接收引用，借用范围与冲突规则见 [parallel 分支借用](#parallel-分支借用)。

`race` 使用相同分支形式，等待首个观察到的完成或失败：

```joky
fn first() @suspends {
    race {
        | fetch_left()
        | fetch_right()
    }
}
```

- 只能出现在 `@suspends` 函数中，至少一个分支；分支必须是挂起函数调用，支持直接调用、
  方法调用及挂起函数值。所有分支必须返回相同的类型 T，`race` 的结果为获胜分支的 T 值。
  Unit 分支仍返回 Unit；不返回获胜编号，也不自动把不同分支类型合成为枚举。
- 支持现有 CPS 可返回的无引用标量、Copy 聚合、String 及完整存储树无引用的拥有聚合与容器。
  引用、含引用结果及函数／闭包值结果不开放；结果类型沿用各分支已确定的函数签名，不由外层上下文改变。
  可以用 `let value = race { | first() | second() }` 接收结果，用尾随分号丢弃结果。
  定时器返回 Unit，和返回数据的请求组合时应分别包装成返回同一枚举类型的挂起函数。
  `Result.Err` 是正常返回的数据，同样可以胜出；race 不会自动等待第一个 Ok。
  可运行的结果与清理用例见 [race_values.jk](../tests/fixtures/race_values.jk)。
- 按源码顺序准备所有分支实参，再按源码顺序轮询。每轮遇到 Pending 才轮询下一个分支；
  首个观察到的完成使 race 成功，首个观察到的失败使整个任务失败，不会继续等其他分支成功。
  同轮多个分支就绪时由轮询顺序决定；较早分支立即完成时，后续分支可能一次也未被轮询。
- 父函数继续前，取消并释放其余分支及所有嵌套子帧；重复 Pending 不重算实参。
  获胜结果的拥有权先交给父任务，再清理其余分支；清理失败时传播错误并释放已取得的结果，
  父流程不会获得成功结果。被丢弃的拥有结果也按正常作用域规则清理。
  取消不撤销已经发生的写入或其他副作用；拥有值沿用同步 Drop，不提供 defer。
- 借用按所有分支同时运行检查，即使某个分支会立即获胜也不放宽冲突规则。
  未被保留的参数借用在 race 结束后释放；所有分支共享根任务 arena，取消不会回收其中的对象。
  分支按参数级效果保留实际保存的引用，沿用普通调用的任务期存储契约。
- 这是单线程协作并发，不承诺抢占或 CPU 公平性。旧 CPS 注册不授予宿主跨 Pending 保留 Joky 引用的权限；
  使用异步借用 v1 的分支在取消时同步执行 stop，确认外部访问停止后才恢复父任务。

## 顺序任务作用域 `scope` 与动态子任务 `branch`

`scope { ... }` 在挂起函数内打开一个顺序主体：主体按普通块的求值顺序执行，
主体中的 `branch { ... }` 在执行到时才把一个子任务登记进当前作用域。
主体与全部已登记子任务（含子任务自己的后代）都完成后，scope 才返回主体末值：

```joky
fn work(a: Int32, b: Int32) @suspends {
    scope {
        let counter = alloc(Counter { value: a })
        branch { bump(counter, b) }
        update(a)
        branch { read(counter, a) }
    }
    finish()
}
```

- `scope` 按普通块规则取得主体末值，支持与 parallel / race 相同范围的无引用结果；
  末值在汇合前保存，全部子任务完成后才交给调用者。空主体或尾随分号返回 Unit，
  外层期望类型可用于主体末值的推导，例如 `let value: Int8 = scope { 42 }`。
  保存的拥有值在汇合失败或任务取消时释放；`return value` 仍表示汇合后返回外层函数，
  不会变成 scope 的普通末值。
  `branch` 主体及表达式仍返回 Unit，主体继续执行，无需等待该子任务。
  两者都只能在 `@suspends` 函数中使用；`branch` 绑定同一函数内最近的 `scope`，
  没有合法所属作用域时是编译错误。被调用函数不会继承调用者的作用域，需要自己声明。
- `branch` 主体按整绑定捕获自由变量：Copy 值在登记时快照，此后修改外层变量不影响子任务；
  非 Copy 拥有值在登记时移出父流程，之后父流程不能使用；引用捕获沿用现有来源与权限检查，
  并在检查中按独占处理——同一绑定被两个分支引用捕获、或分支登记后父流程仍以冲突方式访问，
  都会被编译拒绝。捕获不开放通用闭包，也不按字段隐式拆分。
- 登记时先取得捕获快照并转移清理责任，子任务才对调度器可见；`branch` 表达式内不内联执行子任务。
  主体实际挂起或进入 scope 汇合时，子任务按登记顺序获得轮次推进；一轮中已 Pending 的子任务
  不会被反复重试，全部 Pending 时任务把线程交还调用者。
- 主体正常结束后，scope 汇合：停止接受新的登记，等待全部子任务与后代完成。主体中的
  `return` / `break` / `continue` 越过 scope 时，先保存跳转目标，完成被越过 scope 的汇合
  再跳转；branch 主体内的 `return` 只结束该子任务，`break` / `continue` 不能跳回父流程。
- 主体或任一子任务失败会取消相关任务树并完成清理，再传播首个观察到的错误；
  父任务取消同样取消全部后代。正常汇合只关闭登记通道，不回收任务 arena 中的对象。
- 嵌套的显式 scope 创建新作用域；branch 主体是独立子任务入口，可以再创建自己的 scope/branch，
  父分支必须等这些后代完成才向外报告成功。动态子任务计入解释器帧限额；
  原生后端对单个帧的活动动态子任务数有明确额度，超限返回受控错误。
  借用冲突规则见 [parallel 分支借用](#parallel-分支借用)，动态捕获沿用同一冲突检查。

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

## 内置标准库 `joky/task`

`joky/task` 提供两个返回 Unit 的挂起函数，需要显式导入，关闭 prelude 时也可使用：

| 接口 | 行为 |
|------|------|
| `yield_now() @suspends` | 主动返回一次 Pending；恢复后完成，让 parallel/race 继续轮询其他分支 |
| `sleep_ms(milliseconds: UInt64) @suspends` | 使用单调时钟等待指定毫秒数；零毫秒可以立即完成，无法表示的截止时间报告运行时错误 |

```joky
import joky/io.{println}
import joky/task

fn report_int32(value: Int32) { println(value) }
fn main() @suspends {
    task.yield_now()
    task.sleep_ms(10)
    report_int32(42)
}
```

`joky run`、`joky run --interpreter` 和 `joky build` 都支持无参数的挂起 main，返回 Unit 或 Int32；
退出码与同步 main 一致：Unit 为 0，Int32 取低 8 位，运行失败为 1。
挂起效果仍沿调用链检查，不引入 await，也不会把同步调用自动转换为挂起调用。
完整例子见 [async_main.jk](../examples/async_main.jk)。

CLI 的阻塞驱动器在定时器就绪前休眠，选择所有存活分支的最早截止时间；恢复不会重放已完成的副作用。
这是单线程协作调度，定时器不承诺精确唤醒时间或 CPU 公平性，也不能抢占不让出的循环。
race 取消或任务失败时，唤醒信息随帧清理，不留下定时线程或已取消的定时器注册。
阻塞驱动器遇到 Pending 且没有支持的唤醒来源时才报错；不会对任意宿主回调无限忙轮询。
外部事件使用 [HostExecutor](#rust-宿主驱动任务) 的线程安全唤醒令牌，可中断阻塞等待；
I/O 通过 `runtime::reactor::IoSource` 登记到内置 Mio Reactor，虚假就绪后须重新尝试并在 WouldBlock 后 rearm。

Rust 的 `runtime::driver::block_on(task)` 消耗标量 continuation，优先使用 `VmEngine::prepare_async`
确保首次 poll 已有时钟/唤醒/Reactor 上下文；已启动的普通任务仍可传入。成功返回原始结果位，
失败会取消并清理任务。C/AOT 使用 `joky_cps_block_on` 驱动新建根帧，传入编译器报告的完整槽数；
终态释放子任务及 arena 后保留根帧结果，调用方仍需 frame_free。
公共 C 接口见 [joky_cps.h](../include/joky_cps.h) 与 [joky_task.h](../include/joky_task.h)。

## 内置标准库 `joky/string`

String 拥有合法 UTF-8，允许内部 NUL。它不是 Copy：赋值、按值参数和返回会移动，
`clone()` 显式复制，`release()` 消费并提前释放；作用域结束、失败和取消自动清理。
字面量直接构造预算内的独立 String，构造后不依赖模块常量。`var` 可重新绑定，
`&mut String` 只允许整体替换，例如 `mem.replace(&mut text, next)`，不提供可写字节。

String 固有方法由默认 prelude 提供；关闭 prelude 时显式 `import joky/string`。
同名模块函数使用显式共享引用，方法接收者沿用自动借用：

```joky
import joky/string

fn inspect(text: &String) -> Option<UInt64> {
    let needle = "🙂"
    string.find(text, &needle)
}
fn sample() -> UInt64 {
    let text = "中🙂\0tail"
    let copy = text.clone()
    text.release()
    copy.byte_length()
}
```

| 方法 / 模块函数 | 返回值与行为 |
| --- | --- |
| `String.new()` / `string.new()` | 拥有空串 |
| `text.clone()` / `string.clone(&text)` | 独立复制，不消费输入 |
| `text.release()` / `string.release(text)` | 消费并提前清理 |
| `text.byte_length()` / `string.byte_length(&text)` | UInt64，UTF-8 字节长度 |
| `text.is_empty()` / `string.is_empty(&text)` | Bool |
| `text.equals(&other)` / `string.equals(&text, &other)` | Bool，按字节内容相等 |
| `text.find(&needle)` / `string.find(&text, &needle)` | Option<UInt64>，首次命中的字节偏移；未命中 None，空 needle 为 Some(0) |
| `text.starts_with(&prefix)` / `string.starts_with(&text, &prefix)` | Bool，空前缀为 true |
| `text.ends_with(&suffix)` / `string.ends_with(&text, &suffix)` | Bool，空后缀为 true |
| `String.from_utf8(data)` / `string.from_utf8(data)` | Result<String, Utf8Error>，成功验证后转移 Array<UInt8>，失败也消费并清理输入 |
| `text.bytes()` / `string.bytes(&text)` | 只读 &Slice<UInt8>，依赖整个输入 String；不分配或复制载荷 |
| `text.into_bytes()` / `string.into_bytes(text)` | 消费式 Array<UInt8>，转移原存储及容量 |
| `text.substring(start, end)` / `string.substring(&text, start, end)` | Option<String>，复制半开字节范围；逆序、越界或非 UTF-8 边界为 None |
| `text.rune_count()` / `string.rune_count(&text)` | UInt64，Unicode 标量数量 |
| `text.next_rune(offset)` / `string.next_rune(&text, offset)` | Option<(Rune, UInt64)>，标量和下一字节偏移；末尾 None，非法偏移为带源码位置的终止性 RuntimeError |
| `left.concat(&right)` / `string.concat(&left, &right)` | 独立 String，不消费两侧输入，组合长度沿 Array.reserve 受检 |
| `string.join(&parts, &separator)` | String，借用 Array<String> 与分隔符，返回独立拼接结果；总长度受检 |
| `text.split(&separator)` / `string.split(&text, &separator)` | Result<Array<String>, SplitError>，复制各项；空分隔符为 Err(SplitError.EmptySeparator) |

byte_length、equals、find 等只读查询不消费输入、不创建新的拥有文本，偏移不表示字符数或显示宽度。
非法 UTF-8 与空句柄不能成为合法语言 String。命名借用阻止移动、release 与冲突替换；
单独 String 可跨 Pending 并移动到 parallel/race 分支，`&String` 也可在拥有者存活时跨挂起。
String 和 StringBuilder 支持同步记录、枚举、Option/Result、元组、泛型、Array 元素及 Map value，
嵌套拥有者随移动、消费匹配、作用域退出和失败递归清理。它们仍是非 Copy；容器读取使用
`borrow_ref`，不能通过 Copy `get` 复制文本。完整存储树无引用的 Array、Map、StringBuilder 和其他拥有聚合可跨普通语言挂起；
无引用元素的数组视图同样可跨语言挂起；含引用元素的 Array 仍受限制。
String Map key 已支持借用式内容协议和查询，接口见上文 key 协议。

join 不消费输入，空数组得到空串，单元素也生成独立文本，空分隔符表示直接连接。
split 按非空分隔符从左到右不重叠匹配，保留头尾和相邻分隔符产生的空项；空输入得到一个空项。
各项均复制内容，释放输入后仍可使用；部分构造失败会清理已完成项，分配失败仍为终止性 ResourceLimit。
示例见 [string_collections.jk](../examples/string_collections.jk)，三后端输出 5、11、6。

UTF-8 验证严格拒绝过长编码、孤立续字节、代理项、超出 U+10FFFF 的编码及截断序列，不做替换解码。
`joky/string` 导出 Copy 记录 `Utf8Error { code: Utf8ErrorCode, offset: UInt64 }`；
`Utf8ErrorCode.InvalidSequence` 表示已有字节非法，`UnexpectedEnd` 表示合法序列前缀被截断。
offset 始终指向第一个非法序列的起始字节。错误不拥有文本；分配失败仍为终止性 ResourceLimit。
公开 bytes 不能写入或逃逸来源；活动视图阻止 String 移动、release、into_bytes 和整体替换。
into_bytes 后数组可修改，再构造 String 必须重新经过 from_utf8 验证。
substring 的 start/end 为 UInt64 字节偏移，合法空范围返回拥有空串，结果在输入释放后仍可使用。
rune_count 不计算用户感知字符或显示宽度：`"e\u{301}"` 有两个 Rune。

导入 `joky/string`（默认 prelude 已导入）后，Int32、Int64、UInt32、UInt64 提供
`value.to_string() -> String`，输出无前缀、无前导零的十进制；负数带 `-`，最小负数也正确处理。
不增加字符串 `+`、插值或隐式数值转文本。完整示例见
[string_queries.jk](../examples/string_queries.jk)。

Rust 根入口 `call_string_0/1` 接收零参数或一个 `&str`，返回宿主拥有的 Rust String；
挂起版本 `call_async_string_0/1` 返回 StringContinuation，完成后 `take_result()` 仅提取一次。
输入复制进任务 arena，输出在 arena 释放前复制到宿主，重载或销毁 VM 不改变已取得文本。
失败与取消没有文本结果。数字槽 API 拒绝文本入口。

宿主文本通过显式 `register_text_fn` / `register_text_suspending_fn` 注册；
需要代码 owner 或完整帧槽数时使用对应 `register_owned_text_*`。
输入使用 `JokyText { data, byte_length }` 借用描述符，直接引用受保活的原载荷；
返回描述符在有效期内校验并复制。同步输入仅在调用期间有效，CPS 输入保活到完成或取消。
旧长度前缀 ABI、旧文本注册 API 和五个旧 C 查询已删除；VM 文本 C 入口改用 `_v2` 后缀，
接收 `const JokyText *`，需要重新编译宿主绑定与 AOT 对象。
新 AOT 文本出口由 `AotCompiler::text_exports_v2()` / `text_header_v2()` 给出符号和 C 原型，
结果使用 JokyOwnedTextV2 与配套释放函数。完整布局和生命周期见[架构说明](architecture.md#宿主与字符串-abi)。

同步调用普通 C 文本 API 可使用 `VmEngine::register_text_ffi_fn`，源码沿用 `extern fn`。
首版支持最多四个 `String` / `&String` 参数及 String、整数或 Unit 返回值。注册描述符指定
指针/长度或 NUL 输入、借用/外部拥有输出或受控写缓冲区；内部 NUL、非法 UTF-8、超长输出
和宿主错误均在边界失败。String 输出在输入失效和外部释放前复制成语言拥有值。
AOT 使用 `AotCompiler::register_text_ffi_fn` 注册同一签名，并通过 `text_ffi_header_v1()`
生成描述符声明。该注册不开放源码裸文本指针、公开 CString、任意 C 原型推导或异步保留。

可执行示例：[Rust 注册与 AOT 生成](../examples/text_ffi.rs)、[C 分配/释放适配](../examples/text_ffi.c)。
Rust 示例调用真实 `strlen(const char *)`，输入上限 127 字节、含 NUL 输出上限 128 字节，
返回对调用期临时输入的借用；C 示例调用 `char *foreign_duplicate(const char *)`，返回
malloc 分配的 UTF-8，由适配器移交凭据并交给 free 释放，分配失败返回非零状态。

```sh
cargo run --example text_ffi -- /tmp/joky-text-ffi
cargo build --lib
cc -Iinclude -I/tmp/joky-text-ffi examples/text_ffi.c /tmp/joky-text-ffi/text_ffi.o \
  target/debug/libjoky.a -o /tmp/joky-text-ffi/demo
/tmp/joky-text-ffi/demo
```

上例为 macOS 本地链接命令；Linux 按 runtime 链接要求追加 `-ldl -lpthread -lm` 等系统库。
详细布局、长度计数和释放不变量见[宿主与字符串 ABI](architecture.md#宿主与字符串-abi)。

### Show、print / println 与字符串插值

`import joky/show.{type Show}` 导入静态显示协议；实现写入调用者的 StringBuilder，
不消费或保留输入、输出的借用。签名如下，调用沿用普通 trait 的单态化与一致性规则：

```joky
pub trait Show {
    fn show(&self, output: &mut StringBuilder) @noescape
}
```

内置实现覆盖八种整数、Float32/Float64、Bool、Rune、Unit、String，以及元素满足 Show 的
Array、Option、Result 和 key/value 都满足 Show 的 Map。整数为十进制；浮点沿用 to_text，
包括 `-0`、`NaN`、`inf`；Bool 为 true/false，Unit 为 `()`，Rune 和 String 原样输出。
Array 为 `[a, b]`，Option 为 `Some(a)` / `None`，Result 为 `Ok(a)` / `Err(e)`，
Map 为 `{k: v, ...}`，顺序沿用其无序容器契约。Show 是显示文本，不是带引号转义的序列化格式。
自定义记录/枚举须显式实现，当前不提供自动派生或元组实现。

`joky/show` 的 `to_text(&value) -> String` 生成拥有型文本，
`append(&value, &mut output)` 向既有 builder 追加。
默认 prelude 导入 `joky/io` 的 `print` / `println`；关闭 prelude 时可显式
`import joky/io.{print, println}`。它们接受 `&T`，要求 `T: Show`，
print 无换行，println 添加一个 LF。先完成整次格式化，再通过共享的 TextViewV2 宿主入口
写入 stdout，支持 UTF-8 与内嵌 NUL；写入失败记录运行时错误，已输出的前缀不回滚。
CLI 不再预注册类型专用打印宿主。

```joky
fn main() {
    let count = 42
    let message = "count={count}, next={count + 1}"
    println(message)
    println(count)
    println(count + 1)
    println("again={message}")
}
```

普通函数调用支持受限自动共享借用：已知同步函数的形参为 `&T`，且该参数具有
自动推导或显式契约提供的不逃逸保证时，可以省略 `&`。泛型推断从输入值推断 T，
仍检查 trait 约束；已经是引用的输入沿用原有引用转换规则，不再包一层引用。
`&mut T` 实参继续显式写 `&mut`；按值形参继续按 Copy 或移动规则处理。
具有 `@noescape` 契约的同步函数值也支持此自动转换；挂起调用仍要求显式引用。

拥有型局部/拥有字段直接临时借用，打印不移动输入。Copy 输入在各自实参位置求值并复制到
临时存储；拥有型表达式结果只求值一次，移动到临时绑定后借用。实参从左到右求值，
临时绑定保活到整次调用结束，再按逆序清理；后续实参求值、被调用函数或 Drop 失败时仍清理。
别名冲突、借用逃逸和引用存储契约仍由普通借用检查验证。

挂起调用者可直接打印标量，并在每次同步调用结束后继续挂起；显式 `println(&count)` 同样支持。
Copy 局部值及其内联字段可在恢复后再次借用。无引用拥有状态和 StringBuilder
可在挂起函数内借用；自动借用仍要求被调用的同步函数具有不逃逸保证。
后续实参可以挂起，例如 `read(count, suspending_call())` 保留先前建立的局部借用，
并继续检查后续实参是否冲突访问该位置；求值顺序仍从左到右。

普通 `"..."` 和三引号字符串默认支持插值；`r` 原始字符串不插值。`f"..."` 前缀已移除。
`{expr}` 接受完整表达式（块、函数调用、嵌套插值均可），要求结果实现 Show；
若结果为共享引用，则显示它指向的值。表达式从左到右求值且只求值一次。
拥有型局部/字段仅临时借用，Copy 值可以复制到临时绑定，临时拥有结果在该段显示后清理。
花括号不再是普通文本：`{{` / `}}` 输出字面量花括号，`\n`、`\u{...}` 等转义照常解码；单行字符串的文本段不能直接跨行，三引号文本可以跨行，
插值表达式内可使用正常换行与注释。空表达式、未闭合或孤立右花括号报错，
格式规格如 `{value:.2}` 尚未支持。嵌套插值最多 64 层。

插值隐式引用内置 show/string 定义，`--no-prelude` 下也可使用；自定义 Show impl 的 trait
仍须显式导入。展开使用不可由源码拼写的局部名和模块别名，不捕获用户同名变量或 helper。
所有显示都复用 StringBuilder、任务预算、借用和 Drop 检查；分配、Show 或析构失败不会
返回半成品 String。插值可直接放在挂起函数中；各显示调用仍同步执行，
表达式中的显式挂起按其活跃引用的类型与来源检查。

### StringBuilder

`import joky/string.{type StringBuilder}` 导入构造器。它是非 Copy 的拥有值，私有存储为
Array<UInt8>；公开操作成功后内容始终是合法 UTF-8。构造器可以按值移动、传参和返回，
追加、reserve 和 clear 需要 `var` 绑定或 `&mut StringBuilder`，只读查询使用共享借用。

| 接口 | 返回值与行为 |
| --- | --- |
| `StringBuilder.new()` | 空构造器，零字节容量 |
| `StringBuilder.with_capacity(n)` | 预留 n 字节容量；n 为 UInt64 |
| `StringBuilder.from(&text)` | 复制文本，不消费输入；容量按输入字节数预留 |
| `builder.push(rune)` | 追加一个 Rune 的完整 UTF-8 编码；包括 NUL |
| `builder.push_str(&text)` | 追加已有 String，不消费或修改输入；允许空串 |
| `builder.byte_length()` / `capacity()` | UInt64；分别为当前字节长度和总字节容量 |
| `builder.reserve(additional)` | 确保容量至少为当前长度加 additional；不是在当前容量上累加 |
| `builder.clear()` | 清空内容并保留容量 |
| `builder.finish()` | 消费构造器，将原缓冲区转移为不可变 String，不复制载荷或重新编码，保留多余容量 |
| `builder.release()` | 消费并提前释放；作用域结束或失败也自动清理 |

所有方法均在构造器类型上提供；已有 `string.new/clone/release` 等模块函数仍用于 String。
StringBuilder 不公开字节字段、可写切片或原始指针，不能写入任意字节制造非法 UTF-8。
长度、容量和增长受检，扩容时同时计费旧、新存储的峰值；分配失败终止并清理部分构造。
finish 仅转移文本存储，不承诺所有辅助工作永远零分配。

```joky
import joky/string.{type StringBuilder}

fn greeting(name: &String) -> String {
    var builder = StringBuilder.new()
    let prefix = "Hello, "
    builder.push_str(&prefix)
    builder.push_str(name)
    builder.push('!')
    builder.finish()
}
```

可运行示例见 [string_builder.jk](../examples/string_builder.jk)。finish/release/移动后不能继续
使用原构造器；共享借用与修改冲突会被拒绝。StringBuilder 拥有值可跨挂起移动，
也可通过整个对象的 `&mut StringBuilder` 在内部挂起方法中追加；独占权限持续至调用完成或取消。
挂起函数可以调用同步 helper，在 helper 内构造并 finish，接收独立 String 后继续挂起；
解释器时间片可能中途暂停同步 helper，取消仍清理当前已初始化的字节。
无引用拥有聚合的 CPS 存储、转移和清理见[网络基础阶段](network-runtime.md#6-net1a-分步实施)。

## 跨挂起借用

跨挂起借用统一按存储、生命周期和访问权限检查。支持无引用 Copy 局部值及其字段、
完整存储树无引用的拥有值，以及这些容器的元素、Element 和 Slice 视图。
`&T` / `&mut T` 可跨挂起保存、传入挂起函数，或进入 parallel/race、scope/branch。
接收者方法沿用自动重借用，普通挂起函数实参使用显式 `&` / `&mut`。
挂起返回值仍不开放拥有对象、Element 或 Slice 的引用；普通 Copy 引用返回继续跟踪输入来源。

被借用存储必须保持原位置并活到调用完成或取消。共享引用可并发读取；独占引用期间禁止冲突访问。
Copy 局部的明确不重叠字段可分别借用；普通容器 API 的元素和切片仍借用整个原容器，
不因索引或区间不同而允许多个独占借用。活跃视图阻止冲突修改、扩容、清空、移动、覆盖和释放；
最后使用后恢复权限。序列模式已证明不重叠的元素与余段沿用其原有规则。

静态分支的借用保持到组合结束，动态分支的捕获保持到所属 scope join。
scope 主体直接声明的局部值保留到 join 完成；return、break、continue 也先 join 再清理。
普通嵌套块或循环体的局部值不能被更外层 scope 的子任务借用到其词法范围之外。
取消先停止并清理子调用，再释放父存储；已经完成的修改保留，取消不回滚。

跨挂起不授予存储引用的权限。局部值和容器内部引用不能存入任务 arena，不能逃逸原拥有者；
传给可能保存引用的 helper 时，仍须通过函数体推导或 `@noescape` 证明对应参数不保留借用。
这个参数级契约同样适用于静态和动态子任务，仅有分配能力不会延长所有输入的借用。
来源和借用链穿过 helper 返回、泛型、别名、字段投影和分支合流。
受检异步宿主借用 v1 可接收符合其 C 布局要求的局部或元素引用，stop 返回前必须同步停止访问。

显式 Arena、dyn、闭包、FixedArray 值/引用及含引用存储的拥有者仍遵守各自限制；
容器/Element/Slice 的聚合存储和宿主 ABI 没有开放。解释器内部时间片仍保留调用帧与借用目标。
通过独占包装引用替换拥有字段使用 `mem.replace`，直接字段赋值和部分移出的既有准入不变。

用例 [suspending_borrows.jk](../tests/fixtures/suspending_borrows.jk) 覆盖局部、字段、容器视图和并发；
[suspending_borrow_cleanup.jk](../tests/fixtures/suspending_borrow_cleanup.jk) 覆盖退出与取消清理；
[async_borrowing.jk](../tests/fixtures/async_borrowing.jk) 覆盖异步宿主的局部及元素引用。
[async_owner_views.jk](../tests/fixtures/async_owner_views.jk) 覆盖异步方法、递归和字段替换；
[async_owner_tcp.jk](../tests/fixtures/async_owner_tcp.jk) 展示保存 stream、数组缓冲与偏移的异步接收对象。

## 连接运行组件 joky/session

显式导入 `joky/session`。`Options` 的公开字段为 `max_line_bytes`、`queue_messages`、
`queue_bytes` 和 `idle_ms`；`defaults()` 分别返回 4096、64、65536 和 0。
行长含结束符，容量沿用 TCP／outbox 的有效范围。`idle_ms = 0` 关闭超时；非零值限制
等到下一条完整行的时间，零散字节不会重置期限。

`create(options) @suspends -> Connection` 创建出站队列和控制队列；
`Connection.output()`／`stop_handle()` 为挂起方法，返回可传给业务状态的弱 outbox.Handle。
`run<S,T>(connection, stream, shared: &S, ctx: &T, on_line, on_close) @suspends`
消费 Connection 和 TcpStream，沿用创建时的 Options，借用服务共享上下文与连接上下文。两个回调分别是
`fn(&S,&T,String) @suspends @noescape -> Bool` 和
`fn(&S,&T) @suspends @noescape -> Unit`，可直接传具名函数。

组件竞速接收、发送和控制等待。EOF、TCP／UTF-8／行长错误、超时、控制队列收到消息或关闭，
以及 on_line 返回 false，都会结束竞速并调用一次 on_close；两个邮箱 Owner 保留到回调完成。
业务可用 `Vm.try_call` 在 on_line 中处理脚本失败。组件不会隐式捕获任意回调异常，也不保证
断开前排空输出队列。父任务取消会结构化停止子任务、释放 socket 和邮箱，不运行挂起清理回调；
应用应让账户等资源由外层作用域持有。chat 的成员清理、重试与空房回收策略在业务层显式实现。

## 内置 TCP 模块 joky/tcp

显式导入 `joky/tcp.{type Address, type TcpListener, type TcpStream, type IoError, type ErrorKind, type Shutdown}`。
CLI、独立 AOT 程序和 Godot 脚本使用同一模块，不需要逐脚本注册 socket 宿主。
所有网络操作标记 `@suspends`；bind、地址查询、shutdown、close 通常在首次 poll 完成。
`TcpListener.bind` 同时绑定并进入监听状态，首版没有独立 listen 或 backlog 配置。

地址是 Copy 值，内部字段私有，不执行 DNS：

| 构造/查询 | 语义 |
| --- | --- |
| `Address.ipv4(ip: UInt32, port: UInt16)` | 网络字节序整数；`0x7f000001` 为 127.0.0.1，0 为任意 IPv4 地址 |
| `Address.ipv6(high: UInt64, low: UInt64, port: UInt16)` | 网络字节序上下两半；`(0, 1)` 为 ::1，`(0, 0)` 为任意 IPv6 地址 |
| `address.with_scope(index: UInt32)` / `scope_id()` | IPv6 接口 scope ID；IPv4 的非零 scope 在 bind/connect 返回 InvalidInput |
| `port()` / `is_ipv6()` / `high()` / `low()` | 按值查询端口、地址族与地址位；端口 0 在 bind 时请求系统分配端口 |

网络拥有者不可复制，没有公开构造器，可以移入无引用的聚合、容器和 State。
`&mut` 方法的独占借用保持到完成或取消；无引用的多字段拥有包装同样可跨 Pending 借用。
可直接查询、追加和解析返回数组；数组、Slice 和元素引用均可跨挂起，活跃视图仍阻止冲突修改及扩容。

| 操作 | 返回类型 |
| --- | --- |
| `TcpListener.bind(address)` | `Result<TcpListener, IoError>` |
| `listener.accept()` | `Result<TcpStream, IoError>`；对端地址通过 stream.peer_addr 查询 |
| `TcpStream.connect(address)` | `Result<TcpStream, IoError>` |
| `stream.split()` | 消费完整连接，返回 `(TcpReader, TcpWriter)` |
| `listener.local_addr()` / `stream.local_addr()` / `stream.peer_addr()` | `Result<Address, IoError>` |
| `stream.read(limit: UInt64)` | `Result<Array<UInt8>, IoError>` |
| `stream.write(bytes: Array<UInt8>)` | `Result<UInt64, IoError>` |
| `stream.write_all(bytes: Array<UInt8>)` | `Result<Unit, IoError>`；写完全部输入或返回错误 |
| `stream.shutdown(Shutdown.Read/Write/Both)` | `Result<Unit, IoError>`；保留拥有者和未关闭方向 |
| `stream.close()` / `listener.close()` | 消费拥有者，返回 `Result<Unit, IoError>` |

read 的 limit 必须在 1..=1048576；超过限制或零上限返回 `Err(InvalidInput)`。
`Ok` 最多返回 limit 字节；正上限下的空 `Ok` 表示 EOF。读取任意字节，不隐式验证 UTF-8。
write 消费数组并复制独立宿主快照，单次最多 1 MiB；过大返回 InvalidInput，空数组返回 `Ok(0)`。
非空成功返回实际写入的正字节数，允许只写入前缀；调用方必须保留或重建未写入的后缀。
write_all 同样消费最多 1 MiB 的数组，空输入直接 `Ok({})`，过大返回 InvalidInput。
它在库层保留原数组与偏移，每次复制最多 16 KiB 的后缀交给 write，按实际写入量推进，
有剩余数据时显式让出执行机会；输入、临时分块与宿主快照同时计入存储峰值。
错误或取消可能留下已发送前缀，接口不返回该前缀长度；不能盲目重发整个输入。
取消释放独占借用，连接可继续使用，但应用协议须自行处理不完整消息，必要时关闭连接。
读满指定长度、分隔符与行读取见下文 BufferedTcp。

`IoError.kind()` 返回 ConnectionRefused、ConnectionReset、ConnectionAborted、NotConnected、
AddrInUse、AddrNotAvailable、BrokenPipe、PermissionDenied、InvalidInput、TimedOut、WriteZero 或 Other。
`os_code()` 返回原生 OS 错误码的 UInt32 位表示，无原生码时为 0；业务逻辑优先匹配 kind。
WouldBlock 留在 Pending，Interrupted 会重新排队；协议错误、就绪驱动故障和运行时配额不足
仍终止任务，不包装成 EOF 或业务成功。

未显式 close 时，作用域退出、任务取消和实例关闭自动同步释放 socket。
close 完成本地句柄释放；它不等待对端确认、冲刷应用缓冲或承诺数据送达。
Mio 的句柄析构没有可取回的 OS close 状态，当前正常 close 返回 Ok；shutdown 的系统错误
通过 Result 单独报告。取消不会撤销已经发生的接收或发送前缀。
活跃 socket 绑定首次登记的执行器，不支持跨执行器迁移。

可执行回环用例见 [tcp.jk](../tests/fixtures/tcp.jk)，包含 IPv4/IPv6、部分读取、EOF、
半关闭、端口占用及取消后复用；连接拒绝由独立测试注入保留端口。运行 `cargo run -- run tests/fixtures/tcp.jk`；
加 `--interpreter` 使用解释器，`joky build tests/fixtures/tcp.jk` 可生成独立原生程序。
每个执行器最多准入 4096 个 TCP socket，监听器、空闲连接、正在 connect/accept 的预留名额
均计数；创建前预留，失败、取消、显式关闭或 Drop 同步归还。额度不足为终止性 ResourceLimit。
就绪登记另有 4096 上限，字节请求及任务存储预算继续独立生效。

[组合示例](../tests/fixtures/tcp_composition.jk) 包含三个并发客户端、write_all、
`race { | pending(&mut listener) | task.sleep_ms(2) }` 超时取消、监听器复用和发送取消前缀验证。
该例使用 Unit 分支和真实单调时间取消操作；返回数据时可将请求与定时器包装成相同的 Result 类型，
标准库尚不提供通用超时包装。
[Godot 网络示例](../integrations/godot/demo/network.jk) 与 GDScript 对端收发并验证引擎生命周期。
域名解析使用下述 `joky/dns`；UDP、TLS 等另行分期。

### 独立读写端 TcpReader / TcpWriter

导入 `joky/tcp.{type TcpReader, type TcpWriter}`；`stream.split()` 为消费式 `@suspends` 方法，
通常首轮完成，不进行网络等待。拆分后完整 stream 不再可用。两端不可复制、不可伪造，
各有独立的拥有控制块，保活同一个 socket；不复制描述符，也不增加 socket 准入计数。

| 接收端 | 发送端 |
| --- | --- |
| `reader.read(limit)` → `Result<Array<UInt8>, IoError>` | `writer.write(bytes)` → `Result<UInt64, IoError>` |
| — | `writer.write_all(bytes)` → `Result<Unit, IoError>` |
| — | `writer.write_message(bytes, limit)` → `Result<Unit, IoError>`；四字节大端头与有界载荷 |
| `reader.local_addr()` / `peer_addr()` → `Result<Address, IoError>` | `writer.local_addr()` / `peer_addr()` → `Result<Address, IoError>` |
| `reader.shutdown()` → `Result<Unit, IoError>` | `writer.shutdown()` → `Result<Unit, IoError>` |
| `reader.close()` 消费端 → `Result<Unit, IoError>` | `writer.close()` 消费端 → `Result<Unit, IoError>` |

读写上限、实际前缀、EOF 和取消约束沿用完整 stream。同一连接允许一个读和一个写同时 Pending，
每端仍独占借用，两个同方向操作被拒绝；两项等待分别占用一个就绪登记名额。
普通操作取消仅撤销自己的等待与 lease，另一方向不被撤销；所属任务整体取消仍清理全部分支。

shutdown 仅结束本方向并保留端：成功后重复 shutdown 为 Ok，读取返回 EOF，发送（含空写入）
返回 NotConnected。失败仍报告系统错误，不标记成功；close 无论方向 shutdown 成功与否都释放端，
Drop 同步尝试方向 shutdown，忽略无法返回的错误。发送端 Drop 尝试发送 FIN，接收端仍可读取；
接收 EOF 或接收端 Drop 不终止另一端发送，允许 EOF 后回复。关闭后的两端保留建立连接时的地址。
最后一个端释放时关闭底层 socket 并退还名额。拆分发布期间的配额不足为终止性 ResourceLimit。

[拆分回环用例](../tests/fixtures/tcp_split.jk) 覆盖双向字节序、EOF 后回复、方向关闭与 Drop、
操作取消后复用，以及拥有聚合中的两端。发送端 write_message 的格式、上限、输入消费和前缀约束
与下文 BufferedTcp.write_message 相同；合法空消息在 shutdown 后仍返回 NotConnected。

### 缓冲接收端 BufferedTcpReader

`buffered.split()` 消费已有 BufferedTcp，签名为
`@suspends -> (BufferedTcpReader, TcpWriter)`；已有接收数组、消费偏移、capacity 以及
EOF/已记住的 IoError 全部转移到接收端，不重新读取或复制缓存。原包装不可继续使用。

显式导入 `joky/tcp.{type BufferedTcpReader}`。也可用 `BufferedTcpReader.new(reader)`
或 `BufferedTcpReader.with_capacity(reader, capacity)` 消费原始 TcpReader 建立空缓存；
默认 capacity 为 16384，合法范围为 1..=1048576，非法构造返回 InvalidLimit 并释放传入的读端。

read、read_exact、read_until、read_line、read_message 与 capacity、buffered_len、is_eof
沿用下文 BufferedTcp 的签名、读取顺序、上限和错误语义。接收端不提供发送接口。
local_addr/peer_addr、shutdown、消费式 close 的返回值与原始 TcpReader 相同；它们均挂起。
shutdown 只结束底层接收方向，已收到的缓存仍可读取；缓存耗尽后观察 EOF，已记住的 I/O 错误
仍保持原值。close/Drop 丢弃缓存并结束读端，不释放仍存活的写端。

拆分后可以并行借用一个 BufferedTcpReader 和一个 TcpWriter。完整消息读取或发送在取消、
截断和分帧超限后仍不承诺恢复协议边界；应用须协调并释放两端来结束连接。
[缓冲拆分用例](../tests/fixtures/buffered_tcp_split.jk)覆盖缓存交接、双向消息、EOF 后回复与取消。

### 有界接收缓冲 BufferedTcp

显式导入 `joky/tcp.{type BufferedTcp, type ReadError}`。BufferedTcp 消费一个 TcpStream，
在 Joky 库层保存接收数组、消费偏移和读取状态；不引入新的 socket 或执行器。

| 操作 | 语义 |
| --- | --- |
| `BufferedTcp.new(stream)` | 返回 BufferedTcp，默认缓冲上限 16384 字节 |
| `BufferedTcp.with_capacity(stream, capacity: UInt64)` | `Result<BufferedTcp, ReadError>`；上限必须在 1..=1048576，非法时返回 InvalidLimit 并释放传入的 stream |
| `buffered.read(limit: UInt64)` | `@suspends -> Result<Array<UInt8>, ReadError>`；合法上限为 1..=1048576，返回独立拥有的数组 |
| `buffered.read_exact(length: UInt64)` | `@suspends -> Result<Option<Array<UInt8>>, ReadError>`；读取完整 length 字节，合法长度为 0..=1048576 |
| `buffered.read_until(delimiter: UInt8, limit: UInt64)` | `@suspends -> Result<Option<Array<UInt8>>, ReadError>`；读取到单字节分隔符（包含分隔符），limit 为 1..=1048576 |
| `buffered.read_line(limit: UInt64)` | `@suspends -> Result<Option<String>, ReadError>`；读取 LF 结尾的完整行，移除 LF 或 CRLF，验证 UTF-8；limit 包括结束符，为 1..=1048576 |
| `buffered.read_message(limit: UInt64)` | `@suspends -> Result<Option<Array<UInt8>>, ReadError>`；读取四字节大端长度前缀的完整消息，返回载荷；limit 只计载荷，为 0..=1048576 |
| `buffered.write_message(bytes: Array<UInt8>, limit: UInt64)` | `@suspends -> Result<Unit, IoError>`；消费 bytes，先验证载荷上限再发送长度头与完整载荷；limit 为 0..=1048576 |
| `buffered.capacity()` / `buffered.buffered_len()` | 同步共享借用查询缓冲上限与尚未交付的字节数 |
| `buffered.is_eof()` | 同步查询是否已观察到 EOF；对端关闭但本地尚未读到 EOF 时仍为 false |
| `buffered.write_all(bytes)` | 挂起并转发到原 stream，沿用 `Result<Unit, IoError>` 与发送前缀约束 |
| `buffered.split()` | 消费包装，挂起并返回 `(BufferedTcpReader, TcpWriter)`，保留已有接收状态 |
| `buffered.close()` | 消费包装及缓存，挂起并关闭原 stream，返回 `Result<Unit, IoError>` |

read 优先返回缓存中的字节，不拼接下一次 socket 读取；缓存耗尽后才按 capacity 读取一次。
因此成功长度至多为 limit 和 capacity 的较小值，也可能更短。消费前缀通过偏移保留到下次
填充，不逐字节移除或搬移；返回数组的修改与释放不影响剩余缓存。缓冲按需分配，不预先
分配 capacity 字节；缓存、独立返回数组、填充时的旧/新数组和宿主快照均计入实际存储峰值，
capacity 不代表整个操作的内存额度。

ReadError 的 InvalidLimit 表示非法读取上限或构造上限，Io(error) 保存原 IoError。
非法读取不消费缓存，也不改变 EOF/错误状态。read 正上限下的空成功表示 EOF，后续合法 read
继续返回空成功；第一次 I/O 读取失败后，后续合法 read 返回同一错误，二者均不再次读取 socket。
运行时配额或任务故障仍终止任务，不记成业务错误。

三个完整读取方法不把一次 TCP read 当作消息边界，结果可以超过接收缓冲 capacity。
尚未读取任何消息字节时遇到 EOF，返回 `Ok(None)`；成功读取完整边界返回 `Ok(Some(value))`。
read_exact(0) 不读取 socket，始终返回 Some(空数组)，包括已知 EOF 或读取错误状态；空行返回
Some(空 String)，可与 EOF 区分。read_exact 在收到首个字节后才分配目标载荷容量。
read_until/read_line 要求结束符，EOF 前的非空未结束前缀返回 `UnexpectedEof(count)`，
count 为本次已消费的字节数；read_exact 未读满时同样报告该错误。

read_until/read_line 的 limit 计入分隔符或 CRLF；若正好达到 limit 仍未找到结束符，立即返回
`LimitExceeded`，不再探测下一字节或 EOF。底层预读的后续字节留在缓存中；成功结果长度不超过
limit，失败前缀已消费并释放，不返回部分结果。通常应结束连接；明确知道协议边界时可以自行
丢弃剩余消息来恢复，不能直接重试整个读取。I/O 失败也会释放已累积的前缀并传播 Io(error)。

read_line 移除结束符后调用 String.from_utf8；内部 CR、NUL 等保持不变。
`InvalidUtf8(error)` 保留 Utf8Error 的 code 与相对于该行正文的字节 offset。
这时完整行已消费，后续行仍可读取；没有 LF 的末行先报 UnexpectedEof，不作为正常行返回。
接收阶段每次至多扫描/复制 16 KiB，未完成时 yield；最终 UTF-8 校验沿用同步的
String.from_utf8。结果、接收缓存和宿主请求均计入实际存储峰值。

read_message/write_message 使用固定四字节大端无符号长度，头后的载荷可以是任意二进制数据。
长度不包括头，不依赖本机字节序；read_message 只返回载荷，不自动验证 UTF-8。
limit 为 0 时只接受空消息；空消息仍有四字节零头，返回 `Some(空数组)`，可与正常 EOF 区分。

read_message 在读取头前拒绝大于 1 MiB 的 limit；读完头后先比较声明长度与 limit，超限返回
LimitExceeded，不按声明长度分配载荷或继续读取。四字节头已消费，底层可能预读的载荷仍留在
缓存中；应结束协议连接，不直接把载荷当作下一条消息的头。
只有尚未读取任何头字节时遇到 EOF 才返回 None；头未读满或合法非空载荷缺失/截断均为
UnexpectedEof(count)，count 为本条消息已消费的总字节数，包含头：1..=3 表示头截断，
4 表示有完整头但没有载荷，更多表示部分载荷。I/O 错误保持为 Io(error)。

write_message 对非法 limit 或大于 limit 的载荷返回 IoError 的 InvalidInput，释放输入且不写入
任何字节。验证后分开发送头与载荷，不建立「载荷加头」的大数组，因此正好 1 MiB 的载荷仍
合法。空消息也须成功发送完整零头；发送错误保留原 IoError，可能已经写出头或部分载荷。
发送错误后应关闭连接，不能直接重发整个消息。
这两个方法沿用独占可变借用，一次调用不会被同一包装的另一个读写调用交错。

取消任一读取可能发生在 socket 已接收字节但尚未保存到包装中，或消费偏移已推进但返回值尚未
交付时；取消 write_all/write_message 也可能留下发送前缀。首版要求应用在取消操作后 drop/close
该包装并结束协议连接，不承诺恢复未完成消息或安全重试。

[消息回声示例](../examples/tcp_messages.jk) 在同一程序中并行运行有限的 TCP 服务与客户端，
逐条回传 ASCII、UTF-8 与空消息，最后通过关闭连接结束服务。运行
`cargo run -- run examples/tcp_messages.jk`，输出 hello、你好和一行空行；也支持解释器与独立 AOT。

[双向会话示例](../examples/tcp_duplex.jk)在同一连接上拆分缓冲接收端与发送端，
用 parallel 同时等待推送和发送命令。回环服务独立推送三条消息，再由各自发送端 shutdown
发出 EOF；两端完成后整个会话释放。`cargo run -- run examples/tcp_duplex.jk` 输出 1、2、3，
也支持解释器与独立 AOT。示例用有限轮次结束，应用可将收发循环维持到协议结束或截止时间。

Godot 的[双向会话脚本](../integrations/godot/demo/network_duplex.jk)把网络拥有者留在所属任务中，
通过短 State 调用获取发送命令、发布收发进度。接收空帧或干净 EOF 时通知发送端停止；
截止时间取消整个收发组并释放两端，不能对不完整消息盲目重试。
[引擎验收](../integrations/godot/demo/tests/network_duplex.gd)覆盖接收等待时发送、写背压时接收、
暂停、重载、节点退出/销毁及迟到事件。

Godot 的[缓冲消息脚本](../integrations/godot/demo/network_messages.jk)通过所属任务与短 State 访问
组合消息/行读取；[真实引擎对端](../integrations/godot/demo/tests/network_messages.gd)验证分片与生命周期。


## 内置文件模块 joky/fs

显式 `import joky/fs`。`File` 是非 Copy 的拥有型记录，私有字段为内部资源
`fs.FileOwner`；移动、返回、Result/记录/State 保存与自动清理遵循普通拥有值规则。

| 接口 | 结果 |
| --- | --- |
| `fs.open(path: String, options: fs.OpenOptions) @suspends` | `Result<File, IoError>` |
| `file.read(&mut self, limit: UInt64) @suspends` | `Result<Array<UInt8>, IoError>` |
| `file.write(&mut self, bytes: Array<UInt8>) @suspends` | `Result<UInt64, IoError>` |
| `file.read_all(&mut self) @suspends` | `Result<Array<UInt8>, IoError>` |
| `file.read_all_limited(&mut self, max_bytes: UInt64) @suspends` | `Result<Array<UInt8>, IoError>` |
| `file.write_all(&mut self, bytes: Array<UInt8>) @suspends` | `Result<Unit, WriteError>` |
| `file.read_text(&mut self) @suspends` | `Result<String, ReadTextError>` |
| `file.read_text_limited(&mut self, max_bytes: UInt64) @suspends` | `Result<String, ReadTextError>` |
| `file.write_text(&mut self, text: String) @suspends` | `Result<Unit, WriteError>` |
| `file.seek(&mut self, from: fs.SeekFrom) @suspends` | `Result<UInt64, IoError>` |
| `file.sync_all(&mut self) @suspends` | `Result<Unit, IoError>` |
| `file.close(self) @suspends` | `Result<Unit, IoError>` |

`OpenOptions.new()` 仅启用 read。六个公开 Bool 字段为 read、write、append、create、
create_new、truncate。访问模式只能是 read、write、read+write、append、read+append；
write 与 append 不可同时启用。只读不能创建或截断；create 与 create_new 互斥；
truncate 仅可用于 write，不能与 create_new 同用。create 不隐式截断；create_new
要求排他创建；append 每次写入追加，不承诺多次 write 合为原子操作。

`SeekFrom` 为 `Start(UInt64)`、`Current(Int64)`、`End(Int64)`。定位返回绝对字节位置。
路径是非空 UTF-8 String，最多 4096 字节，拒绝 NUL。read 上限为 1–1048576；
返回空数组表示 EOF，允许短读。write 消费输入数组，最多 1 MiB，允许部分写入；
空写返回 0，非空零进度返回 WriteZero。失败也不还原输入，不保证没有部分副作用。
close 消费 File；自动清理仅安排后台关闭，需要观察错误时显式 close。

`IoError` 拥有 `kind: ErrorKind` 和 `message: String`。ErrorKind 包括 Unsupported、
PermissionDenied、NotFound、AlreadyExists、InvalidInput、Busy、Closed、LimitExceeded、
WriteZero、SystemFailure；消息最多 1024 UTF-8 字节。没有宿主服务时仍可导入/编译/加载，
合法 open 返回 Unsupported；输入校验先于服务选择。Busy 表示服务未准入该请求。

文件内容是字节；用 `text.into_bytes()` 写入文本，用 `String.from_utf8(bytes)` 显式验证和
转为 String，转换失败得到 Utf8Error。单次 read 可能截在 UTF-8 字符中间；多块读取时
应先拼接或由调用方维护跨块状态；`read_text` 与 `read_text_limited` 已封装完整读取与整体 UTF-8 校验。

`read_all()` 从当前游标读到 EOF，短读继续，每次最多 64 KiB；不隐式 seek，不设置额外的
内容大小上限。数组按实际内容增长，底层请求额度与宿主配置的任务内存预算仍然生效；
分配或预算耗尽遵循既有运行时错误语义。无参数不表示无限内存，也不保证持续产生内容的文件会到达 EOF。

`read_all_limited(max_bytes)` 额外检查内容大小。max_bytes 允许 0，是返回内容的字节上限，
不是总内存预算。达到上限后多读 1 字节探测 EOF；有额外内容则返回 LimitExceeded，
已读内容丢弃，游标可能推进 max_bytes + 1 字节；恰好上限且 EOF 则成功。
两种读取遇到 I/O 失败均丢弃累积内容，不回滚游标；扩容与临时块仍占任务内存。

`read_text()` / `read_text_limited(max_bytes)` 分别在对应字节读取成功后严格解码，返回
`ReadTextError.Io(IoError)` 或 `ReadTextError.Utf8(Utf8Error)`；保留 UTF-8 原因及相对
本次读取起点的错误偏移。字符可跨读块；保留 BOM、NUL 与换行，不做替换或规范化。
I/O 或受限版的超限失败先于 UTF-8 校验。

原来的 `read_all(max_bytes)` / `read_text(max_bytes)` 调用需改为 `_limited` 接口；
不需要内容上限的调用直接省略参数。写入接口保持不变。

`write_all(bytes)` 消费完整输入，每次最多写 64 KiB，继续处理成功短写；输入可超过单次
write 的 1 MiB 限制。遇到错误立即停止，不自动重试 Busy。`WriteError` 有公开字段
`written: UInt64` 与 `error: IoError`；written 只统计此前成功确认的字节，不包含失败请求
中未知的副作用，不能据此盲目重试。非空零进度为 WriteZero。空输入仍走底层空写的状态检查。
`write_text(text)` 按 UTF-8 写完整字符串，错误进度也按字节计算。

完整写入不自动定位、创建、截断、同步或关闭；这些由 OpenOptions 与显式操作决定。
成功不保证持久化或多块原子性；追加模式下各块之间可被其他写入者插入。
取消便利方法不返回部分进度、不回滚效果，当前请求沿用下述取消规则。
[可执行示例](../examples/files.jk)演示 UTF-8 文件的完整读写。

CLI 的 JIT/解释器与 `joky build` 生成的程序安装本机服务，相对路径以服务启动时的工作
目录为基准，支持绝对路径；该基准不是安全沙箱。嵌入 VM/AOT 默认没有文件服务，
宿主在加载前显式配置，嵌套 VM 不继承。Godot 适配器仅用于嵌入验证，不提供文件服务或
能力开关；合法 open 返回 Unsupported。详见 [Godot 验证范围](../integrations/godot/README.md#嵌入验证范围)。

文件服务使用独立有界工作池，不占 DNS 的线程池，不在调度器 poll/stop 中调用阻塞文件 API。
默认 2 个 worker、64 个排队请求、128 个未回收请求、256 个文件和 16 MiB 请求存储。
取消尚在队列中的操作不关闭既有 File；取消已经开始或完成待领取的操作会逻辑关闭 File，
后台完成后回收。取消不回滚文件效果，也不保证系统调用马上结束。关闭使用打开时预留的
清理位置，不受普通队列已满影响。宿主配置、保活与卸载见[架构说明](architecture.md#文件服务能力与后台生命周期)。

## 内置 DNS 模块 joky/dns

显式导入 `joky/dns as dns.{type ResolveError, type ErrorKind}`；地址类型从
`joky/tcp.{type Address}` 导入，数组与 Result 使用既有标准库类型。
CLI、JIT、解释器、独立 AOT 与 Godot 共用系统解析实现。

`dns.lookup(host: String, port: UInt16) @suspends -> Result<Array<Address>, ResolveError>`
消费 host，返回可供 `TcpStream.connect` 使用的地址列表。host 和 port 分开传入；
host 限制为 1–1024 字节 ASCII，拒绝 NUL、控制字符、空白、URL、host:port、方括号和 `%zone`。
调用方可传已转换的 punycode，库不执行 IDNA 转换或自行修剪输入；允许系统支持的其他名称，
不额外实施 DNS 标签校验。数值 IPv4/IPv6 直接返回单个地址，不进入线程池；IPv6 不加方括号。
带接口 scope 的数值地址使用 `Address.ipv6(...).with_scope(...)` 构造。

域名查询由系统名称服务处理，包括 hosts、搜索域及系统缓存；不提供 TTL、任意记录查询或
自定义 DNS 服务器。结果保留系统顺序、IPv6 scope ID，按完整地址去重，所有端口使用传入值
（包括 0）。最多返回 64 个地址；超过上限返回错误，空结果同样返回错误。
解析成功不保证连接成功；应用自行选择候选地址。首版不提供 connect_host 或多地址连接竞速。

`ResolveError` 有公开字段 `kind: ErrorKind` 与 `message: String`。错误消息最多 1024 字节，
保持有效 UTF-8；系统消息与平台相关，不能根据其文本可靠区分 NXDOMAIN、SERVFAIL 或超时。

| ErrorKind | 含义 |
| --- | --- |
| `InvalidInput` | 主机名格式或输入长度不受支持 |
| `Busy` | 共享服务队列或未回收请求额度已满；立即返回，不等待空位 |
| `NoAddresses` | 系统查询成功但没有地址 |
| `LimitExceeded` | 去重地址超过 64 个 |
| `SystemFailure` | 系统解析失败或服务关闭时排队请求未能执行 |

每份运行时固定共享 4 个工作线程，最多 64 个排队请求与 128 个未回收请求。未回收计数包含
运行中、完成未领取和取消后仍在执行的请求；取消不能绕过并发限制。任务预算耗尽、服务初始化
失败及运行时协议故障沿用终止性错误，不包装为 ResolveError。

lookup 等待时执行器与 Godot 帧可以继续推进。取消只结束当前等待并撤销交付；已开始的系统
解析可能继续阻塞，其额度直到后台结束才释放。可用既有 Unit 分支 race 与定时器停止等待，
不承诺系统调用有界终止。手动驱动原生 CPS 的宿主须在 poll 前初始化 DNS；关闭/卸载契约见
[架构说明](architecture.md#dns-线程边界与关闭)。

[有限解析示例](../examples/dns_lookup.jk)查询 localhost；
[回环行为用例](../tests/fixtures/dns.jk)覆盖数值地址、输入错误、系统查询及解析后 TCP 连接，
无需公网。运行 `joky run examples/dns_lookup.jk`，加 `--interpreter` 使用解释器；
`joky build examples/dns_lookup.jk` 生成独立原生程序。

## JSON 值与数字

显式导入 `joky/json.{type Value, type Number, type NumberError, type NumberErrorKind}`。
模块提供值模型、数字转换和严格编解码；TCP JSON 组合见下文示例。

Value 为非 Copy 的拥有枚举：Null、Boolean(Bool)、Number(Number)、Text(String)、
Array(Array<Value>)、Object(Map<String, Value>)。Boolean/Text 避开基础类型的保留名。
数组保存顺序，对象无顺序契约；没有隐式深拷贝或数值意义的 Eq/Hash。
构造、共享/独占 match、mem.replace、Array.remove/Map.remove_by_ref 使用普通拥有权规则，
移出的子树独立拥有，取消或作用域退出统一清理。

对象先 contains_key_by_ref 再 value_ref，数组先 len 再 borrow_ref；越界借用仍是终止性错误。
对象删除返回 Option<Value>，None 表示缺失，Some(Value.Null) 表示移出一个存在的空值。
查询不会复制子树，也不新增 Option<&Value> 接口。

Number 私有保存完整且验证过的 ASCII JSON 数字文本，统一硬上限 1024 字节。
原始拼写保留尾零、指数大小写/符号及负零，超出机器数范围的合法文本仍可保存。
长度检查先于扫描或复制；它不承诺任意精度算术。

| 接口 | 行为 |
| --- | --- |
| Number.from_text(input: &String) | Result<Number, NumberError>，验证完整语法并复制输入 |
| Number.from_i64(value: Int64) / from_u64(value: UInt64) | 拥有十进制 Number |
| Number.from_f64(value: Float64) | Result；拒绝非有限值，有限值最短往返格式，负零保留为 `-0` |
| number.as_text() | &String，共享借用已验证拼写 |
| number.to_i64() / to_u64() | Result；只接受无小数/指数的整数拼写，逐位检查范围 |
| number.to_f64() | Result；正确舍入，溢出拒绝，下溢可返回保留符号的零 |

整数转换不经过浮点、不截断小数。`1.0`、`1e0` 返回 NotInteger；这一检查先于整数范围检查。
Int64 接受 `-0` 为 0；UInt64 拒绝带负号的整数拼写，包括 `-0`。
Float64 转换允许精度损失，例如 9007199254740993 舍入为 9007199254740992；文本自身不变。
没有通用 Int64/UInt64/Float64.from_text 入口。

NumberError 的公开字段为 kind、offset；offset 是零基文本字节位置。
kind 包括 InvalidSyntax（首个错误字节或 EOF）、NotInteger（小数点/指数标记）、
OutOfRange（首个使整数超范围的数字，UInt64 负号或浮点溢出为 0）、NonFinite（0）、NumberLimit（1024）。
语法/范围错误是可恢复 Result；内存或执行预算不足沿用终止性运行时错误。
行为样例见 [三后端验收 fixture](../tests/fixtures/json_values.jk)。

### JSON 解码

显式导入 `joky/json.{type Value, type Limits, type DecodeError, type DecodeErrorKind}`。
接口同步返回 `Result<Value, DecodeError>`：

| 接口 | 输入与拥有权 |
| --- | --- |
| json.decode(input: &String) | 默认限额；结果独立拥有字符串、数字文本及子树，不保留输入借用 |
| json.decode_with_limits(input: &String, limits: Limits) | 指定限额，输入保留 |
| json.decode_bytes(input: Array<UInt8>) | 消费字节数组；先检查长度和严格 UTF-8，失败也清理输入 |
| json.decode_bytes_with_limits(input: Array<UInt8>, limits: Limits) | 字节入口的指定限额版本 |

只接受一个完整值及前后空白（空格、tab、CR、LF），顶层可为标量。
拒绝 BOM、注释、尾逗号、未转义控制字符、孤立代理项、重复对象键与尾随数据。
JSON 的 `\uXXXX` 支持成对代理项，解码后的 NUL 和非 BMP 标量保留；不接受 Joky 的 `\u{...}` 语法。
重复键按解码后的字符串判断，`"a"` 与 `"\u0061"` 相同；不做 Unicode 规范化。
Number 保留原始拼写和超出机器数范围的合法数字。

Limits 是 Copy 记录，字段均为公开 UInt64；`Limits.defaults()` 返回以下默认值，它们同时是硬上限。
`limits.is_valid()` 检查配置是否在硬上限内；调用者可降低到零但不能增大，边界按 `实际值 <= 上限` 接受：

| 字段 | 默认/硬上限 | 计数 |
| --- | --- | --- |
| max_input_bytes | 1048576 | 仅解码：完整原始 UTF-8 输入，含空白 |
| max_output_bytes | 1048576 | 仅编码：实际输出字节，含转义及分隔符 |
| max_depth | 128 | 标量根为 0，每层数组/对象加 1，空容器也计入 |
| max_nodes | 65536 | 每个 Value 计 1，含容器和 Null；对象键不另计 |
| max_string_bytes | 262144 | 每个解码后或编码源字符串/对象键的 UTF-8 字节数 |
| max_number_bytes | 1024 | 每个数字的完整原始拼写字节数 |

DecodeError 的公开字段为 kind、offset；offset 是零基原始输入字节位置，EOF 为输入字节长度。
kind 为 UnexpectedEof、UnexpectedToken、InvalidEscape、InvalidUnicode、InvalidUtf8、
InvalidNumber、DuplicateKey、TrailingData、InputLimit、DepthLimit、NodeLimit、StringLimit、
NumberLimit、InvalidLimits。与数字转换的同名标签通过 `DecodeErrorKind.NumberLimit` 区分。
转义错误指向相关反斜线，重复键指向第二个键的起始引号；DepthLimit/NodeLimit 指向当前值起始处，
StringLimit 指向使解码后长度超限的标量或转义起始处，数字/输入超限指向第一个超限字节。
无效 Limits 位置为 0，先于输入长度检查；字节入口之后先验证整个 UTF-8，再扫描 JSON。
数字 token 先受限扫描再验证语法，超长的非法数字可能先返回 NumberLimit。
详细转义错误优先级见 [错误位置契约](json-codec.md#52-错误位置与恢复边界)。

语法和限额错误不返回部分树；内存或执行预算不足仍为终止性运行时错误。
限额不等于实际总内存预算，也不保证同步调用的帧耗时；JIT/AOT 同步 helper 不主动让出。
公开用法及边界样例见 [解码 fixture](../tests/fixtures/json_decode.jk)。

### JSON 编码

显式导入 `joky/json.{type Value, type Limits, type EncodeError}`。
`json.encode(value: &Value) -> Result<String, EncodeError>` 使用默认 Limits；
`json.encode_with_limits(value: &Value, limits: Limits)` 使用指定限额。
两者均同步借用输入，输出独立拥有，成功或可恢复错误后输入树保持不变。

输出紧凑 JSON，不添加空白或 BOM；数组保留顺序，对象无键排序或输出顺序契约。
Number 直接输出已验证的原始拼写，不先转换机器数值；`1e400`、大整数、负零及指数大小写均保留。
字符串与键转义引号、反斜线和控制字符；b/f/n/r/t 使用短转义，其余控制字符使用小写 `\u00xx`。
其余 Unicode 标量直接输出 UTF-8，斜杠不转义。

EncodeError 为枚举：InvalidLimits、OutputLimit、DepthLimit、NodeLimit、StringLimit、NumberLimit，
无 offset 或 JSON 路径字段；同名标签可用 `EncodeError.NumberLimit` 等类型限定形式构造或匹配。
Limits 的硬上限和计数方式同上；编码忽略 max_input_bytes，解码忽略 max_output_bytes，
两者仍检查全部配置是否合法。每个 Value 只计一个节点，键不另计；标量根深度为零。
字符串/数字先检查源长度，输出限额在追加前按实际 UTF-8 字节检查，包含转义增长和所有标点。
限额同时违反时按遍历/追加顺序返回首先触发的错误；InvalidLimits 先于工作分配。

失败清理本次输出及索引栈；内存或执行预算不足仍为终止性运行时错误。
编码不存借用到拥有容器，也不按树深度递归；每步重新借用索引路径上的节点，最多遍历 128 层祖先。
同步接口不主动让出，JSON 上限不保证帧耗时。TCP 载荷仍需单独满足 write_message 的限额。
样例见 [编码 fixture](../tests/fixtures/json_encode.jk)与 [独立语义往返模型](../tests/fixtures/json_model.jk)。

### Struct 与类型化 JSON

`joky/json/typed` 在值 codec 上提供普通 `ToJson` / `FromJson` trait。
struct 可以自动派生或手写字段映射，再调用 `typed.encode(&value)` 或 `typed.decode::<User>(&text)`。
[自动派生示例](../examples/json_derive.jk)与[手写示例](../examples/json_struct.jk)均输出 `Ada`、`37`、`/age`。

```joky
import joky/json/typed
import joky/option.{type Option}

#derive(ToJson, FromJson)
struct User { name: String, age: UInt8, nickname: Option<String> }

#derive(ToJson, FromJson)
struct Envelope<T> { value: T }
```

- `#derive` 位于 struct/pub 前，参数为固定的内置派生名 `ToJson`、`FromJson`；可以只选择一个方向，
  也可分成两个属性。重复、空列表、其他派生名、非 struct 目标均拒绝。
  用户同名 trait 或导入别名不改变派生协议；生成代码自动取得所需标准库依赖。
- 支持命名字段 struct，包括空 struct、泛型及经拥有容器间接存储的有限递归树。
  所有字段按声明顺序映射，JSON 键就是原字段名；私有字段在定义模块内处理，可跨模块使用派生实现。
  字段类型须已有对应 trait 实现，用户自定义嵌套类型可以分别派生或手写。
- 泛型实现按字段义务推导条件约束，例如 `Map<K, V>` 的解码要求 K 满足 ObjectKey、BorrowEq、BorrowHash，
  V 满足 FromJson；未被字段使用的参数不额外要求 JSON trait。支持可转写到类型参数上的关联类型等式，
  无法表达或存在多个可适用约束方案时拒绝，使用手写 impl 表达该情况。
- 生成普通 ToJson/FromJson 方法，复用下面的 Context、缺失/未知字段、null、错误路径与限额规则。
  具体字段类型不满足条件时诊断指向原字段；不满足泛型约束的具体使用在调用处拒绝。
  同一方向同时派生和手写实现按重叠 trait impl 拒绝。可与有效的 `#repr(C)` 组合。
- 每个派生实现的约束推导最多 256 次义务展开、32 层依赖；派生方法及其调用产生的泛型实例
  继续计入全局生成预算。
  本版不提供 enum 派生、字段改名、跳过或默认值注解，也不引入运行时反射。

| 接口 | 行为 |
| --- | --- |
| `typed.encode<T: ToJson>(&T)` | 借用输入，返回独立拥有的 `Result<String, CodecError>` |
| `typed.decode<T: FromJson>(&String)` | 输入保留，返回独立拥有的 `Result<T, CodecError>` |
| `typed.decode_bytes<T: FromJson>(Array<UInt8>)` | 消费输入，含失败路径 |
| 以上接口的 `_with_options` 版本 | 最后一个参数为 `Options`，默认使用 `Options.defaults()` |

`Options` 包含 `limits: json.Limits` 和 `reject_unknown_fields: Bool`（默认 false）。
标准实现覆盖 Unit、Bool、String、Number、八种整数、Float32/Float64、Option<T>、Array<T>
和 Map<String, T>。数组保持顺序；Map 无对象顺序契约。整数沿用 Number 的整数拼写与范围规则，
不经浮点转换；Number 保留拼写；非有限浮点数拒绝编码。Float32 解码先按 Float64 解析再窄化，
窄化溢出拒绝，可下溢为带符号零；不承诺直接十进制到 Float32 的一次正确舍入。
Float32 编码先精确拓宽到 Float64，输出可往返但不保证最短 Float32 拼写。

`ToJson.to_json(&self, &mut Context) @noescape -> Result<Value, MappingError>`
使用 `context.object()` 创建对象，`context.put_field(&mut fields, name, &value)` 编码每个字段，
最后返回 `Value.Object(fields)`。拥有字段可借用；Copy 字段先读入局部变量再借用。
`FromJson.from_json(Value, &mut Context) @noescape -> Result<Self, MappingError>`
使用 `read_object` 消费对象、`take_field::<T>` 移出并解码字段，最后 `finish_object(fields)`
处理剩余字段。默认忽略未知字段，严格模式返回 UnknownField；多个未知字段时不保证选择顺序。
重复字段在文本解析时拒绝；编码 `put_field` 也拒绝重复名称。

缺失字段通过 `FromJson.accepts_missing() -> Bool` 判断是否可用 null 补齐。
默认 false 表示 MissingField；Option<T> 返回 true，因此缺失和显式 null 均解码为 None。
补齐的 null 经过同一映射与限额检查；其他默认值由 struct 的 FromJson 实现自行处理。
编码 None 输出 null；Some 透明编码，
故 Option<Unit>、嵌套 Option 等能产生 null 的类型不保证可区分所有原始状态。

错误分为 `CodecError.Syntax(DecodeError)`、`Mapping(MappingError)` 和 `Encoding(EncodeError)`。
Syntax 保留原始字节 offset，包括解析阶段的限额错误；Mapping 保存拥有的 `path: Array<PathSegment>`
和 `kind`。路径段为 Field(String) / Index(UInt64)，`error.pointer()` 生成诊断 JSON Pointer：
根为 `""`、字段/索引用 `/` 分隔，键里的 `~` 与 `/` 转为 `~0` / `~1`，例如 `/users/1/age`。
Mapping 不声称能恢复原始文本 offset。kind 包括 ExpectedNull/Boolean/Number/String/Array/Object、
MissingField、UnknownField、DuplicateField、NotInteger、OutOfRange、NonFinite、InvalidLimits、
DepthLimit、NodeLimit、StringLimit、NumberLimit、CallLimit。

Context 不可从外部构造。嵌套映射必须经过 `context.write` / `context.read::<T>`，
它们限制同时活动的映射调用为 256，且在成功和错误后恢复容器深度；字段/数组 helper 同样恢复路径。
自定义标量实现调用 `scalar()`，容器调用 `container()`（已计一个节点），字符串/数字额外调用
`string` / `number` 检查长度；`error(kind)` 快照当前路径，`options()` 返回当前配置。
不要绕过 dispatcher 直接递归 trait 方法，也不要为子值重新调用顶层 encode/decode。

映射阶段沿用 JSON 的深度、节点与单字符串/数字限额，先检查再复制载荷；对象键不单计节点。
失败过的尝试与缺失字段的默认值也消耗映射节点预算，恢复深度不会归还节点。
解析先检查完整输入（含会被忽略的字段），映射另起计数；编码先映射后由值 codec 检查完整输出。
自定义实现若直接构造 Value，最终编码仍检查结果树，但无法约束构造前的用户代码；
这些上下文规则是 trait 契约，运行时内存/指令预算仍独立生效，耗尽仍为终止性错误。

Map 实现通过 `ObjectKey` 表达字符串键，内置仅为 String 提供实现。
`to_key(&self) @noescape -> String` 返回独立拥有的完整键名，`from_key(String) -> Self` 消费键名；
自定义键须保证转换互逆且不改写拼写，解码还要求 BorrowEq + BorrowHash，转换后键冲突返回 DuplicateField。
这使用现有完整泛型 impl；没有引入 `Map<String, T>` 部分特化。
类型化映射沿用每次编译 4096 个泛型实例与隐藏闭包入口的合计预算，标准库内部实例也计入。
嵌套 `Map<String, Array<Option<UInt64>>>` 的同程序双向往返由三后端回归覆盖；
编译实例预算独立于 JSON 运行时深度、节点与字节限额。

### JSON over TCP 示例

[tcp_json.jk](../examples/tcp_json.jk) 将紧凑 UTF-8 JSON 放入 `write_message` 的长度前缀帧，
使用 `[kind, id, payload]` 应用信封：kind 为 `request`、`reply`、`push` 或 `error`，
id 必须能精确转换为 UInt64，payload 为任意拥有 Value。服务端先推送，再按序回复；
客户端拆分接收与发送方向，整个会话持有两个方向的所有者。运行
`cargo run -- run examples/tcp_json.jk` 输出 `push`、`error`、`reply`、`error`、`reply`。

示例将帧和编解码输入/输出同时限制为 65536 字节，深度 16、节点 4096、单字符串 32768 字节。
编码成功后才发送帧头；解码消费完整帧，非法 JSON/UTF-8 或不符合信封的值得到业务错误回复，
后续有效请求仍可处理。帧截断、传输长度超限、写失败或取消结束整个会话，不恢复协议前缀。
这些限额和信封属于示例，不新增标准库协议或连接重试策略。

[Godot 示例](../integrations/godot/demo/network_json.jk) 让 JSON 树、编码字符串和 socket
留在所属任务局部，State 只提供标量状态。有限批次成功写入后显式 `yield_now`，
让接收分支有机会处理推送；同步成功的写入和 codec 不保证主动让出。
真实任务时间片、背压与取消验收见 [JSON3 记录](json-codec.md#11-json3-tcp-组合与联合验收)。
