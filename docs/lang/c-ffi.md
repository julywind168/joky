# C FFI：同步调用

第一版支持从动态库调用同步 C 函数。声明中的类型是程序作者提供的 C ABI 契约，编译器不会从动态库反射函数签名。

## 声明与调用

```joky
// macOS 系统 C 库；Linux/glibc 上可改为 "libc.so.6"。
@extern(c, "/usr/lib/libSystem.B.dylib", "abs")
fn absolute(value: Int32) -> Int32;

fn main() {
    println(absolute(-42))
}
```

动态库名称或路径遵循宿主系统的搜索规则，相对路径相对于运行进程的工作目录。声明必须以分号结束，必须显式标注返回类型，没有 Joky 函数体。

```joky
@extern(c, "libc.so.6", "abs")
pub fn absolute(value: Int32) -> Int32;
```

第一版只接受 `c`；`javascript`、`wasm` 等后端会在解析阶段拒绝。

`@extern(c, ...) pub fn ...` 可以从其他模块调用。`pub` 只控制 Joky 模块可见性，不会生成供 C 调用的导出符号。可以在普通函数、闭包和 task 中调用外部函数；这里的闭包仍由 Joky 调用，不会作为 callback 传给 C。

## 类型映射

| Joky 类型 | C 类型 |
|---|---|
| `Int8` / `UInt8` | `int8_t` / `uint8_t` |
| `Int16` / `UInt16` | `int16_t` / `uint16_t` |
| `Int32` / `UInt32` | `int32_t` / `uint32_t` |
| `Int64` / `UInt64` | `int64_t` / `uint64_t` |
| `Float32` / `Float64` | `float` / `double` |
| `Unit`（仅返回值） | `void` |
| `CPtr(T)` | `const T*` |
| `CMutPtr(T)` | `T*` |
| `CStr` | `const char*` |

整数使用固定宽度映射；C 的 `long`、`size_t` 等需按目标平台的实际宽度声明。适配层使用宿主 C ABI，包括窄整数的符号／零扩展与寄存器、栈参数布局。Joky 侧仍采用语言 ABI。

可以使用 `@repr(c)` 声明固定布局结构体，并用 `CArray(T, N)` 声明内嵌固定数组。结构体和数组当前只能通过 `CPtr` 或 `CMutPtr` 传递，不能按值传递或返回。

布局按当前 JIT 宿主计算，包含字段填充、尾部填充、嵌套结构体和数组元素步长。`N` 当前必须是正整数字面量；空结构体、字段默认值、递归值布局、非 C 字段和超过宿主可寻址大小的布局会被拒绝。`CPtr(Unit)` / `CMutPtr(Unit)` 表示 `void*`；通过指针自引用的 C 结构体合法，指针的指向类型也可以是指针（如 `CMutPtr(CMutPtr(Unit))` 对应 `void**`，布局为字宽，不参与值递归）。

C 指针是可复制的外部地址，不参与 Joky 的引用计数或自动释放。它们可以在函数、闭包和 class 字段中保存，库和指针所指内存的有效期由 C API 契约决定。当前支持 C 提供地址、Joky 转发地址、显式构造和检查空指针、`String.as_cstr()` 零拷贝出站、`CStr.to_string()` 拷贝入站，以及 `CMutPtr.alloc` / `.read()` / `.write()` / `.free()` 的最小 C 堆内存单元。

尚不接受 `Bool`、`String`、`Bytes`、托管容器、借用参数、泛型或可变参数。指针是 opaque 值，当前不支持指针运算、自动解引用或自动转为 managed ownership。`CStr` 与 Joky `String` 之间没有隐式转换，只能通过 `as_cstr()` / `to_string()` 显式跨越边界。extern 签名可以接受函数类型参数（见下方 native callback）。

## 空指针

```joky
let p = CPtr.null(Int32)             // CPtr(Int32)
let q = CMutPtr.null(UInt8)          // CMutPtr(UInt8)
let opaque = CPtr.null(Unit)        // CPtr(Unit)，对应 const void*
let text = CStr.null()           // CStr
let array = CPtr.null(CArray(UInt8, 4))

if p.is_null() { println("null") } else {}
```

`CPtr.null(T)` / `CMutPtr.null(T)` 接受一个无标签的编译期类型实参；`CStr.null()` 不接受参数。`T` 遵守现有 C 布局限制，可以是 C 标量、`Unit`、`@repr(c)` 结构体或 `CArray`。三种指针类型均支持无参数的 `.is_null() -> Bool`，也适用于 C 函数返回的指针。

