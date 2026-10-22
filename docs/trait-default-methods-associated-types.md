# trait 默认方法与关联类型开发计划

状态：2026-10-08，初稿基线 `5a769f9`，T0 核对基线 `c932c0a`。
**T0 契约已冻结（见第 8 节）；T1–T5 已完成并验收（见第 9 节）。**
第 8 节保留 T0 时的设计记录；当前语法与边界以 language.md 和第 9 节为准。
这是语言基础补齐的第 4 项；前三项入口见 [syntax.md](syntax.md)，
后续动态分派见[第 5 项：借用型 dyn Trait](borrowed-dyn-trait.md)。

## 1. 基线与交付范围

T0 基线已有静态 trait、Self、共享/独占/按值接收者、无接收者关联函数、条件与 blanket impl、
定义作用域绑定、有限约束证明，以及 `@suspends` / `@noescape` 契约。
trait 声明只有签名，实现必须完整；关联常量属于固有 impl，不能误当成 trait 关联项。
Fn/FnMut/FnOnce 是专门的内建约束，不是已支持泛型参数的普通 trait。

本项交付默认方法、必须由 impl 指定的关联类型、类型投影、关联类型能力约束与等式约束。
全部调用仍静态分派，不新增运行时 vtable 或修改普通函数 ABI。
不同时引入 trait 自身泛型、方法级泛型、关联类型默认值、泛型关联类型（GAT）、
trait 关联常量、继承、特化、`impl Trait` 或运算符重载。dyn 单独实施。

实现入口为 [AST](../src/core/parser/ast.rs)、[声明解析](../src/core/parser/declarations.rs)、
[类型解析](../src/core/types.rs)、[trait 检查](../src/core/ir/traits.rs)、
[泛型规划](../src/core/ir/generics.rs)与[成员选择](../src/core/ir/members.rs)。
T0 基线的 bounds 主要保存 trait 路径与身份；实现已升级为结构化约束，不靠拼接字符串表达投影等式。

## 2. 默认方法

```joky
pub trait Measure {
    fn value(&self) @noescape -> Int32

    fn doubled(&self) @noescape -> Int32 {
        self.value() * 2
    }
}

struct Counter { value: Int32 }
impl Measure for Counter {
    fn value(&self) -> Int32 { self.value }
}

fn read<T: Measure>(value: &T) @noescape -> Int32 {
    value.doubled()
}
```

trait 内的方法可只有签名，也可带 `{ body }`。空方法体 `{}` 是真实默认实现，
不能继续用“空 Block”兼作无方法体的标记；AST 必须显式区分二者。
成员仍沿用现有 trait 方法分隔规则，不把记录字段的逗号规则推广到方法列表。

- 无默认体的方法必须实现；有默认体的方法可省略，也可用同名、完全匹配的 impl 方法覆盖。
  不增加 `override`，不允许额外方法、重复方法或签名不匹配；impl 方法仍不写 `pub`。
- 默认体在 trait 的定义作用域检查，可调用本 trait 的其他要求和可见 helper；
  不能访问某个实现类型的字段，不能假定 Self 是某种具体记录。
  即使 trait 没有实现，或所有实现都覆盖该方法，非法默认体仍报错。
- 默认体的 `self.method()` 固定选择定义时的 trait 身份。对具体 Self 生成代码时，
  调用该 impl 的最终方法，包括覆盖版本；不能被实现类型的同名固有方法劫持。
  `Measure.doubled(value)` 同样选择最终实现，不提供“强制调用父默认体”的新语法。
- 覆盖方法不会获得 trait 模块的私有权限，默认体也不会获得 impl 模块的私有权限。
  trait 别名、模块限定路径与菱形导入复用声明身份，不能重复生成语义不同的默认方法。
- `self`、`&self`、`&mut self` 和无接收者形式均沿用已有静态方法规则；
  `@suspends` 必须逐项匹配，`@noescape` 按参数位置继承并检查默认体与覆盖体。
  抽象调用依赖声明契约，不能因当前默认体恰好纯净就允许未来覆盖体破坏借用保证。
  未标注的具体默认实例仍可参与现有函数体效果推导。

默认方法直接或相互递归属于普通运行时递归；注册全部最终方法后再规划函数体，
不能用递归复制 AST 的方式展开调用。泛型实例复用、深度和数量限制沿用现有机制。
内建 Drop 的特殊身份、禁止显式调用与清理责任不变，不给它补一个隐式空默认实现。

## 3. 关联类型与投影

### 3.1 声明与实现

```joky
pub trait Source {
    type Item
    fn next(&mut self) @noescape -> Option<Self.Item>

    fn skip(&mut self) @noescape {
        let ignored = self.next()
    }
}

struct Counter { value: Int32 }
impl Source for Counter {
    type Item = Int32

    fn next(&mut self) -> Option<Int32> {
        self.value = self.value + 1
        Some(self.value)
    }
}

fn take<S: Source>(source: &mut S) @noescape -> Option<S.Item> {
    source.next()
}
```

trait 用 `type Item` 声明，impl 用 `type Item = Type` 定义；可以与方法交错，
允许前向引用，每个关联类型项与紧随其后的成员以换行或分号分隔，同一行仅空白不算分隔。
每个要求必须且只能定义一次；拒绝未知项、缺项、重复项、`pub type` 和固有 impl 内的关联类型。
关联类型继承 trait 可见性，公开接口仍按现有可见性规则检查，不借投影暴露不可访问的类型。

