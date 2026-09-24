# 迭代协议：Cursor 与 for 泛化

当前语法见[循环与游标](../lang/iteration.md)，实现见[有界迭代](../runtime/bounded-iteration.md)。本文保留历次设计修订；历史兼容接口与任务描述以当前 API 文档为准。

> 状态：设计修订 v6（2026-09-21），阶段 0、1、2、3、4 已实施。
> D4 的泛型 effect 边界与 D5 的 lowering 已落实。
> v4：局部 `var` 落地后的修订——D1 更新可变状态现状，D3 补 `var` 累加语义，
> D5 取出游标统一为 `TakeLocal` 槽位转移（Shared 不再 `Read` + `Dup` + `DropLocal`）。
> v5：循环状态交给 `local_ssa`；顺序 `for` 改用 `SeqBuilder` 尾链接收集，
> `@parallel` 仍走 Batch。D7 的 Bytes / Map / Set 游标已落地。
> v6：D8 SQLite 类型化行与标准 Cursor 已落地，补齐 provider 所有权、取消和清理。

## 现状与动机

设计前，`for item in xs { body }` 是仅接受 `List(T)` 的收集表达式：`check_for`
在 sema 硬性要求 List（`src/sema/control_flow.rs`），MIR lowering
（`src/mir/lower/for_loop.rs`）顺序与 `@parallel` 都走 Batch 协调器
（`BatchNew` / `BatchNext` / `BatchPush` / `BatchFinish`），runtime
（`crates/joky-runtime/src/runtime/batch.rs`）的游标就是输入 List 的剩余
cons 节点指针。也就是说迭代驱动在 MIR/runtime 层已经是游标协议，只是
形状写死为 List。

阶段 1 已引入 `Cursor` 与自定义游标的顺序 `for`。Map/Set 尚无
`keys`/`values`/`entries`，
MutList 只有 `get(index)`，String 无字符迭代（无 Char 类型），无 Range
类型。[原待办记录](../archive/plans/completed-work.md)曾把 sqlite 查询游标推迟到迭代协议稳定后，
是本设计的直接需求方。

## 评审修订摘要

| 问题 | 代码依据 | 当前处理 |
| --- | --- | --- |
| 泛型 trait 约束原先只查是否实现 trait，额外 effect 可被隐藏 | `check_bound_method` 对 `Type::Param` 只取 trait 声明的 `effect_names`；`implements_trait` 不比对 effect | 阶段 0 已补充 impl 与 trait 的声明 effect 比较；具体实例及转发路径统一复验（D4） |
| `advance(self) -> Option((Item, Self))` 要求返回 `Self`，容器无法返回"句柄 + 索引"；`Bytes` 返回剩余切片会因 `jk_bytes_slice` 复制退化为 O(n²) | `crates/joky-runtime/src/runtime/bytes.rs:181` | 协议改名 `Cursor`，除 List 外的容器由 `iter()` / `entries()` 等产出独立游标类型（D1、D7） |
| 消费式 `advance` 与"遍历时修改 MutList、追加可见"冲突；借用游标不能存字段、不能跨挂起 | `1-syntax.md` 第 219、538、547 行 | 可变容器首版只提供消费式 `into_iter()`，撤回实时修改承诺（D7） |
| v2 把无外泄 effect 等同于不会挂起，并承诺现有泛型测试零变化 | `mir/suspending_analysis.rs` 推导 Pending；`tests/cli/aot.rs` 已覆盖 trait 无 effect、impl 挂起 | 保留 MIR 挂起推导；明确泛型 effect 规则收紧与已有测试迁移（D4） |
| v2 把所有游标当成 Owned，统一使用 `TakeLocal` / `Deinit` | `mir/verifier/locals.rs`、`ownership_statements.rs` 只允许对 Owned 使用这些操作 | 按 Copy / Shared / Owned 分别取出和拆解；补 tuple 的 `Deinit` 及混合所有权测试（D5） |
| 表格把具体接收者的 `Trait.method(x)` 归为 trait effect | `qualified.rs:128-142`：只有 `Type::Param` 走 `check_bound_method`，具体接收者转回 `check_method_call` | 表格修正（D4） |
| sqlite 耗尽、错误与清理方式未分清 | 查询 API 返回 `Result`；语言层 Drop 禁止挂起，但 `sqlite.finalize` 标为 `@suspends` | `Item = Result(Row, String)`，错误后耗尽；原生句柄最后一个引用释放时 finalize（D8） |
| `for (k, v) in map.entries()` 被当成键值解构 | `syntax/parser/control.rs` 将两个名字解析为 index / item | 使用 `for entry in map.entries()`，体内读取 `entry.0` / `entry.1`（D1、D3） |

## 设计决策