构造和检查均为纯操作，不分配内存、不需要 effect，直接编译为宿主指针宽度的零地址与地址比较。`CStr.null()` 与指向空字符串 `""` 的非空 C 地址不同。`.is_null()` 只检查地址是否为零；非空不代表地址有效、可解引用或仍在有效期内。

整数不能隐式转换为指针；`CPtr`、`CMutPtr` 和 `CStr` 之间也没有隐式转换。构造空指针不会创建其指向的对象，固定数组仍只描述 C 内存布局。

## 出站字符串：`as_cstr()`

可运行示例见[双向字符串示例](../../examples/ffi/strings.jk)：

```joky
// macOS 系统 C 库；Linux/glibc 上可改为 "libc.so.6"。
@extern(c, "/usr/lib/libSystem.B.dylib", "strlen")
fn c_strlen(text: CStr) -> UInt64;

fn main() {
    let path = "/tmp/data.db"
    println(c_strlen(path.as_cstr()))
}
```

`String.as_cstr() -> CStr` 无参数、无 effect，把字符串的 UTF-8 载荷零拷贝地借给 C。运行时保证每个 Joky `String` 的载荷在 UTF-8 长度之后紧跟一个隐藏的 NUL 字节，`payload_size` 与所有基于长度的操作不受影响，`byte_count()`、拼接和比较照旧。空字符串借出指向单个 `\0` 的非空地址，与 `CStr.null()` 不同。

生命周期契约：返回的 `CStr` 是从托管对象借来的地址，**仅在该 `String` 存活期间有效**。编译器把接收者（包括拼接、`trim()` 等匿名临时值）租借到当前作用域存活的局部来保证求值期间和同作用域内后续 C 调用的有效性；但把返回值存进字段、跨作用域保留或在源字符串失效后使用都不被检查，属于违约。C 侧只能读取；该地址不是 malloc 的块，**绝不能传给会释放它的 C API**（所有权移交的构造 API 属于后续版本）。

内嵌 NUL 字节会被静默截断，语义与 C 生态一致；debug 构建中 `as_cstr()` 会扫描载荷并在发现内嵌 NUL 时报告后中止，release 构建零开销。

## 入站字符串：`CStr.to_string()`

```joky
@extern(c, "/usr/lib/libSystem.B.dylib", "strerror")
fn c_error(code: Int32) -> CStr;

fn main() {
    match c_error(0).to_string() {
        Some(message) => println(message)
        None => panic("no message")
    }
}
```

`CStr.to_string() -> Option(String)` 无参数、无 effect，把 NUL 结尾的 C 字符串**拷贝**为完全拥有的 Joky `String`，与 `as_cstr()` 构成方向相反的一对：出站是借用，入站是拷贝，结果不与源地址的生命周期耦合。读取长度由终止符决定，没有上界参数；有界缓冲区读取属于后续 `(指针, 长度)` API。

`None` 覆盖两种情况：指针为 null，或内容不是合法 UTF-8。语义与 `Bytes.to_string()` 一致——需要区分两者的调用方先 `.is_null()`。指向空字符串 `""` 的非空地址得到 `Some("")`，与 null 保持区分。仅接受 `CStr` 接收者，`CPtr` / `CMutPtr` 没有此方法。

## C 内存单元与输出参数

```joky
@extern(c, "/usr/lib/libsqlite3.dylib", "sqlite3_open_v2")
fn open_v2(path: CStr, db: CMutPtr(CMutPtr(Unit)), flags: Int32, vfs: CStr) -> Int32;

fn main() {
    let db_cell = CMutPtr.alloc(CMutPtr.null(Unit))
    let rc = open_v2(":memory:".as_cstr(), db_cell, 134, CStr.null())
    let db = db_cell.read()
    db_cell.free()
}
```

`CMutPtr.alloc(value)` 在 C 堆（malloc）分配一个格子，写入初始值并返回 `CMutPtr(T)`，`T` 是值的类型，必须是 C 标量或指针。`.read()` 按类型读回、`.write(value)` 按类型写入，宽度由 `T` 决定（`Int32` 格子读写 4 字节）。分配失败返回空指针，用 `.is_null()` 检查。`.free()` 释放格子；`free(null)` 是无操作。