`Item` 是所选 impl 的类型成员，不是 trait 的输入参数；同一 Self/trait 只有一组关联类型。
条件 impl、blanket impl 与孤儿/重叠规则保持不变，不能用不同的 Item 等式绕过重叠检查。
泛型 impl 的定义可引用其类型参数和 Self，类型名在 impl 定义作用域解析。
目标必须是已有规则允许的有确定大小的类型；引用和拥有者保持真实来源、存储与清理限制，
不因包进关联类型就自动获得 Copy、可跨挂起、可存入容器或 FFI 资格。

### 3.2 拼写、身份与歧义

| 写法 | 含义 |
| --- | --- |
| `Self.Item` | trait/trait impl 内，当前 trait 的关联类型 |
| `T.Item` | 从 T 的可用约束中唯一确定 Item 所属的 trait |
| `<T as Source>::Item` | 显式指定 trait 的投影；也接受具体类型和模块限定 trait 路径 |
| `T: Source<Item = Int32>` | 同时要求 Source 能力及 Item 等于 Int32 |
| `type Item: Show` | 要求实现指定的 Item 满足 Show 能力 |

`T.Item` 不是字段访问，也不是靠调用方实现猜测类型。多个约束都声明 Item 时必须用限定投影；
即使它们最终都等于 Int32，也不能任意选择其一。具体类型的简写按作用域内可见 trait
查找，歧义同样拒绝。`Self.Item` 在固有 impl 等没有当前 trait 的环境中沿用唯一候选规则。
限定投影中的 `as` 只在这一类型文法位置识别，不增加值转换表达式。

关联类型能力约束首版支持已实现的普通 trait 与 `+` 组合，例如 `type Item: Eq + Hash`；
不加入 callable bound、GAT、独立 where 子句或在此嵌套等式约束。
`Source<Item = Int32>` 的尖括号内只有具名关联类型等式，不接受位置类型实参；
允许多项及尾逗号，未知/重复名称、矛盾等式和通过导入别名重复的同一 trait 约束均报错。
等式右侧可引用在作用域内的类型参数与合法投影，不接受 `_` 作为未定类型。

投影的语义键包含基础类型、trait 声明身份和关联项身份，而非源码拼写。
刚性模板中未确定的投影保留为投影类型；通过已声明等式或具体 impl 才能规范化。
方法签名匹配、泛型推导、别名解析、类型等价、Copy/拥有者分类和布局必须共用这一规则。
不从一个投影的结果反向猜 Self，也不搜索可能满足结果的多个 impl。

### 3.3 检查与终止

定义检查先收集全部关联项，再验证绑定、能力约束与方法签名，最后检查函数体。
未使用 impl、未调用泛型及未采用的默认体同样检查。未知投影与刚性参数一样不能被当作
无引用 Copy 值；具体实例必须重新验证移动、借用与存储资格。

允许引用其他可证明的关联类型；直接/间接投影环、别名与投影混合环须报告完整依赖路径
和关联项位置。`type Item = Self` 若解析到有限的名义类型，本身不是投影环；
由字段造成的无限值布局继续交给现有布局检查拒绝。
投影规范化与 trait 证明共用有界工作预算和活跃依赖栈，首版沿用 32 层/256 义务上限，
不能每次从别名或默认实例进入就重置预算。函数实例数量另按已有上限控制。
缓存键须包含定义环境和完整实参，失败加载不得污染下一次编译或旧模块快照。

## 4. 实现分工与不变量

- 解析层区分签名/默认体、关联项定义和投影；导入与 resolver 保留定义作用域及源码位置。
- 类型层增加结构化投影及关联等式，补齐 [TypeWalk](../src/core/types/walk.rs)、替换、
  单态化、别名规范化、诊断显示和存储分类；未规范化投影不能进入具体后端布局。
- trait 层统一产生最终方法表：覆盖优先，缺项取默认；关联绑定先确定，方法签名后核对。
  默认实例键包含 trait、方法、具体 Self 与规范化绑定，循环调用复用已登记入口。
- IR 使用原有静态调用与清理流程；按值返回拥有型 Item、默认体忽略返回值、失败/取消时，
  所有资源各清理一次，不能因投影漏登记清理，也不能重复生成默认体内副作用。
- JIT、解释器和 AOT 只接收具体签名，继续执行现有浮点守卫、失败传播、CPS 与代码保活。
  关联类型不是运行时类型标签，不在普通对象中增加字段。

## 5. 开发阶段

| 阶段 | 交付 | 验收门槛 |
| --- | --- | --- |
| T0：契约与基线（已完成） | 冻结 AST、投影文法、约束身份、规范化顺序、错误与限额；盘点标准库 | 第 8 节记录冻结模型、正反例、责任表与基线证据；仅设计验收 |
| T1：默认方法 | 解析、刚性定义检查、覆盖/补齐、定义作用域、具体静态入口 | 三后端默认/覆盖/递归/效果正反例；空体与无体区分；未使用错误拒绝 |
| T2：关联类型与投影 | 声明/定义、具体及泛型投影、限定消歧、别名/签名规范化 | Copy/拥有 Item、跨模块泛型、缺项与投影环；三后端执行 |
| T3：约束与组合 | 关联项能力、等式约束、条件/blanket impl、默认体调用关联类型方法 | 歧义、矛盾、定义期与实例期检查、预算边界；缓存和身份稳定 |
| T4：生命周期与回归 | 所有权、借用、挂起默认方法、失败取消、旧快照 | 三后端行为；解释器切片/取消，受影响 Release 穷举与模块重载 |
| T5：集成发布 | 示例、标准库小规模采用、当前文档与能力表 | full + stress；适配器受影响时 Godot 检查；全部阶段有完成证据 |

