# 阶段 7：C FFI 与原生资源边界

> 历史记录：本文保留原阶段的设计、环境与验收结论，不代表当前能力。归档于 2026-09-24；当前说明见[文档入口](../../README.md)，未完成工作见[计划索引](../../plans/README.md)。

> 状态：✅ 已完成（2026-09-13）。同步调用、指针与字符串边界、原生资源、捕获 callback、C 线程进入 runtime，以及 callback 挂起后的 Pending 交接均已验收。按值 aggregate ABI 等扩展不属于本阶段范围。
> 前置：阶段 6 的模块与 ABI。

## 任务

- [x] 第 1 批：`@extern(c, "library", "symbol")` 声明、动态库加载、符号解析、宿主 C 调用约定、整数／浮点／void 映射，接入模块缓存。
- [x] 第 2 批：opaque 指针、C 字符串边界、固定布局结构体和固定数组指针映射。
- [x] 基础正确性：C 指针排除托管清理；指针/数组索引表与类型身份跨模块保存；递归与溢出布局拒绝；对照宿主 C 编译器验证布局。
- [x] 空指针构造与检查 API：`CPtr.null(T)`、`CMutPtr.null(T)`、`CStr.null()`、`.is_null()`。
- [x] Joky 字符串到 C 缓冲区的显式构造和生命周期契约。
- [x] 提供显式借用/拥有转换，禁止 C 指针隐式进入 managed ownership graph。
- [x] 支持 native callback：同步零捕获回调，以及 `CCallback.new(closure, fallback)` 保留句柄。`function()` 返回 C 函数地址，`context()` 返回 userdata；C 必须把 userdata 作为首参数传回。回调可从外部 C 线程进入并执行 `time.sleep`，runtime 会独立排队 Pending 恢复；`close()` 会取消并等待进行中的调用，作用域关闭和自动 Drop 也执行同样的排空协议。
- [x] 提供显式关闭的 tmpfile 资源包装示例，覆盖 close、错误传播和重复关闭/关闭后使用的编译期拒绝。
- [x] 明确通用原生资源的地址封装和自动释放：实现内建 `Drop` trait，支持 class 在作用域退出时释放资源，并覆盖显式 close 后的幂等回收。
- [x] 增加 ABI layout、空指针和同步 callback 测试（对照宿主 C 编译器验证包含字段 padding 的嵌套布局、alignment 与 offset；回调拒绝路径覆盖 effect/挂起/捕获/非闭包/跨模块）。
- [x] 用真实 SQLite `sqlite3_update_hook` + pthread worker 验收保留 callback：SQLite worker 线程触发 callback，Joky callback 可挂起并恢复，worker join 后再关闭 callback 与数据库资源。

## 验收

可调用一个真实 C 库并完成同步、错误、异步 callback 和资源回收；未定义布局或生命周期的声明在编译期拒绝。

## 阶段收尾（2026-09-13）

- 真实 SQLite `sqlite3_update_hook` 从 pthread worker 调用捕获 callback；callback 进入 Joky runtime 后可挂起、恢复，并在关闭时完成取消与排空。
- 原生资源通过统一 provider scope 注册和关闭，SQLite 验证复用进程级 blocking pool，并按连接保持串行执行。
- 本地 macOS ARM64 完整验证通过：1007 项库测试（10 项忽略）、75 项 CLI 测试、21 项 FFI 测试；严格全目标、全特性 Clippy、格式和 diff 检查通过。
- 按值 aggregate ABI、拥有型 C buffer、有界指针加长度读取、其他宿主实跑，以及完整 SQLite 产品 API 均作为后续按需扩展，不阻塞本阶段关闭。

## 指针基础修复记录（2026-09-12）

- 从托管清理、聚合值复制及 class 字段析构中排除 C 地址，覆盖闭包捕获、挂起恢复和取消路径。
- 修复最终类型表丢失 C 指针/数组索引表的问题；链接时重映射所有 C 类型，泛型身份包含指向类型、可变性和数组长度。ABI/cache 升到 10，旧 artifact 不再复用。
- 布局在全部类型解析后计算，使用宿主大小/对齐，检查递归和溢出；`CArray` 的 Joky 按值使用会报告诊断。
- 用本机 `cc` 对照嵌套结构体、数组和指针的 `sizeof`、`_Alignof`、`offsetof`；独立进程验证跨模块缓存命中和布局变更后的依赖失效。
- 本地 macOS ARM64：978 项库测试通过（9 项忽略）、71 项 CLI 测试和 7 项 FFI 测试通过；严格全目标 Clippy 通过。其他宿主尚未执行该轮验证。
- 按值 aggregate 延后，移除尚未使用的目标分类骨架。后续依次补原生资源包装，再确定出站 C 字符串的构造/生命周期方案。