### D1 协议：内建 trait `Cursor`，消耗式推进；容器不是游标

```joky
trait Cursor {
    type Item
    fn advance(self) -> Option((Item, Self))
}
```

`advance` 按值接收游标，返回一个元素加剩余游标，`None` 表示耗尽。语言没有
`&mut`：任务私有的循环状态（计数、累加）用局部 `var`
（[local-var.md](../archive/plans/local-var.md)）表达，跨任务共享仍使用 class 内部可变性或
Cown。局部可变绑定没有引入可跨挂起点保存的可变借用——借用不能存入字段、
不能返回，普通 class 借用不能跨挂起——因此协议仍为自持状态的消耗式游标，
不引入长期借用或跨挂起的 receiver 借用。按值传递不强制游标为 move-only：
其 MIR ownership 仍可为 Copy、Shared 或 Owned，分别按 D5 处理，非借用状态
可 spill 过挂起。方法名用 `advance` 而非 `next`，避免与 Rust
`Iterator::next(&mut self)` 混淆。`var` 不改变这一选择：它让"接收下一份
游标状态"的手写循环更自然，但不为游标提供 `advance(&mut self)` 形态的
可变 receiver。

命名为 `Cursor` 而非 `Iterable`：实现者是"遍历状态"，不是"可被遍历的
容器"。因为 `advance` 必须返回 `Self`，List 可直接沿用现有剩余链表表示
（`advance` 即 `head`/`tail`，无需额外游标对象）；其他容器由方法产出独立
游标类型（D7）。`List(T)` 由编译器注册为自己的 `Cursor`，`for x in list` 不变。

`for` 对尚未实现 `Cursor` 的容器做 IntoCursor 转换：Map / Set 走
`entries()`，`MutMap` / `MutSet` / `MutList` 走消费式 `into_iter()`。
关联类型约束 `where T.Item: Trait` 已落地，用户自定义 `IntoCursor` 可以
再按需加；首版不引入新的源级 trait，只让现有 std 容器能直接写
`for entry in map`。条目解构用循环内的 `match entry { (k, v) => ... }`。

返回 `Self` 不满足 `Dyn` 对象安全（`Self` 只能出现在 receiver），首版
排除动态分派；需要时另行设计，不阻塞主线。

### D2 `for` 保持编译器驱动，不脱糖为库调用

语言禁止从闭包 `break` 到外层循环，内部迭代（`for_each(f)`）做不了提前
退出，排除；`@parallel` 需要编译器重构循环，也排除库实现。lowering 分
三条路径：

| 输入 | 路径 |
| --- | --- |
| `List(T)` 且 `@parallel` | 保留现有 Batch 协调器（`BatchNew` / `BatchNext` / `BatchPush` / `BatchFinish`），短锁领取与结果按输入序排序 |
| 顺序 `List(T)` | 与其他游标共用 Cursor lowering；`advance` 内联为 head/tail，结果走 `SeqNew` / `SeqPush` / `SeqFinish` 尾链接，无锁、无排序 |
| 其他 `Cursor` 实现 | `For` 节点保留到 MIR，按 D5 的所有权协议调用 `Cursor.advance`；循环骨架、per-iteration branch 排空与 `SeqPush` / `SeqFinish` 复用 |

`check_for` 放宽为：输入是 `List(_)`，或 `implements_trait(input, "Cursor")`
为真，或可做 IntoCursor 转换；否则诊断 `for requires a List, a Cursor implementation, or an IntoCursor container`。
`@parallel` 输入非 List 时单独诊断（D6）。

备选是 HIR 完整脱糖为 `loop` + `break value` + 累积器，MIR 零改动，但收集
语义与 branch 排空需要重新证明。首选保留 `For` 节点；两者不互斥。

### D3 `for` 语义不变

- 仍是收集表达式：正常完成的迭代收集尾值，输出 `List(体类型)`；`continue`
  不产生结果；顺序 bare `break` 返回已收集前缀；不接受 `break value`。
- 输出收集可以用尾部 `continue` 归零（见
  [bounded-iteration.md](../runtime/bounded-iteration.md)）。对游标输入
  这是关键的流式形态：逐项拉取、逐项处理、零输出节点；bare `break`
  提前停止拉取。输入侧从不具体化。