T1 可独立验收；T2/T3 完成后再启动第 5 项的关联类型动态化。
标准库采用只选择确有重复逻辑的协议，不顺带重写容器或引入 Iterator/for。

## 6. 验证矩阵与文档归属

生成代码测试覆盖：默认调用抽象要求和覆盖方法、同名固有方法、定义模块私有 helper、
导入别名/菱形依赖、泛型与 blanket impl、前向投影、同名关联项限定、等式传递、
Copy/拥有 Item、Drop 次序、显式 noescape、默认挂起方法多次 Pending 且副作用不重放。
负例覆盖全部缺项/多项/不匹配、非法未使用模板、能力不满足、投影歧义/环/额度、
拥有 Item 重复消费、隐藏引用逃逸、私有权限与不支持语法。错误定位保留原模块。

遵守 [AGENTS.md](../AGENTS.md)：T0 仅写计划，检查内容、链接与 `git diff --check`，
没有运行 Cargo；T1–T5 的实际实现与验证证据见第 9 节。
实现先做三后端针对性测试；类型系统跨模块改动运行 `./scripts/check.sh --full`，
发布另跑 `--stress`。AOT 正例必须构建并独立执行，不以规划成功替代执行。

完成后 language.md 记真实语法，syntax.md 更新能力及 EBNF，architecture.md 记
投影规范化、默认实例与约束不变量；README 只登记已交付能力。每阶段追加实际命令、
平台、通过项和未验证项，不用计划中的“应通过”代替证据。

## 7. 可复用阶段任务

> 完成 docs/trait-default-methods-associated-types.md 的阶段【编号】。
> 核对前置阶段，保持定义作用域、唯一 impl、刚性模板检查和有界投影规范化；
> 默认方法仍静态分派，不借本阶段引入 dyn、GAT、特化或新的生命周期语法。
> 覆盖三后端行为与非法程序，按 AGENTS.md 验证并记录实际证据和剩余边界。

## 8. T0：契约与基线（已冻结，2026-10-07）

本节是 T1–T5 的实施契约。核对基线 `c932c0a`，工作区在核对前无未提交改动。
T0 仅完成设计与源码盘点；下列结构是拟新增模型，例子是后续阶段的验收输入，
均未声称已由当前编译器接受。没有修改编译器、运行时或标准库。

### 8.1 基线盘点与改动接线

| 基线事实 | 后续必须处理的接线 |
| --- | --- |
| `TraitDef.methods: Vec<FnDef>`，无关联类型集合；`parse_trait` 给每个签名填入空 Block | 新增显式 `TraitMethod.body: Option<Expr>`，`None` 为要求，`Some(Block)` 为默认体，包括空体 |
| `ImplDef` 只有方法与固有关联常量；trait impl 拒绝关联常量并逐项检查完整性 | 新增独立的关联类型定义集合，先绑定其类型再核对方法；常量规则保持既有边界 |
| `TypeParameter.bounds: Vec<String>`，trait 层绑定成 `Vec<u64>`，callable 独立存储 | 将普通约束升级成结构化路径/等式及已绑定身份；callable 保持独立分支 |
| Self 参数 ID 同时作为 trait 身份；导入别名复用同一 ID | 沿用这一声明身份，关联项与方法用 trait 身份加声明内序号标识 |
| `parse_type_inner` 将点路径作为 Named，`::` 已有 token，`as` 是普通标识符 | 在类型上下文识别投影并保存位置；限定投影使用现有 token，不新增全局关键字 |
| `PreparedModule::new` 先 `resolve_aliases`，再 resolver；trait 收集到 IR 规划时才进行 | 提前建立只含声明/约束/impl 头的类型环境，让别名/布局能查询投影，避免等到函数体规划才发现它 |
| `resolve_bound` 只规范化 trait 的参数与返回类型，不访问默认体 | 默认体中的局部注解、显式实参、匿名函数注解等必须按 trait 定义作用域规范化与解析 |
| trait 抽象要求进入 requirements 集合，并跳过函数体规划；具体 impl 变为普通函数 | 默认模板必须参与未使用函数体检查，不能把有体方法也放进“无体可跳过”分支 |
| `substitute`、TypeWalk、名义实例布局和泛型推导按现有 Type 变体工作 | 补齐投影遍历/替换/规范化；保留刚性检查表示，具体后端入口不允许残留投影 |

源码证据：[AST](../src/core/parser/ast.rs)、[trait 解析](../src/core/parser/declarations.rs)、
[类型解析](../src/core/parser/types.rs)、[lexer](../src/core/parser/lexer.rs)、
[PreparedModule](../src/core/prepared.rs)、[类型规范化](../src/core/types.rs)、
[布局](../src/core/types/layout.rs)、[替换](../src/core/types/specialization.rs)、
[导入与定义作用域](../src/core/imports/sources.rs)、[trait 收集](../src/core/ir/traits.rs)、
[模板检查接线](../src/core/ir.rs)及[既有 trait 用例](../src/vm/tests/language/trait_tests.rs)。
这些源码只证明当前接线和限制，不构成新功能的运行证据。

