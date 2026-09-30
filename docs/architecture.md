# 架构与嵌入约束

Joky 保持单个 Rust crate。公开入口为 `parser`、`aot::AotCompiler`、`vm::VmEngine`、
`runtime`，以及根模块的类型、签名和结构化错误。源码目录表达当前职责，不预设拆包计划。
使用方式见 [README](../README.md)，语法见 [语言与内置模块](language.md)。

## 模块与依赖

```text
source → core::parser → AST → core::imports / types → core::resolver → core::ir::ModulePlan
                                                               ├─ backends::native → JIT / AOT
                                                               └─ backends::interp → bytecode

stdlib → modules::ModuleRegistry → core::resolver::BindingRegistry
vm     → registry + backends + runtime::Continuation
cli    → public parser / VmEngine / AotCompiler
```

| 目录 | 职责 |
| --- | --- |
| `src/core/` | 词法、AST、导入解析、类型/签名、诊断及后端无关 IR |
| `src/backends/native/` | JIT/AOT 共用的 Cranelift 符号声明、数据对象及 direct/CPS lowering |
| `src/backends/jit/` | JIT 初始化、入口导出和可执行代码所有权 |
| `src/backends/aot/` | AOT 状态管理、导入布局和对象文件输出 |
| `src/backends/interp/` | 字节码生成/优化、静态绑定、迭代执行和诊断 |
| `src/modules/` | 将纯声明元数据与可选 native entry、owner 组合成不可变注册快照 |
| `src/runtime/` | 原生 ABI、CPS 帧、continuation、协作调度及借用字符串 |
| `src/stdlib/` | `joky/vm` 实现、标准绑定、option/result/integer 源码库及 JIT 地址的弱所有权索引 |
| `src/vm/` | checked embedding API、加载发布、宿主注册和执行限额 |
| `src/cli/` | 参数、入口检查、诊断渲染、原生链接及产物发布 |

`core` 的生产代码不依赖 Cranelift、运行时或具体执行后端；跨后端行为测试可以使用它们。
JIT 和 AOT 共享 `backends::native`，解释器直接消费 `core::ir`。内部代码使用实际模块路径，
`lib.rs` 只保留面向嵌入方的公开 re-export。

`BindingRegistry` 保存声明、模块路径和符号索引；`ModuleRegistry` 增加 native 实现与生命周期。
两者的区别是数据边界，不是两套解析器。resolver 直接接收同一份声明快照；没有仅作转发的
provider trait。标准库每线程初始化一次 registry，以 `Rc` 共享，裸指针和 owner 不要求 Send/Sync。

## 语义与代码生成

词法器借用原始 UTF-8 源码，以字节位置为唯一游标；关键词和数字直接从切片解析，
标识符及解码后的字符串由 AST 所有。`Span` 保存文件身份 SourceId 与半开字节范围；源码文本由 SourceFile / SourceMap 的共享快照保存。
`parser::Diagnostic` 转为 `Error` 时统一保留 Parse 阶段、行号和范围，CLI 在最后一步渲染。

`TypeRegistry::resolve_aliases` 收集模块全部记录、枚举及别名，迭代遍历类型依赖图并检测环，统一改写函数签名和
所有局部注解，保留原始源码范围；返回的 AST 不再携带别名声明。别名使用编译期定义映射，
不会修改宿主注册器；参数/返回值等编译后元数据只保存实际类型。所有类型声明都检查，即使没有使用。
VM/AOT 在内置调用重写及 ABI 检查前执行该步骤，CLI 也在检查 main、print_int 和 print_int64 前执行。
直接处理 AST 的嵌入方可以调用同一个公开 API，输入 AST 保持不变。
parser 的 `Module.type_aliases`、`Module.records`、`Module.enums` 保存独立声明和源码范围；手工构造 Module
的 Rust 调用方需要初始化这些字段。记录及枚举声明具有独立身份，克隆 AST 保留身份，新解析生成新身份。类型与值名称分开解析，类型别名不会出现在函数导出列表或对象符号中。

泛型别名的 `TypeAlias.type_params` 使用声明局部的刚性 Copy 参数。内部 `TypeDefinition` 区分
已解析类型和 `AliasTemplate`；模板目标在定义作用域中规范化，导入绑定保留模板及参数身份，
调用点只替换参数，不按消费者的同名类型重新绑定。未使用的实参也检查 Copy 约束。
`core::types::specialization` 复用元组 / 记录 / 枚举的有界替换，规范化阶段单独缓存最多 256 个不同
别名应用（含刚性参数应用）；重复应用复用目标，参数最多 64 个，目标沿用 65,536 槽 / 256 层上限。
模板不进入构造查找、可执行 `Type`、IR 或 ABI；透明展开不创建名义身份或运行时符号。

core::imports 绑定 prelude 与源码导入；resolver 绑定宿主模块，并只在导入 `joky/vm` 时重写其受限调用形式。
`ModulePlan` 统一检查声明、调用、返回类型、赋值权限、作用域和挂起约定。
`FunctionPlan` 保存 typed slots、操作、对应源码范围、控制流标签及挂起状态。
三个后端消费相同结果，不在 lowering 中重新推导语言类型。

lexer 解析十进制、`0x`、`0b` 和数字间单下划线，整数 AST 保留幅值及直接负号，类型检查从局部上下文确定 Int32 / Int64 / UInt32 / UInt64 后再验证范围。
`core::integer::IntegerType` 集中定义整数位宽、符号及字面量范围，语义检查与原生 lowering 共用这些信息。
共享 IR 的定型提示只查看类型，不执行表达式；实际规划始终按源码顺序，绑定完成即固定类型。
字段、载荷、元组元素和返回值的预期类型向内传递，分号丢弃项没有结果上下文。
公开 AST 的 `Literal::Int32` / `Literal::Int64` / `Literal::UInt32` / `Literal::UInt64` 表示已经定型的值，源码解析产生 `Literal::Integer`。
这会把 Int32 范围错误从解析阶段移到 TypeCheck；超出 UInt64 最大幅值的文本仍在解析阶段拒绝。