格子的典型用途是输出参数：声明为 `CMutPtr(CPtr/CMutPtr/CStr)` 的参数把格子地址交给 C，C 写入后 Joky 用 `.read()` 取回。释放权由调用方契约决定：Joky 分配的格子由 Joky 释放；接管 C 分配的内存前必须确认其分配器与 `free` 兼容。读取和写入不做空指针检查，也不校验对齐或存活期——这些由 `.is_null()` 与 C API 契约负责。自动回收仍需实现 `Drop` 的 class 封装，可运行示例见 [SQLite 示例](../../examples/ffi/sqlite.jk)。

声明必须与 C 头文件的真实签名完全一致：少声明一个参数（如 `sqlite3_open_v2` 的 `zVfs`）时，未初始化的寄存器残值会被 C 当作指针使用并崩溃，类型检查无法拦截。

## Native callback：同步回调

```joky
@extern(c, "/usr/lib/libSystem.B.dylib", "qsort")
fn qsort(base: CMutPtr(Unit), count: UInt64, size: UInt64,
         compare: fn(CMutPtr(Int32), CMutPtr(Int32)) -> Int32) -> Unit;

fn main() {
    let values = CMutPtr.alloc(3)
    qsort(values, 1, 4, fn (x: CMutPtr(Int32), y: CMutPtr(Int32)) -> Int32 {
        x.read() - y.read()
    })
}
```

extern 签名的参数可以是函数类型，Joky 把调用点的闭包包装成 C 函数指针传入。当前批次契约：

- **零捕获闭包**：回调必须是在调用点就地书写的无捕获闭包；编译器为它生成一个 C ABI 静态 trampoline，把宿主参数逐个转回 Joky 调用。捕获环境没有生命周期协议，暂不支持。
- **同步、无 effect、不可挂起**：回调签名与函数体都不得包含 effect 或挂起点——C 在宿主栈上同步调用回调，Joky 的 effect 与 Pending 机制无法跨越该边界。违反在编译期拒绝。
- **单模块**：带函数类型参数的 extern 声明不能是 `pub`（跨模块导入暂不重新解析回调类型）。
- **回调仅在 C 调用期间有效**：C 不得保存函数指针在调用返回后使用。回调里可以用 `CMutPtr.read()` 等单元操作读写 C 传入的地址。

满足这些约束的同步回调可以从 `region { ... }` 中调用。区域检查使用 C ABI
契约和实际捕获依赖；捕获区域 Cown 的回调仍会在编译期被拒绝。

## Effect 与执行

C 函数同步执行，不使用 Pending ABI。长时间阻塞的普通 C 调用会占用当前执行线程，也不能被 Joky 强行取消；需要异步进入时使用下面的 `CCallback` 保留句柄协议。

## Native callback：保留句柄与挂起

需要由 C 保存函数指针，或从 C 创建的线程调用时，使用 `CCallback`：

```joky
eff time { @suspends fn sleep(duration: Duration) -> Unit }

let callback = CCallback.new(fn (value: Int32) -> Int32 {
    time.sleep(1ms)
    value + 40
}, -99)
let entry = callback.function()
let context = callback.context()
// C 调用 entry(context, value)，并在所有线程完成后：
callback.close()
```

`CCallback.new` 只接受闭包字面量和显式 fallback 结果。参数必须是 C 标量或 C 指针，结果必须是同类值或 `Unit`。捕获环境会被保留到 `close()`、所属 runtime scope 关闭且所有进行中的调用排空之后；忘记显式关闭时，拥有该值的作用域会自动执行相同清理。关闭中的调用返回 fallback，并使 `failed()` 变为 `true`。

回调可以从任意 C 线程进入。Joky 会为每次进入创建独立 scope 和 Pending 根；挂起后 C 入口保持阻塞，直到恢复完成、失败或取消。C 传入的地址只需在本次 C 调用返回前保持有效。回调目前只允许 runtime `time.sleep(Duration) -> Unit`，不能捕获拥有资源、`Cown`、函数或带用户 `Drop` 的聚合值；C 端必须严格匹配函数签名并把 `context()` 原样作为第一个参数传回。

