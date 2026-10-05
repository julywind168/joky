# 语言与内置模块

本文描述已实现的语言子集。运行命令见 [README](../README.md)，
嵌入与 ABI 约束见 [架构说明](architecture.md)。
目标语法和完整类型系统另见 [语法与类型系统设计草案](syntax.md)，
其中尚未实现的规则不改变本文描述的当前行为。

## 类型与函数

类型包括 `Int32`、`Int64`、`UInt8`、`UInt32`、`UInt64`、Unicode 标量 `Rune`、`Bool`、`Unit`、不透明 C 对象指针 `Ptr`、不可变 UTF-8 `String`、名义记录、枚举、结构化元组、同步数组 `Array<T>`、无序字典 `Map<K, V>` 和注册过的宿主 opaque 类型。
函数正常结束时最后一个表达式决定返回值，也可用 `return` 提前返回；不写返回注解时为 Unit。
普通函数不能调用 `@suspends` 函数。
`extern fn` 声明宿主实现，完整签名和挂起约定必须匹配注册信息。
String 是非 Copy 的不可变 UTF-8 拥有值；字面量和 clone 在任务预算内分配，按值传参、赋值及返回移动所有权。文本组合使用 concat、join、StringBuilder 或 `"{value}"` 插值（字符串字面量默认插值），尚无 `+`。
函数参数和调用实参之间必须写逗号，允许尾随逗号，例如 `fn f(x: Int32,)` 和 `f(42,)`；
空列表仍写 `()`，不允许开头逗号或连续逗号。换行和注释不能替代列表逗号。

## 同步 C 借用接口

```joky
#repr(C)
struct CPoint { x: Int32 y: Int64 }
unsafe extern "C" fn update(point: &mut CPoint)
unsafe extern "C" fn inspect(point: &CPoint) -> Int64

fn run() -> Int64 {
    var point = CPoint { x: 19 y: 20 }
    unsafe {
        update(&mut point)
        inspect(&point)
    }
}
```

- `#repr(C)` 位于 struct/pub 之前，保证字段按声明顺序使用目标 C 字节布局；普通记录不自动兼容 C。
  记录必须非空、无类型参数，字段只能是四种定宽整数、Float32 / Float64 或其他 `#repr(C)` 记录，
  透明别名按目标处理。浮点字段按 IEEE binary32 / binary64 与 C 布局逐项核对。
  不接受 Bool、Unit、String、Ptr、引用、元组、枚举、packed、union 或位域。
- C 导入写成 `unsafe extern "C" fn`，接受合计零到四个引用或 Ptr 参数，结果为数值、Ptr 或 Unit。
  引用所指类型只能是定宽整数、浮点或 C 记录；不接受其他按值输入、返回引用、变参、泛型或 @suspends 导入。
- 调用必须处于词法 `unsafe { ... }` 块内。块保留普通块的类型、返回、局部作用域和借用规则；
  允许 arena 对象、同步局部值及其字段，不能绕过共享/独占冲突、移动和局部逃逸检查。
- C 调用本身同步完成，可位于 @suspends 函数的挂起点之间；不允许 C 保留引用实参的地址（包括作为 Ptr 返回）、释放/重分配借用对象、
  越界、跨边界 unwind 或以回调重入制造借用冲突。C const 不能证明这些保证，由绑定者承担 unsafe 契约。
- Rust 嵌入用 `VmEngine::register_c_fn(name, signature, ptr, owner)`，与旧 direct/CPS 注册分开校验。
  记录引用的注册签名使用实际记录名，导入记录用规范名称；每次加载后均须匹配真实外部布局。
  可用 `joky::ffi::c_header(&module)` 生成本机目标的 C11 声明和 sizeof/align/offset 断言。
  AOT 的 C 导入保持原生符号名，由链接器提供实现，不随 Joky symbol_prefix 改名。
- `unsafe` 已是保留字；`@` 仍用于函数效果，`#` 用于声明属性。当前只开放 `#repr(C)`，
  允许尾随逗号；未知属性、重复属性或错误使用位置报错。通用属性及属性宏尚未实现；异步借用见下节。