必须保留的语义约束：

- 可变绑定读取生成快照，后续赋值不能改变已求值的操作数或实参；求值顺序从左到右。
- Unit 不产生机器值；普通 ABI 省略 Unit 参数，CPS 参数槽仍保留原始位置。
- 普通 lowering 通过 Cranelift Variable 构造 SSA，补齐循环回边后封闭块；
  CPS 使用槽位和独立恢复状态。表达式序列只影响规划顺序，不增加执行指令。
- 恢复继续当前子调用，不重算参数或重放副作用；每轮循环重新创建其 scope/race 子帧。
- String 字面量通过可重定位数据对象输出，不能把编译进程地址写入 AOT 对象。
- `joky_cps_`、`__joky_cps_`、`joky_vm_`、`joky_runtime_`、`__joky_runtime_` 保留给 runtime。

结构化并发在 AST 和共享 IR 中统一表示为 `Scope`，`ScopeKind::Join` 对应 `scope`，
`ScopeKind::Race` 对应 `race`。两者共用参数求值、Unit 挂起分支约束、借用检查和能力传播。
Join 等全部分支完成，Race 在按源码顺序观察到首个完成时释放全部子帧，再继续父函数；
首个观察到的失败仍沿既有失败路径传播。原生后端复用 `__joky_cps_release_children`，
解释器沿平坦激活表迭代释放未获胜分支的子树并归还帧额度，保留祖先和无关兄弟分支。
取消不回收根任务 arena 中的分配，也不撤销已有副作用；旧 CPS ABI 保持原契约，
异步借用 v1 将强制 stop 回调接入同一帧释放路径，在父任务恢复前确认外部访问停止。

记录、枚举和元组分别规范化为 `Type::Record` / `Type::Enum` / `Type::Tuple`，与 `Type::Named` 的宿主 opaque 标量区分。
`core::types::layout` 统一解析记录、枚举和别名的依赖图，生成成员类型、偏移及扁平标量布局，
拒绝无限布局、非 Copy 成员和超限布局。枚举布局先存一个 Int32 标签，再为每个变体保留
独立的载荷区，宽度为 `1 + sum(payload slots)`；不做重叠 union 存储或类型重解释。
构造初始化全部槽，未选载荷区置为 0 / false；选中载荷按其类型复制，避免 SSA 未定义读取。
标签只由受检构造产生，宿主不能注入裸枚举值。

元组按有序元素类型比较，透明别名在嵌套位置也展开；类型依赖扫描进入元组元素，
所以元组不会隐藏别名环或无限内联布局。`Type::tuple` 供 Rust 嵌入方构建未解析注解，
`resolve_aliases` 验证 Copy 元素、计算布局，并在无名义声明的模块里也执行规范化。
共享零大小元组图的规范化和比较缓存已访问节点，避免指数展开；诊断类型名输出有大小上限。
元组布局为各元素标量宽度之和，保持元素顺序，没有运行时元组对象或新增 native ABI。
`core::ir::tuples` 先求完整初始化器、检查所有模式叶子，再发布新绑定；var 叶子复制到独立存储，
`_` 不创建绑定，零大小和被丢弃元素的副作用仍执行。

IR 的聚合 slot 描述一段连续标量槽；构造、快照、赋值和分支合并展开为普通标量操作。
Unit、空记录及仅含零大小元素的元组不占值存储，但仍参与类型检查和副作用求值；独立 Unit 参数保留旧占位。
`core::ir::enums` 校验模式、穷尽性和可达性后，把 match 降为标签比较、分支及结果复制，
被匹配表达式求值一次，载荷绑定指向稳定快照。最后一个穷尽分支无需重复测试标签；
正常完成的分支才参与结果类型合并。首版按源码顺序测试，尚无跳转表优化。

同步原生内部调用展开聚合参数，聚合返回增加最后一个隐藏结果指针，没有机器返回值；
调用者分配临时结果区，每个标量占一个 64 位槽，低位编码沿用原标量规则。
CPS 保持 `CpsFrame` ABI：聚合结果位于 slots 中参数区之后的固定区域，普通标量仍写 result。
父帧在子任务完成且错误检查通过后复制结果，再释放子帧；Pending 只重新轮询。
记录、枚举和元组复用该协议，它不是 extern/宿主 ABI；公开调用继续使用标量包装函数。
聚合值本身不增加堆对象，解释器及 CPS 原有的激活帧分配仍然存在。

解释器的局部变换、跳转/源码映射、静态适配器和帧存储见 [解释器设计](interpreter.md)。
这些优化不改变共享 IR；死值不能成为删除有错误或副作用操作的理由。

## 同步局部借用

core::ir::locals 在规划前扫描取借用的标识符，在相应绑定建立时为无引用 Copy 值物化稳定位置。
按拼写保守匹配可以使同名但不被借用的局部值也物化；不同声明仍有独立身份，分支不改变绑定表示。
参数、解构叶子及 match 载荷使用相同规则。Binding 的原值槽保留类型信息，place 保存隐藏引用；
后续整值/字段读写经 ReadMemory / WriteMemory，读取生成独立快照，写入沿用预留与权限检查。
不能把 Cranelift SSA 值或可移动的解释器 Value 槽地址作为语言引用。

Op::Local 建立区别于任务 arena 的函数局部来源，Op::EndLocal 标记词法作用域结束。
块、条件、逻辑右侧、match 输入和分支、graph 块及 break/continue 离开的局部范围均发出标记。
borrowing 在 CFG 上拒绝超出原绑定范围的活跃视图，函数返回和任务存储操作拒绝局部来源；
聚合、泛型和调用结果沿用现有来源传播规则。隐藏 place 是存储身份，不作为竞争借用；
对原位置的实际访问仍检查全部活跃视图。借用检查后删除 EndLocal，保留操作与源码范围的对应。
局部存储不产生 uses_arena，不向 helper 的调用者增加任务期存储契约。

