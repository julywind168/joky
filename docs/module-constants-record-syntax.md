# 模块常量与记录字段语法开发计划

状态：2026-10-07，基线 `60d15ea`。M0 契约与盘点已冻结（见第 7 节）；M1 已实现并通过
full 与 Godot 验收（证据见 7.6）；M2 已实现（模块常量与导入，证据见第 8 节，
M3 边界项随 M2 一并覆盖）；M4 集成发布已完成（证据见第 9 节），full、stress 与
Godot 发布验收全部通过。本文目标语法均已成为实际语法。
这是语言基础补齐的第 1 项；第 2 项为[记录解构](record-destructuring.md)，
第 3 项为[匿名函数与闭包](anonymous-functions-closures.md)。

本计划同时交付模块级标量常量与记录字段语法收紧。用户已确定两条规则：
相邻字段必须以换行或逗号分隔；字段默认私有，可显式加 `pub`。
不修改表达式序列、函数参数或枚举变体的分隔规则。

## 1. 基线与交付范围

| 领域 | 基线实际能力 | 本次目标 |
| --- | --- | --- |
| 常量 | 固有 impl 关联常量、标量求值、依赖环与额度检查 | 模块级 `const` / `pub const`、导入、统一依赖求值 |
| 字段分隔 | 声明与构造允许同一行仅用空白分隔 | 声明、构造统一要求换行或逗号 |
| 字段可见性 | 字段默认公开，可用 `private` 修饰 | 默认私有，显式 `pub`；迁移旧声明及跨模块使用 |
| 记录能力 | Copy / 拥有记录、泛型、字段借用和 Drop | 保留原有布局与所有权行为，解构另见第 2 项 |

以源码为准：字段解析见 [declarations.rs](../src/core/parser/declarations.rs)，
关联常量见 [members.rs](../src/core/ir/members.rs) 与 [constants.rs](../src/core/ir/constants.rs)。
基线 language.md 中“常量限四种整数”和“没有字段级可见性”等旧描述已在 M4 修订；
当前实际用法以 language.md 为准。

## 2. 记录字段规则

### 2.1 分隔与解析边界

以下两种声明都合法；构造项使用相同的分隔规则：

```joky
struct User { name: String, pub score: Int32 }

struct Point {
    pub x: Float64
    pub y: Float64
}

let user = User { name: "Ada", score: 42 }
let point = Point {
    x: 1.0
    y: 2.0
}
```

- 相邻两个完整字段之间必须有 `,`，或它们之间的源码空白/注释包含真实换行。
  支持 LF、CRLF、逗号后换行以及末尾逗号；关闭 `}` 前无需分隔符，空记录仍合法。
- 换行仅在完整字段类型或完整初始化表达式之后作为字段分隔；类型内部、括号内和
  表达式续行中的换行不会截断当前字段。不能只比较两个字段起始 token 的行号。
- 注释不自动充当分隔符。同一行块注释无效；跨行注释包含换行时有效。
  判定使用源码跨度或 token 间的换行信息，不能因 lexer 丢弃空白而失真。
- 只有逗号可以在同一行分隔字段；分号不作为字段分隔符。缺失、重复逗号及空字段均报错，
  错误定位在缺失分隔的位置或下一字段，不能退化为难以理解的未知类型/表达式错误。