布局和注册细节见 [FFI 设计](arena-borrowing-ffi.md#5-ffi-c-布局与引用参数)。

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
  `#repr(C)` 记录的共享/独占引用（与同步 C 引用同一布局规则），结果限整数或 Unit。
  无 Ptr/String/引用聚合输入，不返回引用，不接受局部栈借用。
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
  `memory.read/write` 不能接收 Ptr 来读写外部对象，但可通过 `&Ptr` / `&mut Ptr` 读写 Joky 自己保存的指针值。
- Rust 注册使用 `Type::Ptr`；C 头文件和调用 ABI 映射为 `void *`。只能经 `register_c_fn` 跨外部接口，
  旧 direct/CPS 注册、`#repr(C)` 字段及 C 的 `&Ptr` 参数暂不开放。
- Ptr 不记录分配器或拥有权，没有自动释放、重复释放检查或释放后的副本失效；
  正常退出、失败和取消都不会代调 C 释放函数。unsafe 调用方负责满足外部 API 的类型、空值、寿命、别名及释放契约。

## 任务 arena 与引用

同步根调用或挂起任务在需要时自动创建 arena；用户用内建函数分配，无需创建或传递分配器：

```joky
struct Point { x: Int32 y: Int32 }
fn make_point() -> &mut Point { alloc(Point { x: 1 y: 2 }) }
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
| `share(reference)` | 返回 `&T`；注解、参数或返回位置也可把 `&mut T` 收窄为 `&T` |
| `memory.read(reference)` | 复制整个所指 Copy 值；支持标量、记录、枚举、元组和 Unit |
| `memory.write(reference, value)` | 要求 `&mut T`，写入整个所指 Copy 值，返回 Unit |
| `memory.replace(reference, value)` | 要求 `&mut T`，放入新值并返回旧值；支持 Copy 与拥有型值 |
| `reference.field` / 字段赋值 | 自动解引用，支持嵌套记录；共享引用不能写入 |

`memory.replace(&mut T, T) -> T` 是通用机制，支持数组、拥有型记录、元组和任意枚举。
目标地址先求值并保留，再完整求值新值；借用检查通过后才交换内容，交换本身不调用 Drop。
返回的旧值归调用方所有，新值由原存储负责清理。旧值最后使用后可移动或释放原容器，
但仍存活的目标载荷借用会阻止替换。两侧类型必须相同，类型由引用目标推导，不接受显式类型参数。
拥有型替换继续限同步使用；arena Copy 值可在挂起函数中替换。

```joky
fn take<T>(value: &mut Option<T>) -> Option<T> {
    memory.replace(value, None)
}
```

标准库 `Option.take()` 使用上述机制。也可将 `Array.borrow_mut_ref(index)` 或
拥有字段的 `&mut` 作为目标；带 Drop 的父记录仍保持完整初始化，其钩子会观察更新后的字段。
新值求值或交换准备失败时，两侧仍保持各自的清理责任；正常退出、失败与取消均恰好清理一次。
示例见 [memory_replace.jk](../examples/memory_replace.jk)。

- 分配目标支持四种整数、Bool、Unit、Ptr，以及含这些值和共享引用字段的 Copy 记录、枚举和元组。
  暂不允许 String、opaque 句柄、独占引用字段或直接分配引用本身；零大小对象也有独立分配身份。
- `let` / `var` 控制绑定是否可重新赋值，与所指对象是否可写分开。
  `let p = alloc(...)` 可以修改对象；`p = other` 表示替换绑定，要求 var。
- `&mut T` 不可复制：`let q = p` 移动独占引用，此后不能使用 p。
  普通调用自动重借用；返回值携带借用时，输入的冻结延长到结果最后使用。
  `&T` 可以复制，仍受任务生命周期约束，不能反向转换为 `&mut T`。
- 共享视图存活期间不能写入或移动原独占引用；独占重借用存活期间不能通过原引用读写。
  单次赋值先保留目标地址，再求 RHS；`p.x = p.x + 1` 和
  `memory.write(p, memory.read(p) + 1)` 合法，最终写入时检查活动借用。
- 分支和循环进行保守的来源、移动及最后使用分析；arena 对象仍按整对象检查冲突，局部字段规则见下节。
  跨函数按签名合并引用输入的依赖，不分析被调函数体来放宽权限。
  循环重复执行同一分配点可能保守合并来源；分析超出预算会报编译错误。
- 可定义 `type View = &Point` 或 `fn read<T>(p: &T) -> T`，T 仍要求 Copy。
  普通 `&T` 可以作为 Copy 类型实参、元组元素、记录字段或枚举载荷；`&mut T` 不可以。
  数组视图 `&Array<Int32>` 另有不可存储限制，见[数值动态数组](#数值动态数组)。
  仍不支持引用到引用，如 `&&T` 或 `&mut &T`。
- 带引用参数或返回值（含聚合内引用）的函数只作为 Joky 内部 helper，允许跨源码模块调用；不导出为
  Rust VM / C 任务入口。旧宿主注册及原生回调不接受这些引用，宿主引用结果仍不开放；
  同步 C 引用输入和挂起宿主引用输入分别通过独立 FFI 契约开放。
- 同步及 `@suspends` 函数都可分配、读写、传递和返回 arena 引用，
  包括带共享引用的 Copy 聚合和已封存图。挂起只保留对象，不解除已有借用限制。
  `parallel` 分支可以接收受检 arena 引用，共享读取同一对象或独占访问不同对象；同步局部借用见下节，显式 Arena 后续开放；同步 C 和异步宿主借用接口见前节。
- 根任务正常结束、提前返回、运行时失败、取消或 continuation 丢弃时释放全部对象；
  Pending、内部 helper 返回、分支/parallel 完成及 break/continue 不提前释放。终态 continuation 即使仍被调度器保存，也不继续占有 arena。
  当前每次分配使用独立稳定存储，尚未做分块优化。每根任务固定 64 MiB 计费预算，
  包括对齐后的对象大小、零大小占位和逐项管理成本，不等同于进程实际内存上限。
  预算耗尽、布局溢出或分配失败经运行时错误通道传播，不产生空引用；尚无 free/reset。

`alloc`、`share` 是保留的内建函数名，`mut` 是保留字。示例见
[arena.jk](../examples/arena.jk)，JIT、解释器与 AOT 输出均为 13、16。
后续规划见 [Arena 设计](arena-borrowing-ffi.md)。

### 同步局部借用

同步函数可以直接借用局部值，把修改结果留在原变量中，不需要先调用 `alloc`：

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
- 目标限四种整数、Bool、Unit 和不含引用的 Copy 记录、枚举、元组；不支持借用 String、
  opaque 句柄、引用本身或含引用的聚合。泛型按具体实例检查这些限制。
- 引用沿用自动重借用、共享化和最后使用规则；通过原变量读写或修改字段也检查冲突。
  局部对象中明确不重叠的字段可独立借用；已有视图最后使用后，原位置可恢复相应访问权限。
- 引用不能超过原绑定的词法作用域，不能返回指向本函数局部值或按值参数的引用。
  普通 helper 可以把引用参数的视图返回给调用者；Copy 聚合中的引用同样保留来源和有效范围。
- 局部引用不能存进任务 arena，也不能传给具有 arena 分配能力的 helper；后者保守地认为全部
  引用输入都可能被保存，即使其实际只分配无关对象。仅使用局部借用不会获得 arena 分配能力。
- 接受整个局部标识符及其内联记录字段，如 `&point.x`、`&mut player.position.x`。
  根局部值必须是不含引用的 Copy 数据；不接受经引用解引用的字段、元组元素位置或临时值 `&42`。
  借用得到的引用仍可通过字段访问及 memory.read/write 操作，不增加引用算术。
- `@suspends` 函数内不能取局部借用，局部引用不能送入 parallel 分支或异步 FFI；可传入同步 C 借用接口。
  挂起函数可以调用内部使用局部借用的同步 helper；解释器内部时间片暂停会保活该 helper 的局部存储。
- 局部存储由函数调用拥有，原生后端使用稳定栈位置，解释器使用独立的帧属存储；循环再次进入
  同一声明时重新初始化并复用存储，不累积任务 arena 分配。每个具体函数的被借用存储保守限额为
  64 KiB（计入每处声明的零大小占位和对齐余量），不等同于整个调用栈或进程内存上限。

#### 独立字段与重叠位置

```joky
struct Player { hp: Int32 mp: Int32 }
fn increase(value: &mut Int32) {
    memory.write(value, memory.read(value) + 1)
}
fn run() -> Int32 {
    var player = Player { hp: 20 mp: 20 }
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
使用内置定时器或主动让出的任务可交给 `runtime::driver::block_on`，CLI 也支持挂起 main。
等待期间及恢复后使用同一对象；多次 Pending 不重放 prepare、分配或实参副作用。
同一调用链的同步 / 挂起 helper 共享任务 arena，子函数返回引用不会使对象失效。
模块重载或 VM 丢弃不影响仍存活的旧 continuation；其代码与 arena 一起保活到任务终止。
上例没有把 state 传给宿主；宿主保留对象地址须使用独立的异步宿主借用 v1，不能使用旧 CPS 注册。

### parallel 分支借用

`race` 沿用本节的参数和借用冲突规则；它的普通分支借用在获胜分支完成、其余分支取消后结束。
可分配分支保留的任务期存储借用不因取消提前结束。

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
struct Node { value: Int32 next: Option<&Node> }
fn push(head: Option<&Node>, value: Int32) -> &Node {
    share(alloc(Node { value: value next: head }))
}
fn sum(head: Option<&Node>) -> Int32 {
    match head {
        | None => 0
        | Some(node) => node.value + sum(node.next)
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
  首次 `share` 或隐式收窄为 `&T` 时立即封存，原独占能力失效；共享视图最后使用后也不恢复。
  封存覆盖整个对象，包括非引用字段。无引用的 Point 继续遵循上一节的临时借用规则。
- 构造期 `&mut T`（T 含引用）不能作为函数参数或返回值；先封存，再传递 `&T`。
  该限制同样检查具体泛型实例。当前没有独立的 seal 函数，`share` 就完成这一步。
- 引用保存到 arena 对象后，其借用保留至任务结束，即使存储结果被丢弃、字段被覆盖，或变量已经不用。
  仅保存在普通局部 Copy 聚合里的引用则仍按最后使用结束；两者按实际存储操作区分。
- 调用可分配的函数时，所有含普通引用的实参都按可能被保存处理，包括 Unit 返回的函数和间接调用链。
  分配能力随调用图传播，不根据函数是否恰好保存某个参数来放宽规则；只读、无分配 helper 不延长到任务结束。
  返回依赖仍按签名合并。存入对象的引用必须来自当前隐式任务 arena；局部引用被拒绝，显式跨 arena 存储尚未开放。
- 当前可构造链表、树及共享子节点的无环图；自环和循环共享图使用下一节的受检构图块。
  不允许独占引用字段或可变循环图。直接内联递归布局仍拒绝。

示例见 [arena_graphs.jk](../examples/arena_graphs.jk)，三后端均输出 42、42。
支持运行中增删、重连的通用可变链表尚未实现；后续能力见[容器扩展规划](arena-borrowing-ffi.md#61-容器与所有权扩展)。

## 同步无序 Map

显式导入 `joky/map as maps.{type Map}` 使用 `Map<K, V>`；容器是非 Copy 拥有者，
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
| `borrow_mut_ref(&mut Map, key: K) -> &mut V` | 无引用 Copy 或拥有型 V 的独占引用；Copy 用 `memory.write`，拥有者可用 `memory.replace` 返回旧值 |
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
不支持不同 key 的同时独占借用，也不能把这些视图放入 Option/记录/元组、交给宿主或跨语言挂起。
同步 helper、泛型函数、别名及跨模块返回视图沿用同一规则。
`get`、`remove`、`insert` 返回的旧值及 `element.read` 复制出的普通共享引用依赖实际内容来源，
不依赖 Map 的存储；Map 移动/释放后仍可读取，原来源的冲突修改直到任务结束仍被拒绝。

keys/values 的相同下标在借用期间对应同一项；顺序未指定，不承诺插入顺序或稳定遍历顺序。
使用 `joky/slice` 的 len/get/borrow 等函数配合 while 遍历。Copy value 可用 get/set 或 Element；
拥有型 value 用 borrow_ref/borrow_mut_ref，可原地修改或 memory.replace，不能用 get/set 复制读取或替换。
切片使用既有视图头分配及预算规则，不复制载荷。没有公开桶下标、可变 key 切片或通用 Iterator。

from_entries/extend 按原输入顺序处理；每次替换的旧 value 在处理下一项之前清理。
它们以输入长度为 reserve 上界，即使大量 key 重复也不承诺只按最终大小分配；
容量运算受检，分配、协议或析构失败按终止性错误清理 Map、当前项及剩余输入。
into_entries 消费 Map 并移动各项到新数组；pop/导出得到的拥有值和普通共享引用可继续使用。
输入数组、已导出的 Map 均不可复用；普通共享引用的实际来源保留沿用 Array 规则。
容器不能跨语言挂起，也不能隐式复制或部分移出字段。示例 [maps.jk](../examples/maps.jk) 与 [map_batches.jk](../examples/map_batches.jk)
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


## 数值动态数组

显式导入 `joky/array` 后可使用 `Array<T>`。元素支持四种整数、Bool、Unit、Ptr，
普通共享引用，以及由这些类型组成的 Copy 记录、元组和枚举；另支持下述拥有型记录元素；
长度、容量和索引为 UInt64；不需要新增关键字、索引语法或 unsafe 指针操作。
数组不加入 prelude，关闭默认 prelude 后也可单独导入使用。
构造函数写 `Array.new::<T>()` / `Array.with_capacity::<T>(n)`，实例支持 `values.push(x)`、`values.get(i)` 等方法。
原有 `array.new::<T>()` / `array.push(&mut values, x)` 等模块函数保持兼容，两种调用风格可混用。
不从返回注解或后续写入反推元素类型；其他方法从接收者推导 T。
零大小元素仍受逻辑长度/容量检查，但不分配元素载荷；数组自身始终不是 Copy。

```joky
import joky/array as array.{type Array}

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
  也不能通过 memory.read/write、Ptr 或宿主 ABI 访问控制块。
  元素存储树不含引用的 `Array<T>` 拥有者可以直接作为 `@suspends` 的参数、返回值和中间值；
  该能力覆盖普通数组及其中的无引用拥有元素，但不开放借用视图、含引用元素或用户自定义拥有记录的直接跨挂起。
  无引用元素的数组拥有者也可进入 parallel/race 分支；数组视图和含引用元素仍不能进入结构化并发。
  挂起函数可调用内部使用数组的同步 helper。解释器内部时间片、取消、VM 重载及丢弃都保留正确存储寿命。
- 创建和扩容使用当前任务 arena。数组视图不可被任务期对象保存，因此即使 helper 分配，
  无借用返回值时也会在调用后归还数组权限；普通引用输入仍遵守原来的任务期保留规则。
- push/set 等存储不能保存局部栈借用；写入的共享引用保留至根任务结束，pop、覆盖及丢弃数组都不释放该借用。
  带有含引用元素的可写数组输入（拥有者或独占视图）的 helper 对普通引用实参施加这一契约，即使不分配或索引越界。
  规则按签名、泛型实例和规范化别名判定；不分析被调函数体来放宽条件。泛型推导不隐式收窄独占引用，使用 share(alloc(...)) 创建共享元素。
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
它们及数组视图仍限同步使用。完整示例见
[array_management.jk](../examples/array_management.jk)，三后端输出 22、1、4、42、0。

共享引用元素示例：

```joky
import joky/array as array

fn answer() -> Int32 {
    var values = array.new::<&Int32>()
    array.push(&mut values, share(alloc(42)))
    let copied = array.get(&values, 0)
    array.pop(&mut values); // copied 借用原对象，不借用数组载荷。
    match copied { | None => 0 | Some(value) => memory.read(value) }
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
  可通过 `memory.replace` 替换整个元素或其拥有字段并取得旧值；带 Drop 的记录仍禁止单独替换拥有字段。直接按值读取不能移出资源。
  String 元素只允许整体替换，不能修改文本字节；派生的 `bytes()` 视图继续借用整个原数组。
- `release`、覆盖、作用域退出和失败清理按索引逆序析构剩余元素。单个记录先运行自身 Drop，再按字段声明顺序清理子拥有者。
  某个 Drop 失败时继续清理其他元素，保留最早错误；失败替换的旧元素不重复析构，右侧新实参也会清理。
- 普通函数、方法、记录和透明别名可以使用拥有型泛型实参，特化后重新检查移动与借用；枚举和元组也支持拥有者。
  无引用元素的 `Array<T>` 拥有者可以进入普通 `@suspends` 调用链；数组视图、含引用元素以及元素资源引用仍限同步使用，
  无引用元素的数组拥有者可以进入 `parallel`、`race`；数组视图、含引用元素数组、arena 对象和宿主 ABI 仍拒绝。
  元素资源引用不能存入聚合，不能通过 `memory.read/write` 移动或替换资源，也不能通过共享引用取得独占访问。
  枚举元素支持 `match values.borrow_ref(index)`，载荷借用仍依赖整个原数组。

```joky
import joky/array.{type Array}
struct Resource { id: Int32 values: Array<Int32> }

fn inspect(values: &Array<Resource>) -> Int32 {
    let resource = values.borrow_ref(0)
    resource.id // 元素仍在数组中，由数组负责 Drop。
}
```

完整示例见 [owning_arrays.jk](../examples/owning_arrays.jk)，输出 42、20、22。

元素与切片借用使用独立的只可借用目标 `Element<T>`、`Slice<T>`，分别从 joky/element 和 joky/slice 导入：

```joky
import joky/array as array
import joky/element as element
import joky/slice as slices

fn borrowed_answer() -> Int32 {
    var values = array.new::<Int32>()
    array.push(&mut values, 20)
    array.push(&mut values, 21)
    let region = array.slice_mut(&mut values, 0, 2)
    let item = slices.borrow_mut(region, 1)
    let old = element.read(item)
    element.write(item, old + 1)
    // item 最后使用后恢复 region，region 最后使用后恢复 values。
    let answer = slices.get(region, 0).unwrap_or(0) + slices.get(region, 1).unwrap_or(0)
    array.push(&mut values, 99)
    answer
}
```

- array.borrow / borrow_mut 返回 `&Element<T>` / `&mut Element<T>`；element.read 复制 T，element.write 替换 T。
- array.slice / slice_mut 返回 `&Slice<T>` / `&mut Slice<T>`。区间是半开 [start, end)，必须满足 start <= end <= len，允许空切片。
  借用构造越界会报告带位置的运行时错误；slices.get / set 的越界仍返回 None / false。
- slices 提供 len、is_empty、get、set、borrow、borrow_mut、slice、slice_mut；子切片索引相对当前切片。
  元素借用不分配；每次创建切片/子切片均分配一个计入任务预算的头部，不复制元素，并在根任务结束时回收。
- 所有派生视图借用整个原始缓冲区，不证明不同索引或区间互不重叠。共享视图允许共享读；
  独占视图阻止其他冲突读写，子视图最后使用后恢复父权限。视图存活时不能冲突修改、扩容、pop、移动或覆盖拥有者。
- element.read / slices.get 的复制值不延长存储借用；复制出的普通共享引用仍保留元素内容来源。
  Element/Slice 不能按值使用，其视图继承数组视图的不可存储、宿主和直接挂起限制；
  可通过明确声明形状的同步泛型 helper 或跨模块别名传递，不能返回局部拥有者的视图。
- 取独占借用后求值的后续实参不能冲突读取原数组；先计算复制值，再调用 element.write / slices.set。
  含共享引用元素的写入沿用任务期存储契约，不能写入局部栈借用。

### 拥有型元素的切片借用

拥有型元素也支持 `Array.slice` / `slice_mut` 及 `slices.slice` / `slice_mut` 子切片，
沿用半开区间检查；索引相对当前切片，空切片可以继续创建空子切片。

- `slices.borrow_ref(&Slice<T>, UInt64) -> &T` /
  `slices.borrow_mut_ref(&mut Slice<T>, UInt64) -> &mut T` 接受拥有型及无引用 Copy 元素，
  越界产生带位置的运行时错误。可调用方法、借用字段、共享/独占匹配枚举，
  并通过 `memory.replace` 替换整个元素或其拥有字段，旧值交给调用者清理。
- `len` / `is_empty` 可用于拥有型切片；`get` / `set` 及 Element 接口
  继续仅支持 Copy 元素，不能从借用切片直接移出资源。
- 每层子切片和元素引用都保留整个原数组的借用依赖，不证明不同区间或索引独立。
  活跃子借用阻止冲突访问父视图；最后使用后恢复父权限，所有视图最后使用后恢复数组权限。
  切片不拥有元素，不运行元素 Drop；替换后的新元素仍由原数组负责清理。
- 同步泛型 helper 和跨模块别名可传递、返回来自借用输入的切片或元素引用；
  不能返回局部或按值输入数组的视图，不能存入聚合、arena 对象、宿主 ABI 或进入挂起函数。
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
    private values: Array<T>
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
  从 `&Batch<T>` / `&mut Batch<T>` 可重借用拥有字段；独占字段借用可交给 `memory.replace`，直接按值读取仍被拒绝。
  模块内可写 `&self.values` / `&mut self.values`；同步 helper 可返回来自借用输入的字段视图。
- 字段的 `private` 修饰限定为定义模块内可访问，适用于构造、读取、赋值和借用；
  未修饰字段沿用既有可见性。模块内函数与 impl 均可访问自己的私有字段，跨模块泛型实例保留定义作用域。
  `private` 只在字段修饰位置有特殊含义，`private: Int32` 仍是合法字段声明。
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
字段赋值语法只支持拥有的局部记录；通过借用记录替换拥有字段时使用 `memory.replace(&mut record.field, next)`。右侧不能消费目标记录后再用字段赋值复活它。

## 受检循环图构造

`graph { ... }` 使用当前任务的隐式 arena，完成后统一封存，并返回所选根节点的 `&T`：

```joky
struct Node { value: Int32 next: Option<&Node> }
fn make_cycle() -> &Node {
    graph {
        let a = graph.alloc(Node { value: 20 next: None })
        let b = graph.alloc(Node { value: 22 next: None })
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
  share、作为普通函数/方法实参或带出构图块。普通数据在初始化前按现有表达式规则计算。
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
  拥有型实参在特化后重新检查移动：依赖重复复制参数的模板仅能用于 Copy 实例，用户 trait 约束不能绕过这些限制。
  独立泛型函数值的类型实参仍限 Copy；拥有型实参可通过普通函数包装后取得函数值。
  泛型函数可声明固定类型的 String / opaque 参数或返回值，它们遵循各自宿主与生命周期规则。
- 推导先收集实参中已有的类型，再为仍未确定的整数字面量默认 Int32；例如 `first(1, true)`
  得到 `T = Int32, U = Bool`。重复出现的同一类型参数必须完全一致，没有隐式宽化。
  显式类型实参可以给未定型字面量提供上下文，`identity::<UInt64>(0xffff_ffff_ffff_ffff)` 合法。
- 外层预期返回类型不反向决定泛型实参：`let n: Int64 = identity(1)` 报类型不匹配，
  应写 `identity::<Int64>(1)` 或先声明 Int64 局部值。没有实参约束的参数、只有提前跳转的实参
  及其他无法局部推导的情况需要显式类型实参；不做跨绑定或全局约束求解。
- **所有泛型函数在定义时检查，包括未使用的函数**。T 是任意允许的值类型，不能对它执行算术、
  比较、整数转换、未知字段访问或构造；`fn bad<T>(x: T) -> T { x + x }` 因缺少能力约束而报错。
  可以传递、复制、返回 T，操作显式元组结构，并调用其他泛型函数；固定 Int32 等类型上的操作不受影响。
- 同步及 `@suspends` 泛型调用共用现有求值顺序、快照、失败、Pending、取消与代码保活机制。
  普通函数不能调用挂起函数；`parallel` 分支仍须调用返回 Unit 的挂起函数。
- 编译期按“函数声明 + 具体类型实参”缓存实例。别名复用实例，不同名义类型分别实例化；
  同型递归与互递归可用。每个函数最多 64 个类型参数，每个模块最多 256 个实例，
  新实例依赖链最多 32 层；超限在编译期报错，缓存命中的递归不消耗新的层数。
- 泛型模板及内部实例不公开导出，也不能直接经 `VmEngine` 或 `vm.register_int32` 调用 / 注册。
  为宿主声明普通、具体签名的包装函数，例如 `fn identity_int32(x: Int32) -> Int32 { identity(x) }`。
  `__joky_generic_` 为编译器保留的函数前缀，CLI 的 `main` 不允许类型参数。

泛型记录、枚举、透明别名、固有 impl 和方法独立类型参数已实现，见[泛型记录](#泛型记录)、[泛型枚举](#泛型枚举)、[泛型类型别名](#泛型类型别名)、[泛型固有 impl](#泛型固有-impl)和[方法类型参数](#方法类型参数)。静态 trait 与能力约束见[静态 trait 与泛型约束](#静态-trait-与泛型约束)。泛型 extern、
常量泛型和闭包仍未实现；同步及挂起函数值见下一节。完整示例见 [generics.jk](../examples/generics.jk)。

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
- 保留已有可选分号和相邻表达式序列语法。首版不增加任意表达式的连续括号调用：
  返回函数值时写 `let selected = choose()`，再 `selected(41)`；不要把 `choose()(41)` 当作间接调用。
- 参数和返回类型的别名、泛型替换、跨模块名义身份都递归检查。共享函数类型图缓存比较与替换，
  函数类型嵌套最多 256 层，引用目标中的函数类型展开另限 32 层；递归函数类型别名
  和包含自身签名的递归值布局仍拒绝。
- 间接调用保守视为可能失败、分配并保留普通引用。它沿用 arena 的存储和独占借用检查，
  不承诺“不保留参数”：局部借用不能传入未知函数值，独占借用被保留后也不能重新访问原对象。
  需要这种调用时使用满足现有存储契约的 arena 引用；精确的非保留效果留待后续设计。
- 被引用的目标必须是语言函数；extern、转换内建函数、方法取值和闭包尚未开放。
  需要宿主操作时声明效果匹配的语言包装函数。函数值都可跨 Pending 保存，调用时保留同步或挂起约定。
- 函数值不能跨 C / 宿主 ABI，也不与 Ptr 或 `vm.Function` 互换；没有空函数值、整数转换、相等比较或裸地址访问。
  含函数值的语言签名不作为宿主入口导出。标量入口保留原接口；任务和旧模块快照持有其可达代码，
  在重载、取消或 VM 丢弃时，仍不会提前释放尚可执行的函数代码。

完整示例：[function_values.jk](../examples/function_values.jk)。

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
- `parallel` / `race` 分支可以调用返回 Unit 的挂起函数值，包括记录函数字段。
  各分支的目标与实参按源码顺序求值，在开始轮询任何分支前完成快照；失败和取消沿用现有结构化规则。
- 挂起函数值继承间接调用的保守分配和引用保留契约；不放宽局部借用、独占借用或宿主 ABI 限制。
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
  main 仍限 Unit / Int32，`joky/vm` 的 `call_int32` / `register_int32` 保持 Int32 契约。
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

`match` 的裸分支结果及 `parallel` 分支最外层的 `|` 始终是分支分隔符。
在该位置使用按位或须加括号或块：`| Empty => (a | b)` 或 `| Empty => { a | b }`。
调用参数、元组元素、记录字段及块内部已由括号或花括号定界，可直接使用 `|`，如 `parallel { | worker(a | b) | worker(c) }`。
嵌套分支遵守同一规则，换行不参与消歧。示例见 [bitwise.jk](../examples/bitwise.jk)。

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
offset 逐项核对过），C 引用函数返回浮点也开放。浮点输出使用
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

- 首个不写类型注解的接收者可为 `self`、`&self` 或 `&mut self`，分别对应目标值、共享引用和独占引用。
  self 绑定不可重新赋值；&mut self 可通过引用修改调用方对象。`Self` 在 impl 内保留，在类型、构造和关联成员位置表示目标类型。
- 有 self 的函数支持 `value.method(args)` 和显式 `Type.method(receiver, args)`；显式调用必须按签名传值或引用，无 self 的关联函数只能用类型调用。
  普通成员查找不按实参数量或类型重载；枚举变体与关联成员也不能同名。
- 按值接收者可以是局部变量、字段、调用、分组、块或构造结果，支持链式调用；负数字面量作接收者写 `(-1).method()`。
  接收者先求值一次，再从左到右求值其他实参；可变值按值快照，后续实参中的修改不会改变已传入的接收者。
  接收者的类型独立确定，无约束整数仍默认 Int32，不从方法返回注解反推类型。
- 全部方法体在定义时检查，包括未调用者；递归和互递归静态解析为普通内部调用。
  `@suspends`、失败、提前返回与 Pending 遵循普通函数规则，`parallel` 也支持返回 Unit 的挂起方法调用。
  泛型函数可以对已知具体类型调用方法，不能对无约束 T 猜测方法能力。
- `const NAME: Type = expression` 必须有注解，目前限四种整数和 Bool。
  支持标量字面量、其他关联常量、整数内建转换、一元操作和二元算术/位运算/比较；不支持普通函数调用、
  控制流、局部绑定或运行时状态。前向引用可用，依赖环报错，每个表达式最多 256 个语法节点，依赖链最多 32 层。
  常量在编译期按普通整数规则检查并计算；环绕保持一致，除零和非法移位为编译错误。常量读取无需运行时函数。
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
- 值接收者的自动借用沿用局部借用边界：无引用 Copy 局部值、参数或内联字段；临时值和经引用解引用的内联字段暂不能取借用。
  `alloc(...).method()` 的接收者本身是引用，可以调用对应借用方法。按值方法不隐式读取引用，需要显式 memory.read。
- 含共享引用对象先按原规则封存，再通过共享引用调用 &self 方法；自动借用不放宽构造期独占引用或任务期存储契约。
  局部引用不能传入具有分配效果的方法，也不能跨语言挂起；arena 引用的方法调用可沿用既有 @suspends / parallel 保活和取消规则。
- 不支持自定义解引用和接收者重载；trait 支持静态派发，见后文；数组拥有者的公开别名仍使用 array 模块函数。

完整示例见 [borrowed_methods.jk](../examples/borrowed_methods.jk)，三后端均输出 42。

### 泛型固有 impl

Copy 参数化记录和枚举可声明 `impl<T, U> Type<T, U> { ... }`。
类型实参必须按 impl 形参的声明顺序各出现一次，覆盖目标的全部类型参数；
形参名可以不同于记录 / 枚举定义，例如 `impl<V> Pair<V>`。

```joky
struct Pair<T> { left: T right: T }
impl<T> Pair<T> {
    const SIZE: Int32 = 2
    fn make(left: T, right: T) -> Self { Self { left: left right: right } }
    fn first(self) -> T { self.left }
    fn swap(self) -> Self { Self.make(self.right, self.left) }
}
fn example() -> Int64 {
    let p = Pair.make(0, int64(42))
    p.swap().first()
}
```

- impl 类型参数覆盖目标、成员签名和方法体，离开 impl 后不可用；每个成员体均按刚性 Copy 参数检查，
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
struct Pair<T> { left: T right: T }
impl<T> Pair<T> {
    fn zip<U>(self, other: Pair<U>) -> Pair<(T, U)> {
        Pair { left: (self.left, other.left) right: (self.right, other.right) }
    }
    fn combine<U>(value: T, other: U) -> (T, U) { (value, other) }
}
fn example() {
    let p = Pair { left: 42 right: 7 }
    let q = Pair { left: true right: false }
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
  任一显式列表必须完整，不支持部分实参或 `_` 占位；返回注解不反向推导方法参数。
- 两层类型参数合计最多 64 个，不得重名，也不能把方法参数命名为 `Self` 或 impl 目标的名称。
  不同方法可以各自声明同名 U，身份互不混淆。即使未使用的参数也必须满足 Copy 限制。
- 所有模板体都先按刚性参数检查，再按完整的两层实参缓存实例；继续遵守每模块 256 个实例和 32 层依赖限额。
  定义作用域、成员可见性、同步 / CPS、失败、取消和重载后的旧任务保活规则不变。
  固有方法可声明带约束的参数；trait 要求的方法本身不接受独立类型参数。拥有型记录实参可用；闭包尚未实现。

### Option 与 Result 基础方法

默认 prelude 提供下列普通源码方法，载荷和默认值支持 Copy 类型及拥有型数组、记录、枚举和元组。
除 `take(&mut self)` 外，拥有型实例的方法按值消费接收者；未选中的拥有型默认值、备选值和载荷在退出时清理。
`Option.zip` 的载荷元组支持 Copy 和拥有型元素；任一侧为 None 时清理另一侧的拥有型值。

`Option<T>`：

| 方法 | 语义 |
| --- | --- |
| `take(&mut self) -> Option<T>` | 用 None 替换原值并返回旧 Option，支持 Copy 与拥有型载荷 |
| `is_some() -> Bool` / `is_none() -> Bool` | 判断当前变体，不读取或比较载荷 |
| `unwrap_or(fallback: T) -> T` | Some 返回原载荷，None 返回 fallback |
| `and<U>(next: Option<U>) -> Option<U>` | Some 返回 next，None 返回 None，可改变载荷类型 |
| `or(fallback: Option<T>) -> Option<T>` | 保留原 Some；原值为 None 时返回 fallback，载荷类型保持 T |
| `zip<U>(other: Option<U>) -> Option<(T, U)>` | 两边均为 Some 时返回载荷元组，否则返回 None |
| `map<U>(transform: fn(T) -> U) -> Option<U>` | Some 调用一次 transform，None 不调用 |
| `and_then<U>(transform: fn(T) -> Option<U>) -> Option<U>` | Some 返回回调结果，None 不调用 |

`Result<T, E>`：

| 方法 | 语义 |
| --- | --- |
| `is_ok() -> Bool` / `is_err() -> Bool` | 判断当前变体 |
| `unwrap_or(fallback: T) -> T` | Ok 返回原成功载荷，Err 返回 fallback |
| `ok() -> Option<T>` | Ok 转为 Some，Err 转为 None，舍弃错误载荷 |
| `err() -> Option<E>` | Err 转为 Some，Ok 转为 None，舍弃成功载荷 |
| `and<U>(next: Result<U, E>) -> Result<U, E>` | Ok 返回 next，Err 保留原错误；成功类型可变，错误类型 E 必须相同 |
| `or<F>(fallback: Result<T, F>) -> Result<T, F>` | 保留原 Ok；原值为 Err 时返回 fallback，舍弃原错误；成功类型 T 必须相同，错误类型可变 |

回调方法另提供 `Result.map<U>(fn(T) -> U) -> Result<U, E>`、
`Result.map_err<F>(fn(E) -> F) -> Result<T, F>` 和
`Result.and_then<U>(fn(T) -> Result<U, E>) -> Result<U, E>`。
map / and_then 仅在 Ok 时执行回调，map_err 仅在 Err 时执行；其余分支保持原载荷。
这些方法的函数值参数仍先求值，但回调函数体仅在选中分支执行一次，失败正常传播。

带实参的方法均按普通按值调用规则，先计算接收者，再从左到右计算实参。**默认值和备选值总会求值**，
即使它们不会被选中；实参的副作用、失败或挂起仍会发生。例如 `Some(42).unwrap_or(expensive())`
仍执行 expensive，`Ok(42).or(other())` 仍执行 other。这些接收普通默认值或备选值的方法没有惰性回调语义。
Copy 实例遵循快照规则；拥有型实例的状态判断和取值也会消费原绑定，不可复用。

关闭 prelude 后可显式导入 `joky/option`、`joky/result`；选择性类型导入、重命名及模块限定调用保持相同的名义身份。
`joky/result` 单向依赖 `joky/option`，但不重导出 Option / Some / None；单独导入 Result 后可以链式调用
`result.ok().unwrap_or(default)`，需要直接书写 Option 类型名或构造器时再显式导入它们。
基础方法不依赖闭包或 trait；map / map_err / and_then 使用同步函数值，仍由普通 Joky 源码实现。
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

- trait 声明只有方法签名；`Self` 表示实现目标。支持 `self`、`&self`、`&mut self`、
  无接收者的关联函数、空标记 trait 和 `@suspends`。实现必须完整，参数、返回值、接收者形式与挂起标记逐项匹配。
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
  约束证明复用相同义务，单次证明最多 256 个义务、32 层依赖；函数实例仍受原有 256 / 32 限额约束。
- `import module.{type Eq as Equal}` 导入 trait 别名；`import module` 支持 `module.Eq.eq(...)`，
  也让该模块公开的 trait 参与点调用。库模板保留自己的定义作用域，私有 trait 方法不会暴露给消费者。
  trait 和 impl 只影响编译期，生成普通静态调用；JIT、解释器和 AOT 无 vtable、额外类型标签或新增运行时 ABI。

借用、返回引用来源、参数求值顺序、挂起与取消继续沿用普通函数规则。
未声明效果的抽象 trait 调用保守地视为可能分配、失败和保留普通引用。
语言函数和 trait 方法可在参数列表后写 `@noescape`，保证全部借用实参不被返回或保留；
`@noescape(key, other)` 只约束指定参数。属性不能与 `@suspends` 组合，不能用于 extern
或函数值类型。它不承诺无分配、无失败或无副作用；有风险的未标注 helper 仍按保守规则检查。
实现方法继承 trait 声明，允许实现参数改名；递归调用也必须通过函数体检查。
函数值不会携带此保证，间接调用继续采用原有保守效果。
因此 `fn inspect<T: Eq>(x: T) { x.eq(&x); }` 不能把局部借用交给未知实现；可改为接收引用参数，
由具体调用处检查存储条件，或使用 arena 对象。单态化后根据实际方法体计算效果，纯实现仍可接受同步局部借用，
释放操作仍区分 `needs_arena` 与真正分配的 `uses_arena`。
点调用也保留现有局部借用限制：不借用临时值、不直接借用经 arena 引用解引用后的内联字段。

首版不支持 trait 自身的类型参数、trait 方法独立泛型参数、默认方法体、关联类型 / 常量、
继承、`dyn Trait` 或运算符重载。标准 Eq / Hash 也是普通源码 trait，不是内建运算符协议；
编译器检查签名与约束，不证明相等关系、哈希一致性等代数规律。
泛型类型实参支持 Copy 与拥有者；自动清理及 `Drop` 规则见下一节。

### Drop 与自动清理

同步函数中的 Array、拥有记录及拥有枚举在离开作用域时按逆声明顺序清理。移动转移清理责任，
赋值先求出右侧新值，再析构旧值；提前 return、break、continue 和运行时失败也会清理。
独立表达式产生的拥有临时值最迟在所在词法作用域结束时清理。

需要自定义逻辑时显式导入编译器认可的 `joky/drop.Drop`：

```joky
import joky/array.{type Array}
import joky/drop.{type Drop}

extern fn report(id: Int32)
struct Resource { id: Int32 private values: Array<Int32> }
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
  实现 Drop 的记录允许整体移动；拥有字段不能直接移出或通过赋值替换，但可通过独占重借用与 `memory.replace` 交换，始终保持父记录完整初始化。
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
- 拥有型数组元素的整体移动与 Drop 已开放；单独 String 及无引用元素的 Array 拥有者可跨普通语言挂起，
  Map、StringBuilder 和其他拥有容器仍不开放，Ptr 和 vm.Handle 的显式释放协议不变。
  普通共享引用的任务期保留规则不因数组提前析构而缩短。Drop 不由 prelude 隐式导入。
完整可运行示例见 [static_traits.jk](../examples/static_traits.jk)。

## 导入与 prelude

显式导入 `joky/check.{require}` 可调用 `require(condition: Bool, message: &String) -> Unit`。
参数按从左到右的普通调用规则求值（condition 为 false 时仍求值 message）；true 正常返回，
false 产生终止性 RuntimeError，跳过后续普通执行并完成资源清理。错误拥有消息文本，
直接调用定位到 require 的真实源码调用处，别名与跨模块调用保持来源；经函数值调用时定位到
被调用函数定义。解释器保留已有调用轨迹；原生同步调用在失败传播时保留源码调用点，
通过 `Error::native_call_sites()` 查询，CLI 渲染文件与行号；不提供字节码 PC 或原生栈。
析构中失败继续必要清理并保留首个错误。传入命名消息的 `&message`；消息构造先失败时报告 ResourceLimit，require 尚未执行。诊断消息独立于任务 arena 保活。该接口不依赖宿主自定义注册，未加入默认 prelude。

源码库包括 `joky/option`、`joky/result`、`joky/integer`、`joky/array`、`joky/string` 和 `joky/prelude`；
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

别名可前向引用其他别名，最终目标必须是支持的基础类型、共享/独占引用、模块内记录、枚举、Copy 或拥有型元组或已注册的
宿主 opaque 类型，例如 `type Handle = vm.Handle`。别名不创建新类型、构造函数或运行时值，
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
struct Pair<T> { left: T right: T }

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
  即使 `type Ignore<T> = Int32` 不使用 T，`Ignore<vm.Handle>` 也会报错。
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

- 字段类型支持 `Int32`、`Int64`、`UInt32`、`UInt64`、`Bool`、`Unit`、`Ptr`、共享引用、嵌套记录、有限枚举及 Copy 元组，允许空记录 `struct Empty {}`。
  仅含这些 Copy 字段的记录按值复制，空记录仍保留名义类型身份，不等于 Unit。
  另支持[拥有记录与 Copy 元数据](#拥有记录与-copy-元数据)，此类记录非 Copy，暂限同步使用。
  String 作为拥有叶参与同步聚合布局和清理；宿主 opaque 字段仍不接受。
- 字段声明及构造项的逗号可选，允许尾随逗号；换行只作为空白。
  构造必须提供全部字段，不能重复、缺失或使用未知字段，不提供隐式默认值或字段简写。
  初始化按源码书写顺序求值，布局按声明顺序；Unit、空记录的初始化副作用也会执行。
- 字段访问支持 `p.x`、`shape.point.x`、`translate(p, 1).x` 和 `(Point { x: 1 y: 2 }).x`。
  `var` 根绑定允许字段赋值、嵌套字段赋值以及整个记录的替换；`let` 和函数参数不可修改。
  读取可变值时保留快照，后续初始化项或参数中的修改不影响已经求出的值。
- 不同声明的记录不能互换，即使名字或布局相同。重新加载源码创建新身份，
  原任务继续持有旧代码和类型。透明别名可用于注解，但构造仍使用原始记录名。
- 同步和 `@suspends` 函数都可传递和返回 Copy 记录；分支、提前返回及 parallel 参数遵循同样规则。
  `extern fn`、宿主注册和公开标量调用入口不接受记录签名，CLI main 仍限 Unit / Int32。
- 所有声明均检查，包括未使用的声明。直接或经别名形成的纯记录无限内联布局会报
  `recursive_record`；含枚举的环报 `recursive_type`，纯别名环仍报 `cyclic_type_alias`。当前每个记录最多展开为 65,536
  个标量槽，嵌套最多 256 层，超过限制报 `resource_limit`。
- 记录不支持解构、默认值、整体相等比较或用户定义析构；方法通过独立的 `impl` 声明。

解析只依赖 token：名称之后的 `{}` 或以 `字段名:` 开始的花括号解析为构造，
其他花括号可作为相邻独立块；例如 `let x = 1 { let y = x y }` 保持原有行为。
因此 `value {}` 若意在先读变量再求空块，要写 `value; {}` 或 `(value) {}`。
`if` / `while` 条件及 `match` 被匹配表达式中未被括号、实参列表或内层块包住的构造受限，避免吞掉条件后的块；
使用 `if (Point { x: 1 y: 2 }).x > 0 { ... }` 明确边界。
`.` 字段访问优先于一元及二元运算；模块限定调用仍有效，局部值遮蔽模块名前缀时按该值的方法解析，
找不到方法即报错，不回退为模块调用。完整示例见 [records.jk](../examples/records.jk)。

### 泛型记录

记录可声明 Copy 或拥有型记录实参的类型参数，构造使用字段推导、上下文注解或显式 `::<...>`：

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
  Copy 载荷支持 Int32 / Int64 / UInt32 / UInt64 / Bool / Unit / Ptr / 共享引用 / Copy 记录 / 有限嵌套枚举 / Copy 元组。
  载荷也可为 Array 或拥有型记录、枚举；此时整个枚举非 Copy，按值构造、绑定、传参、返回及赋值均转移所有权。
  同一拥有型枚举的其他载荷必须是拥有者或无引用 Copy 数据，不能混入共享引用及数组视图。
  记录、枚举和元组可以相互组合，但不允许无限内联循环，String 使用拥有载荷规则；opaque 载荷仍不支持。
- 枚举至少一个变体，名称不能重复，`_` 保留给通配模式。变体声明的逗号可选；
  载荷类型和单个模式绑定允许尾随逗号。类型声明支持前向引用及透明别名。
  不同用户声明或不同次解析产生不同类型身份，即使名字、变体及布局相同；标准库身份按编译环境共享。
- `match value { | pattern => expression ... }` 是表达式，被匹配值只求值一次，类型必须为枚举值或枚举引用 `&Enum` / `&mut Enum`。
  模式支持 `Variant`、`Enum.Variant`、`Variant(binding)`、`Variant(_)` 和整个分支的 `_`。
  有载荷模式可递归写元组及嵌套构造，例如 `Some((left, right))`、`Some(None)`、
  `Some(Some(value))`；元组形状和元素数量必须与载荷类型一致。模式中的导入名称按声明身份检查；
  其余无限定模式由被匹配类型确定。裸的无载荷构造器按当前载荷的枚举类型解析，其他裸名称创建绑定。
  有限定模式可使用声明名或导入别名，载荷模式必须匹配声明的有无。
  例如本地 `Choice.None` 与 prelude 的 `None` 属于不同枚举，应使用 `Choice.None` 消除歧义。
  载荷绑定不可变，分支及被匹配表达式中的局部声明不会泄漏；按值或共享匹配的 Copy 绑定是独立快照。
- 必须穷尽所有变体，否则报 `non_exhaustive_match` 并列出缺失项；`_` 覆盖剩余变体。
  重复变体、完整覆盖后或 `_` 后继续写分支均报 `unreachable_code`。
  未知变体、错误限定名及模式载荷数量错误报 `invalid_pattern`。
- 所有正常完成的分支必须同型，也可返回 Unit、记录或枚举。`return` / `break` / `continue`
  等无正常值的路径不参与类型合并；不隐式贯穿，不根据常量被匹配值省略其他分支的静态检查。
  分支体是一个表达式，多步操作用块或分号序列；`;` 仍是可选的连接运算符，
  `| A => first(); value | B => fallback` 合法，分支末尾的分号将其正常值变为 Unit。
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
`type Option` 的选择性导入也能使用限定构造器；裸构造器必须单独导入或由 prelude 提供。
当前不支持 guard、字面量模式或整体相等比较；模式递归深度与类型布局同样限制为 256 层。
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
    | Some((left, right)) => { let displayed: Int32 = left.id + right.id println(&displayed) }
    | None => { let displayed: Int32 = 0 println(&displayed) }
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
  已有独占引用也可显式 `share` 后使用上述共享匹配规则。
- 现有引用边界不变：局部借用与拥有型资源引用限同步使用；符合 arena 存储规则的 Copy 枚举共享引用
  可在 `@suspends` 中匹配，挂起恢复保留绑定且不重复执行分支副作用。
  泛型函数仍按具体实例检查：从共享匹配中返回按值 `T` 的 helper 只适用于 Copy 实例。

完整示例见 [borrowed_matches.jk](../examples/borrowed_matches.jk)。

独占借用匹配可写 `match &mut value`，也可直接匹配已有的 `&mut Enum`。
每个绑定均为对应载荷的 `&mut T`，包含 Copy 标量、记录、元组及拥有者：

```joky
var value = Some((resource(1), 0))
match &mut value {
    | Some((item, count)) => {
        item.id = 42
        item.values.push(42)
        memory.write(count, item.id)
    }
    | None => {}
}
```

- 标量引用通过 `memory.read/write` 访问，记录字段与方法沿用自动解引用。
  绑定名称本身不可重新赋值；修改的是原存储，后续匹配可观察更新。
- 递归模式中不同元组成员可同时独占借用；编译器验证它们来自不重叠的载荷路径。
  原枚举及来源数组仍受借用约束，不能借此替换父值、切换其变体或修改其他数组索引。
  匹配会重借用已有独占引用，载荷引用最后使用后可继续使用原引用。
- Copy 载荷即使取成 `&mut Int32` 等普通引用，仍保留拥有型来源的存储依赖；
  同步 helper 返回、`share` 和分支合并均不会丢失该依赖，不能存入任务期对象或引用数组。
- 借用不移动资源或改变清理责任，更新后的资源在正常退出、失败及取消时仍恰好清理一次。
  `memory.write` 只可替换 Copy 数据；`memory.replace` 可在独占借用结束后替换拥有者并返回旧值。
- 语言仍不支持多层引用，因此独占模式不能绑定本身为引用的载荷；可忽略它或改用共享匹配。
  泛型实例在特化后检查此限制。拥有型引用和局部借用限同步；arena 中的 Copy 枚举可独占匹配并跨挂起恢复。

完整示例见 [mutable_borrows.jk](../examples/mutable_borrows.jk)。

### 泛型枚举

枚举可声明 Copy 或拥有型类型实参的类型参数，用 `Option<Int32>` 等类型应用指定实例。Option / Result
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
  仍不支持递归内联数据。未经过引用、经类型应用、元组、记录或别名形成的声明依赖环会报 `recursive_type`，
  第一版保守拒绝这些环，包括经过未存入载荷的类型参数形成的环。
- 编译期间缓存实例，规范化和 IR 规划各自最多 256 个不同枚举实例（含刚性参数实例）；
  函数实例限额独立计算。布局最多 65,536 标量槽，类型嵌套最多 256 层，未用于载荷的类型实参也计入嵌套深度。
  共享类型图的比较、替换及身份检查避免重复展开。
- 枚举实例沿用标签加独立载荷区的内部布局，支持三后端同步 / CPS、值快照、Pending、取消和旧任务保活。
  没有新增运行时泛型值、extern 聚合 ABI 或公开聚合调用入口。

方法可使用[泛型固有 impl](#泛型固有-impl)；静态 trait 已实现，泛型记录见[泛型记录](#泛型记录)。
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
- 元素支持 Int32 / Int64 / UInt32 / UInt64 / Bool / Unit / Ptr、Copy 聚合和拥有型数组、记录、枚举、元组，
  不接受宿主 opaque 元素。支持透明别名、任意有限嵌套、记录字段和枚举的单个载荷。
  `(Int32, Bool)` 与同结构别名等价，与 `(Bool, Int32)`、`(Int32,)` 不同；
  元组中的名义类型仍按原身份比较，重载产生的新记录或枚举不会因外层元组而变得兼容。
- 全部元素为 Copy 时，元组为 Copy；包含拥有者时，整个元组按值移动。构造、传参、返回、赋值
  都转移所有权，移动后不能再次使用原值。拥有型元组的其他元素必须是不含引用的 Copy 数据。
  解构消费整个元组，将每个名称对应的拥有型子树转移到独立绑定；忽略项在解构结束时清理。
  元组本身没有自定义 Drop；成员记录的 Drop 及嵌套拥有者按元素声明顺序清理。
  引用不允许从借用元组中解构出拥有者。拥有型元组和它的视图限同步函数，可作为引用自由的 Array 元素。
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
- Copy 元组支持同步/CPS 参数、返回值、if/match 合并、递归及 parallel 参数，挂起恢复不重放前面的元素。
  拥有型元组支持同步调用和分支合并，并在返回、失败或取消时清理剩余成员。
  `(Unit, Empty)` 这类零存储元组仍有结构类型，构造和调用的副作用不会被省略。
  元组沿用聚合值的 65,536 标量槽、256 层布局上限；类型/绑定模式语法嵌套也有限额。
  经过元组的循环依赖继续按参与声明报别名环、记录环或枚举环错误。
- 首版通过解构访问元素，不提供 `.0`、索引、解构赋值、参数模式或整体比较。
  match 仍只匹配枚举；元组载荷先用 `| Value(pair) => { let (x, y) = pair ... }` 解开。
  extern、宿主注册和公开标量入口暂不接受元组，CLI main 仍限 Unit / Int32。

完整示例见 [tuples.jk](../examples/tuples.jk) 和 [owning_tuples.jk](../examples/owning_tuples.jk)。

## 字符串字面量

双引号字符串支持 UTF-8 文本，以及 `\n`、`\r`、`\t`、`\0`、`\"`、`\\` 六种简单转义，
另支持 Unicode 转义 `\u{H...}`：花括号内为 1–6 个十六进制数字，允许前导零，
不允许空内容或分隔符；数值必须为 `0..0x10FFFF` 中排除 `0xD800..0xDFFF` 的 Unicode 标量。
例如 `"\u{4E2D}\u{1F642}"` 与 `"中🙂"` 内容相同；`\u{0}` 同样允许内部 NUL。
花括号是普通文本；转义只解码一次，`"\\n"` 的内容是反斜杠和字母 n。
`\0` 表示文本中的 NUL 字节，String 按字节长度传递，NUL 不截断后续内容。
未知转义、裸 LF / CR、反斜杠续行和未闭合字符串均为解析错误，定位使用原始源码字节范围。
迁移旧源码时，将裸换行改成 `\n` / `\r`，字面反斜杠改成 `\\`。

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
恢复时继续当前调用，不重复执行此前的赋值或副作用；循环中的 `parallel` 每轮创建新的分支。
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
else 可以换行，但不能用分号与 then 块分开。`||` 按最长匹配分词，不是两个 parallel 分支符。
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
parallel/race 分支符 `|` 或输入末尾前才解析为裸 return。二元或一元运算中使用跳转需加括号或块。

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
失败或取消不会转为成功返回。parallel/race 分支中的 return 仅结束该分支；调用者跳转前须等整个操作结束。
parallel 等全部分支完成，race 在首个分支完成并取消其他分支后结束。
两者的实参都先于所有分支求值，实参中的跳转可以阻止整个并发操作启动。
完整示例 [examples/jumps.jk](../examples/jumps.jk) 依次输出 `0`、`4`、`13`。

## Rust 嵌入持久实例

`joky::vm` 提供 `InstanceId`、`InstanceValue`、`InstanceStatus`、`InstanceLimits` 和 `InstanceCleanupReport`。
JIT 与 Interpreter 都支持；游戏脚本可选 Interpreter，无需为脚本生成原生代码。
这是宿主 API，不是 `joky/vm` 模块内的语言函数。
[Godot G0/G1 适配器](../integrations/godot/README.md)使用此接口提供解释器脚本实例。

```rust
use joky::vm::{InstanceValue as V, VmEngine, VmMode};

let mut vm = VmEngine::with_mode(VmMode::Interpreter);
vm.load_source(r#"
    struct State { count: Int32 elapsed: Float64 }
    fn make(count: Int32) -> State { State { count: count elapsed: 0.0 } }
    fn tick(state: &mut State, delta: Float64) @noescape -> Int32 {
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
元素递归无引用的 Array、Map、StringBuilder、含这些字段的嵌套记录/枚举/元组和 Drop。类型筛选穿透别名、
具体泛型实参、零大小字段和全部嵌套元素；任何层级的引用、视图、Ptr、宿主 opaque、
函数值均不允许。Map 的 key/value 均须通过完整检查，空 Map 也不能隐藏不合格类型。
构造参数、方法额外参数和结果都是 `InstanceValue` 标量，
严格同型，不隐式做数值转换；Unit 参数仍需显式传入 `V::Unit`。

方法入口是顶层同步函数，首参为该实例名义类型的 `&State` 或 `&mut State`，并声明
`@noescape` 或 `@noescape(state)`。可以在此入口内调用固有方法；宿主暂不直接查找 impl 成员。
普通 `function_signature` / 标量调用不暴露这类引用入口；`instance_type(id)` 返回根类型快照。
`instance_function_signature(name)` 只读取当前模块经编译器检查的实例入口签名，不执行构造、
不创建根、不暴露地址；不合格或不存在的入口返回 None。旧实例的方法仍由其旧模块决定。

`create_instance` 成功才发布 ID。每次 `call_instance` 使用独立临时存储，根状态持续到关闭。
签名、句柄或重入检查失败保持原状态；进入方法后的执行错误释放根并标记 Failed，
不能继续调用，外部副作用不回滚。用 `instance_status` 和 `instance_error` 查询状态/首错。
状态有 Ready、Busy、Failed；关闭后查询报错。同实例同步重入被拒绝，不同实例可交叉调用。
回调中关闭立即使 ID 失效，实际存储在当前调用退出时释放，该调用不发布成功结果；
方法已报错时保留该错误，否则返回关闭错误。

ID 绑定所属 VM，关闭后不复用；重复关闭和跨 VM 使用均报错。VM 销毁释放全部实例。
重载成功后新实例使用新代码，旧实例继续绑定旧布局和方法；加载失败保留原模块。
P1–P3 不支持接收者跨挂起、保状态热迁移或 C VM 实例 API。

拥有字段重借用沿用既有规则，替换使用 `memory.replace(&mut state.field, next)`，
Array 可原地 push、replace、clear；Map 可扩容、插入、替换、删除和 clear，StringBuilder 可追加、
reserve、clear，移出后可消费式 finish；直接按值读取借用字段仍被拒绝。clear 保留容器容量，
用 memory.replace 换成空容器或关闭实例才释放预留容量。调用不会深复制状态，
只迁入拥有分配的 arena 登记，并在出口保留最终可达的拥有图；临时值不延长到实例寿命。
入口转移预检失败保持 Ready，执行、Drop 或出口预算/转移失败进入 Failed，不发布结果。

`set_instance_limits` 的 `max_instances`、`max_bytes`、`max_call_bytes`、`max_peak_bytes`
默认分别为 4096 个身份、64 MiB 活根、64 MiB 调用额外分配、128 MiB 实例与活跃调用合计。
导入已有拥有分配不重复计费，出口计入新持有的载荷；释放旧值退还 arena 计费。
Failed 身份占数量但不占活根字节，需关闭才能退还身份配额。计费包含分配载荷、对齐与
逻辑登记项、实例及根/Buffer 元素清理描述项；不包含 Rust 容器预留容量、解释器局部栈、编译代码、诊断
和其他普通 VM 调用/异步任务，不代表进程 RSS。普通任务的预算保持原有规则。
`instance_count` / `instance_bytes` 返回身份数量和最近持久出口的活根计费，
`instance_peak_bytes` 返回实例及其活跃调用当前的合计计费，不是历史最高值。
解释器执行限额独立；JIT 调用不受解释器指令预算抢占。

`close_instance` 先使 ID 失效，再运行 Drop；Drop 报错仍继续释放其他拥有字段，返回首错。
VM 销毁也继续清理全部实例；可预先保存 `vm.instance_cleanup_report()`，销毁后通过
`report.error()` 检查首个根清理错误。`instance_cleanup_error()` 可在 VM 存活时查询同一报告。

## 内置标准库 `joky/vm`

直接在 Joky 程序中创建 VM、加载源码并调用函数：

```joky
import joky/io.{println}

import joky/vm

fn main() -> Int32 {
    let engine: vm.Handle = vm.create()
    let source = "fn add(x: Int32, y: Int32) -> Int32 { x + y }"
    let name = "add"
    vm.load_source(engine, &source)
    { let displayed: Int32 = vm.call_int32(engine, &name, 20, 22) println(&displayed) }
    vm.close(engine)
}
```

`vm.load_source` / `reload_source` 的源码、`register_int32` / `call_int32` 的名称均使用 `&String`。
`vm.call_int32` 接收句柄、借用函数名和零到两个 Int32 参数。目标函数必须同步返回 Int32，
调用前检查完整签名。字符串中的花括号是普通文本，源码字符串不做插值。
`vm.create_interpreter()` 创建不生成机器码的解释器 VM；构建时的依赖仍见架构说明。
当前原生模块中的同步 Int32 函数可以作为宿主函数注册：

```joky
fn host_add(a: Int32, b: Int32) -> Int32 { a + b }
let name = "host_add"
let status = vm.register_int32(engine, &name, host_add)
```

`vm.register_int32` 支持零到两个 `Int32` 参数；被加载的源码用同名 `extern fn` 声明即可调用。
此特殊注册语法的函数名必须来自 JIT/AOT 原生代码；解释器函数不能转换成原生指针。
它与语言内函数值分开：后者支持三个后端，但不能传给此原生注册接口。
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
显式多源码加载使用 Rust API；`joky/vm` 仍只暴露同步调用入口；
同步 `Int32` 宿主函数可通过 `vm.register_int32` 注册。

`@suspends` 函数可以用 `parallel` 并发等待多个 `Unit` 分支：

```joky
fn main() @suspends {
    parallel {
        | fetch_left()
        | fetch_right()
    }
}
```

每个分支必须是 `@suspends` 的函数调用；parallel 轮询所有未完成分支，全部完成后继续执行。
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

- 只能出现在 `@suspends` 函数中，至少一个分支；分支必须直接调用返回 Unit 的挂起函数。
  `race` 本身返回 Unit，首版不返回获胜分支编号或值。
- 按源码顺序准备所有分支实参，再按源码顺序轮询。每轮遇到 Pending 才轮询下一个分支；
  首个观察到的完成使 race 成功，首个观察到的失败使整个任务失败，不会继续等其他分支成功。
  同轮多个分支就绪时由轮询顺序决定；较早分支立即完成时，后续分支可能一次也未被轮询。
- 父函数继续前，取消并释放其余分支及所有嵌套子帧；重复 Pending 不重算实参。
  取消不撤销已经发生的写入或其他副作用，也没有用户析构或 defer 回调。
- 借用按所有分支同时运行检查，即使某个分支会立即获胜也不放宽冲突规则。
  不分配分支的普通借用在 race 结束后释放；所有分支共享根任务 arena，取消不会回收其中的对象。
  可分配分支仍保守保留引用输入的任务期存储借用。
- 这是单线程协作并发，不承诺抢占或 CPU 公平性。旧 CPS 注册不授予宿主跨 Pending 保留 Joky 引用的权限；
  使用异步借用 v1 的分支在取消时同步执行 stop，确认外部访问停止后才恢复父任务。

## 顺序任务作用域 `scope` 与动态子任务 `branch`

`scope { ... }` 在挂起函数内打开一个顺序主体：主体按普通块的求值顺序执行，
主体中的 `branch { ... }` 在执行到时才把一个子任务登记进当前作用域。
主体与全部已登记子任务（含子任务自己的后代）都完成后，scope 才以 Unit 完成：

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

- `scope` 主体必须以 Unit 结束；`branch` 表达式返回 Unit，主体继续执行，无需等待该子任务。
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
import joky/task as task

fn report_int32(value: Int32) { println(&value) }
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

驱动器在定时器就绪前休眠，选择所有存活分支的最早截止时间；恢复不会重放已完成的副作用。
这是单线程协作调度，定时器不承诺精确唤醒时间或 CPU 公平性，也不能抢占不让出的循环。
race 取消或任务失败时，唤醒信息随帧清理，不留下定时线程或已取消的定时器注册。
只有 Pending 且没有支持的唤醒来源时才报错；不会对任意宿主回调无限忙轮询。
外部 I/O reactor 和通用宿主唤醒注册尚未实现，继续通过嵌入 API 手动恢复外部事件。

Rust 的 `runtime::driver::block_on(task)` 消耗 `VmEngine::call_async` 返回的 continuation，成功返回原始结果位，
失败会取消并清理任务。C/AOT 使用 `joky_cps_block_on` 驱动新建根帧，传入编译器报告的完整槽数；
终态释放子任务及 arena 后保留根帧结果，调用方仍需 frame_free。
公共 C 接口见 [joky_cps.h](../include/joky_cps.h) 与 [joky_task.h](../include/joky_task.h)。

## 内置标准库 `joky/string`

String 拥有合法 UTF-8，允许内部 NUL。它不是 Copy：赋值、按值参数和返回会移动，
`clone()` 显式复制，`release()` 消费并提前释放；作用域结束、失败和取消自动清理。
字面量直接构造预算内的独立 String，构造后不依赖模块常量。`var` 可重新绑定，
`&mut String` 只允许整体替换，例如 `memory.replace(&mut text, next)`，不提供可写字节。

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
单独 String 可跨 Pending 并移动到 parallel/race 分支，普通 `&String` 不能跨挂起。
String 和 StringBuilder 支持同步记录、枚举、Option/Result、元组、泛型、Array 元素及 Map value，
嵌套拥有者随移动、消费匹配、作用域退出和失败递归清理。它们仍是非 Copy；容器读取使用
`borrow_ref`，不能通过 Copy `get` 复制文本。无引用元素的 Array 可跨普通语言挂起；Map、StringBuilder、数组视图、
含引用元素的 Array 及含文本的其他拥有聚合仍不能跨语言挂起。
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
`@noescape`（全参数或按名称指定）契约时，可以省略 `&`。泛型推断从输入值推断 T，
仍检查 trait 约束；已经是引用的输入沿用原有引用转换规则，不再包一层引用。
`&mut T` 实参继续显式写 `&mut`；按值形参继续按 Copy 或移动规则处理。
函数值类型不携带 noescape 契约，间接调用及挂起函数不提供此自动转换。

拥有型局部/拥有字段直接临时借用，打印不移动输入。Copy 输入在各自实参位置求值并复制到
临时存储；拥有型表达式结果只求值一次，移动到临时绑定后借用。实参从左到右求值，
临时绑定保活到整次调用结束，再按逆序清理；后续实参求值、被调用函数或 Drop 失败时仍清理。
别名冲突、借用逃逸和引用存储契约仍由普通借用检查验证。

自动转换沿用现有局部借用支持范围：一般局部存储只在同步函数内提供，挂起调用者目前仅支持
String 局部的同步借用。CPS 流程需要打印标量或复杂状态时，可通过短同步 helper 完成；
传入已有合法共享引用的调用仍按原规则执行。自动借用不允许引用跨 Pending。

所有 `"..."` 字符串字面量默认支持插值，`f"..."` 前缀已移除。
`{expr}` 接受完整表达式（块、函数调用、嵌套插值均可），要求结果实现 Show；
若结果为共享引用，则显示它指向的值。表达式从左到右求值且只求值一次。
拥有型局部/字段仅临时借用，Copy 值可以复制到临时绑定，临时拥有结果在该段显示后清理。
花括号不再是普通文本：`{{` / `}}` 输出字面量花括号，`\n`、`\u{...}` 等转义照常解码；文本段不能直接跨行，
插值表达式内可使用正常换行与注释。空表达式、未闭合或孤立右花括号报错，
格式规格如 `{value:.2}` 尚未支持。嵌套插值最多 64 层。

插值隐式引用内置 show/string 定义，`--no-prelude` 下也可使用；自定义 Show impl 的 trait
仍须显式导入。展开使用不可由源码拼写的局部名和模块别名，不捕获用户同名变量或 helper。
所有显示都复用 StringBuilder、任务预算、借用和 Drop 检查；分配、Show 或析构失败不会
返回半成品 String。接收者借用与 StringBuilder 的同步限制继续适用，较长 CPS 流程应在
短同步 helper 中格式化/打印，再返回拥有 String；不允许插值借用跨 Pending。

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
使用原构造器；共享借用与修改冲突会被拒绝。StringBuilder 及其引用仍只能在同步函数内使用。
挂起函数可以调用同步 helper，在 helper 内构造并 finish，接收独立 String 后继续挂起；
解释器时间片可能中途暂停同步 helper，取消仍清理当前已初始化的字节。
无引用元素的 Array 已完成普通语言挂起的三后端验收；Map、StringBuilder 及其他拥有聚合跨语言挂起仍按专项计划后续阶段验收。