Op::Project 为内联记录字段建立地址和借用，保存根引用、字段路径及是否为写预留。
local_path 统一解析借用和直接字段访问，field_target 校验各级记录字段；不会经过引用解引用。
借用预扫描物化根对象，Project 不额外分配存储。原生和解释器都按 memory_field_path 的字节偏移
生成字段地址，零大小字段可处于对象末端，不依赖地址不同证明独立性。
borrowing 的 Origin 保存根身份与字段名路径，路径前缀表示同一位置或父子重叠；
引用可能来源集合按根和路径排序，用合并扫描检测重叠，编译工作量预算计入路径长度。
只有已证明的 Local 根缩小访问路径；arena 和参数来源维持整对象契约。
直接字段读写及预留同样检查精确位置，整体访问覆盖所有子位置；复制、聚合、分支合并及调用结果
保留可能路径的并集。最后使用、祖先权限和固定签名依赖不变，存储及作用域检查仍看根身份。

JIT/AOT 共用原生 direct lowering，以显式 Cranelift 栈位置实现 Local，按 memory_layout 对齐并初始化。
解释器激活帧独立拥有稳定分配，按声明目标槽缓存地址；循环重入只重新初始化，同一函数递归调用
各自拥有存储。词法借用先结束，底层存储可保留至函数返回；正常完成、失败或取消都随帧释放。
内部时间片暂停可保活同步帧，但源程序仍禁止 @suspends 函数内的局部借用及其进入 scope 分支。
含局部引用的函数跳过解释器仅针对值的优化与槽压缩，后续优化需证明位置身份不变。
每个具体函数的被借用存储按各声明的 max(size, 1) + align - 1 求和，编译期上限为 64 KiB；
刚性泛型模板推迟到具体实例检查，不代表整个调用栈或进程内存限额。

## 隐式任务 arena

Type::Reference 保存目标 Copy 类型与共享/独占权限；共享引用可进入 Copy 聚合，独占引用不可以。
Joky 含引用签名仍沿用内部调用约定；对外仅开放显式同步 C 借用输入，不进入旧宿主 ABI。
core::ir::memory 规划分配、共享化和字段/整对象读写，Type::memory_scalars 将对象字节布局映射到标量槽。
Bool 为一字节，Unit 不读写内存；字段的对象字节偏移与 IR 槽偏移保持分离。
MemoryLayoutTarget 显式描述引用的指针大小/对齐，默认使用宿主指针布局；不据此声称 C 兼容。

core::ir::borrowing 对 CFG 做引用活跃性与来源/移动状态分析。调用结果按签名依赖引用输入；
独占结果只能别名独占输入或新对象。可能来源取并集，允许访问的祖先关系取交集，循环分配点保守合并；
分析有槽数、指令数和工作量预算，超限拒绝编译。写目标预留允许 RHS 只读访问，最终写入时检查独占权限。

来源分析逐个追踪聚合中的引用槽，包括未选枚举载荷的空来源、构造/复制/解构/返回和内存读写。
写入 arena 的借用独立于槽活跃性保留到任务结束；对象内容来源用于字段读取，不把读取视为新分配。
含引用对象共享化时撤销其构造期独占能力；跨函数的构造期独占签名拒绝，包括泛型实例。

core::ir::graph 在词法作用域内区分构造句柄与普通引用：只有组内 graph.alloc 建立的句柄可用作
连接端点和根；禁止普通观察、逃逸、嵌套构图及跨块跳转，组操作只出现在构图块顶层。
GraphOp::Link 更新组内内容来源而不提前发布共享借用；GraphOp::Seal 一次撤销全部句柄并保留组的共享借用。
借用检查完成后，专用操作才展开为现有 Reference/WriteMemory/Literal，三个后端复用原有 arena 内存实现。
构造句柄的新分配与不可观察性由前端保证，不能把普通引用送入这些操作；外部初始化引用仍走普通存储借用检查。
块中途失败沿用任务级清理，不需要按图遍历或逐节点引用计数，源码范围在展开时保留。

FunctionInfo::uses_arena 从实际分配沿普通调用和 Op::Scope 分支边传播，区别于仅有引用值的函数。
引用能力也沿这两类边传播，原生回调检查不能因嵌套 scope 而丢失能力约束。
分配能力代表保留输入的存储契约：调用方保守保留全部引用实参，不使用逐参数函数体借用摘要。
不分配的只读 helper 仍可按结果最后使用结束借用。两种能力均可进入 @suspends 调用链及 scope 分支；
宿主引用输入只对独立的同步 C 和异步借用 v1 开放。
Op::Scope 在父函数 CFG 中表示整个 fork/join，所有分支实参中的引用槽同时作为活跃性输入。
这也使较早的参数快照在后续参数求值/挂起期间保持借用，不能被冲突访问或移动。
borrowing 在该操作检查全部分支的访问后，再保留具有分配能力的分支输入；多个共享读取合法，
独占借用要求来源与其他分支不重叠。普通借用结束于整个 join，存储借用则保留到任务结束。
带引用签名（包括聚合中的引用）的函数只有内部入口。
原生同步内部函数在已有参数（含聚合结果指针）后增加隐藏 arena 指针；对外包装入口保持原签名，创建 arena、
调用主体后不论错误标志都执行清理。不使用 arena 的代码不增加参数或包装。__joky_arena_ 前缀保留。
解释器由 Execution 独占 arena，原始引用仅在受检内部槽中；分配函数和含引用操作的函数暂时跳过仅针对值的优化和槽压缩。
未选枚举载荷使用内部空引用占位，不能由语言代码构造可解引用的空引用；解释器读取前亦检查空指针。