## 空指针 API（2026-09-12）

- 空指针构造与检查为纯内联操作，不分配、不派发 runtime effect，不引入指针隐式转换。
- 类型实参覆盖标量、void、C 结构体与固定数组，复用 C 布局校验；拒绝非 C 指向类型、错误实参和非法指针转换。
- MIR 增加带类型的空指针构造和地址检查，校验非法签名；链接重映射构造目标类型，ABI/cache 升到 11。
- 真实 C fixture 验证 NULL 输入/返回、非空地址以及空 C 字符串的区别；覆盖模块缓存、闭包挂起、class/struct 字段和两种编译入口。
- 修复旧入口展开带缩进 `pub` 声明时漏加模块前缀的问题。
- 本地 macOS ARM64：981 项库测试通过（9 项忽略）、71 项 CLI 测试、8 项 FFI 测试通过；文档测试 1 项忽略，严格全目标/全特性 Clippy、格式和 diff 检查通过。

## 最小原生资源包装：tmpfile

- macOS 与 Linux/glibc 示例直接使用系统 `tmpfile`、`fflush`、`fclose`，`FILE*` 用 `CMutPtr(Unit)` 表示。
- `flush(&self)` 借用、`close(self)` 消费；关闭失败同样消费对象。打开失败不创建资源包装，NULL 不传入 fflush/fclose。
- 显式保存刷新结果后关闭，再传播错误；刷新和关闭同时失败时合并错误消息。
- C fixture 使用真实临时文件并注入打开/刷新/关闭错误，检查调用次数和存活资源，验证每个成功打开的流恰好关闭一次。
- 验证系统 C 库调用、两种编译入口、默认模块化跨模块方法/所有权/effect 契约和磁盘缓存。
- 本地 macOS ARM64 验证：新增 2 项编译器测试、全部 11 项 FFI 集成测试通过；严格全目标/全特性 Clippy、格式与 diff 检查通过。Linux/glibc 示例尚未在对应宿主实跑。
- `Drop` 仅能由 class 实现，方法固定为同步 `fn drop(&self) -> Unit`；不得挂起、panic、启动任务或直接调用。Drop effect 会传播到 owning caller；带 Drop 的值不能进入 `Cown` 或可复制 closure。模块 ABI/cache 版本已升至 12。
- tmpfile 包装通过 `release` 先清空句柄，再执行 `fclose`，显式 `close(self)` 与自动 Drop 共用幂等逻辑。裸 C 指针仍是 Copy 值，不具备自动释放语义；自动回收必须由 Drop class 显式封装。SQLite 所需输出参数、出站字符串留在后续。

## 出站字符串：`String.as_cstr()` 零拷贝（2026-09-12）

- 运行时为每个 `String` 载荷在 UTF-8 长度后额外分配并写入一个隐藏 NUL 字节；`payload_size` 与全部长度语义不变。header 的 `allocation_size` 独立于 `payload_size`，释放布局不受影响；新增 `allocate_object_with_padding` 区分逻辑载荷与分配大小。
- `String.as_cstr() -> CStr` 为无参、无 effect 的纯借用：不拷贝、不分配、不派发 runtime effect。MIR 新增 `StringAsCString` intrinsic，lower 时把接收者引用租借到作用域存活的合成局部，经 `Read`（无引用增加）传入；codegen 取载荷指针包装为 `CStr`，不做 managed drop，引用计数一进一出平衡，匿名临时值（拼接、`trim()` 结果）在 C 调用期间与整个作用域内保持有效。
- 生命周期契约：借出的 `CStr` 仅在源 `String` 存活期间有效，逃逸作用域不被检查；该地址不是 malloc 块，禁止传给会释放它的 C API。所有权移交（adopt/into_raw）与 Joky 侧原生缓冲区分配仍属后续。
- 内嵌 NUL 字节按 C 惯例静默截断；debug 构建中 `as_cstr()` 经 `jk_string_c_string_check` 扫描载荷并在发现内嵌 NUL 时报告后中止，release 构建零开销。
- ABI/cache 升至 13，旧 artifact 不再复用；链接期 intrinsic 无类型载荷，无需重映射。
- C fixture `jk_cstr_check` 同时校验 strlen 与终止符位置；覆盖多字节 UTF-8、空串非 null、拼接/trim 临时值、闭包参数、跨模块缓存序列化（包装函数位于被导入模块）与 legacy 单层展开。
- 本地 macOS ARM64：991 项库测试通过（9 项忽略）、71 项 CLI 测试、12 项 FFI 集成测试通过；严格 Clippy 与格式检查通过。Linux/glibc 示例尚未在对应宿主实跑。

