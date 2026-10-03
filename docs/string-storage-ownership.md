# String 设计与分阶段开发计划

状态：2026-10-03，**S0–S1 验证完成；S2–S8 未开始，正式拥有型 String 尚未实现**。
仓库基线为 `8068f1c`；Array 与 Map 已完成首版所有权和稳定性验收。
本文沿用 [Map 计划](map-storage-ownership.md)的组织方式，每阶段可拆成独立开发目标。

第一版目标是 **不可变、拥有型 UTF-8 String + 可变 StringBuilder + Rune**：
字面量直接是 String，动态文本能进入容器，支持内容查询、构造、String key 和同步文本 FFI。
不公开 Str、Substring 或 ffi.CString，不引入隐式共享、GC、引用计数或写时复制。
StringBuilder 是构造工具，不是另一种只读文本表示；Rune 是字符标量，不是字符串存储单元。

本文负责决策、依赖、迁移和验收。类型目标摘要在 [syntax.md](syntax.md#101-基础类型与字面量推导)；
阶段完成后，实际接口进入 `language.md`，布局与 ABI 不变量进入 `architecture.md`，
性能方法进入 `benches/README.md`，实测结果进入 `interpreter-performance.md`。
本计划不修改当前实现说明，也不把拟议接口放进已实现能力表。

## 1. 设计边界与现有基础

| 基础 | 当前状态 | 对计划的影响 |
| --- | --- | --- |
| String 字面量、参数、返回值与五个查询函数 | 已实现，原生端是借用的长度前缀视图 | 不能只更改类型分类；必须迁移调用、常量、错误消息和宿主入口 |
| UInt8、Rune、字符字面量、Unicode 转义 | UInt8/Rune 与两类字面量已实现 | S1 仍需完整验收与三后端边界回归；不顺带实现全部整数位宽或 Unicode 算法 |
| Array 拥有存储、移动、提前回收、借用 | 已实现 | 复用通用缓冲区与预算；不新建不计费的字符串堆 |
| 拥有型记录、枚举、元组、泛型容器 | 已实现相应子集，String 被排除 | S5 系统开放 String；不能仅删掉一个类型拒绝条件 |
| String 固有方法及引用 | 当前不支持 | S2 补受检方法与借用，规则进入共享 IR |
| Map key | 仅无引用 Copy key，Eq/Hash 按值 | S6 需要借用协议和拥有型 key 清理；String 有内容 hash 还不够 |
| 容器长度、容量和下标 | UInt64 | 新文本 API 同样使用 UInt64；旧 Int32 查询 ABI 单独兼容 |
| 旧 String 的语言挂起和宿主 CPS 路径 | 已存在 | 拥有型迁移必须保持这些路径可用，不以“同步首版”掩盖回归 |
| 通用拥有容器跨语言挂起 | 尚未全面开放 | 单独证明 String 的必要能力，不自动开放 StringBuilder、Array、Map 跨挂起 |
| C 导入与异步借用 | 已有窄范围 ABI 与 poll/stop 协议 | 动态文本不是既有 C 记录或 Ptr，必须显式增加适配契约 |

现有依据为 [runtime/string.rs](../src/runtime/string.rs)、[stdlib/string.rs](../src/stdlib/string.rs)、
[Array 方案](array-storage-ownership.md)、[宿主 ABI](architecture.md#宿主与字符串-abi)和
[借用与 FFI 方案](arena-borrowing-ffi.md)。旧解释器原生边界会编码自有缓冲区，
原生字面量依赖模块数据；三后端内部差异不能变成新的语言语义差异。

## 2. 核心决策与不变量

### 2.1 String、字面量与不可变性

- String 是不可隐式复制的拥有值；赋值、按值参数和返回默认移动，`clone(&self)` 显式复制内容。
- String 内容一经构造便不可修改；`var` 允许重新绑定，`&mut String` 只允许整体替换等
  通用拥有操作，不能暴露可写字节、扩容或原地追加。容器提供 `&mut String` 不违反此规则。
- 只读接口接受 `&String`。命名实参显式写 `&text`，方法接收者沿用已有自动借用规则。
  首版不引入“按值参数自动借用”或根据调用上下文改变字面量类型的规则。
- `"hello"` 直接构造 String。首版将可重定位模块常量复制到受预算管理的拥有存储，
  不让完成构造的 String 保留对模块常量字节的依赖；空串允许零载荷分配。
- 子串返回新的 String 并复制选定字节；没有借用型子串，也没有单独的 Str 类型。
- 不新增字符串 `+`、插值或任意运算符重载；首版用具名 concat/join 与 StringBuilder。
  普通拼接保留输入，生成独立结果，不暗中消费左操作数来改变分配行为。

拟议用法（不是当前可执行示例）：

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

首版用命名临时量借用字面量，不依赖尚未验证的 `&"text"` 临时借用延寿。
字面量分配成本是明确的首版取舍；后续优化不得改变移动、预算失败和模块卸载安全契约。

### 2.2 存储、构造与预算

StringBuilder 优先包装私有 `Array<UInt8>`；String 使用同一缓冲区所有权机制保存合法 UTF-8。
String 保留内建类型身份以承接现有字面量和签名，但不要求暴露内部布局，
也不假设现有三后端能直接把内建 String 当普通拥有记录。S0 用探针决定最小类型/布局接入点。

库算法优先写在 Joky：追加、查询、UTF-8 编解码、整数格式化、拼接与批量构造。
编译器/运行时只增加必须的字面量构造、所有权/布局接入、受检缓冲区转移及边界适配；
不得用宿主 Rust String 集合绕过任务预算，也不得给每个查询增加后端专用操作。

必须满足：

1. String 可见范围内全部字节已初始化且组成合法 UTF-8；允许内部 NUL，不要求尾部 NUL。
2. 缓冲区任一时刻只有一个清理责任；共享借用不能取得可写载荷或转移所有权。
3. Builder 每次成功的公开操作后也保持合法 UTF-8，只接受 Rune 或已验证的 String。
4. `finish(self)` 消费 builder 并转移缓冲区，不复制文本、不重新编码；不承诺清理登记等
   辅助工作绝对零分配。不得通过复制内部拥有句柄伪造转移。
5. `reserve(additional)` 按字节预留；`clear()` 清空 builder 并保留容量。
   长度加法、容量增长、指针宽度转换及可分配字节上限均受检；新旧缓冲区同时存活的峰值计费。
6. 分配失败沿用终止性 ResourceLimit，不承诺 OOM 后恢复执行；全部临时量和部分构造必须清理。
   编码错误等普通输入错误可返回 Result/Option，不与 OOM 混淆。
7. `finish` 可以保留多余容量。首版不强制收缩；否则会破坏直接转移目标。

必要的“冻结缓冲区”机制仅由可信库使用，普通程序不能把任意字节直接标记成合法 String。
输入字节构造先完整验证，再转移所有权；错误路径不得读取未初始化容量或遗失传入缓冲区。

### 2.3 Rune 与 UTF-8 操作

Rune 是 Copy 标量，使用 32 位存储；有效范围为 `0..0x10FFFF`，排除 `0xD800..0xDFFF`。
它不是 UInt32 别名，不开放任意整数算术制造非法 Rune；`from_u32` 返回 Option，
`to_u32` 显式取得数值。Rune 可以按标量值比较和哈希，不隐式转换为字节或整数。

字符字面量 `'a'`、`'中'`、`'\n'`、`'\u{1F642}'` 解码后必须恰好一个合法标量。
字符串增加 `\u{...}`；字符增加单引号转义。具体转义集合、花括号内位数和错误跨度在 S0 冻结，
保留现有转义只解码一次、NUL 不截断、裸换行拒绝和原始源码 UTF-8 字节定位。

- 长度、偏移、容量使用 UInt64，沿用 Array/Map；这里修正讨论中曾提出的 Int32 新接口方案。
- byte_length 按字节计；rune_count 扫描标量，二者不等于显示宽度或用户感知字符数。
- find 返回字节偏移；substring 使用半开字节范围并检查两端边界。
- `next_rune(offset)` 返回 Rune 与下一个字节位置，用 while 遍历，无须 Iterator、for 或闭包。
- Eq/hash 按 UTF-8 字节内容；不做大小写折叠、规范化、语言区域排序或字素簇分段。
- 拒绝过长编码、孤立续字节、代理项编码、截断序列及超出 Unicode 范围的编码。
  不将任意外部字节以替换字符静默修复；宽松解码另列后续目标。

### 2.4 热更新、任务与宿主结果

拥有型字面量和动态文本都由自身存储保活，不因重载新模块改变内容。
旧任务执行代码仍需现有代码 owner 保活；String 不负责解决正在执行的旧代码卸载问题。

任务内移动和返回 String 仍服从任务 arena 契约。**拥有型不等于可把语言句柄直接交给宿主长期保存**：
根调用返回文本时，受检嵌入 API 必须在任务清理前复制到宿主拥有缓冲区，或使用明确的
版本化拥有结果句柄；首版选择复制到宿主缓冲区，不引入跨任务 String 共享。
宿主传入文本反向复制为语言拥有值。仅同步调用期间的只读借用可以通过独立的受检入口省略复制。

迁移已有 String CPS 功能时，任务帧中的拥有值、未消费实参、返回临时量与旧 ABI 适配缓冲区
都必须覆盖 Pending、恢复、分支失败、scope/race 取消和 continuation 丢弃。
跨挂起普通 `&String` 不自动开放；需要的宿主借用由适配缓冲区和既有保活协议承担。
StringBuilder 与含 String 的 Array/Map 跨语言挂起仍按既有拥有容器限制拒绝，另行扩展。

### 2.5 旧 ABI 与新的 C 文本边界

旧 native String ABI 是本机 u64 长度前缀加 UTF-8 字节。保留其导出符号、布局和 unsafe
生命周期契约，不能让同一宿主入口悄悄接收拥有缓冲区句柄。语言源码允许一次有文档的签名迁移，
二进制兼容和源码兼容必须分别报告；旧 AOT 产物与新版 runtime 的支持范围在 S0 明确。

旧签名注册需要显式标记 legacy 适配种类，避免单凭新的 `Type::String` 推定旧槽位约定。
输入适配借用期间编码临时长度前缀缓冲区；宿主返回旧视图时，在其契约允许的有效期内校验并复制。
旧生成代码向外返回视图的入口继续按旧契约处理；新拥有型返回必须用新入口，
不得把任务结束前才有效的指针伪装成旧接口合法结果。所有旧 String 标量槽位调用点必须审计。

S7 增加独立的同步文本适配种类，不自动放宽任意 `extern "C"` 签名：

| C 场景 | 首版契约 |
| --- | --- |
| 接收只读 UTF-8 指针与长度 | 适配器保活输入，仅调用期间访问；长度按目标 C 类型受检转换 |
| 接收 NUL 结尾文本 | 检查内部 NUL，构造末尾带 NUL 的临时缓冲区；内部 NUL 返回可区分错误 |
| 返回指针与长度，或返回 C 字符串 | 按该外部 API 的有效范围读取、校验 UTF-8、复制到语言拥有存储；适配器执行规定的外部释放 |
| 写入输出缓冲区 | 写入受控字节缓冲区，检查返回长度再验证 UTF-8；不直接修改 String 载荷 |
| 调用后继续保存指针或异步访问 | 首版新文本 FFI 不支持；不得用 unsafe 标记代替保活设计 |

无公开 `ffi.CString`、`String.as_ptr()`、任意字节到 String 的 unsafe 源码构造，
也不为模仿 `withCString` 提前引入闭包。首版由 Rust/C 宿主适配器与注册元数据限定借用作用域。
指针真实有效性、外部分配器、C 回调重入及是否越界仍由 unsafe 绑定契约保证，UTF-8 校验无法证明它们。
Rune 不自动映射为 C char/wchar_t；需要时通过 UInt32 适配并验证返回值。

## 3. 第一版 API 契约

以下为拟议接口，均未实现；S0 冻结精确拼写与模块导出方式，之后变更需记录理由。
String 固有查询以共享接收者调用；StringBuilder 通过 `joky/string` 导入。
不增加 String 的可写字节视图；只读字节优先复用已有 `&Slice<UInt8>`，来源跟踪依赖原 String。

| 阶段 | 接口 | 所有权与错误语义 |
| --- | --- | --- |
| S1 | `Rune.from_u32(n)` / `to_u32()` | `Option<Rune>` / UInt32；非法标量不能构造 |
| S2 | 字面量、`String.new()`、`clone()`、`release(self)` | 拥有、移动、显式复制、作用域或消费式清理 |
| S2 | `byte_length()`、`is_empty()`、`equals(&other)` | UInt64 / Bool / Bool；不转移文本 |
| S2 | `find(&needle)`、`starts_with(&prefix)`、`ends_with(&suffix)` | Option<UInt64> / Bool / Bool；find 首次命中，空 needle 为 Some(0) |
| S3 | `StringBuilder.new()` / `with_capacity(n)` / `from(&text)` | 空缓冲区、预留字节、显式复制已有文本 |
| S3 | `push(rune)`、`push_str(&text)` | 独占借用 builder，追加 UTF-8，不消费输入 String |
| S3 | `byte_length()`、`capacity()`、`reserve(additional)`、`clear()` | UInt64 字节数；clear 保留容量 |
| S3 | `finish(self) -> String`、`release(self)` | 消费；finish 转移文本缓冲区 |
| S4 | `from_utf8(bytes: Array<UInt8>)` | Result<String, Utf8Error>；无论成功失败均消费输入，失败清理字节并返回错误偏移 |
| S4 | `bytes()`、`into_bytes(self)` | 只读借用切片 / 消费式 Array<UInt8>；不复制拥有者 |
| S4 | `substring(start, end)` | Option<String>；越界、逆序或非 UTF-8 边界为 None；合法空范围返回空串 |
| S4 | `rune_count()`、`next_rune(offset)` | UInt64 / Option<(Rune, UInt64)>；末尾 None，非法偏移为带源码位置的终止性参数错误 |
| S4 | `concat(&left, &right)`、四种现有整数的十进制转文本 | 返回独立 String；不依赖 `+`、插值或隐式数值转文本 |
| S5 | `join(&parts, &separator)`、`split(&separator)` | String / Result<Array<String>, SplitError>；输入借用，输出拥有；split 规则见 S5 |
| S6 | 内容哈希、拥有型 key 插入与借用查询 | 独立借用协议，不能复用消费式旧 Eq/Hash 假装无复制查询 |
| S7 | 版本化同步宿主文本适配 | 不把内部 String 布局承诺为 C ABI |

Utf8Error 使用无文本拥有字段的错误码与 UInt64 字节偏移，避免验证失败为了描述错误又构造文本。
空查询、Bool 结果与 Option 未命中是新语言 API；旧五个 `joky_string_*` C 符号的 Int32
返回值和无效输入规则保持兼容。S2 同步迁移源码调用、示例和测试，不保留同名重载猜测接口。
新语言 String 不允许空指针或非法 UTF-8 作为有效值；原始宿主输入错误在边界处理。

## 4. 阶段与可独立执行的目标

主依赖为 `S0 → S1 → S2 → S3 → S4 → S5 → S6 → S7 → S8`。
S2 的 ABI/CPS 迁移是硬门槛，不能推迟到 S7；S7 是新增的直接 C 文本适配。
S6 的借用式 key 协议是另一个硬门槛，不在未解决时将 Map<String, V> 标为支持。
每阶段可拆子提交，但公开的新能力必须在该提交具有基本失败清理与三后端行为测试。

| 阶段 | 当前状态 | 可交付结果 |
| --- | --- | --- |
| S0 | 验证完成 | 可行性探针、迁移清单、布局/ABI/API 决策；S2 的四项前置机制已列明 |
| S1 | 已完成 | UInt8、Rune、字符字面量与 Unicode 转义 |
| S2 | 未开始 | 拥有型 String、字面量、借用、基础查询与旧调用/CPS 迁移 |
| S3 | 未开始 | StringBuilder 和消费式 finish |
| S4 | 未开始 | UTF-8 边界、字符遍历、字节转换、拼接和整数格式化 |
| S5 | 未开始 | String 聚合/泛型/容器、join 与 split |
| S6 | 未开始 | 借用式 key 协议与 Map<String, V> |
| S7 | 未开始 | 同步 C 文本适配与嵌入示例 |
| S8 | 未开始 | 稳定性、资源压力、示例、测量与首版验收 |

### S0：验证表示、冻结契约与迁移边界

**S0a：** 核对所有 Type::String 使用点、标量槽、直接/间接函数调用、CPS 参数/返回、
常量数据、标准库注册、`joky/vm`、`joky/check`、诊断消息、C 头文件和安装布局。
用最小可执行探针验证内建拥有类型接入缓冲区、共享/独占整体借用、返回和 finish 转移。
Rune/UInt8 的目标布局也须分别检查，不能把 u64 VM 槽宽等同于实际元素宽度。

**S0b：** 提交决策表，冻结：语言布局接入、私有原语、API 签名、符号版本、legacy 注册种类、
根调用拥有结果提取，以及现有挂起 String 的迁移方式。列出需要修改的旧源码调用和二进制支持范围。
证明错误消息在低预算下能保留首错并完成清理，不能因 `require` 消息分配失败递归构造错误文本。

**验收：** 三后端执行最小构造/借用/返回/失败例，负例拒绝拥有者复制、视图逃逸及私有存储访问。
尚缺能力记录最小复现与独立前置任务，不发布占位 API，不为使探针通过放宽类型检查。
S0 完成只表示方案可实施，不表示正式字符串完成。

#### S0 冻结决策与验证记录（2026-10-03）

S0 对生产实现的审计基线为 `f80c13c`，本阶段不改变内建 String 或任何生产 ABI。
S0 使用[存储探针](../tests/fixtures/string_storage_probe.jk)、
[VM 探针及负例](../src/vm/string_storage_tests.rs)和[CLI 三后端执行](../tests/cli.rs)。
`TextProbe`/`BuilderProbe` 是测试模块中的私有 Int32 数组包装，**不是 String 或 UTF-8 实现**。
它们验证通用所有权的可复用部分；实际内建 String 接入仍属于下面的 P-storage。

| 探针 / 审计项 | 结论与限制 |
| --- | --- |
| 私有单拥有字段包装、消费字段转移、返回与跨模块转发 | 可执行；finish 模型直接移出 Array，不遍历或编码载荷 |
| `&TextProbe` 查询、Slice 返回、最后使用后整体替换 | 可执行；替换移出旧拥有值，两个实例独立清理 |
| 非法复制、finish 后复用、借用冲突、视图逃逸、私有字段 | JIT/解释器/AOT 拒绝；不为方便调用放宽原规则 |
| 正常/失败路径的资源记录 | 跟踪完成构造与 Drop，JIT/解释器核对 arena LIVE；CLI/AOT 核对相同资源输出及首错 |
| arena 预算逐级扫描 | 覆盖构造/替换的失败直到成功；每次调用结束都完整清理 |
| 转移的禁止分配窗口 | 只包围 finish/forward，不包围后续 Slice 创建；原生转移可复用载荷，解释器允许清理快照产生受控 ResourceLimit |
| 禁止后续 arena 分配后的 require / 分配失败 | 主运行时错误或 ResourceLimit 保留，后续 Drop 的 require 不覆盖首错，原资源回收 |
| 重载/销毁 VM 后恢复与取消 | 解释器在同步 helper 的时间片持有资源，旧任务继续/取消均清理；这不等于拥有值已能跨语言挂起 |
| 内建 String 引用、存储布局、Array<String>、记录字段 | 当前均不支持；负例固定缺口，不能把 Int32 包装的成功当作 String 支持 |
| 拥有包装的挂起签名和旧 host ABI | 当前拒绝；分别需要 P-cps 和 P-abi，不自动开放通用容器 |
| UInt8 / Rune 布局 | 已分别验证为 1/1 与 4/int32_align；S1 完整检查已通过 |

“无载荷复制”不等于“后续整个查询零分配”：当前 Slice 创建也可能收费。
预算窗口在查询前结束，不把不同操作的开销归因于 finish。低预算诊断探针证明的是
**当前旧 String 错误通道不依赖任务 arena 的后续分配**，不是进程级内存耗尽仍能构造任意长诊断。
S2 的错误路径必须保留这条边界：先保存首错，诊断从已有字节复制到宿主错误对象，
绝不递归构造语言 String 来报告 String 分配失败；后续清理错误不替换首错。

**迁移审计清单。** 以下覆盖直接 Type::String 分支，也覆盖间接的拥有分类/槽位机制；
S1/S2 实施时应重新搜索，不把这张基线清单当作永久穷尽的文件列表。

| 入口 / 文件 | 当前行为 | 迁移要求 |
| --- | --- | --- |
| `core/parser/{lexer,types,ast}.rs`、`core/integer.rs` | 字面量到 String，单个标量槽，String 不属于 buffer owner | 保留词法身份；UInt8/Rune 独立类型；String 新所有权叶分类 |
| `core/types/{walk,layout,memory_layout,specialization}.rs`、`core/types.rs` | String 无地址布局，聚合/泛型/引用准入受限 | 同步更新字节布局、拥有路径、别名/实例化/借用，不能只改 is_owner |
| `core/ir.rs`、`ir/{inference,constants}.rs` | Literal 是旧常量；require 的绑定签名固定 Bool/String | 新字面量构造有分配/失败效果，require 借用已拥有文本，常量折叠不能消掉必要资源效果 |
| `ir/{buffers,locals,records,enums,tuples,control_flow}.rs` | Array 所有权与视图、作用域退出、返回移动 | String 作为单一拥有叶参与同一套移动/清理和整体替换 |
| `ir/{calls,function_values,graph,generics}.rs` | 直接/间接调用、效果传播、借用来源及泛型 | 分配能力、may_fail、根结果转移与引用来源跨所有调用形式一致 |
| `backends/native/{mod,lower}.rs` | 模块输出旧长度前缀常量；机器值为指针 | 常量只作可重定位复制源；新增受检构造；直接/CPS 调用不能把句柄当旧字符地址 |
| `backends/interp/{mod,execute,optimize,host,memory,buffers}.rs` | 常量与值使用 Rc<str>；原生边界编码 Box；拥有对象另有清理快照 | 拥有文本进任务预算；优化器不把构造当纯 Rc 克隆；旧边界转码有明确保活和计费 |
| `runtime/{string,buffer,arena,cleanup,error}.rs` | 旧 ABI 编解码、缓冲区释放、TLS 首错 | 保留旧符号；新存储复用 buffer；错误读取走独立版本适配 |
| `runtime/{bindings,abi,cps,suspension}.rs`、`vm/engine.rs` | 宿主逻辑类型和 wire 类型未分离；continuation 结果主要为原始数值位 | 显式 legacy 标记、拥有返回的类型化提取、帧资源和 Pending 临时缓冲区保活 |
| `modules/mod.rs`、`core/{resolver,ffi}.rs`、`stdlib/{mod,vm,string}.rs` | 注册/校验仍认为 String 是旧 scalar；string 模块当前为 native bindings | 新逻辑签名与物理 ABI 分离，String 源码模块和私有桥接按定义身份注册 |
| `stdlib/check.jk`、`core/imports` | require 为可信源码内建；模块加载/可见性依赖身份 | 只更新可信定义；不按同名用户函数授予 String 构造权限 |
| `include/joky_{vm,cps,runtime}.h`、安装脚本、AOT wrapper | C 名称/结构、runtime archive 与导出函数约定 | 新入口版本化，旧头文件结构保持；安装后独立 C/AOT 联编验证 |
| `vm/strings_tests.rs`、AOT `tests/strings.rs`、字符串 CLI fixture、embedding 示例 | 字符串复用、返回、Pending、重载、NUL 已有覆盖 | 改为显式借用/clone 后保留同等行为；不得通过删用例避开迁移 |

**表示与私有接入冻结。** 下述是后续实施目标，不是 S0 增加的生产 API。

- 保留 `Type::String`，将其接为一个拥有叶：内部机器表示为目标指针宽度的通用 Buffer 控制块句柄，
  载荷元素为 UInt8；不是直接指向 UTF-8 首字节，也不是公开 `#repr(C)` 结构。
  拥有句柄进入现有 cleanup group；`is_owner/buffer_elements/storage_layout` 等必须一起接入。
- StringBuilder 为可信源码的私有 Array<UInt8> 包装。冻结操作的私有签名为
  `freeze_utf8(Array<UInt8>) -> String`、`into_bytes(String) -> Array<UInt8>`、
  `bytes(&String) -> &Slice<UInt8>`；前两者消费转移，最后一个仅共享借用。
  由编译器定义身份绑定，普通模块不得导入；不靠跨模块访问 Array 私有字段或位转换实现。
  freeze 的 UTF-8 前置条件只由经过验证的可信库调用保证，原始字节入口先验证。
- 字面量用独立的受检 IR 构造操作，运行时复制入口命名 `__joky_string_literal_v2`；
  复用 arena/Buffer 分配。freeze/into_bytes 是所有权转移，不要求再分配一份字节缓冲区。
  旧 `Constant::String` 数据源可保留，但执行构造不能再是纯指针常量或 Rc 克隆。
- 新的 require 读取入口为 `__joky_runtime_require_failed_v2`，旧无后缀入口仍读取 legacy 视图。
  两种错误入口都写同一个首错通道，不能再进入语言字符串分配路径。
- UInt8 大小/对齐为 1/1；Rune 大小为 4，对齐使用目标 int32_align。
  两者参数/结果各占一个逻辑标量槽；u64 CPS 槽不改变对象大小，Rune 进入语言前校验有效性。
  S1 必须对真实类型补充布局与生成代码读写，S0 不新增假 UInt8/Rune。

**API 与源码规则冻结。** 第 3 节表中的方法归 String/StringBuilder；模块函数显式接受对应引用，
方法只是同一源码算法的包装。StringBuilder、Utf8Error、SplitError 由 `joky/string` 导出，
String/Rune 为内建类型。Rune.from_u32 接受 UInt32，to_u32 返回 UInt32；
整数格式化统一为 `to_string(self) -> String`，需导入 `joky/string`。
UTF-8 错误位置为第一个不合法序列的起始字节，错误载荷不含 String；
split 的空分隔符为 `Err(SplitError.EmptySeparator)`。S6 借用 key 协议的名字仍由 S6a 单独冻结。

Unicode 转义在两种字面量中均为 `\u{H...}`，1–6 个十六进制数字，允许前导零，
不允许分隔符、空内容、超过 6 位、代理项或超范围值。字符串保留原六种简单转义并增加 Unicode 转义；
字符支持同样转义和 `\'`，解码后必须恰好一个标量。原始换行、续行仍拒绝，
错误跨度覆盖出错转义或字符字面量的原始源码字节。String 不接受隐式整数或 Rune 转换。

**ABI、返回值与兼容范围冻结。**

1. 逻辑语言签名与宿主 wire 签名分离。新增显式 `LegacyStringV1` 注册种类及
   `register_legacy_string_fn` / `register_legacy_string_suspending_fn` 入口（沿用 owner/frame_slots 能力）；
   普通 register_fn 不再根据新 Type::String 猜测旧布局，应拒绝并提示迁移。
   legacy 参数的语言类型可为 String 或同步 &String，wire 均为旧视图；按值输入在适配返回后清理。
   legacy String 结果必须在有效期内复制为任务拥有值；先检查宿主错误，再读取结果地址。
2. `JokyString`、旧 `joky_string_*`、`joky_vm_*` 和 `__joky_runtime_require_failed` 的
   C 符号/长度前缀布局保持。标准库新的语言查询通过源码方法提供，旧 C 查询不冒充新的 UInt64/Bool/Option 接口。
   旧生成 AOT 对象仍要求匹配其 runtime archive；不承诺混合不同编译器版本的内部函数/CPS ABI。
3. 新含 String 参数/结果的 AOT 导出使用版本化文本入口名，不复用旧函数裸地址调用约定：
   保留 `__joky_user_text_v2_` 前缀，并输出逻辑函数名到入口的元数据。JIT 函数描述符也记录 ABI 种类，
   通用旧标量地址 API 对拥有文本签名拒绝。不能仅添加链接版本标记却仍允许 C 按旧原型调用同名新函数。
4. 受检 Rust 根调用首版提供 `call_string_0` / `call_string_1`，返回宿主 Rust String；
   单文本输入为 `&str`，进入任务前复制。挂起对应 `call_async_string_0/1` 返回
   独立 StringContinuation，完成时先复制输出再清理任务；`take_result()` 移出宿主 String。
   旧数值 Continuation::cps_result 不返回文本指针。取消/失败没有文本结果，完成后宿主结果不依赖 VM。
   复制钩子必须放在生成的根出口/解释器终态路径、`__joky_cps_finish` 或 arena release 之前，
   不能在旧 call_async 返回之后再解引用结果槽。若退出清理随后失败，丢弃已复制结果并报告首错。
5. C/AOT 拥有根结果用 `JokyOwnedTextV2 { uint8_t *data; uint64_t byte_length; }` 出参，
   有状态返回码、失败置空，成功先完成宿主分配/复制再结束任务；释放统一为 `joky_string_result_free_v2`。
   此结构仅是跨边界复制结果，不是语言 String 布局或跨任务共享句柄；内部 NUL 仍按长度保留。
   Rust 结果与 C 结果各自由匹配分配器释放。新函数原型由编译器生成，不能从名字猜测。
6. 旧 CPS 宿主借用参数须在任务完成/取消前有稳定适配缓冲区；同步 &String 不自动变成可挂起借用。
   错误消息、legacy 输入/输出和根结果复制均验证预算/长度上限，校验期间不得将未经验证的地址当安全值。

**需要完成的前置机制。** 这些是探针定位出的实施工作，不算已实现；S0 可以以验证与明确缺口验收，
但 S2 不能在门槛未过时切换公开语义。

| 子目标 | 最小复现 / 当前证据 | 必须交付的修复与依赖 |
| --- | --- | --- |
| P-storage（S2a） | `fn f(s: &String) {}`、`struct Text { value: String }` 被拒绝；Type::String 没有 storage_layout | String 独立拥有叶、方法/引用准入、可信 buffer 转移、字面量失败效果；依赖 S1 的真实 UInt8 |
| P-abi（S2b） | `register_fn` 当前把 Type::String 解释为旧裸指针；受检根调用只支持既有标量结果 | 新 legacy 标记、参数/结果适配、版本化导出、类型化根结果；不能靠 reinterpret cast |
| P-cps（S2b） | `fn f(s: probe.TextProbe) @suspends {}` 被拒绝；旧 String 却已有跨 Pending 测试 | 在共享 IR 与帧清理中接入 String 专有拥有叶的移动/分支/返回；不开放未经验证的其他容器 |
| P-error（S2a/S2b 共同门槛） | 当前 require 在 arena 耗尽后仍保留消息和首错；新字面量将有分配效果 | require 改借用、诊断与语言分配分离；消息构造本身先失败时报告 ResourceLimit，不能伪称 require 已执行 |

S0 的完成不将这些门槛视为自动解决。实际内建 UTF-8 String 的构造、布局、CPS 和新的 ABI
必须在 S1/S2 使用真实类型重新执行同等测试。已有旧 String 的返回/转义/Pending/重载与独立 AOT
回归继续作为迁移基线保留，后续只改必要的源码所有权表达，不删掉测试场景。
S1/S2 落地时将对应“缺口拒绝”用例改为真实类型正例；这些用例不是永久禁止新增能力的规范。
S2a 可先提交内部机制，但 P-storage/P-abi/P-cps/P-error 全部就绪前不得切换默认 String 语义。

**S0 验收完成。** 新增 7 个 VM/布局/负例测试和 1 个 CLI 三后端测试；没有新增生产类型、
原语或 ABI。`./scripts/check.sh --quick` 与 `./scripts/check.sh` 均通过：Debug/Release
各 725 个单元测试、75 个 CLI 测试、22 个源码模块测试通过；完整检查额外通过 4 个单元穷举和
4 个 CLI 穷举，以及 while/Map 三后端基准正确性。显式性能测量测试保持忽略，本阶段未计时。
测试源码中的代理类型仅证明既有通用机制，S1/S2 必须继续落实真实 UInt8/Rune/String 与上述前置门槛。

### S1：字节、Rune 与字面量语法

**实施记录（2026-10-03）：** 已实现字符串 `\u{H...}`，遵守 S0 冻结的
1–6 位十六进制、合法标量、前导零、单次解码和原始源码字节跨度规则。
词法测试覆盖 UTF-8 宽度边界、代理项、越界、空/超长/非法内容和未闭合转义；
CLI 在 JIT、解释器及独立 AOT 上执行参数/返回往返、内容与字节长度核对、内部 NUL
及单次解码，并核对三入口拒绝非法转义。`cargo test unicode_string_escapes` 与
`./scripts/check.sh --quick` 通过；随后完成 UInt8/Rune 类型、布局、JIT/解释器/AOT 与完整
`./scripts/check.sh` 验收，S1 已完成。

**S1a：** 增加 UInt8 的真实一字节内存布局、上下文字面量、同型整数操作、受检移位、
明确 wrapping/checked 转换，以及 Array<UInt8>/Slice 的元素访问。沿用现有整数框架，
覆盖 JIT/解释器/AOT 和标量 CPS，不一并开放尚未设计的 C 记录字段 ABI。

**S1b：** 增加 Rune、受检 UInt32 转换、合法标量比较和字符字面量；字符串与字符支持 Unicode 转义。
词法错误覆盖空/多标量字符、代理项、超范围、非法十六进制、未闭合及混合中文源码跨度。

**验收：** 0/255/256 的 UInt8 边界，UTF-8 的 1/2/3/4 字节分界与标量上下界；
生成代码往返数组及参数结果，非法 Rune 不能经转换、泛型存储或宿主解码进入语言。
旧整数与旧字符串转义回归全部保留。

**实施记录（2026-10-03，UInt8）：** `UInt8` 已接入 AST、推导、IR 常量、解释器和
Cranelift/AOT 一字节布局；支持数组/切片元素、泛型/记录/枚举载荷、同型算术和受检移位，
并提供 `uint8` 转换与标准库范围检查。VM 行为测试覆盖 JIT、解释器、AOT、同步/挂起宿主
调用、预算失败清理、重载后 continuation、取消和非法程序；CLI 覆盖 0/255 边界、算术、
除零/移位错误与三后端输出一致性。Unicode 字符串转义的实施记录见本节 S1 前文。
已运行 `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、聚焦 VM/CLI/AOT
测试；S1 的 Rune、字符字面量、三后端边界与完整检查均已纳入验收。

**实施记录（2026-10-03，Rune）：** `Rune` 已接入类型解析、4 字节目标布局、IR 常量、
解释器与原生后端；字符字面量支持单字符 UTF-8、简单转义和 Unicode 转义，`to_u32()`
提供显式数值转换。Rune 只允许相等比较，不能参加整数算术；非法空/多字符、代理项、
越界和未知转义在共享词法入口拒绝。VM 测试已覆盖 JIT 与解释器的 ASCII、中文、emoji、
NUL/边界标量和非法程序；AOT 生成路径随共享原生 lowering 编译验证。S1 完成后，
UInt8/Rune 的布局与字面量边界作为 S2–S5 的前置契约。

### S2：拥有型 String、基础查询与旧路径迁移

**S2a：** 引入预算内缓冲区构造、移动、clone、提前清理、共享借用与整体替换。
字面量使用可重定位常量作为复制源；JIT/AOT 都不能嵌入编译进程地址。
迁移五个现有查询到第 3 节的新语言签名，保留旧 C 导出适配；支持 String 固有方法，
同时更新 `check.require` 与 VM 标准库只读文本参数。补齐 String 作为拥有载荷的基础分类、
布局与清理，并以最小 Option/Result<String> 用例验证，为 S4 的 from_utf8 结果提供前置能力；
通用嵌套聚合、用户泛型和容器的全面准入与验收留到 S5。

**S2b：** 在切换默认 Type::String 行为的同一可验收目标中完成旧宿主、函数值与 CPS 路径迁移。
提供根结果提取的受检接口；新签名对不安全的原始槽返回明确拒绝或使用新 ABI，不能返回已清理载荷。
验证单独拥有 String 在已有语言挂起功能中的移动和清理，普通资源引用跨挂起保持受限。

**验收：** 空串、中文、emoji、内部 NUL、clone 后独立生存、移动后拒用、提前 release、
宿主输入/输出复制、根任务清理后宿主文本可用、查询输入不被消费。
预算扫描、旧任务重载后恢复/取消、VM 销毁后 continuation 清理、scope/race 未选中分支、
Pending 不重放与首错保留；不以删除旧挂起字符串测试消除回归。
旧 ABI 单独联编 C 用例，错误注册种类和布局不匹配应可诊断。

### S3：StringBuilder 与缓冲区转移

提供 new/with_capacity/from、push/push_str、byte_length/capacity/reserve/clear/finish/release。
可变性遵循现有 `var` 和独占借用；finish 后 builder 不可使用。
builder 不公开内部 Array，不能通过外部 get_mut 写入非法字节。

**验收：** 1–4 字节 Rune、NUL、空追加、连续增长、reserve 后追加、clear 后复用、
已有文本显式复制、finish 后文本独立读取。测试证明 finish 不复制载荷或重新编码，
但不把辅助分配次数写成稳定接口；扩容峰值失败和中途取消无双重释放、无泄漏。
Builder 跨语言挂起/非法共享修改有拒绝测试。

### S4：UTF-8 转换、查询与文本组合

提供 from_utf8、bytes/into_bytes、substring、rune_count/next_rune、concat，
以及 Int32/Int64/UInt32/UInt64 的十进制格式化；处理最小负数，不能通过溢出取绝对值。
只读字节 view 按现有 Slice 规则依赖整个 String；活动借用阻止移动、release、into_bytes 和整体替换。
UTF-8 验证和 Rune 编解码复用同一源码实现，避免各后端各自定义宽松解码行为。

**验收：** 与独立 UTF-8 模型核对合法/非法字节语料和边界；substring 空范围、逆序、
中间字节、超界；next_rune 能精确遍历到末尾；组合重音不被误计为一个 Rune。
from_utf8 失败消费输入且返回准确偏移，bytes 不能逃逸或写入；concat 保留两侧输入。
执行大长度算术的边界测试而不要求实际申请接近 UInt64.MAX 的内存。

### S5：聚合、泛型、容器与批量文本

**S5a：** 开放 String 与同步 StringBuilder 在拥有记录、枚举、Option/Result、元组、
泛型参数和 Array 元素中的合法实例，Map 的 value 同步受益；禁止把拥有值变成 Copy。
嵌套 String 必须参与移动/匹配/析构和失败清理；借用字段仍按现有来源规则，不增加任意借用聚合。

**S5b：** join 借用 Array<String> 和分隔符，不消费元素；空数组为空串，单元素生成独立结果。
split 按非空分隔符从左到右作不重叠匹配，保留头尾及相邻分隔符产生的空项；
空输入得到一个空项，空分隔符返回明确的输入错误，避免隐含字节/标量分割歧义。
split 的输出元素均拥有复制的内容，释放输入后仍可使用。

**验收：** 多拥有字段、嵌套 Array/Map value、消费式 match/元组解构、借用与 memory.replace、
跨模块别名及泛型 helper；记录清理事件并核对 arena 归零。join 总长度算术受检，
split 部分结果构造失败时释放已完成项；拥有型数组的 Copy get 和消费后复用仍拒绝。

### S6：借用式 key 协议与 String Map key

**S6a（协议门槛）：** 现有 Eq/Hash 按值且服务于 Copy key，不能直接改签名或为 String
实现一个会消费 key 的伪兼容协议。设计独立的借用式协议及“不保留查询借用”的效果检查，
沿用静态分派；保留现有 Copy key 调用方式。精确协议和查询方法名在本子阶段冻结并记录迁移表。
不通过复制 String、普通标记 trait、Map 私有编译器特判或部分特化绕过约束。

**S6b：** 在同一 Map 算法中开放无引用拥有型 key，String 实现字节内容相等与哈希。
insert 消费新 key；相等时保留驻留旧 key、清理传入的新 key，替换并返回旧 value。
查询/删除使用 `&K` 入口，不消费、不 clone 查询 String；首版查询与 key 同类型，
不引入 Str 或任意异构 Borrow 查询体系。remove 清理被移除 key 并返回 value，
pop/into_entries 转移 key 和 value；clear/release 清理两者，keys 仍只读。

**验收：** 相同内容不同分配的 String 替换/查询，空串、NUL、多字节文本、碰撞和增长；
缓存 hash 重建不调用用户协议。迁移后所有 Copy key 回归保持通过。
Hash/Eq/Drop 失败、替换/移除中断、批量重复 key 和部分输入消费均恰好清理；
活动 key 借用阻止结构修改和整体替换，不允许通过 `&mut key` 改变索引语义。
不声称内容不可变就自然满足旧 PlainCopy 准入条件。

### S7：同步文本 FFI 与宿主适配

实现第 2.5 节的独立同步文本适配；S2 的 legacy 支持继续保留。
首版直接 C 文本能力通过注册适配器提供，任意源码 C 函数原型推导不在本阶段范围内。
文档给出 Rust 与 C 示例、真实原型、编码、长度上限、输入有效期、输出分配/释放方和失败行为。
可表达的接口才生成头文件，不把内部 String/Rune 默认当作 C 兼容记录。

**验收：** 用 cc 联编独立 C fixture，三后端验证 pointer+length、NUL 结尾、内部 NUL 拒绝、
合法空缓冲区、非 ASCII、非法 UTF-8、输出长度越界与宿主错误。
外部内存按约定恰好释放一次，即使验证/语言分配失败也不能泄漏；临时转换缓冲区归入预算或
明确受限的宿主适配预算，不允许未计费的无限复制。拒绝异步保留和不受支持的声明组合。
受检 ABI 检查不能冒充对任意 C 越界写或悬空指针的内存安全保证。

### S8：稳定性、示例与首版验收

固定种子对照独立模型，覆盖字面量/clone/substring/Builder/UTF-8 转换/容器/Map 混合操作；
每轮检查内容、资源计数与预算复用。默认保留短周期和资源边界回归，长模型、逐指令取消及
额外类型矩阵按 `exhaustive_` 命名并忽略，由完整检查在 Release 执行。
解释器时间片测试与三后端通用失败测试分别报告，不把解释器调度能力写成原生能力。

示例至少包括 Unicode 遍历、Builder 构造、join/split、字符串键频次统计、宿主文本往返；
示例在 JIT/解释器/独立 AOT 上验证，避免仅执行 Rust helper。
按现有基准方法独立测量字面量创建、clone、追加、finish、查询、子串、Map 字符串键及内存。
区分 ASCII/中文/emoji、长度与追加片段规模；分开建表与查询、构造与扫描，记录调用边界、
构建身份、原始样本和峰值内存口径，不把单次首次基线写成性能改善。

**完成条件：** S0–S7 全部验收；完整检查通过；没有未解释的三后端差异、资源泄漏或旧路径回归；
源码迁移、legacy ABI、新文本 FFI 和挂起限制均有准确文档。所有阶段状态与实现一致后才标记首版完成。

## 5. AI 目标设置与共同验收

可按阶段或子目标推进；跨阶段目标仍须逐阶段满足各自验收，不能用子目标完成替代全部完成。
S0–S1 验证已完成，当前阶段为 S2；P-storage、P-abi、P-cps 与 P-error 仍未实现。
基本所有权安全必须随首次实现提交，S8 扩大覆盖而不是补救此前缺失的清理设计。
发现前置缺口时记录最小复现、必要修复和受影响阶段，不以扩大 unsafe 或隐式复制代替设计。

可复用目标模板：

> 完成 docs/string-storage-ownership.md 的阶段【编号与名称】。先核对前置状态与当前仓库，
> 沿用已完成实现。遵守不可变拥有型 String、StringBuilder、Rune、无公开 Str/CString 的边界，
> 优先复用 Joky 源码库与通用缓冲区机制。新增编译器/运行时机制须有最小必要性用例，
> 保持 JIT、解释器和 AOT 语义一致，覆盖迁移、错误、资源清理及非法程序拒绝。
> 按 AGENTS.md 的改动范围选择验证，满足本阶段验收要求，更新相关文档并按
> Conventional Commits 提交。报告实际能力、检查结果、提交号及剩余边界；未通过全部验收不标完成。

每阶段共同检查：

- 行为测试执行生成代码，包含成功、空输入、边界、重复操作、失败和对应所有权负例。
- 内容正确性与资源清理分别证明；不得仅以结果文本正确认定没有泄漏或重复释放。
- 覆盖模块别名、普通函数/方法/函数值、适用的泛型实例和宿主注册路径。
- 动态预算包括字面量、临时拼接、扩容峰值和适配缓冲区；错误清理保留首错。
- 重载后的旧任务、根返回结果及宿主保留文本分别验证，不能用代码 owner 替代数据 owner。
- 内嵌 stdlib 修改后重新编译；默认与 exhaustive 测试分层沿用 AGENTS.md。
- `./scripts/check.sh` 包含 Release 穷举与基准正确性，但不运行计时；性能另行执行。
- `language.md`/README 只新增已实现能力，`architecture.md` 同步 ABI 不变量，本文件更新阶段状态。

## 6. 首版之后的独立目标

| 后续能力 | 必须先解决的问题 |
| --- | --- |
| 小字符串、字面量共享、驻留与 COW | 性能证据、共享所有权、预算、模块卸载与清理成本；不改变公开值语义 |
| Str/Substring 或零复制拆分 | 是否有足够收益，以及借用来源或共享存储保活；不自动开放 |
| 格式化、插值、字符串运算符 | 格式协议、求值顺序、临时值清理与语法，不通过隐式转换猜测 |
| Unicode 字素簇、规范化、大小写与区域排序 | Unicode 数据版本、语义与更新策略，不更改既有字节 Eq/Hash |
| 任意位置可变编辑、shrink_to_fit | Builder API、边界检查、迁移峰值与借用失效规则 |
| 异步 C 文本、跨任务共享、容器/Builder 跨语言挂起 | 显式保活、恢复、取消、stop 和宿主所有权协议 |
| 异构 Map 查询、抗碰撞 hash | 独立借用查询协议、Eq/hash 一致性或明确安全目标 |
| 更多整数位宽、USize、宽松解码及平台路径字符串 | 类型基础、错误策略与非 UTF-8 平台边界，不能默认为 String |

这些目标不作为 S8 完成条件，不在首版开发中无限追加。
在完成本阶段发布验收后，S1 的类型与字面量边界已冻结；后续 S2 只依赖这些真实布局，
不再使用 S0 的代理探针替代 Rune/UInt8。