### 8.2 AST、绑定模型与身份

冻结下列逻辑结构；Rust 字段拆分可随实现调整，但不能用空表达式、拼接名称或额外隐藏
类型参数冒充这些状态。普通 `FnDef.body: Expr` 保持必需；trait 成员单独保存可选函数体，
有体成员在规划时转换为普通模板，而不是把全部普通函数的 body 改成可选。

| 模型 | 必需信息 |
| --- | --- |
| TraitMethod | 名称、签名、接收者、效果与 noescape、可选 body、声明序号、名称/签名/体 Span；作用域来自所属 TraitDef |
| AssociatedTypeDecl | 名称、普通 trait 能力列表、声明序号、名称/约束 Span；无目标类型和默认值 |
| AssociatedTypeDef | 名称、目标 Type、名称/目标 Span；作用域来自所属 impl |
| TraitBound（源码） | trait 路径、`(关联项名称, RHS Type, Span)` 列表、路径 Span |
| BoundTrait（绑定后） | trait ID、按关联项身份排序的等式、已绑定 RHS、诊断用源码位置 |
| TypeProjection（源码） | 基础 Type 或待分解的点路径、可选显式 trait 路径、关联名、源码位置 |
| Projection（绑定后） | 基础 Type、trait ID、关联项 ID；显示名称与 Span 不参与语义等价 |
| ImplHead / FinalMethod | 唯一 impl 身份、目标模式、参数约束、关联绑定；方法记录覆盖体或默认体来源及其定义作用域 |

trait/impl 中的方法与关联类型分属值/类型成员名称空间；各自重名拒绝。
关联类型名不成为裸类型名：`Item` 仍按普通类型作用域查找，当前 trait 的成员写 `Self.Item`。
关联名遵守普通类型声明的保留名称规则，不能用 Self、Fn/FnMut/FnOnce 等覆盖已有类型身份。
Self 在源码绑定时保留当前 trait 身份，不能先替换为具体目标后丢失 `Self.Item` 所属的 trait。

关联项 ID 为 `(trait ID, 关联类型声明序号)`，方法身份同理；两种序号各自按源码顺序分配。
它们在一次编译及其导入图内稳定，不承诺跨重载或跨进程相同。
菱形导入共享声明，别名只改可见名称。不要从 HashMap 顺序生成身份或把名称视为全局唯一。
类型参数 ID 继续区分同名但不同声明的参数。

约束键是 trait ID 加规范化的关联等式，源码等式顺序不影响身份。
同一参数上的同一 trait 仍只能出现一次，包括 `A + AliasOfA` 或
`Source + Source<Item = Int32>`；不把两次声明自动合并成新规则。
一个约束内部的同一关联项也只能绑定一次，即使两次 RHS 相等也报重复。
impl 唯一性仍按既有 trait/目标与 blanket 规则检查，关联绑定不是特化选择条件。

### 8.3 类型文法与名称解析

以下是新增文法片段，属于目标文法，不加入 syntax.md 的“当前可运行” EBNF：

```ebnf
TraitMethod       = "fn", Identifier, Parameters, Attributes, [ "->", Type ], [ Block ] ;
AssociatedDecl    = "type", Identifier, [ ":", TraitPath, { "+", TraitPath } ] ;
AssociatedDef     = "type", Identifier, "=", Type ;
TraitBound        = TraitPath, [ "<", AssociatedEqualities, ">" ] ;
AssociatedEqualities = AssociatedEquality, { ",", AssociatedEquality }, [ "," ] ;
AssociatedEquality  = Identifier, "=", Type ;
QualifiedProjection = "<", Type, "as", TraitPath, ">", "::", Identifier ;
ProjectionSuffix  = ".", Identifier ;
```

Type 原子增加限定投影，已解析的类型原子可接零个或多个 ProjectionSuffix；
例如 `Pair<Int32>.Item`、`<T as Source>::Item`、`T.Item.Output`。
原有函数/引用/元组和类型实参文法沿用；复杂基础可加括号，投影表达式不进入值文法。
TraitPath 沿用模块点路径，不接受普通 trait 位置类型实参。
尖括号等式列表不可为空，拒绝位置实参、`_`、缺逗号和重复逗号。
泛型参数先分配完整声明的全部参数 ID，再绑定约束 RHS，支持后声明参数如
`fn use<S: Source<Item = U>, U>(source: &mut S)`，不漏掉 callable 内的同一绑定过程。

类型位置的点路径按以下顺序绑定：

1. 根是 Self 或在作用域内的类型参数时，后续路径逐段作为投影；参数不因同名模块变成模块。
2. 否则优先尝试完整路径作为已有可见类型；若不存在，取最长可见类型前缀，余下逐段投影。
   因此 `config.Item` 仍可表示导入类型，`config.Counter.Item` 可表示该类型的关联投影。
3. 简写投影只从声明约束或可见且适用的 trait 取得候选；零候选报未知关联项，多候选报歧义。
   不按最终 RHS 相等、源文件排列或 HashMap 顺序消歧。
4. 显式限定先绑定 trait，再证明基础类型满足该 trait，并查找关联项；限定语法不能绕过能力证明。

`Self.Item` 在 trait/trait impl 中优先指当前 trait 的项；当前 trait 没有该项时直接报错，
不能悄悄转向另一可见 trait。固有 impl 中的 Self 只是目标类型，使用普通唯一候选规则。
`as` 仅在限定投影的位置按标识符拼写识别，`::` 复用现有 token；不把 as 改成全局保留字。

