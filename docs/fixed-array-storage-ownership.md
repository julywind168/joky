# FixedArray 设计与分阶段开发计划

状态：2026-10-18，**F0–F5 已完成，首版范围验收完成**。
已实现常量参数、紧凑布局、数组值、无引用 Copy 聚合/嵌套、Array 元素、同步间接调用、元素引用/切片及 C 数组字段/同步引用；未开放组合仍在共享前端拒绝。
实施前核对基线为 `c5035d1`，替代原 `ff8cefb`：除动态 Array、String、借用式 Map key 和
String S7 外，窄整数与浮点 C 布局、同步闭包、模块常量、持久实例、状态域及同步显式 Arena
已加入实现。下文按当前能力重新限定组合范围，不把这些能力自动推广到 FixedArray。
FixedArray 不依赖 String S8 的性能验收，但新增存储与借用机制必须保留已有容器和文本回归。

本文承接[容器与递归布局](syntax.md#103-容器与递归布局)和
[arena 容器路线图](arena-borrowing-ffi.md#61-容器与所有权扩展)。
阶段状态与设计决策放在本文；实现后的源码用法归 language.md，存储与 ABI 不变量归
architecture.md。README 只在对应阶段验收后增加能力，不提前宣称 FixedArray 可用。
下面的完整数组接口为 F0 冻结的目标契约；首版当前用法见 [language.md](language.md#常量参数与-fixedarray-类型)，
F5 已完成集成验收；首版之后的独立扩展见第 6 节。源码接点与冻结记录见第 7 节。

## 1. 首版目标与边界

`FixedArray<T, N>` 是内联存放 N 个元素的定长值，N 属于类型。首版交付：

- 无引用 Copy 元素，以及由它们组成的受支持聚合、嵌套 FixedArray。
- 完整初始化、整体复制、同步参数传递与返回、Copy 聚合字段和受检元素访问。
- 同步共享/独占元素借用及切片视图，不隐式转成动态 Array。
- C 兼容元素的内联数组字段、整个数组的同步引用，以及显式首元素指针适配。
- JIT、解释器和独立 AOT 的一致行为、预算检查及失败清理。

首版不开放拥有型元素、含引用元素、部分移动、任意常量表达式、数组索引运算符、VLA、
C flexible array member、数组或含数组记录按值跨 C ABI、语言挂起时持有新数组/视图，
以及宿主异步保留。解释器时间片暂停同步函数必须正确保活，不能据此推定语言 CPS 已开放。
普通挂起函数仍可调用符合已有边界的同步 helper。
各使用位置的首版准入见 §2.6；拒绝规则递归检查外层包装，不能靠记录或容器隐藏数组。

拥有型元素的目标语义是整体移动和逐元素析构，详见第 6 节；F5 的首版完成不包含该扩展。

## 2. 必须保持的契约

### 2.1 类型、长度和常量参数

`FixedArray<Int32, 4>` 与 `FixedArray<Int32, 8>` 不同；长度不能在运行时修改。
透明别名展开后按元素类型与规范化长度判断等价，长度值还必须参与泛型实例缓存、函数签名、
方法选择和符号唯一性，不能只保留元素类型。

**F0 冻结首版采用 `<T, const N: UInt64>`**，与现有容器长度接口一致；实例化时检查
目标地址空间、逻辑元素数量和布局上限（§7.3）。原 USize 拼写不再作为本计划的待选方案，
也不引入新的运行时整数类型。该语法及常量泛型已由 F1 实现。

首版常量实参仅接受整数字面量或已声明的常量参数；不接受运行时变量、负数、浮点值、
函数调用、`N + 1` 等常量运算、通用 const fn、模块常量或关联常量。
模块常量虽已实现，首版也不接受其名称作为长度实参；显式求值、可见性、类型转换及跨模块
规范化规则另行设计，与[模块常量计划](module-constants-record-syntax.md#4-开发阶段)保持一致。
普通实参类型中已经存在 FixedArray 时，可按精确结构推导 N；不能反解长度方程。
无法推导时要求显式参数，不猜测默认长度。
表达式位置的 N 是实例化后已知的 UInt64 值，不新增隐藏运行时参数；
函数体仍可对它执行普通算术，但计算结果不能因此成为新的类型长度实参。

常量参数先覆盖函数、记录、类型别名及固有 impl，保证库 API 与普通用户泛型使用同一机制。
常量参数化 trait、依赖长度的 trait 求解等另行扩展；不借此改变既有 trait 重叠规则。

### 2.2 布局和内部表示

有效值只包含连续排列的元素，不携带长度、容量、拥有者指针或 Buffer 控制块。
内联描述的是值布局，不限定物理分配位置：值可以位于局部存储、记录字段或容器载荷内。

- 元素步长为完整 `sizeof(T)`，包含元素自身尾部 padding。
- 数组大小为 `N * sizeof(T)`，对齐为 `alignof(T)`；乘法、偏移及目标可表示性均受检。
- 嵌套数组逐层使用相同规则，最后一维连续，保持 C 多维数组顺序。
- 语言侧允许 N=0；大小为 0、对齐跟随 T。T 为零大小类型时，逻辑长度仍为 N。
- N=0 不消除对 T 的类型合法性检查，也不使无限内联递归类型合法。
- 不能用零字节大小绕过类型深度、元素数量、实例数量或编译工作量限制。

布局描述应有独立数组形状，保存元素布局、步长与长度，不能生成 N 份字段描述。
IR 按有类型的连续存储与位置访问处理数组，不能把大数组展开成 N 个标量槽或 N 条复制指令。
即使实现使用内部地址传递，语言按值复制仍必须产生独立内容，不能悄悄变成共享句柄。
私有语言调用约定与公开 C ABI 分开，不以二者都使用地址为由认定兼容。

紧凑表示必须贯穿含数组的记录、元组、Option / Result、嵌套数组及 Array 元素，覆盖构造、
字段读写、赋值、分支合并、直接/间接调用和返回。尤其嵌套数组的 get 返回 Option<FixedArray<…>>，
不能仅让数组类型紧凑，再由外围聚合展开成 N 个槽。F0 已逐项核对当前 scalar_types、
parameter_slots、memory_scalars / storage_scalars 及其调用者，明确哪些路径改为存储位置操作、
哪些保留旧标量表示（§7.1、§7.4）；类型元数据和生成 IR 的规模不得随数组长度逐元素增长。

F0 冻结局部布局、IR 活跃值、任务预算与私有调用约定；F2a 实现分配策略与诊断。
不直接在机器栈上放入任意大的数组，也不因新类型随意调大现有编译器限额。
需要动态物化的存储计入任务预算，并在作用域退出时回收；解释器暂停/取消仍须保活和清理。
按值实参、返回值和赋值所需临时存储的同时存活峰值必须计费。

语言 Copy 能力与物理存储的回收责任分开：复制完整值必须得到独立内容；编译器管理临时存储
不使数组成为语言拥有型值，也不为 Copy 数组调用用户 Drop。以下责任表的实现映射冻结于 §7.4：

| 存储位置 | 回收与复制要求 |
| --- | --- |
| 局部物化值、参数副本 | 由所属调用/作用域管理，正常退出、提前返回、失败和解释器取消均只回收一次 |
| 返回暂存、赋值暂存 | 明确创建、提交与失败回收者；返回后不得依赖已退出的被调方存储，不能浅拷贝内部地址 |
| 聚合字段、Array 元素 | 内联字节由外层存储管理，字段不独立释放；读取出的按值副本有独立存储 |
| 元素引用、切片 | 只借用原存储；视图描述符的分配/回收单独计费，不承担载荷释放责任 |

自赋值、源/目标可能重叠的合法复制与赋值求值失败按 §7.4 处理，不能提前释放仍需读取的源。
重复循环构造与覆盖必须证明预算可复用，不能仅在根任务结束时核对资源归零。

### 2.3 初始化、复制和访问

没有隐式未初始化构造，也没有“全零字节就是任意 T 的默认值”的规则。
首版提供以下能力；准确签名、构造原语与诊断分别冻结于 §7.2、§7.4 和 §7.6，算法放在 Joky 库中：

| 能力 | 目标行为 |
| --- | --- |
| `repeat::<T, N>(value)` | T 为 Copy；表达式求值一次，复制为 N 个元素；N=0 仍求值一次 |
| `from_fn::<T, N>(init)` | 接收同步 FnMut(UInt64) -> T，按索引从 0 到 N-1 各调用一次；N=0 不调用回调体 |
| `len` / `is_empty` | 返回 UInt64 / Bool，不消费数组 |
| `get` / `set` | 沿用 Array 的 Option / Bool 越界规则，set 需要独占访问 |
| `borrow_ref` / `borrow_mut_ref` | 返回元素引用；越界产生受检运行时错误 |
| `slice` / `slice_mut` | 借用半开区间，检查 start <= end <= N，允许合法空区间 |
| 整体赋值、参数和返回 | Copy 数组复制元素值；结果不依赖原变量后续修改 |

F0–F5 首版不引入数组字面量；后续已支持上下文定型的 `[a, b]`，见[数组字面值](language.md#数组字面值)。from_fn 复用[已有同步闭包能力](language.md#同步匿名函数与闭包)，
接受满足 FnMut 的函数指针或闭包，按值接收 callable 并在内部可变绑定；不接受仅满足 FnOnce
的回调或挂起回调。隐藏 callable 类型参数的准确声明与推导见 §7.2，沿用既有末尾 callable
参数省略规则，不要求调用者显式命名闭包环境类型。
回调表达式及捕获初始化仍只求值一次；N=0 不调用回调体，但仍结束按值传入环境的清理责任。
正常完成、回调失败、预算失败和解释器取消均清理环境及部分构造存储，保留首错；不得重放回调，
不得让回调访问尚未发布的数组。捕获已有类型沿用闭包规则；捕获 FixedArray 本身的边界见 §2.6。
构造期间不能把部分初始化数组发布成普通值；复制不能读取未初始化的语言值，不能暴露 padding 内容。

### 2.4 借用与视图

元素引用和切片的来源必须指向实际数组及其所在位置，而非临时包装对象。
首版保守地借用整个数组：共享借用期间禁止写入，独占借用期间禁止冲突访问；借用存活时
禁止整体替换、销毁或使原存储失效。不提前增加基于常量下标的借用拆分。
从记录字段或 `Array<FixedArray<T, N>>` 派生的视图还必须约束相应外层拥有者，禁止外层扩容、
移除和替换；不能因为 FixedArray 自身不扩容就丢掉这一层来源。

沿用 `&Slice<T>` / `&mut Slice<T>` 的公开语义，推广底层连续视图对存储来源的表达。
不能为了复用仅面向 Buffer 的代码，临时制造具有虚假释放责任的 Array/Buffer 控制块。
视图本身可有内部描述符，但不属于 FixedArray 值布局；返回来源、noescape 检查、错误传播、
解释器暂停和退出清理都必须保留原存储的有效期。禁止局部视图逃逸及超出当前支持范围的挂起借用。

### 2.5 C 数组布局与调用边界

只有 T 递归满足 C 兼容布局，且 N>0、T 非零大小时，FixedArray 才能进入首版 C 边界。
首版复用当前定宽整数（Int8/16/32/64、UInt8/16/32/64）、Float32 / Float64、已验证的
`#repr(C)` 记录，并递归支持这些类型的数组。当前 is_c_value 已接受 UInt8、其他窄整数和浮点；
F4 新增的是数组组合与头文件声明符验证，不重复立项标量支持。浮点数组与含浮点字段的记录数组
均须覆盖。Bool、Rune、String、普通枚举、Ptr/引用字段和动态容器不自动获得 C 兼容性。
FixedArray 的专用数组布局也不意味着开放任意泛型 #repr(C) 记录；其受信任类型身份见 §7.2，
F4 递归验证元素与维度，保留普通 C 记录的现有限制。

| Joky 目标场景 | C 声明或规则 |
| --- | --- |
| `#repr(C)` 字段 `data: FixedArray<UInt8, 16>` | `uint8_t data[16]` |
| 字段 `matrix: FixedArray<FixedArray<Int32, 3>, 2>` | `int32_t matrix[2][3]` |
| 参数 `&FixedArray<Int32, 4>` | `const int32_t (*values)[4]` |
| 参数 `&mut FixedArray<Int32, 4>` | `int32_t (*values)[4]` |
| 接收 `const int32_t *` / `int32_t *` 的现有 API | 首版由 C 包装函数接收上述数组引用，再显式传首元素指针 |
| 按值传入/返回数组，或按值传入/返回含数组记录 | 首版拒绝，需独立验证目标平台聚合调用 ABI |

C 中 `void f(int32_t values[4])` 的数组参数会被调整为 `int32_t *`，并不是四元素按值参数，
也不由该类型替调用者检查长度；数组对象布局相同不代表调用约定相同。
依据见 [WG14 C 标准草案 N3220](https://www.open-std.org/jtc1/sc22/wg14/www/docs/n3220.pdf)
的函数声明符规则。Joky 不做隐式数组到首元素指针退化。

首版优先使用小型 C 包装函数：包装函数接受 `T (*)[N]`，调用真实 API 时显式传 `*values`，
长度参数按真实 C 类型受检转换。固定长度可以由包装函数提供，动态有效长度仍需检查不超过 N；
这不要求同时放宽现有 C FFI 的所有数值参数组合。包装接口仍走现有 unsafe C 注册和同步借用契约。
数组引用不隐式变成指针/长度二元组，也不凭地址相同就把不同 C 原型相互转型调用。

外部写入只允许发生在已经初始化、可独占访问的 C 兼容数组内。失败时不承诺撤销 C 已执行的
合法写入；越界写、错误原型和调用后保留指针仍违反 unsafe 绑定契约，布局检查无法证明这些行为安全。
首版不自动接入跨 Pending 借用协议。

头文件生成器要按 C 声明符结构渲染字段、嵌套数组与指向数组的指针，不能简单拼接类型名。
延续当前 size/alignment/offsetof 静态断言，增加数组大小、元素步长和嵌套维度验证。
零长度数组不映射成 C 编译器扩展，flexible array member 也不以 N=0 表示。

`FixedArray<UInt8, N>` 只是字节存储，不自带 UTF-8、NUL 或有效长度保证。
向 String 转换必须按真实已写长度显式校验；不能扫描到容量之外，也不能修改已有 String 载荷。
S7 的 TextFfiV1 继续遵守自己的文本输入/输出协议，不因新增 FixedArray 就隐式接受它。

### 2.6 使用位置与组合准入

以下为 F5 验收后的首版支持矩阵；各项准入检查见 §7.5，测试证据见 F5 完成记录。
无引用 Copy 是元素的必要条件，不代替存储、调用或外部寿命资格检查。

| 使用位置 | 首版目标与验收阶段 |
| --- | --- |
| 同步局部、直接函数参数/返回、整个数组的同步引用 | F2a；元素范围先覆盖 §7.2 的非数组无引用 Copy 类型；元素引用不随之开放 |
| 同步函数值间接调用 | F2b；含数组的参数/返回与直接调用使用同一值语义，函数值不因此获得宿主 ABI |
| Copy 记录、元组、Option / Result、嵌套 FixedArray | F2b；构造、匹配/解构、字段操作与分支合并均保持紧凑表示 |
| Array<FixedArray<…>> | F2b；内联元素、复制取出、扩容和释放；元素/切片借用由 F3 验收 |
| 同步元素引用与 Slice | F3；来源覆盖实际存储与外层拥有者，不能返回局部视图 |
| #repr(C) 数组字段、同步 C 数组引用 | F4；按 §2.5，不能按值跨 C ABI |
| 普通 alloc、graph、Arena.alloc | 首版拒绝含数组的载荷，另行验收分配与引用寿命，不因 Copy 自动放行 |
| 闭包借用/move 捕获 FixedArray 或含数组聚合 | 首版拒绝；from_fn 捕获其他已支持类型不受此新增限制影响 |
| 其他拥有聚合、List/Map/Set 中的数组存储 | 首版拒绝；Array 外层同样不得通过这些包装进入未验收位置 |
| 持久实例、状态域、快照/候选、迁移 | 首版拒绝递归含数组的状态及尚未支持的边界输入/结果；不自动获得序列化或迁移兼容性 |
| 语言 CPS 帧、parallel/race、宿主跨 Pending 保留 | 首版拒绝数组及其包装值/视图；已支持的同步 helper 可在挂起点之间完成内部数组操作 |
| 普通宿主 direct/CPS、文本适配、VM 公开值接口 | 不自动开放数组参数/结果，继续使用各自准入规则 |

所有已开放入口的类型等价、泛型实例标识和代码快照匹配均包含规范化长度与元素类型。
后续开放持久化或迁移时必须同样检查，不能仅比较总字节数；旧代码保活不等于新旧布局兼容。
Copy 元素白名单与 Eq / Hash / Show 等能力的准入见 §7.2，不能从“可存储”推导已支持
任意 trait；尚未开放的组合须在共享语义检查中递归拒绝，覆盖别名、泛型与跨模块包装。

## 3. 分阶段实施

主依赖：`F0 → F1 → F2a → F2b → F3 → F4 → F5`；F2a / F2b 合称 F2。
每个实现子阶段单独提交基本失败行为与三后端测试，
不能把内存安全和预算计费推迟到 F5。F0–F5 已完成；后续工作不得隐式扩大本矩阵。

### F0：冻结表示、常量参数和支持矩阵

核对 parser/type walk、泛型替换和实例缓存、memory_layout、IR 局部存储、借用来源、
native/interpreter 直接与间接调用、清理和 C 头文件的实际接点；按 §2.6 核对新增能力的拒绝入口。
冻结常量参数整数类别、库模块名 `joky/fixed_array`、接口拼写、首版元素范围、
长度/字节/深度上限、局部物化策略和私有按值调用约定，更新第 2 节及 syntax.md。
from_fn 的 FnMut 签名、常量参数与 callable 推导组合、受信任数组类型身份均须在进入 F1 前明确。

**验收：** 给出包含嵌套数组和零大小元素的布局算例、32/64 位目标的上限检查方案、
大数组及其外围聚合不逐元素展开的 IR/存储方案，以及具体存储回收和借用责任表。
列出标量展开路径的改造清单、§2.6 的准入检查位置、F2a / F2b 各自开放与拒绝的组合。
若引入可执行探针，按测试范围验证；
该阶段不能把仅有 sizeof 查询当作可执行数组已经实现。

**完成记录（2026-10-18）：** 对照 `1ee7fad` 完成第 7 节源码盘点、目标接口、紧凑存储/调用
约定、限额、布局算例、回收和准入检查设计，并同步 syntax.md。此次仅文档冻结；未新增探针，
未实现新类型或执行数组代码。验证范围与尚未验证内容见 §7.7。

### F1：常量参数与定长类型

增加常量参数声明/实参表示、解析、作用域、规范化、替换、推导和实例标识。
增加紧凑数组布局节点，接入所有类型遍历及递归布局检查；允许普通别名、函数、记录和固有 impl
使用受支持常量参数。泛型实例和布局限额在申请大容器或展开前检查。

**验收：** N 不同类型拒绝混用；同值字面量归一；常量参数转发、别名和模块导入正常；
拒绝负数、超范围实参、运行时长度、模块/关联常量实参、未支持常量运算和递归内联布局；零长度、嵌套维度及
零大小元素不能绕过限额。以返回常量参数值及常量参数转发的泛型函数执行三后端测试；
尚无执行路径的数组操作明确拒绝，不输出不完整机器码。

**完成记录（2026-10-18）：** 已实现带种类的有序实参、独立常量声明身份、UInt64 字面量规范化、
函数/记录/别名/固有 impl 的替换和结构推导；实例键、模块导入、函数取值和领域状态描述符保留常量。
可信内嵌别名安装专用数组类型，数组及外层聚合使用紧凑元数据，完整值逻辑预算包含枚举标签；
零长度、零大小元素、phantom 参数、共享类型 DAG、递归依赖及形状缓存分别受检。
共享前端拒绝所有尚未支持的数组值操作，标量内存展开也不接受数组标记。
关联常量仍沿用声明阶段求值，不支持依赖 const 参数；F1 明确拒绝此组合，读取 N 使用普通方法。
三后端共用 `const_generics.jk` 与独立模块 fixture，布局验证覆盖 §7.3 两种目标模型。
此处为 F1 的历史记录；后续 F2a 已替换独立数组的执行门禁，完成记录见下节。

**验收记录：**

- `cargo test --lib fixed_array_f1 -- --nocapture`：8 项通过，包括 JIT/解释器执行、独立 AOT
  链接运行、布局/限额/递归/非法用例及常量实参元数据往返。
- `./scripts/check.sh`：默认 Debug 检查通过，包含格式、Clippy、默认测试和三后端 while/Map
  短输入正确性；未做计时。
- 最后的实参种类与引用别名检查调整后，复跑 `cargo fmt --check`、
  `cargo clippy --all-targets -- -D warnings` 和 `cargo test generic`，分别覆盖 95 项库测试、
  9 项 CLI 测试与 8 项模块测试，均通过。
- `cargo test --test cli exhaustive_generic_map_cli_type_matrix -- --ignored`：额外泛型 CLI
  类型矩阵通过；补跑原因是共享泛型实参表示影响已有 Map 专化，未扩大到无关所有权/取消穷举。
- 新增本地文档链接/锚点及 `git diff --check` 通过。未运行 Release、全量 stress 或性能测量；
  32/64 位仅验证布局输入模型，不代表在两种架构上执行了数组机器码。

### F2a：独立数组的连续存储与值语义

先实现非数组无引用 Copy 元素的 repeat/from_fn、受检 get/set、整体复制和同步直接参数/返回。
Copy 元素不需要 C 兼容，但必须属于 F0 白名单。物化、复制、初始化和作用域退出共用受计费
存储原语，避免逐元素 IR 展开和无上限栈分配；嵌套与外层聚合等 F2b 组合在本阶段明确拒绝。

**验收：** 三后端执行 N=0/1/常规长度/限额边界、零大小元素、重复值构造、回调顺序、
复制后独立修改、直接按值实参与返回、自赋值及循环预算复用。from_fn 覆盖函数指针、Fn/FnMut
捕获、N=0 环境清理与失败/取消，拒绝仅 FnOnce 或挂起回调。失败初始化不能重放回调或发布
半成品；预算不足、提前返回、赋值失败和解释器取消后临时存储/回调环境只清理一次。
递归拒绝拥有型/含引用元素及 §2.6 未开放位置，不能因编译器代管存储而改变语言 Copy 语义。

**完成记录（2026-10-18）：** 已实现上述 F2a 范围，包含整个数组的同步引用、解引用读写及
mem.replace/mem.swap。公开接口由 [fixed_array.jk](../src/stdlib/fixed_array.jk) 实现；
[fixed_arrays.rs](../src/core/ir/fixed_arrays.rs) 提供显式位置操作和紧凑复制描述，
[place.rs](../src/runtime/place.rs) 为载荷、初始化前缀与可复用清理登记统一计费和回收。
JIT/AOT 共用原生 lowering，解释器共享类型布局并保留独立 Place 值标签；返回区先接收完整副本，
被调者清理成功后再发布。数组不再进入标量展开；N=1 与 N=65,536 的描述/IR 规模相同。
以上为 F2a 的历史记录；聚合嵌入/嵌套/间接调用和元素引用/切片分别在下节 F2b、F3 开放，C 数组 ABI 留给 F4。

**验收记录：**

- `cargo test --lib fixed_array`：17 项通过、1 项穷举按默认忽略，覆盖 F1 布局回归及 F2a。
  [行为 fixture](../tests/fixtures/fixed_array_f2a.jk) 在 JIT、解释器和独立 AOT 执行，
  覆盖零长度/零大小/长度上限、有限聚合元素、复制/替换/交换、直接调用、回调及循环复用。
  [清理 fixture](../tests/fixtures/fixed_array_f2a_cleanup.jk) 验证回调失败、空环境、提前退出、
  赋值失败及清理失败不发布结果；VM 预算扫描和运行时测试检查释放、登记回滚与失败转移。
- `cargo test --lib exhaustive_fixed_array_f2a_cancellation -- --ignored`：逐指令取消通过，
  检查回调不重放、环境至多清理一次且任务 arena 归零。
- `cargo test --lib exhaustive_closure_interpreter_cancellation_every_instruction -- --ignored`
  与 `cargo test --lib exhaustive_cps_local_borrows_cancel_at_every_instruction -- --ignored` 均通过；
  补跑原因是数组调用涉及共享实参临时值、匿名闭包借用结束和清理帧路径。
- `./scripts/check.sh`：默认 Debug 格式、Clippy、测试及三后端短输入基准正确性通过。
- 本地文档链接及 `git diff --check` 通过。未运行 Release、全量 stress 或性能测量；
  32/64 位仍仅验证布局模型，未宣称跨架构数组执行或 C 数组互操作通过。

### F2b：聚合嵌入、嵌套数组与动态数组组合

接入 Copy 记录、元组、Option / Result、嵌套 FixedArray、Array 元素及同步函数值间接调用。
含数组的构造、匹配/解构、字段读写、分支合并、复制与返回复用紧凑存储路径；外围聚合不得
重新展开数组元素。元素字节由外层存储管理，取出的按值副本独立，外层搬迁不能留下临时地址。

**验收：** 三后端覆盖多层嵌套、Option<FixedArray<…>>（含嵌套数组 get 的返回）、Result 的
两个分支、记录/元组字段、分支结果及直接/间接调用；执行 Array<FixedArray<…>> 的读取、
覆盖、扩容、移出与释放。断言副本独立、预算峰值及失败回收；通过结构检查确认类型元数据和
生成 IR 不随 N 逐元素增长，不以性能计时充当正确性验收。F2a 与 F2b 均完成后才标记 F2 完成。

**完成记录（2026-10-18）：** 已实现上述 F2b 范围。含数组的 Copy 聚合整体使用 Place，
构造、字段读写和模式投影按紧凑布局的字节偏移执行；match 保活被匹配值直到判别及绑定结束。
同步函数值与 callable 返回数组/含数组聚合时复用调用者返回区；嵌套 from_fn 保留回调顺序及清理责任。
动态 Array 内联保存完整元素，通过相同复制描述读取独立副本，移出前准备结果存储；扩容沿既有
Buffer 协议搬迁。解释器位置与 Buffer 操作共享按类型缓存的复制描述，原生描述仍可重定位。
可信 Array 类型身份在源码绑定时安装，其他拥有包装、List/Map/Set 和闭包捕获继续拒绝；本阶段未开放的元素/切片借用见下节 F3，C 数组 ABI 留给 F4。

**验收记录：**

- `cargo test --lib fixed_array`：23 项通过、2 项穷举默认忽略，包含 F1/F2a 回归及 F2b。
  [组合行为 fixture](../tests/fixtures/fixed_array_f2b.jk) 在 JIT、解释器及独立 AOT 执行，
  覆盖多层嵌套、零大小/边界长度、记录/元组/Option/Result、模式与分支、整体替换/交换、
  直接/间接调用、回调返回含数组值，以及 Array 的读取/覆盖/扩容/移出/释放。
- [失败与取消 fixture](../tests/fixtures/fixed_array_f2b_cleanup.jk) 覆盖嵌套初始化失败及拥有回调清理，
  VM 按预算扫描失败点，确认回调不重放、环境最多清理一次、任务 arena 归零；1,000 次循环在
  固定 32 KiB 预算内复用数组聚合及 Array 元素存储。AOT 独立链接运行正常与失败路径。
- 聚合描述、IR 操作数与槽数在长度 1 / 8,192 的嵌套形状下相同；拒绝矩阵覆盖
  get/Option 返回包装工作量超限、浅名义引用物化后的资格、长度不匹配和未开放存储/借用位置。
- `cargo test --lib exhaustive_fixed_array -- --ignored`：F2a/F2b 逐指令取消均通过。
  共享模式作用域及 callable 路径受影响，另跑
  `cargo test --lib exhaustive_closure_interpreter_cancellation_every_instruction -- --ignored` 和
  `cargo test --lib exhaustive_cps_local_borrows_cancel_at_every_instruction -- --ignored`，均通过。
- `./scripts/check.sh`：默认 Debug 格式、Clippy、测试及三后端短输入基准正确性通过。
  最后的解释器复制描述共享调整后，复跑 fixed_array、buffer_storage、数组取消穷举、格式及
  `cargo clippy --all-targets -- -D warnings`，均通过。
- 本地文档链接/锚点及 `git diff --check` 通过。未运行 Release、全量 stress 或性能测量；
  未验证实际 32 位数组执行，未开放元素视图或 C 数组 ABI。

### F3：元素借用与共享切片能力

接入 borrow_ref/borrow_mut_ref、slice/slice_mut 和现有 Slice 操作，保留外层数组/记录来源，
支持已有 noescape 的通用传递规则。重构共享视图设施时覆盖原动态 Array 和 String 字节视图。

**验收：** 三后端覆盖读取、独占写入、空切片、越界失败、借用结束后的整体赋值；
负例覆盖局部视图返回、借用期间替换/销毁、外层容器扩容及冲突读写。
模块别名、普通方法、函数值和泛型 helper 的来源传播一致；解释器时间片暂停/取消无悬空视图。

**完成记录（2026-10-18）：** 四个入口及同名固有方法已接入 JIT、解释器和原生 AOT。
`PlaceOp::View` 从原数组位置受检投影元素或创建共享 Buffer 视图头部，不为数组制造拥有型包装。
记录引用上的数组字段直接按布局投影，返回视图保留原记录来源；类型检查区分借用型目标与内联布局。
同一数组所有索引/区间共用借用范围，保留父 Buffer 的 storage/interior loan；空区间与零大小元素也不例外。
数组引用及其普通标量投影不能被保留到任务 arena，helper 不能通过返回普通 `&Int32` 擦除存储限制。
函数值继续使用保守返回来源；数组引用本身不可存储，间接调用不把它误判为任务级保留。
已开放动态 Array 含数组元素的引用、Element 及 Slice 操作，沿用既有共享视图与 noescape 规则。

**验收记录：**

- `cargo test --lib fixed_array`：29 项通过、3 项穷举默认忽略，包含 F1/F2 回归。
  [视图行为 fixture](../tests/fixtures/fixed_array_f3.jk) 在 JIT、解释器和独立 AOT 执行，
  覆盖读取/独占写入、嵌套数组、带对齐字段的记录、借用枚举模式、原位置方法投影、
  模块别名/函数值/泛型 helper/noescape、所有现有 Slice 操作、视图结束后的整体赋值和父 Array 扩容。
  同一 fixture 回归动态 Array 与 String 字节视图；空数组、零大小元素和合法空区间均执行。
- 越界索引（含 UInt64 最大值）、反向区间、过大末尾及零大小元素越界产生受检错误；VM 检查诊断和 span，
  独立 AOT 检查错误传播和清错后继续调用。
- 借用负例覆盖局部/按值参数返回、记录字段与间接返回的来源、冲突读写/复制/替换、空切片限制、
  父 Array 扩容/覆盖/移出/释放，以及通过标量引用或 helper 保留数组存储；未开放 F4 等位置仍拒绝。
- [清理与预算 fixture](../tests/fixtures/fixed_array_f3_cleanup.jk) 扫描预算失败点，
  核对视图保活、拥有者清理一次、回调不重放及任务 arena 归零。64 次构造 1,024 项数组并切片在
  64 KiB 预算内完成，确认载荷及时复用；1,000 次元素引用不积累头部，1,000 次切片在该预算下
  受检失败、默认预算下成功，确认头部单独计费并在根任务退出时全部释放。
- `cargo test --lib exhaustive_fixed_array -- --ignored`：F2a/F2b/F3 逐指令取消全部通过。
  共享视图与字段借用路径受影响，另跑
  `cargo test --lib exhaustive_async_owner_views_cancel_every_instruction -- --ignored` 和
  `cargo test --lib exhaustive_cps_local_borrows_cancel_at_every_instruction -- --ignored`，均通过。
- `./scripts/check.sh`：默认 Debug 格式、Clippy、默认测试及三后端 while/Map 短输入基准正确性全部通过。
  包含既有动态 Array、拥有型元素/Slice、String 字节视图与 CLI/独立 AOT 回归。
- 433 个本地文档链接/锚点及 `git diff --check` 通过。未运行 Release、全量 stress 或性能测量；
  未验证实际 32 位执行。C 数组引用/字段仍待 F4，模型集成验收仍待 F5。

### F4：C 字段、数组引用和显式指针适配

复用已有整数/浮点 C 布局并递归扩展到数组，接入同步数组引用注册、原生 lowering、解释器调用和
头文件生成。真实首元素指针 API 使用明确包装，保留准确原型及同步有效期。
补齐 C 读/写包装示例；字节数组示例分别记录容量、有效长度和文本编码，不把它自动解释成 String。

**验收：** cc 编译独立 fixture，对照 size/alignment/offsetof/stride 及嵌套维度；
JIT、解释器、独立 AOT 均验证记录内数组、只读引用、独占写入、pointer-to-array 包装、
首元素指针转换、全部定宽整数、Float32/Float64 及含浮点字段记录的数组、非 ASCII 字节和有效长度检查。
拒绝不兼容元素、零长度/零大小元素、错误维度、可变性不匹配、按值 C 参数/返回及异步保留声明。
确认调用无隐式动态数组包装或复制；C 的错误返回与借用结束不破坏后续语言访问。

**F4 完成记录（2026-10-18）：**

- C 准入递归覆盖非空数组、全部八种定宽整数、Float32/Float64、已验证 C 记录及嵌套维度。
  未扩展普通泛型 C 记录；空数组、零大小/不兼容元素、按值参数/返回及跨 Pending 保留仍拒绝。
  异步借用检查解析浅名义记录后再检查数组字段，防止用外层记录隐藏未开放载荷。
- 注册签名支持数组内命名 C 记录，加载时递归解析并核对实际源码签名。
  即使总字节数相同，维度换序、元素名义身份或可变性不同仍拒绝；加载失败保留已加载模块。
  普通 direct/CPS 与 TextFfiV1 未扩展，`register_c_fn` 的标量结果矩阵仍为整数、Ptr、Unit。
- 原生和解释器复用已有单指针引用通道，无新增返回约定、Buffer 包装或隐式数组复制。
  结构化头文件生成区分数组字段、嵌套维度与 pointer-to-array，同时补齐窄整数和浮点类型名，
  生成 sizeof/alignof/offsetof 以及数组 stride/各维长度断言。
- [Joky fixture](../tests/fixtures/fixed_array_f4.jk) 与 [C fixture](../tests/fixtures/fixed_array_f4.c)
  经 `cc -std=c11 -pedantic-errors -Wall -Wextra -Werror` 编译，JIT、解释器及独立 AOT 均执行：
  记录内数组、数组内含数组记录、浮点字段记录数组、全部整数宽度与浮点、共享读和独占写；
  地址比较验证无副本，C 首元素读/写包装检查容量并显式转换到 `size_t`。
  错误状态后的合法写入可见，借用结束后可修改或整体替换；非 ASCII 字节分别验证容量、有效长度、
  UTF-8 前缀转换、超长和截断拒绝。解释器逐指令暂停恢复及选定取消边界均回收全部 arena。
- `cargo test --lib fixed_array`：32 项通过、3 项穷举默认忽略；`cargo test --lib ffi`：18 项通过
  （两组包含重叠测试）。F4 三项专项覆盖三后端行为、拒绝矩阵与注册精确匹配。
- `cargo test --lib exhaustive_fixed_array -- --ignored`：F2a/F2b/F3 三项逐指令取消通过；
  另运行共享拥有者视图与 CPS 局部借用取消穷举，各一项通过，核对同步/异步引用边界回归。
- `./scripts/check.sh` 默认 Debug 检查通过：格式、Clippy、库测试 1,445 项通过（98 项忽略）、
  CLI 268 项通过（5 项忽略）、其余集成测试，以及 while/map 三后端短输入基准正确性检查。
  文档本地链接与 `git diff --check` 通过；未运行 Release、全量 stress 或性能计时，未验证实际 32 位执行。
  F5 的集成模型与完整首版矩阵验收尚未开展。

### F5：集成验收与文档发布

用固定种子与独立模型核对构造、复制、替换、切片和 C 往返；每轮核对内容、资源计数与预算复用。
保留短边界回归；需要的长模型或取消穷举单列 ignored exhaustive 测试。
按第 4 节完成默认 Debug 检查与受影响的 Debug 穷举，补齐语言文档、架构不变量、Rust/C
示例及 README 能力入口。阶段完成与文档发布不自动触发 Release 发布验收。

**完成条件：** F0–F4 全部验收，三后端无未解释差异、泄漏或既有视图回归；
§2.6 所有开放项有行为证据、拒绝项有递归包装负例，首版范围与限制文档准确。
性能计时按 benches/README.md 独立执行，若未测量则明确记录，
不能把正确性检查写成性能改善。

**F5 完成记录（2026-10-18）：**

- [独立 Vec 模型与驱动](../src/backends/aot/tests/fixed_array_model.rs) 使用固定种子
  `0`、`1`、`0x55aa01239876fedc`、`u64::MAX`，生成操作参数和逐轮期望值。
  [Joky 执行体](../tests/fixtures/fixed_array_f5.jk) 与 [真实 C 检查点](../tests/fixtures/fixed_array_f5.c)
  在 JIT、解释器及独立 AOT 执行同一语料：默认每种子 48 轮，长模型每种子 1,024 轮，
  每个后端每种子重复两次。前 12 轮强制覆盖所有操作，后续由种子决定操作与区间。
- 每轮比较当前值和独立副本的全部 16 个元素、C 写入次数、命令次序及 Drop 次数。
  操作包含 repeat/from_fn、复制/恢复、受检 set（含越界无修改）、空/满/子区间切片、
  swap/replace、嵌套记录和数组、Array 元素借用后扩容、Option/Result 与函数值间接调用，
  以及首元素指针 C 包装的原地旋转/写入和错误状态后的继续访问。
- VM 短模型限制为 64 KiB、长模型为 512 KiB；每轮按值复制 1 KiB scratch，
  长模型仅 scratch 累计载荷就达 1 MiB，超过预算但按作用域复用可通过，切片头部仍按根任务计费。
  两个 VM 扫描分配预算失败点并验证后续调用可恢复、清理不重复、arena 归零。
  解释器以单指令预算恢复完整语料，默认抽取宿主事件/清理附近边界，ignored 用例逐指令取消。
  AOT 核对相同内容和 Drop 账本；低预算扫描使用 VM 测试入口，不宣称 AOT 暴露可调任务预算接口。
- 递归包装验收发现 `graph.alloc` 遗漏含数组 Copy 记录的拒绝检查；已在共享前端补齐。
  本模块和跨模块别名/泛型/Option 包装均在 JIT、解释器、AOT lowering 前以带 span 的类型错误拒绝。
  其他关闭边界的递归负例与状态/迁移保护见下表；迁移候选拒绝不改变旧版本、schema 或资源计数。
- 新增 [Rust 注册示例](../examples/fixed_array_ffi.rs)、[Joky 调用示例](../examples/fixed_array_ffi.jk)
  和 [C 包装示例](../examples/fixed_array_ffi.c)。Rust 示例执行两个 VM 并可生成头文件/对象；
  C 示例独立联编运行，保留 pointer-to-array 原型、长度检查和同步有效期。
- `cargo test --lib fixed_array_f5 -- --include-ignored`：最终 11 项通过，含长模型、逐指令取消、
  跨模块拒绝及实际迁移候选保护。Rust 示例在 JIT/解释器均返回 42，额外生成头文件/对象并与 C 示例
  及同一 Debug 运行时联编执行通过。
- `cargo test --lib exhaustive_fixed_array -- --ignored`：5 项通过；共享拥有者视图与 CPS 局部借用
  逐指令取消穷举各 1 项通过。`cargo test --lib arena_cycles`：6 项通过，核对既有 graph 行为。
- `./scripts/check.sh` 默认 Debug 检查通过：格式、Clippy、库测试 1,452 项通过（100 项忽略）、
  CLI 268 项通过（5 项忽略）、其余集成测试及 while/map 三后端短输入正确性。
  此后仅补充跨模块/迁移负例和取消边界采样，已由上述最终 F5 专项覆盖，并复核
  `cargo fmt --check` 与 `cargo clippy --all-targets -- -D warnings`，均通过。
- 本地文档链接/锚点及 `git diff --check` 通过。
  未运行 Release、全量 stress 或性能计时；未执行实际 32 位目标，不把正确性结果解释成性能改善。

| 首版位置 | 验收证据 |
| --- | --- |
| 同步局部、完整初始化、整体复制、直接参数/返回、整数组引用 | [F2a 行为/失败](../tests/fixtures/fixed_array_f2a.jk)、[清理](../tests/fixtures/fixed_array_f2a_cleanup.jk)、F5 模型及预算扫描 |
| 间接函数调用、Copy 记录/元组/Option/Result、嵌套数组 | [F2b 行为](../tests/fixtures/fixed_array_f2b.jk)、[失败清理](../tests/fixtures/fixed_array_f2b_cleanup.jk)、F5 模型 |
| Array 内联数组元素、视图与外层拥有者 | F2b 行为、[F3 视图](../tests/fixtures/fixed_array_f3.jk)与[预算/取消](../tests/fixtures/fixed_array_f3_cleanup.jk)、F5 模型 |
| C 字段、数组引用、显式首元素指针与字节文本边界 | [F4 Joky](../tests/fixtures/fixed_array_f4.jk) / [C](../tests/fixtures/fixed_array_f4.c)、F5 模型与 Rust/C 示例 |
| alloc、graph、Arena.alloc；借用/move 捕获 | [F5 递归包装及跨模块负例](../src/vm/tests/language/fixed_array_tests.rs)，graph 三入口诊断回归 |
| List/Map/Set、其他拥有聚合及 Array 外层包装 | 同文件 F2a/F5 拒绝矩阵；Wrapper、Option、List<Array<Row>> 等组合不能隐藏数组 |
| 挂起函数、parallel/race、宿主跨 Pending 保留 | F1/F2a/F5 包装负例、[F4 异步 C/宿主声明拒绝](../src/backends/aot/tests/fixed_array_ffi.rs)；普通挂起函数可调用不携带数组跨入口的同步 helper |
| 持久实例、状态域、快照/候选、迁移 | F5 核对 persistent/schema/入口资格、#state 拒绝、create_instance/create_domain 拒绝、迁移候选失败后旧状态与资源不变 |
| 普通 direct/CPS、文本适配、VM 公开值接口 | F4 注册拒绝、F5 包装 extern/公开调用拒绝，含数组入口不导出到独立 AOT；既有文本 FFI 回归保持 |
| 元素、零长度、零大小、限额和借用来源 | [F1 类型矩阵](../src/core/types/fixed_array.rs)、F2a/F3 行为/拒绝矩阵；N=0 不绕过元素或来源检查 |

## 4. 验证范围与提交要求

以 [AGENTS.md](../AGENTS.md) 为准：纯计划/普通说明修改仅检查内容、相关链接和
`git diff --check`，不运行 Cargo 或检查脚本；新增可执行示例按代码改动验证。
仅增加独立 Rust 探针/测试时运行格式、Clippy 与受影响测试；涉及生成代码的行为覆盖三后端。
实际类型、布局、ABI、存储回收或共享视图设施改动先运行针对性行为测试，再运行
`./scripts/check.sh`；按影响补跑 FixedArray、共享视图、存储回收和取消路径相关的 Debug 穷举，
不自动运行全部压力检查，也不因阶段完成默认构建 Release。扩大检查须说明具体影响。
quick 与 full 是相同的默认 Debug 检查，无需对同一版本重复执行；仅更新文档不重跑代码检查。

仅在用户明确要求发布验收时运行 `./scripts/check.sh --full --release` 和
`./scripts/check.sh --stress --release`；需要 Debug 覆盖时另跑默认检查。性能测量或优化构建
特有问题可按 AGENTS.md 使用 Release 并说明原因，不能把普通 F5 完成解释成发布验收授权。

编译器/运行时实现必须执行生成代码；FFI 必须与真实 C fixture 联编，不能仅测试 Rust helper。
代码标识符、诊断和测试名用英文。每阶段更新实际状态、已执行检查与未验证项，再按
Conventional Commits 提交；未通过全部阶段验收不得标首版完成。

## 5. 可复用阶段任务

> 完成 docs/fixed-array-storage-ownership.md 的阶段【编号】。先核对前置阶段与当前实现。
> 保持内联布局、紧凑 IR、完整初始化、预算计费和来源准确的借用；不以动态 Array 包装冒充
> FixedArray，不把 C 数组布局等同于按值调用 ABI。覆盖 JIT、解释器和独立 AOT 的行为、
> 失败清理及非法程序拒绝。按 AGENTS.md 执行对应检查，更新阶段状态和所属文档并提交；
> 报告实现范围、检查结果、提交号及剩余限制。

## 6. 首版后的独立扩展

拥有型元素单独立项，先支持 String 和无引用拥有聚合：T 非 Copy 时数组非 Copy，按值操作
整体移动，元素逆序析构。from_fn 失败只析构已初始化前缀，每个元素恰好一次；构造进度属于
临时清理状态，不进入完整数组值布局。N=0 不产生元素析构，也不令非 Copy 元素类型自动变成 Copy。
需要先解决紧凑的逐元素清理、失败 Drop 保留首错、整体替换和解释器取消，再考虑容器嵌套。

数组字面值、下标读写/借用、具体固长解构及序列模式中的借用拆分已作为后续扩展实现，未改变 F0–F5 的元素和挂起边界，见[序列模式](language.md#序列与字面量模式)。
以下能力仍各自验收：含引用元素及其来源汇总、拥有元素部分移出、普通 API 的借用拆分、语言 CPS 与
宿主跨 Pending 保活、更多常量表达式、按值 C 聚合 ABI、VLA 和
flexible array member。它们均不由首版定长布局或 C 数组引用支持自动推出。
§2.6 暂拒绝的 arena 分配、数组捕获、其他拥有聚合/容器、持久实例与状态域/迁移同样独立验收。
混合 C 标量/指针参数、指针字段/输出参数和通用 C 回调属于独立 FFI 工作，不作为 F0–F5 前置条件。

## 7. F0 核对与冻结记录

本节以 `1ee7fad` 为源码核对基线。标为“现状”的内容来自源码静态核查；其余为后续阶段的
实现契约，不表示已有可执行支持。F0 不新增运行时或编译器代码。

### 7.1 源码接点与标量展开改造清单

| 接点 | 核对到的现状 | 冻结的后续改动 |
| --- | --- | --- |
| [parser/types.rs](../src/core/parser/types.rs)、[parser/ast.rs](../src/core/parser/ast.rs) | parse_type_parameters 只生成 TypeParameter；TypeApplication.arguments 是 Vec<Type>，实参统一调用 parse_type | F1 引入有类别的泛型参数/实参及 const 参数身份；不把数值伪装成 Type::Named |
| [types/specialization.rs](../src/core/types/specialization.rs)、[ir/generics.rs](../src/core/ir/generics.rs) | substitute/unify 与别名、记录、枚举、函数实例键均以类型实参为中心；函数泛型/闭包共享生成预算 | F1 将规范化 const 值或刚性 const 身份加入替换、推导、实例键与方法匹配；保留缓存和生成限额 |
| [types/walk.rs](../src/core/types/walk.rs)、[types.rs](../src/core/types.rs) | Storage 遍历部分依赖缓存的 scalars；Fields/Arguments 覆盖名义实参 | 新数组节点只遍历一次元素类型；含数组的聚合按字段遍历，不把私有地址槽当成全部类型内容 |
| [types/memory_layout.rs](../src/core/types/memory_layout.rs) | MemoryLayoutShape 无数组；memory_scalars/storage_scalars 将字段偏移配成逐标量列表 | F1 增加 Array(element, length, stride) 布局形状；F2 用有界共享布局描述执行复制/投影，数组路径不调用逐元素标量展开 |
| [ir.rs](../src/core/ir.rs)、[parser/ast.rs](../src/core/parser/ast.rs) | Slot::scalars、field_path、parameter_slots 与聚合缓存依赖平坦槽偏移 | F2 区分语言类型与执行表示，含数组值用独立存储位置；字段走字节投影，参数槽数不依赖 N |
| [ir/memory.rs](../src/core/ir/memory.rs)、[ir/cleanup.rs](../src/core/ir/cleanup.rs) | Local/cleanup_storage 按声明累加稳定存储，单函数 64 KiB；has_cleanups 主要面向拥有者 | 新增不带用户 Drop 的字节存储登记/结束路径，不伪造 Buffer 拥有者；保留原局部限额 |
| [ir/records.rs](../src/core/ir/records.rs)、[ir/tuples.rs](../src/core/ir/tuples.rs)、[ir/enums.rs](../src/core/ir/enums.rs)、[ir/patterns.rs](../src/core/ir/patterns.rs)、[ir/record_patterns.rs](../src/core/ir/record_patterns.rs) | 聚合构造、匹配与拆构使用现有槽/字段路径 | F2b 为含数组聚合改用位置构造、活动载荷投影及有类型复制；分支结果预留统一目的位置 |
| [native/lower.rs](../src/backends/native/lower.rs) | signature/lower_sync_call/lower_direct 展开参数；聚合返回缓冲区为每槽 8 字节，直接与间接签名分别生成 | F2 将含数组参数/结果路由到 §7.4 的字节位置调用描述；不能把旧聚合返回缓冲区当 C/数组布局 |
| [interp/memory.rs](../src/backends/interp/memory.rs)、[interp/execute.rs](../src/backends/interp/execute.rs) | Locals 用独立 arena 与 allocate_local；Frame::new/complete_aggregate 复制 Value 槽，内存指令 fields 由 storage_scalars 展开 | F2 新位置指令接入 Execution 的任务 arena；帧保存位置和初始化状态，时间片只恢复当前指令/构造进度 |
| [runtime/arena.rs](../src/runtime/arena.rs)、[runtime/cleanup.rs](../src/runtime/cleanup.rs) | allocate/release 已支持登记、稳定地址和退还计费；现有 Cleanups 项包含 Buffer 拥有者、钩子与其快照 | 复用任务分配/释放，扩展独立字节存储清理类别；失败登记不得遗留无人管理的分配 |
| [ir/buffers.rs](../src/core/ir/buffers.rs)、[interp/buffers.rs](../src/backends/interp/buffers.rs)、[runtime/buffer.rs](../src/runtime/buffer.rs) | Buffer 元素有 storage_layout；slice 建立不拥有载荷的 Buffer 头部 | F2b 增加紧凑元素读写，F3 复用 Slice 公开协议并加入数组存储来源，不能建立拥有载荷的假 Buffer |
| [ir/borrowing.rs](../src/core/ir/borrowing.rs) | refs 从标量槽提取引用/拥有者；来源有根、路径、storage/interior、loan 与 retained 信息 | 数组位置单独登记存储身份；借用、读写、整体替换与结束进入数据流，无引用 Copy 值也可能有活动借用 |
| [imports/sources.rs](../src/core/imports/sources.rs)、[sources/intrinsics.rs](../src/core/imports/sources/intrinsics.rs) | 受信任 SourceKind 绑定类型及原语身份，普通同名源码不获权限 | F1/F2 加入 §7.2 的数组定义与库身份，泛型实参校验按类别匹配 |
| [ffi.rs](../src/core/ffi.rs) | C 准入与注册、头文件生成独立；数组不在 is_c_value 范围内 | F4 在声明、注册、布局与声明符生成四处同时扩展数组，沿用同步借用契约 |

未含数组的类型保留现有标量槽表示与 ABI；不得为减少 match 分支而全局切换记录调用约定。
新增表示必须在优化器、活跃性、效果传播和错误清理中显式处理，不允许未适配路径落回
scalar_types 的默认单标量分支。F1 尚无执行存储时，所有需要数组值的路径都先给出诊断。

### 7.2 类型身份、接口与泛型冻结

公开模块为 `joky/fixed_array`，类型名 `FixedArray<T, const N: UInt64>`，不加入默认 prelude。
使用普通导入及固有 impl 提供方法；既有 Slice 类型仍从 `joky/slice` 使用。
计划新增受信任 `FixedArrayStorage` / `FixedArrayLibrary` 源码类别：私有
`joky/internal/fixed_array` 安装数组类型别名和存储原语身份，公开模块只重导出类型别名与安全库函数。
解析后规范化为专用 `Type::FixedArray`，携带元素类型及 const 长度；类型身份来自受信任定义，
不是判断字符串名，也不是带 N 个字段的普通泛型记录。别名透明，用户同名声明不获数组布局。

泛型规则冻结如下：

- 声明列表保留源码顺序，类型参数和 `const N: UInt64` 可以交错；合计最多 64 个。
  函数、记录、类型别名及固有 impl 支持 const 参数；枚举自身声明、trait 声明/trait impl 的
  const 参数不开放，已有 `Option<T>` / `Result<T,E>` 仍可用具体数组作为类型实参。
- AST 分别表示 Type 参数、Const 参数及其实参；const 参数使用独立声明身份，不借用名字或
  TypeParameter.id 的隐含类别。实参中的裸名字先保留待绑定形态，再按形参类别解析。
- const 实参为现有整数字面量的非负 UInt64 值或在作用域内的 const 参数；先检查 UInt64
  范围再检查数组限额，不接受负号、计算、转换调用、`_`、默认 const 值或模块/关联常量。
  相同数值的合法字面量规范化成同一 u64。刚性 N 与 M 只有声明身份相同时才等价。
- N 在值表达式中是只读 UInt64；实例化时替换为立即值。禁止形参/局部绑定再次遮蔽同一作用域
  的 const 参数；它可遮蔽外层模块常量，类型位置与值位置均绑定同一参数身份。
- 无显式实参时只从普通实参的精确类型结构统一长度；不解方程、不根据期望结果猜 N。
  使用显式列表时须给齐非 callable 参数，只有末尾 callable 参数可省略推导。泛型函数取值
  仍需给齐所有实参；实例键包含模板身份及带类别的实参，未使用的 const 形参也须验证。
- 透明别名只按展开后目标类型等价；未使用的 const 参数不人为制造新名义类型。记录实例仍保留
  全部泛型实参身份。替换/相等/符号分配、关联类型投影和模块限定都使用同一规范化规则。

首版元素白名单为全部定宽整数、Float32/Float64、Bool、Rune、Unit，以及递归由这些类型组成
的有限 Copy 记录、元组、枚举（含 Option/Result）。F2b 再加入嵌套 FixedArray。
拒绝 Ptr、函数值、opaque、引用、拥有值、闭包、dyn、资源及带自定义 Drop 的元素；包括零长度
数组及名义类型的泛型实参，不能用 phantom 字段绕过。刚性泛型暂存元素资格义务，具体化时
递归复核，不能把普通 T 的 Copy 默认约束解释为已证明整个白名单；不新增用户可实现的资格 trait。
数组自动获得 Copy，不自动获得 Eq/Hash/Show 或它们的 derive 支持；含数组的自动派生遇到缺失
能力时拒绝。普通用户方法/已支持的具体 trait 实现仍须按现有规则检查，不按字节比较 padding。

下表为目标签名记法，不是当前可编译示例；`A` 仅在表中缩写 `FixedArray<T,N>`，索引及长度均为 UInt64。

| 公开入口 | 冻结签名/返回与使用方式 |
| --- | --- |
| repeat | `repeat<T, const N: UInt64>(value: T) -> A` |
| from_fn | `from_fn<T, const N: UInt64, F: FnMut(UInt64) -> T>(init: F) -> A`；调用写 `from_fn::<T,N>(init)`，内部 `var` 持有 F |
| len / is_empty | `len(&A) -> UInt64` / `is_empty(&A) -> Bool`；也提供同名共享接收者方法 |
| get / set | `get(&A, index: UInt64) -> Option<T>` / `set(&mut A, index: UInt64, value: T) -> Bool` |
| borrow_ref / borrow_mut_ref | `borrow_ref(&A, index: UInt64) -> &T` / `borrow_mut_ref(&mut A, index: UInt64) -> &mut T` |
| slice / slice_mut | `slice(&A, start: UInt64, end: UInt64) -> &Slice<T>` / `slice_mut(&mut A, start: UInt64, end: UInt64) -> &mut Slice<T>` |

除构造器外均提供上述自由函数与同名固有方法；无 new 或默认初始化。F0–F5 之后已补充[字面值](language.md#数组字面值)和[下标运算符](language.md#数组与切片下标)。
get 越界返回 None，set 越界返回 false 且不改数组；普通实参仍按既有顺序求值，不能因越界省略
value 表达式。借用索引及切片区间错误为受检运行时错误；引用返回只依赖第一个数组实参。
F2a 支持整个数组的同步共享/独占引用以实现 get/set；投影元素引用和 Slice 留给 F3。

### 7.3 限额、布局与目标算例

新增限额是首版保守契约，不是测得的性能阈值；不提高既有槽、局部存储或生成预算。

| 项目 | 冻结上限与检查时机 |
| --- | --- |
| 单维 N | `0..=65_536`；具体化时检查，const 参数的数值类别仍为 UInt64 |
| 展开逻辑工作量 W | 单个含数组值最多 65,536；用下述递归公式计算，绝不实际展开 N 份类型 |
| 字节布局 | 每个含数组完整值不超过 `min(64 MiB, target.max_size, target_isize_max)`；含尾部 padding，先受检乘加再申请 |
| 类型深度、普通字段/槽 | 保留 256 层及既有 65,536 成员/槽限制；数组算一个布局节点，长度另由 W 限制 |
| 类型实例 | 新增每个规范化/规划阶段最多 256 个不同数组形状的缓存；别名、记录、枚举仍分别沿用既有 256 实例限制 |
| 函数/闭包 | 保留合计 4096 个生成入口、32 层生成依赖链；const 专化共享额度，不另开无上限缓存 |
| 稳定局部描述 | 保留单函数 64 KiB 的现有上限；数组载荷不放进此区域，位置指针和进度/清理元数据计入该上限 |
| 运行时 | 载荷、分配登记及新增动态元数据共用现有任务 64 MiB 峰值预算；布局合法不保证复制峰值可分配 |

W 的叶子（包括 Unit）为 1，记录/元组为 `max(1, sum(W(fields)))`，枚举为
`1 + sum(W(payloads))`，数组为 `max(1,N) * W(T)`。该上界保守计入所有枚举载荷；共享类型图
缓存计算结果，但每个实际内联位置按乘法/求和计数。所有子类型先验证，N=0 也不能跳过递归类型、
元素资格、深度、子布局和 W 检查。只需带饱和上限的计算，不生成逐元素清单。
例如 `FixedArray<Unit,65536>` 的 W 为 65536；两层长度 256 的 Unit 数组同样为 65536；
外层改成 257 即超限，零长度外层也不使不合法的内层类型变合法。
包装值和 API 返回类型各自检查上限；例如 W 已达 65,536 的数组再装入 Option 会超限，
不能因数组本身合法而跳过外层检查。F2b 的 get/返回边界用例须覆盖这种诊断。

先在 UInt64/受检宽整数中规范化长度与字节运算，再检查目标范围，最后转换为目标 usize。
32 位按 `2^31-1`、64 位按 `2^63-1` 约束可寻址偏移，并取显式 target.max_size 的更小值；
不能使用编译进程的 usize 宽度替代目标范围。下面仅为设计计算模型，不宣称已经支持对应目标 ABI：
A32 为 pointer=4、int32_align=4、int64_align=4，A64 为 pointer=8、int32_align=4、int64_align=8。

| 目标类型/布局 | A32：size / align | A64：size / align | 说明 |
| --- | --- | --- | --- |
| `FixedArray<Int32,4>` | 16 / 4 | 16 / 4 | stride=4 |
| `FixedArray<FixedArray<Int32,3>,2>` | 24 / 4 | 24 / 4 | 外层 stride=12，最后一维连续 |
| C 记录 P：x:Int32、y:Int64 | 12 / 4 | 16 / 8 | y 偏移分别为 4、8 |
| `FixedArray<P,3>` | 36 / 4 | 48 / 8 | stride 包含 P 的 padding |
| C 记录：tag:UInt8、data:FixedArray<Int32,3>、tail:UInt16 | 20 / 4 | 20 / 4 | 字段偏移 0、4、16，尾部补齐到 20 |
| `FixedArray<Float64,0>` | 0 / 4 | 0 / 8 | 语言合法，C 边界拒绝 |
| `FixedArray<Unit,64>` | 0 / 1 | 0 / 1 | 逻辑长度仍为 64，C 边界拒绝 |

F1 的目标布局测试须覆盖这两组输入、各上限及 +1、乘加溢出和零大小嵌套；F4 再用实际目标 cc
核对布局。即使 64 MiB 限额先拒绝了某个巨大对象，目标地址溢出路径仍须单独测试，不能依赖宿主截断。

### 7.4 紧凑存储、复制与私有调用约定

**执行表示：** 引入与 Type 分离的值表示分类，未含数组者沿用 Scalars；FixedArray 和任何
内联含数组的 Copy 聚合统一为 Place。Array 本身仍为现有拥有句柄，其元素布局可含 Place 类型。
Place 保存有类型的根位置/投影信息，后端只需传一个内部地址；它不是语言 Ptr，不使值成为引用。
scalar_types/storage_scalars 对 Place 不生成伪标量或逐元素 Vec，调用者必须先分流；字段选择
按布局的字节偏移，枚举保留现有 tag 加各载荷独立区域，不在本阶段引入 union 压缩。

**分配：** 所有独立 Place（包括零大小值）统一从当前任务 arena 分配稳定载荷，使用
Arena::allocate / release；零大小物化按既有 size.max(1) 规则取得对齐地址并计费。没有按 N
选择栈/堆的双路径，不复用解释器的独立 Locals arena，也不把载荷塞进 CPS 帧或旧聚合结果槽。
内联字段只投影外层位置，不另分配。每个独立位置先预留清理登记，分配成功立即挂接；任何失败
都回收本次申请，正常、return、break/continue、失败和取消沿同一登记结束，不依赖用户 Drop。
原生与解释器共用计费/释放原语，帧/调用独占登记责任；新增动态登记容量本身也须计费并有界复用。
数组使用使函数传播 needs_arena、uses_arena 和 may_fail；仅释放也保留 needs_arena。

**初始化：** 安全库的 repeat/from_fn 循环调用受信任的 begin/write_next/finish 原语。
begin 返回仅可信库可操作的非 Copy 构造令牌，含目的位置与已初始化前缀，不是可传给普通代码的 A；
write_next 只写下一个完整 T 并在成功后推进计数，finish 验证计数=N 才发布数组。令牌仅允许
可信库同步局部使用，不能存入用户聚合、返回给用户或跨语言 Pending；解释器时间片保留其
位置、索引和 callback 状态。前缀与清理状态都在值布局之外；finish 消费令牌并把同一分配的
登记责任交给完整 Place，失败则由令牌登记回收，
不制造第二份释放责任。Copy 元素失败时无逐元素 Drop，但回调的拥有环境仍沿既有清理路径结束。
构造完成前产生的引用不得逃出可信原语。清零原始存储只用于初始化 padding/非活动区域，不能当作
已初始化的 T，也不能将 begin 作为用户可用的未初始化构造器。

**复制与替换：** 布局描述包含共享的标量、字段、数组循环与枚举活动载荷节点。运行时遍历
逻辑值字段；数组复制使用循环，简单连续无 padding 叶子可批量复制，不读取未初始化字段或
把 padding 暴露成值。描述/IR 规模与 N 无关。读取字段/get/返回按值结果产生独立副本。
赋值先求值 RHS 到独立暂存，再在借用检查允许时提交到目标；源目标重叠时同样先快照，
失败发生在提交前则目标保持原值；提交为无回调、无分配的有界复制，不能在半提交处给解释器让出。
自赋值保持内容不变，普通 RHS 副作用仍执行一次。外部 C 写入失败仍遵守 §2.5，不提供回滚。
循环内作用域退出及时 release 载荷和暂存，重复执行重用有界登记项；不会每次迭代累积载荷。
普通解引用读写以及 mem.replace/mem.swap 同样进入位置路径；replace 先保留旧值的独立返回
副本，swap 在预留暂存后提交，两者都在修改目标前完成可失败步骤，保留独占借用/别名检查。
它们随各自载荷类型在 F2a/F2b 开放，不允许通过旧标量泛型原语意外复制内部地址。

**参数/结果：** 统一的私有调用描述决定每个参数是原有标量序列还是一个 Place 地址。
按值 Place 实参在调用方按求值顺序建立独立副本，登记后才求值后续实参；被调方将该副本视作
自己的值位置，但物理释放责任仍在调用方，成功/失败返回后统一回收，不转成共享语言句柄。
`&A` / `&mut A` 直接指向原位置，由借用规则约束，不复制载荷；返回来源不能来自按值参数副本。
Place 结果使用调用方预先登记的目的缓冲区，按“显式参数、结果地址、任务 arena”的顺序传入
原生函数；闭包隐藏环境维持既有首参数位置。返回时在被调方局部结束前复制完整值到结果区，
调用与被调方清理均成功才由调用方发布结果；错误时释放未发布结果，不留下指向被调方局部的地址。
原有非数组聚合结果仍使用原有槽缓冲区，二者不能按“都是指针”混用。

F2b 的直接调用、函数值间接调用及 FnMut 生成入口共用上述描述；参数/结果的完整语言类型
仍参与签名匹配。解释器保存独立 Place 值标签和位置编号，禁止普通 Value::clone 变成数组的
语言复制；复制由显式指令执行。父帧在子调用期间存活，返回区及参数副本由父登记保活；
取消先结束子帧/回调，再回收父实参，最后回收任务 arena。源码 AOT 使用相同可重定位布局
描述和运行时符号，不能嵌入编译器进程地址。公开宿主/实例/状态域包装入口不采用该新私有 ABI。
布局描述由相应编译模块持有，解释器暂停及 JIT 旧调用沿现有代码 owner 保活模块；不能引用
临时编译上下文。普通源码函数可在内部使用数组签名，宿主入口的拒绝不应误伤这些内部调用。

### 7.5 借用、视图与拒绝入口

每个独立 Place 建立自己的根身份；按值复制建立新根，字段/元素投影保留原根及字段路径。
F3 对同一数组所有下标保守借用整个数组，N=0、零大小元素或空切片也不解除来源与权限限制。
从 Array 元素派生的来源还包括父 Buffer 的 storage/interior loan；扩容、替换、移出和结束
都检查这些 loan。整个 Place 的读取、复制、覆盖及 release 进入 borrowing 的操作数/转移规则，
不能因其没有语言引用字段就在 refs 提取中被遗漏。同步 helper 返回元素/切片只能依赖仍存活的
引用参数，不能返回局部 Place 或按值参数副本的视图；noescape 推导同样保留真实根。

Slice 的首版物理协议保持现有不拥有载荷的 Buffer 视图头部，F3 增加从已验证连续位置创建
头部的可信入口。固定数组值本身不包 Buffer，也不伪造载荷释放责任。头部按既有 Slice 规则
计入任务 arena 并保留到根任务结束，因此 helper 返回视图不会依赖自己的局部描述符；载荷
寿命仍由借用检查约束，头部存活不保活已结束载荷。暂不引入头部的跨调用转移/提前释放协议。
重复创建视图可能耗尽头部预算，这是受检限制：F3/F5 分别核对载荷可及时复用、头部计费有界且
根任务结束全部归零，不把预期的头部保留误报成载荷泄漏，也不能以“视图不拥有”漏计头部。

| 准入/拒绝位置 | 检查接点与要求 |
| --- | --- |
| 元素与普通类型 | types.rs / specialization.rs / walk.rs 增加带身份缓存的数组资格检查；显式实例、别名、未用声明与泛型具体化均覆盖；N=0 不跳过 |
| 普通 alloc、graph、Arena.alloc | ir/memory.rs、ir/graph.rs、ir/arenas.rs 在分配/封存前拒绝递归含数组载荷，保留同步局部数组引用的独立准入 |
| 聚合/其他容器 | ir/records.rs、tuples.rs、enums.rs、buffers.rs 及类型元素验证只开放 §2.6 的 Copy 组合与 Array；检查完整外层名义类型，不能因 List/Map/Set 底层复用 Array 就放行 |
| 闭包捕获 | ir/closures.rs 的 prepare_closure/捕获绑定前拒绝含数组的捕获类型，也追踪引用指向类型；from_fn 的普通回调环境照常检查 |
| 挂起/并发 | ir/memory.rs、buffers.rs、borrowing.rs、task_scopes.rs 检查签名、局部和捕获；首版直接拒绝 @suspends 函数体中物化数组或数组视图，仅允许不携带数组跨入口的同步 helper |
| 函数值与泛型 helper | ir/function_values.rs 与 generics.rs 保留完整语言签名及不逃逸/保留效果；F2a 拒绝数组签名的函数取值/间接调用，F2b 才开放 |
| 持久实例/状态域/迁移 | core/instance.rs 的 persistent/persistent_graph、ir/cleanup.rs 的 domain_storage、ir/domain_schema.rs 及 vm/state_domains/migration/view.rs 递归拒绝数组；读取/写入、值包与迁移入口不能仅检查 Copy/字节数 |
| C 与普通宿主边界 | core/ffi.rs、core/builtins.rs、core/signature.rs 和 VM/AOT 注册/导出入口分别核对；F4 只放行兼容数组引用/字段，不放行普通 direct/CPS、文本或公开值接口 |

拒绝判断使用完整字段、名义实参、Buffer 元素与引用目标图，保留循环检测/深度限额；内部
Place 地址不参与源码类型资格判断。F4 必须校验 `register_c_fn` 的实际签名与源码解析结果一致。
当前源码 C 校验允许 numeric 结果而注册校验仍限 integer/Ptr/Unit；数组阶段不借机扩大标量
结果范围，浮点数组 fixture 可返回整数/Unit。该差异属于独立 FFI 一致性工作，不影响数组布局契约。

### 7.6 诊断与阶段交付边界

诊断沿用现有 ErrorCode，附具体类型/长度及源码 span；下列英文为冻结的诊断主题，补充上下文
可包含参数名、目标平台和实际数值，不把平台上限或元素类型藏在泛化报错中。

| 情况 | 代码/阶段及诊断主题 |
| --- | --- |
| 非法 const 类别/非支持表达式 | 解析错误或 TypeMismatch：`Constant arguments require UInt64 literals or constant parameters` |
| 无法由实参推导 N | TypeMismatch：`Cannot infer constant parameter` |
| 元素不在白名单 | TypeMismatch：`FixedArray requires supported reference-free Copy elements` |
| 维度/逻辑数量/字节/元数据超限 | ResourceLimit：`FixedArray layout or expansion limit exceeded`，区分具体限额 |
| 在未开放位置使用 | InvalidCall / TypeMismatch：`FixedArray is not supported in this context`，列明场景与外层类型 |
| 借用冲突或局部逃逸 | 沿用借用分析诊断，显示数组根/来源；不能绕开已有共享与独占规则 |
| 借用索引/切片区间越界 | RuntimeError：`FixedArray index or slice is out of bounds` |
| C 元素/零长度/零大小/按值边界不兼容 | AbiError：`FixedArray is not compatible with this C boundary` |
| 分配/登记或峰值预算失败 | 沿用运行时 ResourceLimit；清理后保留最先发生的错误 |

F1 可执行的是 const 参数返回/转发；数组只有类型与布局元数据，不能被当作执行阶段完成。
F2a 支持白名单中的非数组 T、独立数组、整个数组引用和直接调用；get 返回的 Option<T> 仍可
使用旧非数组聚合表示。F2b 才开放数组作为元素/聚合载荷、Array 元素和数组签名的间接调用，
包含 from_fn 回调返回嵌套数组。F3 开放元素引用与切片，F4 开放 C 数组引用/字段；各阶段
负例须覆盖下一阶段与 §2.6 的拒绝项，不能只在库函数名上做门禁。

### 7.7 F0 验收证据与未验证项

- 已静态核对 §7.1 的类型/实例键、标量展开、私有调用、任务分配/释放、借用与视图入口；
  §7.5 列出拒绝入口。完整冻结项分别落在 §7.2–§7.6，不再留给 F1 临时选择表示或接口。
- 已用独立算术计算核对两组目标的 7 行布局算例、字段偏移和 W 边界；检查了 35 个本地链接/
  锚点及节号引用，外部标准链接返回 HTTP 200，`git diff --check` 通过。
  算例仅为设计计算，不是新数组类型执行结果；32/64 位目标只是布局输入模型。
- 未新增可执行探针，未运行 Cargo、check.sh、Release、三后端数组行为或 cc 数组 fixture；
  以上是 F0 文档冻结时的验证边界；F1–F5 的实现证据分别按第 4 节补齐。F0 本身未开放语言能力。