## 入站字符串：`CStr.to_string()`（2026-09-12）

- `CStr.to_string() -> Option(String)` 无参、无 effect，把 NUL 结尾的 C 字符串拷贝为完全拥有的托管 `String`，与 `as_cstr()` 构成借用/拷贝的反向对。`None` 覆盖 null 指针与非法 UTF-8 两种情况，语义对齐 `Bytes.to_string()`；空串得到 `Some("")`，与 null 保持既有区分。读取长度由终止符决定，没有上界参数；有界读取留给后续 `(指针, 长度)` 缓冲区 API。
- runtime 新增 `jk_string_from_cstr`（`CStr::from_ptr` + UTF-8 校验 + 拷贝，失败返回 null）；MIR 新增 `CStrAsString` intrinsic，verifier 顶层校验接收者为 `CStr`、目标为 `Option(String)`；codegen 镜像 `BytesToString` 把 null 结果包装为 `None` 标签。ABI/cache 升至 14。
- C fixture 新增多字节 UTF-8 与非法 UTF-8 字符串；集成测试覆盖 ASCII/多字节内容与字节长度、空串非 None、null 与非法 UTF-8 转 None、`is_null` 先行判别、拷贝结果可再次 `as_cstr()` 出站、跨模块缓存序列化与 legacy 单层展开。
- 本地 macOS ARM64 验证：真实 libc `strerror`/`getenv` 冒烟、992 项库测试（9 项忽略）、13 项 FFI 集成测试通过；严格 Clippy 与格式检查通过。

## C 内存单元与输出参数（2026-09-12）

- 放开嵌套指针：`c_layout` 与类型表达式求值不再拒绝指针指向类型为指针的声明（`CMutPtr(CMutPtr(Unit))` 即 `void**`），布局恒为字宽且不参与值递归，每层 pointee 仍由同一校验环验证。
- `CMutPtr.alloc(value)` 在 C 堆 malloc 一个格子并写入初始值，内容类型为 C 标量或指针；`.read()`/`.write()` 按类型宽度读写，`.free()` 释放（null 无操作），分配失败返回空指针。MIR 新增 `CCellAlloc/Read/Write/Free` intrinsic，verifier 校验接收者与内容类型匹配，链接期重映射内容类型，ABI/cache 升至 15。
- SQLite 里程碑示例：`:memory:` 数据库 open（4 参签名含 `zVfs`）、prepare/step/column_text/finalize/close 全链路，输出参数走 cell，SQL 文本 `as_cstr()` 出站、列文本 `to_string()` 入站。示例曾漏声明 `zVfs` 导致寄存器残值被 sqlite 当指针 strcmp 崩溃，印证错误签名不由类型检查保证并已写入文档。
- C fixture 覆盖标量格子往返、`const char**`/`void**` 输出参数、cell 与 C 侧状态同步、跨模块缓存序列化与 legacy 单层展开；拒绝测试覆盖非 C 内容、错误接收者与参数数量，另增嵌套指针正例（含三层与 `CPtr(CStr)`）。
- 本地 macOS ARM64 验证：真实 libsqlite3 示例通过、998 项库测试（9 项忽略）、14 项 FFI 集成测试通过；严格 Clippy 与格式检查通过。Linux/glibc 示例尚未在对应宿主实跑。

## 第 1 批范围

源语言及生命周期契约见 [C FFI：同步标量调用](../../lang/c-ffi.md)。使用 JIT 适配器调用宿主 C ABI；动态库保持加载至 runtime scope 和 JIT 代码释放，模块缓存只保存声明。固定数组、指针、callback、异步和 AOT 均不计入第一版完成范围。

## 第 1 批验收记录

- 在本机 macOS 调用系统 C 库的 `abs`，并使用 `cc` 生成测试动态库校验全部整数宽度、符号／零扩展、Float32/Float64、void、零参数及超出寄存器数量的混合参数布局。
- 默认模块化入口、独立进程缓存命中、`--no-cache`、`--legacy` 均通过；覆盖跨模块导出、闭包和 task 中调用。
- 库或符号缺失时有明确诊断；缓存命中也重新加载库。更换符号绑定使调用方缓存失效，跨模块冲突 C 签名在执行前拒绝。
- 完整验证：970 项 library 测试通过（9 项忽略）、71 项原有 CLI 测试和 4 项 FFI 集成测试通过，1 项文档测试忽略；格式检查与 `cargo clippy -- -D warnings` 通过。
- Linux/glibc 提供对应示例；其他平台的实际 C ABI 验收仍需在对应宿主上执行。