关联类型项与紧随其后的任何成员之间需要换行或 `;`，末项后可直接 `}`；
注释内真实换行按记录字段分隔使用的源码间隙原则处理，同一行注释不充当分隔。
方法之间仍允许原有连续 `fn` 声明，方法体由花括号结束，不机械强制换行。
不接受用逗号分隔 trait 成员；一行 `type A type B` 拒绝。

泛型记录、枚举和透明别名当前不能声明 trait 约束，本项不扩大该范围。
因此 `type ItemOf<T> = T.Item` 没有 Source 能力，定义时就拒绝；
`type ItemOf<T> = <T as Source>::Item` 也不能用限定写法补出未声明能力。
可使用已有 `type Wrap<X> = Option<X>`，再在受约束函数里书写 `Wrap<S.Item>`。
具体投影如 `type Number = <Counter as Source>::Item` 可在别名中规范化。

### 8.4 规范化、证明与规划顺序

采用一个模块编译内共享的类型环境与求解上下文，按下列依赖顺序执行；不能把“解析到身份”
和“展开到具体类型”合成一次必须成功的字符串替换。

1. **登记声明头。** 收集名义类型、别名、trait 关联项/方法及 impl 头，保存导入身份、
   源码与 DefinitionScope；建立前向查询入口，不检查方法体。
2. **绑定名称与约束。** 在各定义作用域绑定参数、trait 路径、关联等式和投影；保留刚性
   Self 与未具体化投影。拒绝重复项、未知名、非法目标形式及既有孤儿/重叠问题。
3. **规范化类型依赖。** 用按需求解展开普通别名、具体投影和关联等式；
   名义实例/字段布局需要投影时进入同一求解上下文，不能另开一个无限预算的解析过程。
   先建立所有 impl 头与关联绑定，再查询具体投影，避免源码顺序决定可用性。
4. **验证实现。** 用 impl 的参数和约束证明所有关联绑定满足声明能力；替换 Self 并
   规范化签名，核对显式方法，补齐默认方法来源，登记全部最终方法入口。
5. **检查定义体。** 刚性规划全部默认模板、impl 模板和普通泛型，即使未调用/被覆盖。
   抽象签名提供声明契约，默认体属于真实函数体，参与效果固定点与 noescape 验证。
6. **生成具体实例。** 已知 Self 与完整参数后替换、规范化、复核能力/签名/存储，
   实例化最终方法。默认体继续使用 trait 作用域，覆盖体使用 impl 作用域。
   然后进入已有布局、IR、后端编译和原子加载发布流程。

普通别名仍在其定义作用域解析；求解环境建立要早于任何需要具体投影的布局请求。
PreparedModule 的职责可拆分为头部绑定和类型规范化两个步骤，但不能等到 IR 规划
才补齐已被早期错误拒绝的别名或签名。无投影的现有输入保持原有解析/错误路径。

刚性类型的规范化结果分为“已知具体类型”和“合法未定投影”，未定不等于报错。
未定投影可作为参数、返回值或 `Option<S.Item>` 等模板组成部分；其操作只能依赖
已声明能力，不允许猜测成 Int32、无引用 Copy 或具有某种字段的记录。
模板检查若使用临时布局占位，它只属于检查，不能作为可执行布局或 Copy 证据。
具体实例按真实 Item 再检查移动、引用、容器资格、挂起资格、宿主 ABI 和清理。

约束 `Source<Item = U>` 允许将相同身份的投影规范化到 U；
具体投影从唯一适用 impl 的绑定取得目标，之后递归规范化。
`type Item: Show` 为刚性投影提供 Show 能力，默认体和普通模板可据此调用 Show 方法。
等式也可把它变成有约束的另一个参数；不能向参数添加原声明未给出的任意能力。
类型推导先从普通参数位置确定 Self 等实参，再验证投影与等式；
单凭传入 Int32 不反向推导哪个 S 的 Item 是 Int32。剩余未定参数要求显式类型实参。

投影等价先比较绑定身份，再比较规范化结果；透明别名等价，名义类型仍保持名义身份。
恒等约束 `S: Source<Item = S.Item>` 只证明自身等价，不递归展开或提供新知识；
非恒等的循环等式/关联定义拒绝，首版不通过共归纳或无限类型把环当作证明成功。
默认体中的递归调用复用登记入口，不属于类型展开环，也不通过复制默认体来展开。

### 8.5 限额与诊断契约

每次顶层“证明能力/规范化投影”请求共享一个工作预算：根深度为 0，依赖深度最多 32，
第 33 条依赖边拒绝；最多 256 个不同义务，第 257 个拒绝。
义务包括 `Implements(Type, Trait)`、`Normalize(Projection)` 和此请求中新进入的别名展开。
cache 命中不重复扣费，活跃义务仍占数量，展开途中不重置预算；相同规范化的重复子图共享结果。
成功义务在本次请求内复用，编译内环境可复用声明与实例缓存；后者不得绕过请求的深度/
依赖验证。原有普通类型语法/嵌套的 256 上限、64 类型参数、函数实例与隐藏闭包入口 4096/32 上限独立保留。
不把所有普通别名链改成统一 32 层：该共享额度针对进入 trait/投影求解的请求。