声明可附加普通 `effects { native }` 契约，调用者必须声明或处理相应 effect。由于 C 函数没有可检查的 Joky 函数体，编译器保守地将所声明 effect 中的全部 operation 计为使用。这样的声明是效果标注，不会把 C 调用转换为 handler request，也不会让 handler 拦截 C 实现。包含挂起、可恢复或中止 operation 的 effect 标注在第一版被拒绝；未标注 effect 的声明由作者保证相应契约。

## 原生资源包装：临时文件

可运行的最小跨平台包装见 [tmpfile 示例](../../examples/ffi/tmpfile.jk)：

```sh
cargo run -- run examples/ffi/tmpfile.jk
```

示例直接调用系统 C 库的 `tmpfile`、`fflush`、`fclose`，成功时输出 `temporary file closed`。`tmpfile` 不需要字符串输入，返回的 `FILE*` 暂用 `CMutPtr(Unit)` 表示，Joky 不读取其内部布局。

| 接口 | 生命周期契约 |
|---|---|
| `open_temp_file() -> Result(TempFile, String)` | NULL 转成打开错误，非空地址交给唯一的包装对象 |
| `file.flush()` | 借用 `&self`，失败后仍保有资源，仍需关闭 |
| `file.close()` | 消费 `self`，成功或失败都不能再使用该包装对象 |
| `exercise_temp_file()` | 保存刷新结果，始终执行关闭，再返回结果；两者同时失败时保留两条错误消息 |

这些接口声明 `effects { stdio }`。它标注同步外部 I/O 依赖，不让调用挂起，也不把 C 调用转为 handler 派发。错误消息目前只区分打开、刷新和关闭失败，没有读取 `errno`。

重复关闭、关闭后调用 `flush`、通过借用消费资源，都会在编译期被拒绝。`fclose` 返回错误也会使流失效，因此不能为了重试而保留包装对象。方法还会防御性地拒绝空地址，避免把 NULL 传给 `fclose`，或触发 `fflush(NULL)` 的“刷新全部流”语义。

这是一个同时支持显式关闭和自动回收的包装示例。普通 class 释放时只清理 Joky 对象，保存一个 C 地址不会自动调用 `fclose`；实现内建 `Drop` 的 class 会在作用域退出时调用 `drop(&self)`。`Drop` 只能用于 class，签名必须是同步的 `fn drop(&self) -> Unit`，不能挂起、panic 或启动任务。示例中的 `release` 先把句柄置空，再调用 `fclose`，因此 `close(self)` 与自动 `Drop` 共用幂等逻辑，不会重复关闭。Drop 的 effect 会传播到拥有该对象的调用者。

唯一所有权约束的是 `TempFile` 对象；其裸地址依然是 Copy 值。带用户 `Drop` 的对象不能放入 `Cown` 或可复制的 closure 环境，也不能直接调用 `drop`。目前字段可读、构造器隐式生成，因此这个示例还不是隐藏原生地址的封闭接口。使用时只通过工厂取得资源，不复制 `handle`、不重复包装同一地址、不直接调用底层关闭函数。裸 `CPtr`/`CMutPtr` 本身仍不会自动释放，必须由实现 `Drop` 的 class 封装。

## 模块缓存与动态库生命周期

MIR artifact 只保存库名、符号名和签名，绝不保存函数地址或动态库句柄。每次执行都会重新加载库并解析符号，包括模块缓存命中时。当前会解析链接后所有 extern 声明，即使某个声明没有被调用；缺失库或符号会在进入程序前报告诊断。

动态库由当前 JIT backend 持有，等待所属 runtime scope 完成并释放 JIT 代码后才卸载。不同库中的同名符号分别从对应库句柄解析。修改声明会更新 ABI 指纹并使依赖缓存失效；替换 C 库本身不触发 MIR 重编译，因此替换后的库必须继续满足声明的 ABI。C 库内存错误、异常跨越 C 边界或错误的实际签名不由 Joky 的类型检查保证。

完整示例见[跨平台 abs 示例](../../examples/ffi/abs.jk)。

SQLite 的异步 callback 验收位于 `tests/fixtures/ffi/sqlite_callbacks.c` 与 `tests/ffi/sqlite.rs`：C shim 在 pthread worker 中打开真实 SQLite、注册 `sqlite3_update_hook`，再由 hook 调用 `CCallback`。因此 SQLite 的同步查询示例和异步 hook 验收分开维护；运行 `cargo test --test ffi sqlite_update_hook_calls_retained_callback_from_worker_thread` 可重复执行该场景。