原生 CPS 入口保持单个 CpsFrame 指针的 ABI，不经过同步 arena 包装。分配能力沿语言调用链传播，
有该能力的入口在分派恢复状态前通过 __joky_cps_arena 获取稳定 arena；首次创建、后续轮询复用。
父帧在创建子帧前已经持有 arena，子帧继承其 Rc<UnsafeCell<Arena>>，只对 arena 控制块计数，不对对象计数。
单线程顺序 poll 和分配 helper 的短期独占访问构成 UnsafeCell 契约；宿主借用 v1 可以保留受检对象地址，但不能获得 Arena 能力或保留帧/槽地址。
__joky_cps_finish 在成功或失败退出时同步释放子帧及当前帧的 arena 所有权，保留结果槽供调用方读取。
子帧完成时父帧仍持有 arena，因此返回引用不会失效；scope 分支也共享根任务 arena，
分支或整个 join 完成都不回收分配或重置计费预算；根帧终止则立即回收全部对象。
Continuation 对非法 CPS 状态和错误通道失败同样收尾；丢弃 pending 帧通过所有权自动回收。
解释器的 Execution 在 Pending 保留 arena，在完成/失败时先释放活动帧再释放 arena；poll 预算让出不重放分配。
graph 块即使出现在挂起函数中，其内部表达式仍按同步效果检查，封存结果可以跨后续挂起使用。

runtime::arena 管理独立、对齐且不移动的分配，正常返回及失败统一释放，没有逐对象引用计数或 GC。
对象布局溢出、系统分配失败及默认 64 MiB 计费超限经错误通道传播，分配错误保留源码范围。
计费含管理条目但不承诺进程 RSS 上限；解释器帧和原生递归栈继续遵守既有约束。
尚未开放 arena 配置、显式 arena、普通局部借用或 C 引用调用。

## 泛型函数的编译边界

parser 为每个函数类型参数创建独立的 `Type::Parameter` 身份，`FnDef.type_params` 保存声明，
`Expr::Call.type_args` 保存显式实参（空列表表示推导）。手工构造 AST 的 Rust 调用方需要初始化这些新字段；
克隆保留参数身份，重新解析创建新身份。类型别名规范化时使用函数局部的临时参数环境，
参数可进入元组及枚举类型应用，但不写入宿主类型注册器。

`core::ir::generics` 管理一个只存在于编译期间的上下文。`ModulePlan` 先登记签名，
以刚性参数检查每个泛型模板，包括未使用者，再规划普通函数并迭代处理实例队列。
推导只读取类型提示；按实参源码顺序收集已有类型，再为未定参数尝试整数字面量默认类型，
实际求值与副作用仍由原调用规划执行。模板只具有 Copy 能力，不能依赖某次具体实例的运算或字段。

实例键是模板名和规范化类型实参，透明别名共享实例，名义类型保持区别。入队前先写缓存，
因此同型递归和互递归不会无限生成代码；每模块最多 256 个实例，新实例依赖链最多 32 层。
实例化替换签名及函数体内全部类型注解、显式类型实参，重新建立具体元组 / 枚举布局，
再使用现有 `FunctionPlan` 检查并规划；共享类型图使用节点缓存避免指数展开。
模板计划被丢弃，最终计划只含具体函数，错误传播元数据在实例齐全后统一计算。

内部函数使用保留前缀 `__joky_generic_`。原生后端从计划而非原 AST 声明和生成函数，
实例使用 `Linkage::Local`；解释器把实例保存在内部 callee 列表。两者均从公开入口和签名查询中排除模板与实例，
宿主通过具体签名的普通包装函数调用。刚性参数不得进入 native / host ABI，
不新增字节码指令、运行时类型标签、C helper 或 CPS 帧协议。
编译上下文及模板 AST 不进入 continuation；具体代码沿用所属模块的保活机制。

## 参数化记录、枚举与布局

`RecordDef.type_params` / `EnumDef.type_params` 使用声明局部的刚性参数身份；未解析的类型应用用
`Type::Applied(Arc<TypeApplication>)` 保存，避免扩大每个 Type / Slot 的大小。
`Expr::Field.type_args` 仅用于 `Option.None::<Int32>` 等空载荷构造；手工构造 Field 的调用方需初始化该字段。
载荷构造复用 `Expr::Call.type_args`，投影和赋值拒绝显式类型实参。
`Expr::Record.type_args` 保存 `Pair::<Int64> { ... }` 的显式参数；手工构造 Record 表达式的调用方需初始化该字段。

`core::types::layout` 的声明依赖图进入类型应用的全部参数，在实例化前检查环；
未经过引用的声明依赖环保守拒绝，包括只经过 phantom 参数的环。经过引用的名义目标由
core::types::indirect 保存浅层声明身份；解引用或泛型推导将其用作值类型时，从模块名义表物化完整布局。
物化缓存共享类型节点，不形成 Arc 拥有环，也不重复展开零大小类型 DAG。
浅层目标明确标记为 indirect，直接查询其对象布局报错，避免被误当成零大小对象。
记录及枚举模板以自身刚性参数建立符号布局，
未实例化的模板也验证字段 / 载荷类型、重复字段 / 变体和布局限制。
`core::types::specialization` 为函数、记录与枚举共用类型替换，按声明 ID 与规范化实参缓存实例。
具体实例保留原声明模板引用用于再次替换；相等性比较声明 ID 及实参，不依赖实例分配地址。
元组、记录与枚举参数的比较、替换、有效性及名义身份检查使用访问缓存，避免共享类型图指数展开。