- 本阶段未增加构造字段简写、记录更新 `..value` 或默认字段值。
  构造字段简写已在后续实现，当前规则见[记录](language.md#记录)。
  第 2 项的记录模式也沿用本节的字段分隔规则。

以下写法必须拒绝：

```joky
struct User { name: String score: Int32 }
let user = User { name: "Ada" score: 42 }
struct Point { x: Float64 /* gap */ y: Float64 }
```

字段以逗号或真实换行分隔；完整表达式的换行规则另见 [syntax.md](syntax.md#5-表达式连接块与分号)。

### 2.2 默认私有与显式公开

```joky
pub struct User {
    name: String
    pub score: Int32
}
```

`pub struct` 只公开类型，不连带公开字段。字段默认在定义模块内可见，同模块函数和 impl
可以访问；`pub` 字段在类型本身可见时允许跨模块访问。私有类型中的 `pub` 字段不会导出类型。
公开字段的类型不得借此绕过现有公开接口的类型可见性检查。

- 跨模块读取、写入、取借用及后续解构都检查字段可见性；别名、泛型实例、模块限定名和
  模板实例化不能绕过检查。模板按定义作用域检查，调用方不获得模板的私有权限。
- 记录字面量构造须有权访问所有被初始化字段，且仍须完整初始化。因此含私有字段的公开记录
  由定义模块提供公开工厂函数，其他模块不能直接构造。全部字段公开的记录可以外部构造。
- `pub` 不改变字段顺序、布局、Copy 判定、Drop、`#repr(C)` 布局或借用约束。
  可见性是语言访问规则，不能据此声称 C ABI 隐藏了内存。
- 旧 `private field: T` 修饰语法在迁移后拒绝，并给出改为 `field: T` 的诊断；
  不把 `private` 新增为全局保留字，`private: Int32` 作为普通字段名仍可用。
- 不提供按文件版本偷偷切换默认值的兼容模式；同一次编译统一采用新规则。

### 2.3 迁移清单

迁移普通 `.jk`、标准库 `include_str!` 源码、Rust/C 测试中的源码字符串、基准 fixture、
Godot 示例、文档中的可运行示例以及生成源码的工具。补齐分隔符；移除旧 `private` 修饰；
原先供其他模块访问/构造的字段显式加 `pub`，内部状态字段保持私有。
不可为了通过测试给所有字段机械添加 `pub`。

标准库的公开包装记录、跨模块泛型、宿主可见记录与 Godot 元数据路径需分别审查。
文档迁移必须与解析器切换同批落地；修改内嵌标准库后重新构建。

## 3. 模块常量

### 3.1 声明、导入与名称

```joky
pub const DEFAULT_SPEED: Float64 = 120.0
const MAX_ENTITIES: UInt32 = 4096
const DOUBLE_SPEED: Float64 = DEFAULT_SPEED * 2.0
```

常量有显式类型，默认模块私有；无可变存储身份，不能赋值或取地址，不产生模块初始化函数。
顶层 `var`、普通全局 `let`、`static` 和 `const fn` 不在范围内。
常量声明结束沿用已有顶层声明/关联常量的表达式边界，不把记录的换行规则推广到所有表达式。
`import` 仍须位于所有声明之前，`const` 也算声明。

```joky
import app/config as cfg.{DEFAULT_SPEED as SPEED}

fn speed() -> Float64 { SPEED + cfg.DEFAULT_SPEED }
```

- 选择性导入常量使用值名，不加 `type`；模块限定访问和导入重命名保留原声明身份。
  普通 import 不重导出，不增加 `pub import` 或通配导入。
- 同一值作用域中，常量与函数、裸枚举构造器等同名声明/显式导入冲突时报错；
  类型、模块别名与值之间的冲突沿用现有名称空间规则，M0 列出完整矩阵。
  局部绑定可遮蔽裸常量；局部值遮蔽模块前缀时不再回退查找模块。
- 常量表达式在定义模块解析；消费者的同名声明不改变其含义。公开常量可依赖私有常量，
  只导出计算后的值和合法的公开类型，不导出私有名称。
- 支持同模块前向引用和合法导入图中的跨模块依赖。现有模块导入环规则继续有效，
  常量依赖图不成为允许循环 import 的理由。

### 3.2 求值与额度

首版覆盖八种整数、Float32、Float64、Bool、Rune，以及解析到这些类型的透明别名。
不接受 String、引用、Ptr、函数值、记录/枚举/元组/容器常量。
Rune 字面量保持有效 Unicode 标量检查，不因常量求值开放任意整数转 Rune。

表达式白名单为标量字面量、可见的模块/关联常量、已有合法一元及二元运算、显式整数和浮点
转换。运算是否合法仍由对应类型的普通规则决定，不给 Rune 或 Bool 增加数值运算。
普通函数/方法调用、局部绑定、赋值、控制流、短路逻辑及分配首版拒绝；以后扩展单独验收。

模块常量与关联常量共用按声明身份建立的依赖图，互相引用也必须检测环，给出完整依赖路径
与源码位置。每个表达式最多 256 个语法节点、依赖链最多 32 层；别名与菱形引用复用已计算值，
不得反复展开造成指数工作量。类型和表达式错误不能因常量未被使用而漏检。

整数按目标位宽环绕；整数除零与非法移位为编译错误。浮点按目标精度和标准浮点环境计算，
保留负零/次正规数，NaN 按现有规则处理；不可用宿主 FTZ 或舍入配置决定结果。
浮点除零按既有 IEEE 规则，不套用整数诊断。
常量读取降为普通标量立即值，不增加运行时依赖或可供宿主读取的全局数据符号。
重载后新代码使用新常量；旧实例和旧代码快照仍保留旧值。

## 4. 开发阶段

| 阶段 | 工作 | 验收门槛 |
| --- | --- | --- |
| M0：契约与迁移盘点 | 冻结名称冲突矩阵、字段分隔判定、可见性和诊断；列出源码迁移入口 | 正反例覆盖注释、续行、泛型、跨模块构造；明确当前与目标差异 |
| M1：字段语法与可见性 | 解析器切换、私有默认、显式 pub、源码/示例迁移 | 同模块私有访问及跨模块公开访问执行通过；非法访问和旧语法拒绝；三后端、Godot 回归 |
| M2：模块常量与导入 | AST、名称绑定、导入、类型检查、依赖图和求值统一 | 前向/别名/私有依赖/关联常量混合依赖正常；环与限额稳定拒绝；三后端读取结果一致 |
| M3：边界与稳定性 | 标量矩阵、模块替换、错误位置、浮点宿主环境、非法程序 | 按位/独立整数模型核对；旧快照保留旧值；未使用声明也检查；不引入运行时初始化 |
| M4：集成与发布 | 更新实际语法、架构和能力入口，清理迁移遗漏 | full 与 stress 发布验收，Godot 检查通过；所有阶段证据齐全后标完成 |

M1 与 M2 可独立开发，但 M4 必须同时验收；记录解构的最终集成依赖 M1。
模块常量不是常量泛型：本计划不自动放宽 [FixedArray 计划](fixed-array-storage-ownership.md)
中的长度实参范围，后续如需接受模块常量须显式设计与测试。

## 5. 验证与文档归属

遵守 [AGENTS.md](../AGENTS.md)：纯计划变更只审查内容、链接和 `git diff --check`，
不运行 Cargo；本页示意不计为已经可运行的示例。实现阶段先执行针对性行为测试，
跨模块语义修改提交前运行 `./scripts/check.sh --full`；发布另跑 `--stress`。
M1 涉及共享源码与 Godot 脚本，需运行 `./integrations/godot/check.sh`，不能仅验证解析器。
三后端执行标量返回及真实模块导入，AOT 在独立进程运行；纯语法负例不需要假造运行路径。

每阶段记录实际检查、平台及未验证项。完成后 language.md 收录真实用法，syntax.md 更新文法
和状态，architecture.md 记录依赖求值及访问边界；README 只在交付后登记能力。
本计划保留决策、阶段与验收证据，不复制完整语言手册。

## 6. 可复用阶段任务

> 完成 docs/module-constants-record-syntax.md 的阶段【编号】。核对前置阶段和当前源码，
> 保持字段换行/逗号分隔、私有默认与显式 pub；按范围完成旧源码迁移及模块常量语义。
> 覆盖三后端行为和非法程序诊断，按 AGENTS.md 验证，更新阶段证据与所属文档；
> 报告实际完成范围、检查和剩余限制，未验收能力不得标为已实现。

## 7. M0 冻结产物：契约与迁移盘点（2026-10-06）

本节是 M0 的交付：后续阶段按此实现，不再重新讨论规则。盘点基于基线 `60d15ea`
的源码扫描，实现期间新增的源码同样按第 2、3 节规则处理。

### 7.1 字段分隔判定

- 解析器按 token 判定，不依赖空白保留：设前一字段最后一个 token 的行号为 `L`，
  下一字段首个 token 的行号为 `N`。若下一 token 是 `,`，消费后继续；若下一 token 是
  关闭 `}`，字段列表结束；否则当且仅当 `N > L`（token 之间存在真实换行，LF 或 CRLF）
  视为已分隔，其余情况报错。类型、括号和表达式内部的换行不参与判定，因此
  多行类型/多行初始化表达式之后的同行字段仍会被正确拒绝。
- 判定必须比较"前一字段结束 token"与"下一字段起始 token"的行号，不能比较两个字段
  起始 token（多行类型会误判），也不能用源码文本近似。
- 注释：M1 实现时语言只有 `//` 行注释，行注释总是以其后的换行结束，因此注释后的字段
  自然满足 `N > L`。M1 基线尚不支持块注释 `/* */`，该阶段未新增；
  计划第 2.1 节中 `/* gap */` 反例当时以语法错误拒绝，不依赖分隔逻辑。
- `;` 不是分隔符；同一行只有 `,` 能分隔字段。末尾逗号合法；关闭 `}` 前无需分隔符；
  空记录 `{}` 合法。
- 诊断目录（M1 实现需逐条覆盖）：
  - 同行缺分隔：`Expected ',' or a newline between record fields`，定位在下一字段
    首个 token（声明与构造同一措辞）。
  - 连续逗号/空字段：`Record fields cannot be empty`，定位在多余逗号。
  - 字段名位置出现非标识符（如 `;`、`}` 前的悬空状态）：沿用
    `Expected an identifier, got …`，不得退化为未知类型错误。
  - 旧 `private` 修饰：`The 'private' field modifier is no longer supported; fields are
    private by default — write 'field: T', or add 'pub' to make the field public`，
    定位在 `private` token。`private` 后跟 `:` 仍是普通字段名。

### 7.2 可见性与名称冲突矩阵

字段可见性：字段默认私有；`pub` 修饰后公开。`pub struct` 只公开类型。同模块判定沿用
`DefinitionScope.records`（定义模块自身记录 id 集）与入口模块 `local_records`；别名、
泛型实例（实例保留模板 id）、模块限定名与模板按定义作用域检查，不能绕过。`pub` 不改
变布局、Copy、Drop 与 `#repr(C)`。读取、写入、取借用、构造和解构（第 2 项落地后）共用
`check_field` 一个入口。私有字段访问诊断保持
`Field '<record>.<name>' is private`。

值名称空间冲突矩阵（常量加入后的完整规则；✓=允许，✗=报错）：

| 同一值作用域内组合 | 规则 |
| --- | --- |
| 模块 `const n` vs 本模块 `fn n` / 裸枚举构造器 | ✗ DuplicateDefinition |
| 模块 `const n` vs 本模块另一 `const n` | ✗ DuplicateDefinition |
| 模块 `const n` vs 本模块类型/别名/trait | ✓（值与类型不同名称空间） |
| 导入常量（裸名或重命名）vs 本模块 `fn`/`const`/构造器 | ✗ DuplicateDefinition |
| 导入常量 vs 模块别名 | ✗ DuplicateDefinition |
| 导入常量名命中整数转换/`alloc` | ✗ ReservedName |
| 导入常量 vs prelude 隐式名 | 显式导入覆盖隐式（与现有函数一致） |
| 局部 `let/var n` vs 裸常量 `n` | ✓ 局部遮蔽裸常量 |
| 局部绑定 `n` 后写 `n.MEMBER` | ✓ 走字段访问，不回退模块查找 |
| 常量 vs 关联常量 | ✓（`NAME` 与 `Type.NAME` 不冲突） |
| `import m.{C}` 但 `m` 未导出 `C` | ✗ InvalidImport（沿用 does not export value） |

模块限定读取 `alias.NAME` 仅当完整点分路径命中 `scope.constants` 时按常量处理；
未命中时沿用函数值/构造器/类型的既有解析，最终失败给出 unknown 类诊断。

### 7.3 模块常量求值契约

- 常量身份以声明为准：定义模块内裸名与 `模块路径.名字` 规范键绑定（`prepare` 改名后
  关联），导入重命名保留原声明身份；依赖图按声明身份建边，模块常量与关联常量共用
  一张图，环检测输出完整依赖路径与两处源码位置。
- 白名单与限额沿用关联常量实现：256 语法节点、32 层依赖链、别名/菱形复用已计算值。
  常量值中的裸标识符只在命中本作用域常量绑定时成为依赖边，否则拒绝；模块限定读取
  同理。关联常量值中引用模块常量、模块常量引用关联常量均走同一边机制。
- 求值发生在 `ModulePlan::with_import_layouts_and_types` 的标准浮点环境 Guard 内，
  复用 `core::integer`/`core::float` 共享内核；宿主 FTZ/舍入配置不影响结果。
- 未使用的常量声明同样完成类型检查与表达式检查。

### 7.4 迁移入口清单（基线扫描结果）

分隔规则受影响点（同行仅空白分隔的字段/初始化器）：`.jk`/`.gd` 共 501 处、99 个文件
——tests/fixtures 63 文件 429 处、examples 23 文件 52 处、benches/programs/map.jk 4 处、
godot 6 文件 15 处 + gameplay_diagnostics.gd 1 处、src/stdlib/map.jk 2 处（:58-59）。
Rust 测试源码字符串中受影响点约 250 处、82 个文件（vm 测试为主，另 tests/cli.rs、
tests/source_modules.rs）。docs 示例 43 处（language.md 25、arena-borrowing-ffi.md 14、
syntax.md 1、其他 3）。

旧 `private` 修饰：stdlib 3 文件（array.jk:8、map.jk:14-19、string.jk:204）、
godot node.jk:4/:57、fixtures 8 文件、Rust 测试字符串 11 处、docs 若干。

需要显式 `pub` 的跨模块字段（审查结论，不机械全加）：

| 记录 | 原因 |
| --- | --- |
| `godot/node.jk` `Vector2.x/y` | demo `mover.jk`/`async_mover.jk` 跨模块构造；`gameplay_diagnostics.gd:31` 对 `mover.jk:25` 做精确字符串替换，必须同步修改 |

其余审查结论：stdlib 记录字段全部为定义模块内部使用（方法访问），迁移后保持私有；
`tests/fixtures/map_invariants.jk` 依赖拼接进 `joky/map` 模块作用域的机制
（sources.rs `map_test_library`），私有字段访问必须继续放行；
`string_storage_probe.jk` 的跨模块私有字段负例语义保留（拒绝原因从 private 修饰
变为未标 pub）。关联常量现状：float 已支持（fixtures/float.jk），language.md
"限四种整数和 Bool"为过期描述，M4 修正。

### 7.5 M1 测试矩阵（验收清单）

正例：逗号分隔单行/多行、换行分隔、CRLF、末尾逗号、空记录、混合注释、多行类型后
换行、多行初始化表达式后换行、泛型记录、`pub` 与私有字段混用。
执行正例：同模块私有字段读写、跨模块 `pub` 字段构造/读取，三后端一致。
负例：同行无分隔（声明+构造）、同行 `;`、连续逗号、空字段、同行块注释（语法错误）、
旧 `private` 修饰、跨模块私有字段读/写/借用/构造、`pub` 不绕过 `#repr(C)` 布局。
Godot：`./integrations/godot/check.sh` 全量通过。

### 7.6 M1 完成证据（2026-10-06）

实现：`parse_record`/构造初始化器共用 `expect_braced_separator`（逗号或
前一字段末 token 与下一 token 之间的真实换行，`Parser.last_line` 跟踪）；字段
`pub` 修饰解析；旧 `private` 修饰拒绝并给出迁移诊断；`RecordDef.public_fields`
替代 `private_fields`，布局计算与 `check_field` 反转为默认私有；同模块判定沿用
`DefinitionScope.records`/`local_records`，泛型实例保留模板 id。

迁移：`.jk`/文档/`.gd` 共 633 处分隔修复（编译器驱动复核至零分隔错误）；
Rust 测试源码字符串 316 处；`private` 修饰全部移除。显式 `pub` 仅两处：
`joky/string.Utf8Error` 的 `code`/`offset`（examples/string_queries 跨模块读取）
与 godot `Vector2.x/y`（demo 跨模块构造，`gameplay_diagnostics.gd` 精确替换串
同步）；tests/source_modules 的 `Pair`/`Payload`/`Stats`、tests/cli 的 `Batch`
等测试内嵌模块按同一规则补 `pub`。`tests/fixtures/map_invariants.jk` 拼接进
`joky/map` 作用域的私有字段访问保持放行；`string_storage_probe` 跨模块负例
语义保留（拒绝原因变为未标 pub）。

检查（macOS aarch64，Debug+Test profile）：
`cargo test --lib` 900 通过；`cargo test --test cli --test source_modules`
103+23 通过；`./scripts/check.sh --full` 通过（fmt、Clippy `-D warnings`、
Debug/Release 测试、短输入基准正确性，覆盖 JIT/解释器/AOT 三后端）；
`./integrations/godot/check.sh` 结果见提交信息。

测试落点：解析器正反例
`core::parser::tests::declarations::record_fields_require_commas_or_real_newlines_and_default_to_private`；
跨模块可见性执行 `tests/source_modules.rs::record_fields_default_private_and_require_pub_for_cross_module_access`
（JIT+解释器）；三后端私有边界沿用 tests/cli.rs
`owning_array_methods_match_all_backends_and_private_module_boundaries`。

M1 当时未验证项/限制：同行块注释 `/* */` 因当时语言不支持块注释而以语法错误拒绝
（见 7.1，非分隔逻辑覆盖）；常量泛型、记录解构（第 2 项）不在本次范围。
2026-10-18 已补充可嵌套块注释及字段换行分隔回归，当前规则见 [语言说明](language.md#注释)。

## 8. M2 完成证据：模块常量与导入（2026-10-07）

实现（按 7.2/7.3 契约）：

- 解析与 AST：`parse_const` 统一模块级与关联常量的 `const NAME: Ty = value` 解析，
  `Module.constants` 独立于 `items`（见 [module.rs](../src/core/parser/module.rs)、
  [declarations.rs](../src/core/parser/declarations.rs)）；`pub const` 公开，默认模块私有；
  `const` 计入声明边界，`import` 须位于其前。
- 名称绑定：`ImportScope.constants` 把每个合法拼写（定义模块裸名、`alias.NAME`、
  完整路径、selector 导入与重命名）映射到声明身份键 `<模块路径>.<NAME>`（入口模块无
  路径，用裸名）；`bind` 为入口与源模块共用，`prepare` 在路径盖戳后补齐源模块拼写；
  私有常量从不进入 `exports.constants`，消费者无法拼出其名字。源模块 `reexport_imports`
  的 selector 再导出补上常量分支。
- 值域冲突（7.2 矩阵）：同模块 `const` 与 `fn`/裸枚举构造器冲突在 `bind` 报
  DuplicateDefinition；selector 导入常量与本地 `fn`/`const`/模块别名/保留名沿用既有
  检查（本地常量并入 `local_values`）；同模块重复 `const` 以规范键判重，不同模块各自
  的私有同名常量互不冲突。
- 依赖与求值：模块常量并入关联常量的同一张声明身份依赖图（`ConstantOwner` 区分
  `Module(path)` 与 `Associated(Type)`，`constants_by_key` 建索引），共享 256 节点 /
  32 层限额、环检测与求值器；常量表达式中的裸标识符与点分路径统一按声明作用域的
  `ImportScope.constants` 拼写解析（selector 导入与限定名在常量表达式内同样可用），
  私有可见性由拼写表本身保证；求值发生在 `with_import_layouts_and_types` 的标准浮点
  环境 Guard 内，复用 `core::integer`/`core::float` 内核（整数环绕、除零/非法移位
  拒绝、浮点按 IEEE 含无穷）。类型仅收八种整数、Float32/64、Rune、Bool；未使用声明
  同样完成类型与表达式检查。
- 读取：函数体内裸名（未被局部绑定遮蔽时）与 `Module.NAME` 在字段投影前解析为常量，
  内联求值后的字面量，不产生运行时全局数据；跨模块限定读取未命中时报
  `Undeclared constant `path``（UnknownName，替代此前误导性的 `Undefined variable`）。

测试落点：`tests/module_constants.rs`（18 例：前向引用、导入身份与重命名、标量全覆盖、
白名单拒绝、共享内核算术边界、环、未使用检查、32 层与 256 节点限额、私有跨模块读取
与诊断、值域冲突矩阵、常量表达式内 selector/限定引用、pub 依赖私有、模块常量与关联
常量双向混合依赖与互环、菱形共享声明、JIT/解释器一致）；三后端执行
`tests/cli.rs::user_file_module_constants_match_across_all_backends`
（JIT、解释器、AOT 独立进程，多文件模块）。

检查（macOS aarch64）：`cargo test --lib` 901 通过；
`cargo test --test cli --test source_modules --test module_constants`
104+23+18 通过；`cargo fmt --check`、`cargo clippy --all-targets -- -D warnings` 通过；
`./scripts/check.sh --full` 通过（含 Release 测试与短输入基准正确性，三后端）。

该阶段未验证项/限制：prelude 常量机制经 selector 同构路径实现，标准库当前无常量声明，无
直接正例；嵌入 VM 热重载下"旧快照保留旧值"依赖既有代码快照机制（常量值内联进函数体
IR），未新增专项测试；模块常量不放宽 FixedArray 长度实参（见第 4 节）。

## 9. M4 完成证据：集成与发布（2026-10-07）

文档集成（按第 5 节归属）：

- language.md：新增"模块常量"章节（声明、导入、求值与冲突规则，示例经实际运行验证）；
  修正关联常量类型范围（八种整数、两种浮点与 Bool，白名单补浮点转换与模块常量引用）、
  字段默认私有与显式 `pub`（`pub struct` 不连带公开字段）、字段分隔规则（逗号或真实换行）
  及两处未迁移的同行构造示例。
- syntax.md：能力表新增模块常量行并更正名义记录行；顶层声明列表加入 `const`（计入
  import 边界）；10.2 字段分隔与可见性描述对齐实现；EBNF 的 `Item` 加入 `ModuleConst`、
  `RecordField` 改 `[ "pub" ]` 并注明换行分隔不在 EBNF 内表达；第 12 节实施清单补
  模块常量。
- architecture.md：`core::ir::constants` 段记录模块常量与关联常量的统一声明身份依赖图、
  拼写到键映射、浮点 Guard 与 selector/限定名解析；可见性段补记录字段默认私有与
  模块常量导出边界。
- README.md：语言能力行登记模块级标量常量与字段可见性。

发布验收（macOS aarch64）：`./scripts/check.sh --full` 通过；`./scripts/check.sh --stress`
通过（Release 穷举与长输入基准）；`./integrations/godot/check.sh` 通过（G0–G2 全部
PASS）。full 之后仅修改文档（含本节），按 AGENTS.md 无需重跑代码检查；文档中的新
可执行示例已用当前编译器逐个运行验证。

## 10. 独立复核与修复（2026-10-07）

复核基线 `885e9ae`。模块常量遗漏了声明类型的规范化，导致 `type Count = UInt32`
配合 `const N: Count = 42` 被错误拒绝。现与关联常量共用类型/注解规范化路径，
支持本地、泛型及导入的透明别名，并保持定义模块作用域；不合法的别名目标仍拒绝。

新增 CLI 用例 `module_constant_aliases_resolve_in_their_defining_scopes` 在 JIT、解释器和
独立 AOT 中执行多文件私有类型别名、公开泛型别名、同名遮蔽和浮点计算，并检查非法目标。
`module_constants_keep_values_in_old_instance_snapshots` 补齐旧快照证据：两个执行模式下，
旧/新实例分别使用旧/新常量，失败重载不破坏现有实例，关闭后实例存储归零。
第 8 节列出的热重载未验证项由此补齐；自定义 prelude 常量正例仍未单独验证。

本轮三份开发文档的共同验收（macOS aarch64）：

- `./scripts/check.sh --full` 通过：Debug/Release 各 941 个单元测试、107 个 CLI
  测试、23 个模块测试，以及三后端短输入 while/Map 正确性检查。
- 常量集成测试 Debug 18 个、Release 19 个通过；新增快照用例在 full 的 Debug 阶段后
  加入，另行运行 Debug 定向测试及全目标 Clippy 均通过，Release full 已包含该用例。
- `./scripts/check.sh --stress` 通过：15 个 Release 单元穷举、5 个 CLI 矩阵，以及
  三后端长输入 while/Map 正确性检查，包含闭包逐指令取消穷举。
- `./integrations/godot/check.sh` 通过：7 个 Rust 测试及 G0–G2 的 16 个真实引擎进程，
  并检查 G1 截图。未进行性能测量或其他平台验证。