缓存键包含规范化的基础类型、trait/关联项身份、相关参数等式及定义环境身份。
缓存只属于本次编译，不用字符串键跨重载复用，不把失败求解发布到旧快照。
活跃依赖栈用于环诊断，正常完成才缓存成功结果；无解与预算耗尽必须区分。
每次类型环给出完整活动依赖链和原声明模块，不只输出最后一个名字。

沿用 [ErrorPhase/ErrorCode](../src/core/error.rs) 的已有分类，不在 T0 新增公开枚举：

| 错误 | 阶段 / 分类 | 定位及英文诊断要点 |
| --- | --- | --- |
| 文法不完整、缺分隔、不支持的 trait 泛型/GAT/关联默认值 | Parse / Generic | 实际出错 token；明确需要的 token 或未支持结构 |
| 重复方法/关联项/约束/等式、重叠 impl | TypeCheck / DuplicateDefinition | 第二处定义；包含对应 trait/关联名 |
| 未知 trait 或类型 | TypeCheck / UnknownType | 路径或目标类型 Span |
| trait 中不存在关联项、impl 多定义项 | TypeCheck / UnknownName | 关联项名称 Span；例如 `Trait 'Source' has no associated type 'Missing'` |
| 缺关联项/方法、能力/等式/签名不满足、投影歧义 | TypeCheck / TypeMismatch | impl 或调用/投影 Span；列出缺项、预期/实际类型或候选 trait |
| 关联定义或投影等式环 | TypeCheck / RecursiveType | 触发投影/定义 Span；完整依赖链 |
| 纯普通别名环、无限名义布局 | 原有分类 | 继续使用 CyclicTypeAlias、RecursiveRecord/RecursiveType；混合投影环用 RecursiveType |
| 求解/实例/类型嵌套超额 | TypeCheck / ResourceLimit，纯语法深度沿用 Parse | 触发请求 Span；明确限额与义务种类 |
| noescape、借用、移动、挂起、宿主 ABI 错误 | 原有分类 | 保留既有来源与 ABI 诊断，不统一改成关联类型错误 |

解析成功不等于能力成立；限定投影缺能力在定义期 TypeMismatch，不能等使用时才报未知布局。
一个关联项同时存在多个问题时按名称绑定、完整性、能力/等式、签名、函数体的顺序报告，
同一优先级按源码顺序，三后端前端入口一致。源码位置不参加缓存等价，报错仍使用本次请求位置。

### 8.6 存储、效果与实例责任表

| 对象/类型 | 谁保留身份与存储 | 必须验证的责任 |
| --- | --- | --- |
| 抽象要求 | trait 环境保留签名/能力；无可执行函数体 | 默认采用保守效果，显式契约才能给模板保证 |
| 默认模板 | trait 定义作用域，刚性 Self 与关联投影 | 定义期即检查，包括未使用/被覆盖；不获得实现类型字段或私有权限 |
| 最终默认实例 | 唯一 impl 与具体 Self，普通单态化代码所有者 | 身份含方法、Self、相关绑定；复用递归入口；原函数/CPS ABI |
| 覆盖实例 | impl 定义作用域，参数替换后的具体签名 | 接收者/效果逐项匹配，继承 noescape 并验证；不继承 trait 私有权限 |
| 未定 Item | 类型环境中的投影及声明能力，没有运行时存储类别保证 | 允许符号转交；不据占位布局证明 Copy/无引用/可挂起 |
| 具体 Copy Item | 原有 Copy 值存储 | 普通复制、参数/返回及数值规则，不增加类型标签 |
| 具体拥有 Item | 原有 owner 与清理登记 | 移动转移责任；忽略结果、失败、return、取消各清理一次 |
| 具体引用/含引用 Item | 原有完整来源集合与借用权限 | noescape 检查也覆盖隐藏引用；别名/投影不延长来源或增加存储资格 |
| 模块/实例快照 | 既有不可变产物及可达代码所有者 | 新实例用新绑定/默认体，旧实例用旧入口；失败加载保持原产物 |

默认体声明 `@noescape` 时，以声明中的参数位置检查；覆盖体参数改名后按位置继承。
抽象调用的保证始终来自 trait 契约，不能只从默认体推导后传播给所有覆盖版本。
`@suspends` 默认方法沿用既有挂起/取消及引用规则，禁止与 noescape 组合。
与 Drop、拥有关联返回值的组合只能使用现有所有权能力；本项不改变 Drop 的特殊规则。

### 8.7 最小验收输入与标准库盘点

以下是未来阶段输入的最小集合；完整跨后端/生命周期矩阵仍按第 6 节执行。

**T1 正例：**空默认体、抽象要求与覆盖选择必须能由一个程序观察到：

```joky
trait Measure {
    fn value(&self) @noescape -> Int32
    fn doubled(&self) @noescape -> Int32 { self.value() * 2 }
    fn noop(&self) @noescape {}
}
struct A {}
struct B {}
impl Measure for A { fn value(&self) -> Int32 { 21 } }
impl Measure for B {
    fn value(&self) -> Int32 { 1 }
    fn doubled(&self) -> Int32 { 42 }
}
fn read<T: Measure>(value: &T) @noescape -> Int32 { value.doubled() }
fn main() -> Int32 {
    let a = A {}
    let b = B {}
    a.noop()
    read(&a) + read(&b)
}
```