规范化与 IR 规划使用独立的临时实例缓存，每阶段记录、枚举各上限 256；函数局部类型环境共享所属阶段缓存，
宿主注册器不保留它。65,536 槽 / 256 层限额包含实例字段和载荷，phantom 类型参数也参与嵌套深度限制。
记录 / 枚举构造以字段 / 载荷、显式实参及预期类型推导实例；泛型函数形参与具体实例按声明 ID 及类型参数统一。
两类构造共用先具体类型、后默认值的推导协议，字段提示按源码顺序传播局部绑定，不执行任何副作用。
函数定义检查期间符号载荷参数不需要机器常量，丢弃该计划；具体实例重新规划并初始化全部载荷槽。
字段偏移、标签、平铺载荷、AggregateCall、CPS 帧与失败协议保持不变，三后端无需泛型聚合专用指令。

## 固有成员与源码库

`Module.implementations` 保存 `ImplDef`、方法和关联常量；任意表达式接收者由装箱的 `Expr::MethodCall` 保存，
已有点分调用在 IR 阶段区分局部值、类型和宿主模块。类型规范化先检查具体 Copy 目标，
或按顺序完整应用 `ImplDef.type_params` 的名义记录 / 枚举目标；拒绝别名、特化及无约束目标。
`Self` / `self` 在解析时关联目标类型；泛型 Self 构造及命名空间保留目标名称与类型实参，不能拼成带尖括号的字符串名称。
AST 的手工构造者需初始化 implementations 及 type_params，并遍历 MethodCall 的接收者。

`core::ir::members` 将方法登记为 `Type.method` 内部普通函数，所有签名先登记再检查全部函数体。
类型提示只负责静态查找，实际求值复用普通调用：接收者占首个实参，保持快照及从左到右求值。
`scope` 复用相同解析过程。原生方法使用 Local linkage，解释器放入内部 callee 列表，公开入口不包含成员。
AST 保留 impl 参数与方法独立参数各自的列表和声明身份；类型规范化按两层作用域检查，禁止遮蔽 impl 参数或目标名称。
成员收集时按 impl、方法的顺序拼接内部模板参数，复用 `core::ir::generics` 的刚性检查、签名替换、完整实参缓存与队列限额。
方法查找按名义声明 ID 匹配，成员名称和可见性键使用不含类型实参的规范名称；完整目标约束使该查找不涉及特化选择。
实例接收者 / 显式绑定的类型命名空间提供 impl 实参，成员后的显式列表只绑定方法参数；未绑定命名空间的列表绑定两层参数。
成员调用先校验源码实参数量，受保护的内部调用可只提供 impl 前缀，剩余方法参数复用泛型函数推导；普通函数仍不接受部分显式实参。
链式调用的返回提示使用替换后的签名。模板按定义模块检查，实例中的具体类型身份按当前消费模块校验，
源码名称和私有成员查找仍固定在定义作用域，避免外部类型实参被误拒或调用者同名类型改变模板含义。
参数化聚合上的方法可从泛型函数调用，无约束参数本身仍不能获得猜测的方法能力。

`core::ir::constants` 迭代排序常量依赖并拒绝环，用共享 FunctionPlan 检查受限标量表达式，再计算其直线 IR。
单个表达式限 256 个语法节点，依赖链限 32 层；已计算常量只插入类型明确的字面量，不复制依赖子树。
不会执行用户函数、宿主代码或产生 CPS 状态。普通整数规则同样约束编译期算术，非法除法/移位在编译期报错。
泛型 impl 常量仍限固定整数 / Bool，检查时保留 impl 参数作用域，但每个成员只求值一次，不按类型实参生成常量实例。

`core::imports` 在类型解析前绑定默认 prelude 和显式源码模块；`Module.scope` 保留不可变的 ImportScope。
`core::imports::sources` 按显式依赖顺序解析内嵌源码，检测缺失模块和依赖环。标准库缓存首次使用时初始化，
每个模块只解析一次；TypeRegistry 及其克隆共享已规范化 AST、导出绑定和声明身份。
`option.jk` / `result.jk` 定义普通枚举及状态判断、默认值、组合和载荷转换方法；它们按值、急切求值，不引入特殊短路或运行时指令。
Result 的 ok / err 返回标准 Option，依赖关系仅为 `result → option`；依赖类型及构造器不经 result 重导出。
`integer.jk` 显式导入 Option 及其构造器并定义整数成员。
`prelude.jk` 的选择性导入列出默认导出，整数成员通过其模块依赖提供。标准库模块自身不隐式导入 prelude。
内嵌模块描述表规定是否重导出选中成员，以及无别名整模块导入的兼容简写。
用户源码声明默认私有，`pub` 控制导出，公开枚举同时导出构造器；成员可见性由定义作用域与公开成员集合共同检查。

类型、构造器和函数别名保存原声明绑定；IR 在局部遮蔽检查后查询，解析器不再替换标识符 token。
依赖声明使用规范化模块名称；名称查找只接受当前作用域中的绑定，不把规范名当成额外导入。
库函数、方法和常量保留各自的 DefinitionScope，
包含定义模块的导入、名义类型和可见成员。用户模块的同名值、类型、函数或额外 impl 不会补入库的作用域。
函数特化保留定义作用域，库内函数与方法不成为 VM 或原生模块的公开入口。
依赖合并去重且与导入顺序无关；重复规范化不重复加载，也不会重新应用 prelude 开关。
注入时只克隆并重定位 AST 的诊断范围：显式依赖归属于 import，隐式 prelude 使用空范围，缓存不受影响。

宿主模块的函数别名映射到 registry 原名称；注入的 extern、CPS 完整帧大小、native symbol 和 owner 均保持原绑定。
宿主签名中的 opaque 类型按模块记录导出名，类型别名不改变 ABI。
VM 特殊调用先检查源码中的局部根，再转为用户无法拼写的内部调用名，避免用户的 `vm` 变量捕获另一个模块别名。
默认 prelude 可经 TypeRegistry、VmEngine、AotCompiler 和 CLI 关闭；VmEngine 的开关只作用于后续加载，
AotCompiler 只允许在编译开始前修改配置。