- 局部 `var` 承担任务私有的累加、计数状态，不需要 class 包装。赋值表达式
  返回 `Unit`，因此累加循环只有配合尾部 `continue` 才是零输出的；仅把
  赋值放在循环末尾仍会收集 `List(Unit)`，保留每项一个输出节点的成本
  （当前示例见[循环与游标](../lang/iteration.md#顺序-for)）。
- `(index, item)` 是驱动层的 enumerate（`UInt64` 计数），对任意 `Cursor`
  通用，不要求输入有序或可索引。它不是 item 的 tuple 解构：
  `for (index, entry) in map.entries()` 中 `entry` 的类型仍为 `(K, V)`。
  首版不增加 `for` 的模式解构语法。
- item 绑定不受 List 元素限制，可以是唯一所有权 class；只有收集的体结果
  类型沿用现有 List 元素规则。

### D4 effect：具体游标按 impl 传播，泛型边界在实例化处检查

`advance` 可以声明 effect（`effects { ... }` 是现有 trait 方法语法）。
具体类型的 for-site 按 impl 的 `used_effects` 向外传播，与具体接收者的
普通方法调用一致；尚未检查方法体的前向引用保守使用 impl 声明 effect，
避免漏掉后续定义的 operation。泛型接收者则按 trait 声明检查。生成的 `advance` 调用
必须进入现有 MIR 调用图，由挂起分析传播 Pending 能力，不在 sema 方法
签名中假定已有 `suspends` 字段。由此 sqlite 游标（`effects { sqlite }`）
可直接被具体 `for` 迭代。

现行 effect 来源（代码核实，修正 v1 表格）：

| 调用形态 | effect 来源 | 代码 |
| --- | --- | --- |
| 具体接收者 `x.m()` | impl 的 `used_effects` | `src/sema/calls/methods.rs` |
| 具体接收者的限定调用 `Trait.m(x)` | 同上：转回 `check_method_call` 按 impl 检查 | `qualified.rs:128-142` |
| 泛型接收者 `T: type + Trait` 的 `x.m()` / `Trait.m(x)` | trait 声明的 `effect_names` | `check_bound_method`，`qualified.rs:181` |
| 普通非 static 用户 trait 方法的 impl effect 上界 | 泛型具体实例检查声明 effect 子集；`Dyn` 装箱、static 方法及部分内建 trait 保留各自已有约束 | `sema/instances.rs`、`sema/calls/generics.rs` |

阶段 0 修复了第四行原有的漏洞：模板按 trait 上的空 effect 检查时，
带 effect 的具体实现可能隐藏额外 effect。这是已有泛型 effect 契约的
缺口，不是 Cursor 首次引入的问题。`Cursor` 声明不携带任何 effect 组
（std 命名不了模块自定义的 effect 组），后续 `T: type + Cursor` 将复用
这一检查，拒绝把声明了 sqlite effect 的游标作为泛型实参。

外泄 effect 与挂起是两套机制。泛型实例化后的挂起已经由
`mir/suspending_analysis.rs` 推导，跨模块泛型实例的 ABI 也允许 Pending
（`sema/instances.rs`）。没有外泄 effect 的实现仍可能因为内部 task wait
或内部已处理的 effect 而挂起；本设计不增加 trait 的同步契约。

**阶段 0（前置）：泛型实例化 effect 检查。** 对每个具体类型实参
`T := C` 与其约束 `Tr`，遍历 `Tr` 的每个非 static 方法 `m`，要求：

```text
declared_effects(C.m) ⊆ declared_effects(Tr.m)
```

比较对象明确为声明 effect，展开成已解析的 operation 身份后检查，不比较
方法体检查过程中尚未完整填充的 `used_effects`，也不按 effect 的显示名称
匹配。即使方法体没用到自己声明的 effect，该声明仍必须满足上界；这是首版
有意采用的保守规则。违反时报 trait bound 诊断，列出方法与多出的 effect。

检查分为记录义务与具体实例复验：

1. `sema/calls.rs` 记录泛型调用时保留每个实参与 trait bound 的检查义务。
   已具体化且声明齐备的调用可以提前诊断；实参仍为 `Type::Param` 时不得
   将其视为已经检查过的具体实现。
2. 所有方法与函数体检查结束后，在 `prepare_instance_types` 的具体实例
   工作队列中统一验证；嵌套调用替换出新的具体实参时同样验证。导入模板的
   实例化路径须按 trait 与具体 impl 各自的模块身份解析声明，完成相同检查，
   不能跳过跨模块调用。
3. static 方法保留已有规则。非 static 检查适用于所有 trait，按约束中的
   全部方法检查，不仅检查模板实际调用的方法。编译器注册的 impl 同样提供
   声明契约：已知无 effect 的实现通过，未来带 effect 的内建游标没有豁免。

结果：`T: type + Cursor` 只接受 `advance` 未声明外泄 effect 的游标，但允许
内部挂起，并继续使用现有 MIR 分析和 Pending ABI。声明了 sqlite 等 effect
的游标可被具体 `for` 消费；effect 多态的泛型迭代组合子暂不提供。

这是对已有泛型规则的收紧，原先依赖 effect 被隐藏的代码需要补充 trait 声明。
`tests/cli/aot.rs` 的 `jit_and_aot_resume_concrete_trait_impls_and_owned_results`
与 `jit_and_aot_cancel_generic_trait_frames_and_drop_the_receiver_once` 原先
使用无 effect 的 `Work` trait 搭配带 `time` effect 的 impl。阶段 0 已为其
trait 方法补 `effects { time }`，保留恢复、取消和析构验证，并新增 trait
声明缺少 effect 时拒绝实例化的负测。

跨模块方法元数据分别保存声明 effect 与实际使用的 effect，声明参与 ABI
哈希，使仅修改 effect 声明也会使调用方缓存失效。回归覆盖同名但不同身份
的 effect、同一 effect 的导入别名、未使用的声明，以及纯实现内部 task wait
在冷/热 JIT、debug/release AOT 中的恢复。

### D5 所有权：消费式游标的 lowering 协议

非 List 路径用循环作用域的 `<for-cursor>` local 保存下一轮状态，item 绑定
属于本轮作用域。local 与调用返回值的 ownership 均由实际类型决定，不能因
receiver 为 `self` 就一律标成 Owned。游标状态留在 MIR local 中；结果收集
独立初始化，不能把非 List 输入传给现有 `BatchNew` 的 List 参数。

循环状态（游标与索引）存在普通 local 中，由 `local_ssa::normalize`
（[local-var.md](../archive/plans/local-var.md) 阶段 0）在 CFG 合流点合成 Phi 与边转移。
lowering 不再手工构造循环头 Phi 或回边参数。`break` 时剩余游标的释放、
每轮 branch 排空与返回值保护仍由 lowering 显式处理，不能交给 SSA 规范化
一并删掉。顺序路径的结果收集器是独立的 `SeqBuilder`（尾指针链接
singleton 节点，O(1) 追加、不复制 payload）；并行路径仍用 Batch 的
索引排序。不得改成每轮 `ListCons` 到不可变前缀上，那会引入反复复制。

**取出游标。** 每轮先做取消检查，再按 `Self` 的 ownership 生成调用参数：

| `Self` ownership | 从 `<for-cursor>` 取出 | 调用期间 local 状态 |
| --- | --- | --- |
| Copy | `Read`，复制按值参数 | 槽位保留旧副本，无清理义务；下一轮 `Bind` 覆盖 |
| Shared | `TakeLocal` 转移槽内引用 | local 未初始化；原引用交给被调方 |
| Owned | `TakeLocal` | local 未初始化；所有权交给被调方 |

v4 起 Shared 与 Owned 统一为 `TakeLocal`：verifier（`mir/verifier/locals.rs`）
只对 Borrowed 拒绝该操作，codegen 是纯槽位移动，不触碰引用计数，Shared
游标每轮省去 `Dup` + `DropLocal` 的一对增减，且用户读取 Shared 变量的语义
不变。Copy 保持 `Read`：它没有清理义务，保持槽位在耗尽与 `break` 出口的
合流处一致初始化。`local_ssa::merge_local` 的 Shared 边转移同步使用
`TakeLocal`，使所有跨合流的 Shared 局部（含用户 `var`）沿边转移时不做
引用计数增减。

`advance` 返回值在任何取消检查前必须被清理机制保护，复用
`lower_task_poll_result` 的返回值暂存协议；成功恢复后再检查 Option tag。
None 与 Some 分支分别按实际 aggregate ownership 处理。

**拆解返回值。** `Option((Item, Self))` 与内部 tuple 的 ownership 取决于
两种成员类型，不能只看 `Self` 或 `Item`。

| 返回 aggregate ownership | Some 分支 | None 分支 |
| --- | --- | --- |
| Copy | 普通投影 item / next，无 `Dup`、`Drop` 或 `Deinit` | 无清理义务 |
| Shared | 投影 pair 及成员；对每个 Shared 成员 `Dup`，Copy 成员直接取值；最后 `Drop(opt)` | `Drop(opt)`，没有有效 payload |
| Owned | 同一基本块内移出 pair，再投影全部成员；Owned 成员移出、Shared 成员接管原引用、Copy 成员取值；`Deinit(pair, None)`、`Deinit(opt, Some(0))` | `Deinit(opt, Some(1))`，没有有效 payload |

Shared 路径中的 pair 与成员在 `Dup` 前只是别名，不能再单独 drop pair，
否则与释放 opt 重复。Owned 路径不 drop 已拆解的 aggregate，Shared 成员
因此可以直接接管原引用，不需要额外 `Dup`。`Deinit` 是 verifier 的所有权
结束标记，当前 codegen 不生成运行时释放代码，不表示释放一个堆分配的壳。
两个 aggregate 都要结束其 move protocol；不得在首次移出与两个 `Deinit`
之间插入挂起、取消分支或其他 CFG 边。

以下伪代码仅展示 **Self 与返回 aggregate 均为 Owned** 的路径，其余情况
按上表替换。`Deinit` 的第二个参数是可选 variant 编号：tuple 用 None，
Option 的 Some / None 分支分别用 `Some(0)` / `Some(1)`。

```text
head:
    poll
    cur  = TakeLocal(<for-cursor>)
    raw  = Call Cursor.advance(cur)
    opt  = lower_task_poll_result(raw)    // 取消时清理完整返回值
    tag  = EnumTag(opt)
    Branch tag == None -> exhausted, else -> unpack
unpack:
    pair = EnumProject(opt, Some, 0)
    item = Project(pair, 0)
    next = Project(pair, 1)
    Deinit(pair, None)
    Deinit(opt, Some(0))
    Bind(<for-cursor>, next)
    Bind(<item-local>, item)
    Bind(<for-position>, index)
    poll
    body ...
    collect_if_completed / drain_iteration / Goto head
exhausted:
    Deinit(opt, Some(1))
    Goto finish                          // cursor local 无清理义务
break_cleanup:
    drain_iteration
    cleanup_iteration_locals
    DropLocal(<for-cursor>)
    Goto finish
finish:
    SeqFinish / BatchFinish
```

复用现有 per-iteration branch 排空和结果收集规则。耗尽与 `break` 不直接
合流：前者已交出游标，后者仍持有 next，必须分别完成清理后再进入共同的
finish。Owned / Shared item 只在仍由本轮 local 持有时释放；若用户已经移交
所有权，则由接收者负责，不能无条件再次 drop。

各种退出路径的析构义务：

| 退出 | `<for-cursor>` | 当前 `item` | 说明 |
| --- | --- | --- | --- |
| 耗尽（`None`） | Owned / Shared 已交出，无义务；Copy 旧副本无需释放 | 无 | 按上表清理 opt |
| bare `break` | 释放本循环持有的 next；Copy 无需释放 | 清理仍持有的绑定 | 停止拉取；资源最终释放规则见 D8 |
| `continue` | 保留 next，进入下一轮 | 清理仍持有的绑定 | 先排空本轮 branch |
| body 内 `?` / abort | 清理 next | 清理仍持有的绑定 | 走现有提前返回 / abort cleanup |
| `advance` 期间取消 | 被调方或 provider 清理已接管的参数；for 侧无资源引用 | 无 | Copy 旧副本无清理义务；返回值交接复用调用结果保护 |
| body 期间取消 / 挂起后恢复 | next 随 continuation 保存；取消时清理 | 保存或清理仍持有的绑定 | 按各值的 ownership 处理 |
| 输出收集 | 顺序 `SeqPush` 接管 singleton；并行 `BatchPush` 记索引 | — | — |

所有权与资源测试矩阵（阶段 1 验收，不是可选项）：

- Self × Item 的 Copy / Shared / Owned 组合均须覆盖。至少包括整数 struct
  游标 + 整数 Item、含 Bytes 的 Shared struct 游标 + String Item、Copy /
  Shared 游标产出计数资源 class，以及持有计数资源的 Owned class 游标。
- 退出方式覆盖耗尽、第 k 轮 `break`、全 `continue`、body `?`、body abort、
  body 挂起中取消。可挂起游标另测 `advance` 期间取消及返回值交接时取消；
  同步游标无需构造不存在的 `advance` 挂起点。
- 可挂起游标分两种：通过声明的测试 effect 调用 `@suspends` operation，
  用具体 `for` 验证 effect 传播；无外泄 effect、内部 task wait 的游标，
  同时验证具体与泛型遍历的 Pending 恢复。
- 按每个实际构造的资源对象断言恰好释放一次；Shared 引用计数回到预期基线，
  Copy 值无析构义务。消费式 struct 可逐轮构造新状态，不要求所有状态对象
  合计只析构一次。按需产出资源的测试还须证明提前退出不构造后续 item；
  本次遍历持有的资源最终归零，尾部 `continue` 不分配输出节点。

### D6 `@parallel` 首版仅限 List

协调器的共享游标（裸 cons 节点指针）在短 Mutex 内用 `jk_list_tail`
前进；泛型 `advance` 会执行用户代码甚至挂起，不能在持锁期间发生；结果按
输入序排序也依赖顺序稳定。不是"需要知道总量"：长度只用于
`min(limit, length)` 的 worker 数。后续泛化方向是把游标移入协调器并提供
受控 advance 回调。`@parallel` over 非 List 输入报诊断。

### D7 内建容器游标（暂定，阶段 2 / 3）

impl 目标只解析裸标识符，内建类型不能作为 impl 目标，因此内建游标类型
的 `Cursor` 走编译器内部注册表（与 `PartialEq` / `Hash` 对内建容器的处理
同一模式）。容器方法产出游标；游标类型是新的 `@intrinsic struct`，需要
在 std 声明中列出。

**不可变容器（阶段 2）**

| 容器 | 产出方法 | 游标 | Item | 每步 | 游标持有 |
| --- | --- | --- | --- | --- | --- |
| `List(T)` | 无需（自身即 `Cursor`） | `List(T)` | `T` | O(1) | 剩余 cons 节点 |
| `Map(K, V)` | `entries()` / `keys()` / `values()` | `MapCursor(K, V)` 等 | `(K, V)` / `K` / `V` | 摊还 O(1)，最坏 O(log n) | 根节点 dup + 深度 ≤ log n 的路径栈；整张 Map 存活至游标析构 |
| `Set(T)` | `iter()` | 同 Map 游标 | `T` | 同上 | 同上 |
| `Bytes` | `iter()` | `BytesCursor` | `UInt8` | O(1)，不复制 | `Bytes` 句柄 dup + 偏移 |
| `String` | 首版不做 | — | — | — | 无 Char 类型 |
| `Range(T)` | 范围表达式，自身即游标 | `Range(T)` | 整数 | O(1)，无堆分配 | 当前位置、终点、步长和边界/耗尽标志 |

每个游标类型的文档必须写明上表的"每步"与"持有"两列，作为阶段 2 验收。

**可变容器（阶段 3）**

`MutList` / `MutMap` / `MutSet` 是唯一所有权对象。`for` 输入表达式被消费，
所以"遍历中修改原容器、追加可见、删除跳过"在类型系统内不可表达：循环体
里已经没有 `xs` 可用。借用游标也不成立（借用不能存字段、不能返回、不能
跨挂起）。首版方案：

- `into_iter()` 消费容器，产出 `MutListCursor(T)` 等：拥有容器 + 索引，
  每步 `get(i)` O(1)，游标析构时释放容器。遍历后容器不可再用，这是所有权
  规则的自然结果，无需另写语义。
- 需要遍历后继续使用容器时，先 `to_list()` 拿不可变快照，再对 List 遍历；
  O(n) 复制，语义清晰。
- `retain(&self, keep)` 原地留下谓词为真的元素；编译器按 `sort` 同样方式
  展开，不走 C 回调。
- 借用式遍历、遍历中修改：不在本设计范围，等借用规则允许借用值存入字段
  或有 `&mut` 后单独设计。v1 的实时索引承诺撤回。

### D8 sqlite 游标的错误与资源生命周期（已实施，阶段 4）

`Option` 只表达耗尽，不表达失败。现有 `std/joky/sqlite.jk` 的查询与执行
API 使用 `Result(_, String)`，游标沿用其错误表示：

```joky
struct SqliteRows {
    let statement: Option(SqliteStatement)
}
```

- `SqliteRows` 是普通 Owned struct，不增加编译器内建类型；在标准模块中
  `impl Cursor`，`Item = Result(SqliteRow, String)`。`statement.query()?`
  仅转移语句，不 step；耗尽状态为 `statement: None`。
- `advance` 在 blocking pool 执行 `sqlite3_step`，声明 `effects { sqlite }`
  并挂起，可被具体 `for` 消费，不能作为 `T: type + Cursor` 的实参（D4）。
- 出错时返回 `Some((Err(message), rest))`，`rest` 处于耗尽态，再次
  `advance` 返回 `None`；用户可用 `?` 或 `match` 处理。
- 清理复用 runtime 原生句柄析构，不以用户 `impl Drop` 调用 sqlite effect。
  语言层 Drop 只允许同步、非 abort/resume 的 effect，而现有
  `sqlite.finalize` 标为 `@suspends`，不能从用户 Drop 中调用。
- `SqliteRows` 持有的 statement 句柄持有 `Arc<SqliteStatement>`；已获准的 blocking
  工作在执行期间也持有独立引用。耗尽、`break`、`?` 或取消释放游标侧的
  句柄引用，最后一个引用释放时，由 runtime 的 `SqliteStatement::drop`
  把 `sqlite3_finalize` 加入 blocking 清理队列；耗尽或显式 finalize 则在
  当前 worker 完成。每条语句恰好一次，用户无需显式 finalize。
- 取消不等于正在执行的 `sqlite3_step` 已经退出。尚未执行的请求取消后
  释放排队参数；已执行的请求保留工作侧引用至完成，不能提前 finalize。
  因此资源归零在 provider 工作排空后验收，不要求取消请求发出时立即归零。
  迟到完成不得恢复已取消代码；若结果未交付，producer 必须释放 Row、错误
  字符串、返回游标及其 managed handle，不能只释放 native statement 的 Arc。
- `SqliteRow` 必须独立于下一次 step 和语句 finalize 存活，允许用户保留或
  收集结果。首版在 worker 中物化单行数据，再把拥有数据的 Row 返回；不暴露
  SQLite 临时列指针或依赖“当前行”的借用视图。`SqliteRow` 为持有
  `List(SqliteValue)` 的普通 struct，值覆盖 Null / Integer / Real / Text /
  Blob；getter 返回 `Result(Option(T), String)`，区分 NULL、类型不符和越界。
  不要求物化整份查询结果。API、复杂度和兼容迁移见 [SQLite](../stdlib/sqlite.md)。
- 所有操作进入 blocking pool；同连接的 step 与列复制、execute 与 changes64
  读取在同一锁区间完成。工作和自动清理持有独立 scope lease，作用域退出
  等待全部排空；`close` 消费连接引用，现有语句可延迟物理关闭。
- `bind_text` / `bind_i64` 改为返回 `Result(SqliteStatement, String)`，新增
  NULL / Float64 / Bytes 绑定。provider 校验操作签名和行布局；runtime ABI
  升至 17，JIT 与 AOT 同步使用新契约。

## 与备选方案的取舍

- **Rust 式双 trait 分层**（`IntoCursor { type Cursor }` + `Cursor`）：关联
  类型约束已落地。首版仍不引入源级 `IntoCursor` trait，只对 Map / Set /
  可变容器做编译器适配；用户自定义转换器以后可以再加。
- **容器直接实现协议**（v1）：首版只让 List 沿用现有剩余链表表示。
  Map 可另行设计“每步移除条目”的剩余容器，但不同于路径栈游标；Bytes
  反复复制尾切片会退化为 O(n²)。其他容器统一使用独立游标表示。
- **内部迭代 `for_each(f)`**：与"不能从闭包 break 到外层循环"冲突，排除。
- **借用式游标 `advance(&self)`**：可以描述部分同步 class 游标，但普通
  receiver 借用不能跨挂起，借用容器也不能存进游标字段，不作为首版统一协议；
  `Dyn` 变体日后若需要，再评估。
- **游标走显式 `while` 或 `collect()`**：`while` 手写就是 `for` 的脱糖
  形态，每次重复样板；`collect()` 强制全量具体化，与游标初衷相悖。

## 任务

- [x] 0. 泛型实例化 effect 检查（D4）：impl 声明 effect ⊆ trait 声明；
  对非 static 方法补检查，static 保留已有规则；记录义务并在具体实例工作
  队列与导入模板路径复验。补正反测试，迁移现有 `Work` trait 测试声明，
  保留 MIR 挂起推导。独立提交，不依赖后续任务。
- [x] 1. 协议与 List：注册内建 `Cursor`（sema trait 表 + `check_for` 放宽
  + List 内建实现），现有 List 测试零变化；用户 struct/class 可
  `impl Cursor`；非 List 路径按 Copy / Shared / Owned lowering（D5），
  独立初始化结果收集器；所有权与资源测试矩阵全绿；具体 for-site 传播
  impl effect，调用图传播挂起；无外泄 effect 的可挂起游标可用于泛型遍历。
- [x] 2. 不可变容器游标：`MapCursor`（`entries` / `keys` / `values`）、
  Set `iter`、`BytesCursor`；每个游标写明每步复杂度与持有资源。
  - [x] 2a. `BytesCursor`（`bytes.iter()`）（v4 后）：`Item = UInt8`，
    每步 O(1)、原地前进零分配、不复制内容；游标为 Shared managed 对象，
    持有 Bytes 的一个引用，耗尽 / `break` / 取消时释放，原值不受影响。
  - [x] 2b. Map / Set 游标（v4 后）：`entries()` 的 `Item` 为 `(K, V)`，
    `keys()` / Set `iter()` 为 `K` / `T`，`values()` 为 `V`。游标是唯一
    所有权 managed 对象，持有容器引用 + 深度 ≤ 树高的 DFS 路径栈；
    `advance` 原地回退并下探，摊还 O(1)、最坏 O(log n)；产出侧按投影
    只克隆请求的一半共享引用。遍历顺序为哈希序（进程内确定，字符串键
    跨运行可能不同），文档已写明不承诺顺序。Set 在类型层是
    `Map(T, Bool)`，    与 Map 共享方法表，`iter()` 即键游标。回归覆盖
    compiler / CLI（含 String 键、break、空容器、AOT 与资源排空）。
- [x] 2c. 顺序收集器：顺序 `for`（含 List）使用 `SeqBuilder` 尾链接；
  `@parallel` 保留 Batch。循环状态交给 `local_ssa`，保留 `break` 清理、
  每轮排空与返回值保护。
- [x] 3. 可变容器：`into_iter()` 消费式游标；`to_list()` 快照；
  `retain` 原地过滤；语言说明写明"遍历消费容器"。
- [x] 4. sqlite：完成 D8，关闭[原待办记录](../archive/plans/completed-work.md)对应项。
  - [x] 4a. provider 正确性：操作锁、changes64 ABI、绑定校验、blocking
    执行、取消所有权、迟到结果清理及 shutdown 排空。
  - [x] 4b. 类型化行与 NULL / Float64 / Bytes 绑定，保留 query_one 兼容接口。
  - [x] 4c. SqliteRows 接入标准 Cursor，惰性 step、错误后耗尽、独立行快照；
    修复 continuation 同步结束时循环状态复用，以及移出 Owned 字段时
    Shared 同级字段未释放的问题。
  - [x] 4d. 冷/缓存 JIT、debug/release AOT、资源计数、排队/执行中/拒收
    取消测试、示例与文档。
- [x] 5a. `Range(T)`：`a..b`、`a..=b`、可选 `by step`，复用顺序 Cursor。
  八种整数、极值耗尽、跨模块泛型、冷/缓存 JIT、debug/release AOT 与挂起恢复已覆盖。
- [x] 5b. `IntoCursor` 容器适配（Map / Set / Mut*；依赖已落地的关联类型
  约束 `where T.Item: Trait`）。其余仍视需求：`@parallel`
  泛化（游标移入协调器）、`Dyn` 变体、String 字符迭代。

## 验收

阶段 1 的回归位于 [compiler Cursor 测试](../../src/compiler/tests/cursors.rs)
与 [CLI Cursor 测试](../../tests/cli/cursors.rs)。前者覆盖九种 Self/Item
所有权组合、effect 和控制流；后者覆盖跨模块、冷/缓存 JIT、debug/release
AOT，以及挂起中、循环体内和返回交接时的取消。启用
`runtime-test-support` 后，CLI 测试还检查运行前与排空后的资源计数归零。

阶段 4 回归位于 [provider CLI 测试](../../tests/cli/providers.rs)、
[SQLite runtime 测试](../../crates/joky-runtime/src/runtime/sqlite/provider/tests.rs)
与 [blocking pool 测试](../../crates/joky-runtime/src/runtime/blocking.rs)。
饱和测试须单独使用 `--ignored --exact` 执行，避免占满进程共享线程池影响其他用例。

- 阶段 0：泛型 `T: type + Tr` 实例化为声明了额外 effect 的 impl 时报
  诊断，包括同步 effect 以及声明了但方法体没用到的 effect。覆盖 struct /
  class 方法中的调用、impl 前向引用、嵌套泛型转发及跨模块实例；声明为子集
  的 impl 通过。无外泄 effect 但内部 task wait 的实现仍可实例化并正确恢复。
  迁移已有 `Work` trait 的 effect 声明后，其 JIT / AOT 恢复、取消、析构测试
  继续通过，另增缺少 trait effect 声明的负测。
- 阶段 1：`for` over List 的现有全部测试不变（顺序与 `@parallel`）；用户
  `impl Cursor` 的 struct/class 可迭代，`(index, item)` 可用；D5 所有权
  与资源矩阵全部通过；具体可挂起测试游标的 effect 向外传播、可在 `do` 处理、
  挂起后恢复正确、bare `break` 停止拉取；尾部 `continue` 流式遍历零输出
  节点；`@parallel` over 非 List 报诊断；`for` over 非 List 非 Cursor 报
  诊断；没有外泄 effect 的可挂起游标通过泛型遍历的恢复与取消测试。
- 阶段 2：`for` over `map.entries()` / `set.iter()` / `bytes.iter()` 与手写
  等价循环行为一致；`for (index, entry) in map.entries()` 中 entry 保持
  `(K, V)` 类型。尾部 `continue` 的 Bytes 流式遍历，输入游标侧遍历 n 字节
  的分配次数为 O(1)，不把正常收集产生的 O(n) 输出节点计入该指标；Map
  游标析构后 Map 引用计数回落。
- 阶段 3：`for x in xs.into_iter()` 后引用 `xs` 报所有权诊断；游标析构
  释放容器；`to_list()` 后原容器仍可用；`retain` 按谓词原地压缩。
- 阶段 4：查询错误以 `Err` 出现在 item 中，其后 `advance` 返回 `None`；
  耗尽 / `break` / `?` / 取消四条路径下，provider 工作排空后语句均被
  finalize 恰好一次。覆盖排队取消、执行中取消及完成结果被取消状态拒收的
  竞态，断言语句和未交付 payload 的资源计数归零；保留的 Row 在下一次
  step 及游标释放后仍能安全读取。
- 实施后更新[循环与游标](../lang/iteration.md)与
  [bounded-iteration.md](../runtime/bounded-iteration.md) 的输入范围。