预期返回 84；另加同名固有方法仍不能改变 read 模板选中的 trait。
第 3.1 节 Source/Counter 为 T2 正例，补 `main` 调用 take 后核对返回值、旧来源和资源归还。
T3 最小等式正例为：

```joky
fn first<S: Source<Item = Int32>>(source: &mut S) @noescape -> Int32 {
    source.next().unwrap_or(0)
}
fn exact<S: Source<Item = U>, U>(source: &mut S) @noescape -> Option<U> {
    take(source)
}
```

以 Counter 实例调用，限定/简写投影、泛型别名包裹及跨模块同名类型分别补例。
能力正例使用 `type Item: Show`，让默认体把 `self.next()` 的拥有结果借给 Show，
再清理结果；同时覆盖 Copy 与 String。关联项声明能力也必须约束每个实际 impl。

| 最小负例（独立输入；Source 定义按第 3.1 节） | 预期 |
| --- | --- |
| `trait Bad { fn x(&self) -> Int32 { unknown() } }`，没有任何 impl | 未使用默认体仍拒绝未知调用 |
| `impl Measure for A {}` | 缺少必需 value；不能让 doubled 默认体补出它 |
| `impl Source for Counter { fn next(&mut self) -> Option<Int32> { None } }` | 缺少 Item |
| 同一 impl 中两次 `type Item = Int32`，或定义 `type Missing = Int32` | 分别为重复/未知关联项 |
| `type Item = String` 配合 `next -> Option<Int32>` | 规范化后签名不匹配 |
| `type Item = Self.Item`，或 A/B 互相展开的关联定义 | RecursiveType，包含完整环路径 |
| `fn bad<T>(x: T.Item) {}` 或没有能力的限定投影 | 定义期拒绝，不能靠未来实例补能力 |
| `fn bad<T: Left + Right>(x: T.Item) {}`，两个 trait 均声明 Item | 歧义；改用限定投影后可继续检查 |
| `Source<Item = Int32, Item = Int32>`、同一 trait 的导入别名重复约束 | 重复项，不能静默合并 |
| 实际 Item 为 String，却调用要求 `Source<Item = Int32>` 的 helper | 等式不满足 |
| Item 要求 Show，impl 选用没有 Show 的类型 | 定义期能力不满足 |
| noescape 默认体/覆盖体返回或保存受约束参数引用 | 原有借用契约错误；参数改名不能绕过 |
| 拥有 Item 被重复消费、借用 Item 逃逸、默认体跨非法挂起 | 实例复核拒绝，不能利用刚性占位通过 |
| 33 层依赖/257 个不同义务，另有边界正例与共享子图 | ResourceLimit；重复 cache 命中不虚增义务 |

标准库源码盘点如下；T0 不迁移任何协议，也不预先扩大它们的 API：

| 协议/调用方 | 基线与后续选择 |
| --- | --- |
| [Eq/BorrowEq](../src/stdlib/eq.jk) | Eq 为按值 self/other，BorrowEq 为双共享引用且 noescape；已有受 Eq 约束的 blanket 适配。T1 回归这一优先级，不改成新继承关系 |
| [Hash/BorrowHash](../src/stdlib/hash.jk) | 按值与 noescape 借用协议分离，blanket 和 String 显式实现并存；默认方法不改变现有哈希结果 |
| [Show](../src/stdlib/show.jk) | 向独占 StringBuilder 写入且 noescape；T5 可评估把重复的 to_text 包装做成默认方法，只有减少重复且保持调用兼容时才采用 |
| [Drop](../src/stdlib/drop.jk) | 编译器识别的特殊普通源码 trait；保持必需同步 drop，不增加空默认体、关联类型或动态化能力 |
| [Map](../src/stdlib/map.jk) | 消费上述 Eq/Hash 或 BorrowEq/BorrowHash；真实 Copy/引用/拥有存储资格仍独立检查，不因 Item 约束放宽 |
| [Option](../src/stdlib/option.jk) / [Result](../src/stdlib/result.jk) | 现有同步 callable 与拥有载荷路径作为组合回归；T0/T1 不把它们改为关联类型或普通 Fn trait |

### 8.8 T0 验收证据与交接

已完成：源码接线与标准库盘点；显式签名/默认体及关联项模型；投影与等式文法、
名称消歧和身份；前向/刚性/具体规范化顺序；求解限额、环与错误分类；
存储/效果/代码责任表；最小正反例及 T1–T5 交接范围。
本轮只修改本开发计划和 syntax.md 的阶段入口，检查本次文档的本地链接、代码围栏、
内容一致性以及 `git diff --check`。按 AGENTS.md 不运行 Cargo、full 或 stress。

未验证：新文法的解析结果、类型/借用实现、三后端执行、资源与取消行为、Godot、
性能及其他平台；这些属于 T1–T5 的实现验收。本次 T0 完成不代表第 4 项首版完成。
T1 从显式 TraitMethod 模型、默认模板刚性检查、覆盖/补齐和定义作用域接线开始；
T2 引入投影时必须采用 8.4 的共享类型环境，不沿用“先强制展开所有别名、后收集 trait”的顺序。


## 9. T1–T5 实施与验收（2026-10-07 至 2026-10-08）

### 9.1 前置核对与交付

实施基线为 `1d9cb42`（T0 冻结提交）；继续使用声明身份、DefinitionScope、
孤儿/重叠限制、刚性模板检查、普通泛型实例化及既有所有权/挂起规则。
没有新增 dyn、GAT、特化、trait/trait 方法泛型、关联默认值或生命周期语法。
运行时对象与普通函数/CPS ABI 未新增字段、标签或 vtable。