`core::imports::files` 为每次编译构建用户模块依赖图，与内嵌库共用绑定、规范化及依赖合并逻辑。
文件 provider 把模块路径映射到入口目录下的 .jk 文件；显式 ModuleSources provider 只访问调用方提供的快照。
两种 provider 都检测完整依赖环并限制 128 层深度；菱形依赖只解析一次。源码模块在自己的宿主声明环境中解析，
共享宿主绑定只合并一次，且不向调用方泄漏别名。用户源码的 Span 不重定位；编译错误附带所需 SourceMap，
JIT 模块和解释器 bytecode 持有共享源码快照，continuation 保留原版本。
原生失败通过新增的带 SourceId helper 传回文件身份，保留原有双偏移 helper 的 C ABI；
JIT 的同步与挂起错误附带源码映射，解释器另保留逐文件调用栈。AOT 的独立错误输出仍是消息与字节范围。
`VmEngine::type_registry` 提供只读声明快照，CLI 显式调用 `resolve_file`，
然后把同一 AST 交给 VM 或 AOT，避免二次解析丢失模块依赖。

方法、常量和受检转换不新增运行时指令、C ABI、帧表示或类型标签；失败、Pending、取消及旧代码保活均走既有调用协议。

## 加载与代码所有权

`VmEngine` 保存当前不可变模块、宿主注册和类型环境。只有解析、类型检查、宿主校验和编译
全部成功后才发布替换产物并递增版本；失败保留旧入口与签名。
`ModuleId` 是同一 VM 内的加载版本，不是全局唯一编号。
公共 `function_signature` 返回副本；内部调用检查借用元数据，避免每次解释调用复制参数列表。

JIT 模块拥有导出入口、元数据、可执行内存及外部代码依赖；最后一个 owner 释放时先回收
生成代码，再释放依赖。Continuation 持有创建时的模块，重载、替换宿主注册或销毁 VM
不会使旧调用失效。编译上下文、AST 不进入 continuation。

一个 `AotCompiler` 只接受一次编译尝试。编译失败后禁止继续追加或输出对象。
registry 的固定 symbol 及显式 C 导入不受导出前缀影响；普通源码定义和旧 extern 可通过 prefix 隔离原生符号。
AOT 允许只有 symbol 的绑定，由系统链接器解析；VM 加载缺失 native entry 的绑定会失败。

模块注册先验证重复路径、名称、固定 symbol、direct/CPS 一致性、入口及完整帧大小，
成功后一起发布元数据、opaque 类型和 native 实现。签名声明不能覆盖既有 registry ABI。
VM 只收集源码 extern 依赖，导入内置模块时 resolver 会注入该模块的全部声明。
解释器只为实际使用的导入要求适配器；这不等于任意 ABI 都能执行。

## 宿主与字符串 ABI

Rust 嵌入示例：[examples/embedding.rs](../examples/embedding.rs)。宿主注册入口均为 unsafe：

| 方法 | 生命周期与布局要求 |
| --- | --- |
| `register_c_fn` | 同步 C 接口，合计 0–4 个引用/Ptr 输入、整数/Ptr/Unit 结果；真实布局、引用权限及外部指针契约由注册者保证，可传代码 owner |
| `register_borrowing_suspending_fn` | 异步借用 v1，1–4 个 C 兼容引用输入、整数/Unit 结果；强制 poll/stop，stop 同步停止访问并清理操作状态，可传代码 owner |
| `register_fn` | 指针真实 C ABI 必须匹配完整 FunctionSignature，所有调用期间有效 |
| `register_suspending_fn` | 静态 CPS 回调，只分配参数槽；进度放在 state/host_data |
| `register_aot_suspending_fn` | 提供该入口的完整 frame_slots 和保活代码依赖的 owner |

声明校验无法证明裸指针真实类型；宿主不得向生成代码 unwind、保留借用帧指针或调整 slots 大小。
同名自定义注册只影响后续编译；内置绑定名会被拒绝，避免注册成功但永远不生效。
旧宿主签名中的合法命名类型自动登记；AOT 可显式注册 opaque 类型。注册入口、类型名和 CPS 帧大小
会在修改宿主表前完成校验。
默认已有 `vm.Handle` 和 `vm.Function`，未声明为记录/枚举/别名且未登记为 opaque 的类型在 IR 阶段拒绝；
别名指向未知类型时在别名解析阶段拒绝。宿主注册仍使用实际类型，源码别名不属于注册 API。

同步 C 导入是独立 ABI 种类，不能与旧 direct/CPS 注册互换。共享 core::ir 验证 unsafe 调用和
参数边界，原有借用数据流仍检查别名与生命周期；native lowering 与解释器都传入真实稳定对象地址，
不把 u64 标量槽数组当作 C 记录。记录显式标记 #repr(C)，字段只含定宽整数与兼容嵌套记录。
`ffi::c_header` 输出本机目标的 C11 布局断言；注册名只负责匹配源码类型，不能替代对外部布局的验证。
返回不携带借用，C 不能保留借用地址或转移其所有权，也不能把引用实参的地址作为 Ptr 返回。
不透明 Ptr 是独立的可空 Copy 标量，使用目标指针宽度与对齐，C 参数/结果和头文件映射为 void *。
解释器保留独立 Ptr 值标签，允许空值，不使用 Reference 的非空检查；判空在共享 IR 中降低为与空指针的比较。
普通聚合、arena 存储和 CPS 帧可保存 Ptr 值，但复制或跨挂起不会保活外部对象，也不会触发资源释放。
代码 owner 继续保活注册入口；外部对象及其所属库的实际寿命由绑定/调用方保证。
旧 direct/CPS 外部注册不接受 Ptr；资源拥有包装未开放，异步借用 v1 也不赋予 Ptr 所有权。

String 原生表示为本机字节序 u64 长度，随后是 UTF-8 字节，不要求 NUL 终止。
视图归代码模块或宿主所有，调用方不得持有超过其 owner 生命周期；宿主返回视图也需覆盖
所有保留它的生成调用。解释器在 native 边界使用自有缓冲区跨 Pending 保活。
更多 Rust/C 安全约束以 API 注释及 [VM 头文件](../include/joky_vm.h)、
[CPS 头文件](../include/joky_cps.h) 为准，不复制 Rust Vec 的内存布局。

标准 VM 句柄来自不复用的全局递增编号，但只在创建线程可用。
执行前保留 Rc、释放句柄映射借用，允许嵌套创建/调用其他 VM；正在执行的 VM 不可重载。
错误状态、返回码及完整内置接口见 [语言与内置模块](language.md#内置标准库-jokyvm)。
JIT/AOT Joky 函数可注册为 native entry，目标 VM 可以是 JIT 或解释器；解释器函数本身没有原生地址。

异步借用 v1 使用 `FunctionInfo::borrowing_host` 标记受检的引用 extern，不增加 `uses_arena` 分配能力。
共享 IR 重用 C 所指类型的布局检查，调用及 scope/race 沿用既有借用来源和冲突检查。
JIT/AOT 导入版本化 `BorrowingHostV1` 数据描述符并生成私有 CPS 包装函数，所有调用形态均进入
`joky_cps_poll_borrowing_v1`。帧在首次 poll 之前登记 stop，终态用 Completed/Failed 收尾，
release/Drop 用 Cancelled 收尾；取走清理入口后才调用，保证失败或重复释放不重复 stop。
`CpsFrame::Drop` 覆盖直接 drop 和 C frame_free，HostPending::Drop 在 binding owner 销毁前显式收尾。
停止回调先于对象存储及代码 owner 释放，错误通道使用独立 checked 边界保留原始失败。
描述符错误版本/缺少回调在注册时和运行时都拒绝；同步停止的真实行为仍是 unsafe 宿主契约。

## Continuation 与调度

原生 CPS 入口为 `unsafe extern "C" fn(*mut CpsFrame) -> i32`：0 完成、1 Pending、其他值失败。
生成入口完成时还设置 `DONE`；普通 native 子回调按返回状态完成即可。
Int32 / UInt32 使用槽低 32 位，Int64 / UInt64 使用完整 64 位，Bool 使用低 8 位。
UInt32 写入 CPS 槽时零扩展；有符号与无符号类型不因 ABI 位宽相同而互换。嵌套调用及 scope 使用运行时管理的子帧。
AOT CPS 导入必须传入 `cps_frame_slots` 的完整布局；跨对象编译使用 `compile_with_import_layouts`。

`ContinuationStatus` 区分 Pending、Completed、Failed。终态不能再次 resume，
`cps_result` 只在成功完成后可读。原生错误保留 CPS status，解释器可保留结构化错误及调用轨迹；
`call_async` 首次轮询遇到宿主失败时统一返回 HostError。
生成代码的除零失败在两个 VM 模式均返回带源码范围的 `RuntimeError`，恢复时为 `ResumeError::Failed`。
`call_async` 接受 Int32 参数及 Int32/Unit 结果；`call_async_int64` 接受 Int64 参数及 Int64/Unit 结果。
后者的 `cps_result()` 保存完整原始位，调用者用 `as i64` 解读。同步宽整数入口为 `call_int64_0/1/2`。
无符号入口为 `call_uint32_0/1/2`、`call_uint64_0/1/2`、`call_async_uint32` 和 `call_async_uint64`。
异步入口允许同型整数或 Unit 结果，UInt32 的 `cps_result()` 零扩展为 u64，UInt64 保留全部位。
这些入口均精确检查签名，不能混用位宽或符号；CLI main 与内置 C VM 标量入口保持 Int32 契约。

`Scheduler` 是手动驱动的单线程协作调度器：闭包按入队顺序执行，continuation 按单调 TaskId
顺序轮询。ID 仅属于创建它的调度器。`take_completed`、`take_failed`、`cancel` 删除记录；
终态在收取前计入 task_count，只有 Pending 计入 suspended_count。resume_all 遇首个错误返回。
真实 I/O 就绪和再次轮询的时机由嵌入方提供。解释器预算见
[Resource limits](interpreter.md#resource-limits)。

## 整数转换内建函数

`core::integer::IntegerType` 集中维护四个转换名称及整数位宽/符号元数据。
共享 IR 检查转换的实参数量和整数类型，独立推导源类型后生成 `Convert(dst, src)`，
两个 typed slot 保留源类型及目标类型；不向函数签名注册器伪造 Any 参数、重载集合或宿主 extern。
转换结果类型也进入纯类型提示，保证算术、条件和聚合中的局部定型一致。
调用规划位于 `core::ir::calls`，避免继续增大递归表达式规划器的栈帧。

JIT / AOT 共用 narrowing `ireduce`、signed widening `sextend`、unsigned widening `uextend`；
同位宽直接复用原生值。解释器用 `Convert` 指令按源值标签扩展，再按目标类型截断/解释。
转换自身不设置 may_fail 或增加 CPS 状态，但实参中的调用、除零、移位和 Pending 仍按原规则传播。
没有新增运行时 helper、C ABI 或帧布局；纯同步转换不要求链接运行时。

名称在共享类型解析的声明检查及 IR 规划中保留，CLI / VM 在查询 extern 宿主实现之前拒绝重定义；
宿主及模块绑定注册同样拒绝占用这些名字。它们不是模块导出或可取地址的普通函数，
带 Integer 能力约束的转换签名仍是未来设计。用户语义见 [显式整数转换](language.md#显式整数转换)。

## 位运算与解析边界

位运算和移位复用 typed IR 的 Binary / Unary，不增加运行时值类型或 CPS 状态结构。
移位两侧由共享语义检查保证同型。原生 lowering 使用 `band` / `bor` / `bxor` / `bnot`、
`ishl`，以及依符号选择的 `sshr` / `ushr`。位运算按源码求值顺序保留快照。
parser 的分支上下文将最外层 `|` 保留给 match / scope；进入括号、调用参数、元组、字段或块时
允许按位或，退出时恢复外层状态。优先级和用户写法见 [位运算与移位](language.md#位运算与移位)。

## 原生错误传播

Int32 除法先检查零，再将两个操作数符号扩展到 I64 做有符号除法，最后截断回 I32。
Int64 除法在零检查之后，将 -1 除数替换为安全的 1，再选择环绕取负结果；其他除数执行 I64 有符号除法。
UInt32 / UInt64 使用相应位宽的无符号比较和 `udiv`，同样先检查除零。
这样两个有符号位宽的 `MIN / -1` 都环绕为 MIN，不执行会触发除零或有符号溢出的机器除法；不使用 signal、
longjmp 或跨生成代码的 Rust unwind。已有标量签名继续使用原有 C 参数及返回值 ABI，聚合内部调用使用隐藏结果指针，CPS 帧布局不变。

移位先以操作数完整位宽做无符号 `count >= width` 检查，同一检查也拒绝负的有符号移位量。
检查发生在机器移位之前，避免处理器对移位量掩码；失败调用 `__joky_runtime_invalid_shift`，
通过现有错误通道携带表达式源码范围，再走同步 / CPS 的失败返回路径。

`runtime::error` 用线程局部通道保存本次调用的首个结构化错误及半开 UTF-8 字节范围。
生成代码在可能失败的调用之后、消费结果之前检查错误，并立即向上传播；失败返回的机器值
不具备语言含义，也不能作为 String 指针解引用。IR 通过调用图固定点传播 `may_fail`，
将所有外部调用保守视为可能失败；无除法、移位和外部依赖的纯函数无需生成调用后检查。

`VmEngine`、continuation 及解释器 native 适配器的 Rust 调用边界保存外层错误、隔离嵌套调用，
退出时恢复外层状态。嵌套 VM 中已由调用者处理的失败不会污染外层执行或下一次调用。
该通道与 `joky/vm` 的公共错误查询独立。CPS 除零或非法移位返回 `JOKY_CPS_RUNTIME_ERROR`，
设置 `JOKY_CPS_FAILED` 并释放子帧；scope 不再轮询后续分支。失败任务没有成功结果，不能重试。

AOT 编译后用 `AotCompiler::requires_runtime()` 判断是否需要链接 `libjoky.a`；含除法、移位、
外部调用或 CPS 的对象需要 runtime。C 调用方按 [joky_runtime.h](../include/joky_runtime.h)
在顶层调用或每次 CPS poll 前清除错误，返回后先查询状态，再读取结果；宿主再次调用生成函数时
必须传播尚未处理的失败。跨动态库需要共享同一个 runtime，不能分别静态链接独立错误通道。
新增 helper 依赖要求旧分发环境更新配套 archive 和头文件，无需改变现有函数签名或复制帧布局。
CLI 自动链接所需 runtime；独立程序除零或非法移位时报告错误和字节范围并以状态 1 退出，无需保留源码文件。

## 已知边界

- 四种整数的除法语义已一致；原生错误携带出错范围但没有完整语言调用轨迹，解释器仍提供
  `runtime_trace`。Span 不带文件 ID，跨模块或重载后的嵌入方必须使用对应模块的源码。
  不捕获宿主自身的机器陷阱，不能把 catch_unwind 当作机器陷阱处理。
- 解释器执行栈是平坦存储，但解析器、AST 规划和原生执行仍有递归；解释器限额不约束
  解析/编译内存、源码嵌套深度、原生回调或回调新建的 VM。当前不是不可信代码隔离环境。
- `core` 已与机器码生成分层，但单 crate 仍编译 Cranelift；没有 interpreter-only feature，
  也没有证明可在所有禁止可执行内存的平台构建。可序列化字节码也尚未定义。
- resolver 仍认识 `joky/vm` 的内置调用形式；有第二种真实模块需求后再考虑通用重写协议。
- 保持公开 API/C 符号和显式 ABI 包装，避免为减少几行代码引入宏生成或任意函数指针调用。

## 构建与验证

Release 使用 opt-level 3、Thin LTO、一个 codegen unit 和符号裁剪，保留 unwind。
CLI 的 Release 原生链接也清理无用代码/符号。LTO 可能增大含中间代码的 archive，
分发体积应看最终可执行文件；导入 `joky/vm` 会带上 JIT 编译器。
`joky build` 没有独立模式参数，要用 Release CLI 和配套 Release runtime 构建发布程序。

原生产物先在输出目录内链接，成功后 rename 发布；失败清理临时文件并保留旧输出。
安装和 runtime 查找方式见 [README](../README.md#安装与产物)。Cranelift 依赖保持同一版本系列，
Cargo.lock 固定精确版本。C 示例链接所需系统库可用 `cargo rustc --lib -- --print native-static-libs` 查询。

提交前统一运行 `./scripts/check.sh`，测试要求见 [CONTRIBUTING](../CONTRIBUTING.md)。
回归覆盖三个后端的类型与控制流、重复 Pending、不重放副作用、失败/取消、重载后所有权、
真实 AOT 链接和安装布局。检查脚本只验证基准结果，不进行计时。性能测量独立运行，
工作负载和统计边界统一维护在[基准方法](../benches/README.md)，历史数字保留在[实验记录](interpreter-performance.md)。