| 阶段 | 实际实现与对应证据 |
| --- | --- |
| T1 | TraitMethod 显式区分签名与默认体；默认体独立刚性检查；覆盖优先并补齐最终静态方法。测试覆盖空体、直接/相互递归、共享/独占/按值/无接收者、固有同名方法、定义模块 helper、未使用与已覆盖的非法体、效果与 noescape |
| T2 | 关联声明/绑定、Self/T/限定投影、前向布局、透明别名和泛型签名；投影以 trait 与关联项身份比较。Copy、String、拥有记录、嵌套投影及跨模块/别名/菱形身份均有执行或拒绝测试 |
| T3 | 关联能力、等式与后声明参数 RHS；条件/blanket impl 在统一证明入口验证等式，空泛型函数也不可绕过。共同工作栈覆盖投影基础、证明和别名；32/33 深度、256/257 义务及共享 DAG 有边界测试 |
| T4 | 默认体丢弃/失败时 Drop 一次；String Item 跨多次 Pending、取消和完成，副作用不重放且 arena 归零。失败加载保持旧模块；成功加载同时修改默认体与 Item 后，旧任务仍返回 42，新任务返回 99。解释器逐指令取消另有 Release 穷举 |
| T5 | 新增 Source 示例；Show.to_text 默认方法集中 builder 包装，自由函数及 io.print 复用，io.println 保持直接追加换行。README、language、syntax、architecture 同步；发布检查结果见下表 |

主要行为证据位于 [trait_tests.rs](../src/vm/tests/language/trait_tests.rs)、
[AOT 独立 C harness](../src/backends/aot/tests/members.rs)、
[生命周期 fixture](../tests/fixtures/trait_associated_lifecycle.jk)与
[CLI 示例测试](../tests/cli.rs)。AOT 正例实际链接并运行独立程序；非法程序也经过 AOT 编译入口。
[Source 示例](../examples/trait_sources.jk)在 JIT、解释器与独立原生程序中输出 `42`。

### 9.2 实际验证

平台为 macOS arm64，Rust `1.97.1 (8bab26f4f 2026-07-14)`；
Godot 为 `4.7.2.stable.official.ed1daf0bf`，AOT 使用本机 `cc`。
本次跨类型解析、约束与代码规划，且 T5 明确要求发布验收，因此执行 full + stress；
编译前端和标准库同时供适配器使用，补做 Godot 检查。

| 实际命令 | 结果 |
| --- | --- |
| `cargo test associated_types_bound_normalization_depth -- --nocapture` | 1 项通过；关联展开与嵌套基础均覆盖 32/33 边界，正例执行三后端 |
| `cargo test associated_types_report_binding_locations -- --nocapture` | 1 项通过；三前端核对未知目标/关联名位置与完整环依赖链 |
| `./scripts/check.sh --full` | 退出码 0，`Checks passed (full)`；fmt、Clippy 通过，Debug/Release 各 958 个单元测试、108 个 CLI、19 个模块常量、23 个源码模块测试通过；两配置的 while/Map 短输入均匹配三后端参考结果 |
| `./scripts/check.sh --stress` | 最终代码退出码 0，`Checks passed (stress)`；Release 16 个库级穷举与 5 个 CLI 穷举通过，包含新增逐指令取消用例；while/Map 默认长输入均匹配三后端参考结果 |
| `./integrations/godot/check.sh` | 退出码 0；格式、Clippy、7 个 Rust 测试、扩展构建及 G0/G1/G2 编辑器/运行/异步回归通过；诊断负例的预期 ERROR 输出不代表脚本失败 |
| 文档本地链接、代码围栏及 `git diff --check` | 通过 |

full 的默认运行各忽略 19 个库测试和 5 个 CLI 测试；stress 显式运行其中的 16 + 5 个穷举，
其余非穷举诊断/测量探针未运行。最终 full、stress、Godot 均通过后仅更新文档，未再修改代码。

回归过程中发现并修复：完整点分宿主类型被误拆为投影、前向类型前缀未纳入查询、
非法类型参数应用的错误阶段变化、Self 后续投影错误沿用当前 trait，以及相邻嵌套尖括号解析。
别名实例缓存进入求解时重放展开，不利用编译内缓存绕过本次请求的依赖额度。
布局依赖遍历保留新规范化类型的所有者，避免临时 Arc 地址复用影响按指针去重；
新增双投影元组字段的三后端执行用例，并验证未知关联目标的位置与投影环路径。

### 9.3 保持的边界与未验证项

- 默认方法仍静态分派；不提供调用“父默认体”的语法。类型参数命名空间仍遵循既有规则，
  无接收者要求使用 `Trait.method::<Self>(...)`，不能写 `Self.method()`。
- 普通用户拥有型记录仍不能跨普通挂起。生命周期正例以 String Item 跨 Pending；
  带 Drop 的用户记录验证同步丢弃/失败清理，不以此声称拥有记录挂起能力已开放。
- 投影不改变引用来源、容器存储资格、FFI、Copy 或挂起资格；后端不接收未确定投影。
- 泛型记录、枚举、透明别名声明仍不能带普通 trait 参数约束；无能力的投影别名在定义时拒绝。
- 检查证明当前平台的语义和资源回收行为，不包含性能测量、其他操作系统/架构或动态分派验收。
