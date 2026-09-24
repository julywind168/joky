# 局部可变绑定：`var`

> 历史记录：本文保留原阶段的设计、环境与验收结论，不代表当前能力。归档于 2026-09-24；当前说明见[文档入口](../../README.md)，未完成工作见[计划索引](../../plans/README.md)。

> 状态：设计修订 v2（2026-09-20）；阶段 0、1、2 已完成。
> 实施顺序：阶段 0（SSA 与生命周期基础）→ 阶段 1（局部 `var`）→
> 阶段 2（`move fn` 可变捕获）。各阶段的验收通过后再开放对应能力。
> 不依赖 [iteration-protocol.md](../../plans/iteration-protocol.md) 的后续容器阶段；
> 已实现的用户 `Cursor` 可用于验证移出后重新赋值的手写遍历。

## 现状与动机

`let` 创建不可变绑定，允许遮蔽（`1-syntax.md` 绑定一节）。本设计实施前，`var`
只能出现在 class 字段上，且只能由类自身方法通过 `self.field = ...` 修改。
阶段 1 已开放局部 `var`；语言仍不提供 `mut` / `&mut`。

开放局部可变绑定的动机与实现边界：

1. **循环需要方便的局部状态。** `let` 遮蔽不跨迭代；计数和累加目前往往需要
   `let c = Counter(); while c.value < n { c.bump() }` 一类 class 包装。
2. **局部状态可以保持任务私有。** 只要不隐式共享变量槽，局部重绑定不会放宽
   class 的唯一所有权或 Cown 的共享访问规则，也不要求为每个计数器分配堆对象。
3. **复用 MIR 原语与通用赋值数据流。** `TakeLocal` / `DropLocal` / `Bind`
   表达移出、清理和初始化；`for_bind` 本身创建的是新 local。后端
   `Environment.locals` 仍是编译时 SSA 值的 HashMap，阶段 0 新增的
   `src/mir/local_ssa.rs` 在清理插入后生成一般局部变量的 Phi 和边参数。
   Cursor 循环的游标与索引已改用同一套 local 读写，不再手工构造循环头 Phi。
4. **函数值已有唯一所有权。** `Type::Function(_)` 在 `is_owned_inner` 中为
   owned（`src/sema/type_table.rs`），调用借用函数值；可变捕获可沿用这一类型
   契约。不过持久环境访问、调用优化和清理仍需要专门实现（见 D6）。

## 设计决策

### D1 语法：`var name[: T] = value`，只限局部

```joky
var count = 0
var label: String = "ready"
count = count + 1
```

- `var` 是表达式，返回 `Unit`，与 `let` 同位置、同遮蔽规则；`let x` 可被
  `var x` 遮蔽，反之亦然。
- 必须带初始值。不提供"先声明后赋值"；需要延迟初始化用 `Option`。
- 类型在声明处固定；赋值右侧按 `TypeExpectation::require(声明类型)` 检查。
- `var` 只绑定运行时值；类型值仍使用 `let`，`var T = Int32` 等声明拒绝。
- **新增的 `var` 绑定只限局部。** 已有 class 字段规则不变。参数不可变
  （要改就 `var x = x` 遮蔽）；struct 无 `var` 字段；模块级无 `var`
  （全局可变状态仍走 Cown）。`let` 仍是默认。
- 赋值 `x = value` 是表达式，返回 `Unit`，与字段赋值一致。复合赋值（`+=`）
  不在本设计内。
- 方法体内裸名 `x = ...` 只解析局部 `var`；字段仍需 `self.x = ...`。现有规则
  "局部与字段同名时裸名优先表示局部"不变，且此前裸名赋值一律报错，无兼容问题。

实现：`ExprKind::Let` / `CoreExprKind::Let` 增加 `mutable: bool`；解析器
（`src/syntax/parser/control.rs:201`）在 `var` 关键字处复用 `let` 路径；
`Binding`（`src/sema/scope.rs:13`）增加 `mutable: bool`；`check_assignment`
新增 `ExprKind::Name` 目标分支：查 `lookup(name)`，非 `mutable` 报新诊断
`ImmutableBinding { name, span }`，`type_value.is_some()` 的类型绑定同样拒绝。
赋值与捕获应关联到解析后的绑定身份，而不是只保存裸名字；遮蔽产生新的绑定。
后续状态分析与 HIR/MIR 转换必须保留该身份及 `mutable` 属性。

### D2 所有权：先求右值，再替换；允许移出后再赋值

`x = rhs` 按以下顺序执行：

1. RHS 只求值一次。此时不提前释放或清空 `x`；RHS 仍可读取、借用或消费旧值。
2. RHS 正常完成后，按此时 `x` 的初始化状态替换：旧值仍存在则释放其持有份额；
   已被 RHS 移出则不再释放。新值的所有权在求值完成后立即进入清理保护。
3. 将新值移交给 `x`，赋值表达式返回 `Unit`。旧值的析构遵守现有 Drop effect
   规则并传播相关 effect；提交过程不加入可使新旧值同时失去清理所有者的窗口。

RHS 若经 `?`、abort 或取消退出，不执行替换步骤，按退出路径上的实际所有权
清理。尚未移出的旧值仍由原槽持有；已移出的旧值由接收者负责。赋值不回滚 RHS
的副作用或 move。`x = x`、`x = transform(x)` 都必须遵守这一顺序。

| `var` 持有的类型 | 读取 | 赋值 |
| --- | --- | --- |
| 可复制（整数、Bool、Duration…） | `Read` | 无旧值析构；定义新 SSA 版本 |
| 共享（String、Bytes、List、Map…） | `Read` + `Dup`，读取不移出原槽 | 若仍已初始化则 `DropLocal` 旧引用，再绑定新值 |
| 唯一所有权（class、MutList、闭包、唯一所有权游标…） | 借用 `BorrowLocal` 或移出 `TakeLocal`，与 `let` 一致 | 若仍已初始化则 `DropLocal` 再绑定；若已移出则直接绑定 |

表中的绑定操作必须经过 D3 的 SSA 与所有权转换，不能理解为只发射一个 `Bind`。
游标按实际类型的 Copy / Shared / Owned 分类，不一律视为 Owned。

允许移出后再赋值是硬需求，否则手写游标循环不可写：

```joky
var cursor = make_owned_cursor()
loop {
    match cursor.advance() {          // 消费 cursor
        Some(pair) => { let entry = pair.0; cursor = pair.1; ... } // 重新初始化
        None => break
    }
}
```

其中 `make_owned_cursor()` 表示用户定义的唯一所有权 Cursor 工厂；不依赖尚未
实现的 `MapCursor`。示例中的省略号代表元素处理代码。

**确定初始化分析：** 对每个绑定按身份做逐路径的"已初始化 / 已移出"状态分析。
这一分析必须覆盖 Copy、Shared、Owned；阶段 2 的显式捕获会消费 Copy/Shared
的 `var` 绑定，但普通读取仍遵守上表。分析在类型及 ownership 已知的控制流上
执行，在进入 codegen 前报告源级诊断，不要求在递归 AST 类型检查中重复构建 CFG。

- 读取或借用时必须"确定已初始化"；否则报带源位置的 `UseAfterMove`，不留到
  MIR verifier 才报告内部错误。
- 赋值前后分别计算状态；是否清理旧值由 RHS 正常完成后的状态决定。
- 首版不引入 drop flag。同一仍在词法作用域内的绑定，在可达前驱中的初始化状态
  不一致时，**在合流处直接报错**，不等到后续读取或赋值。包含 `if` / `match`
  合流、循环回边和出口的 `break` / `continue` 路径；已终止路径不参与合并。
  诊断指出不一致的分支，并提示在相应路径重新初始化或统一移出。
- 循环：循环头按不动点合并；`var cursor` 例子中每轮进入 `match` 时确定已初始化，
  `Some` 分支移出后重新赋值，`None` 分支 `break`，合流点状态一致。
- 即使合流后直接离开作用域，也执行上述检查，例如只有一个分支消费 `x`、
  随后没有再使用 `x`，仍然报错。各分支自己的局部绑定在离开各自作用域时清理，
  不当作同一绑定比较。
- 作用域结束时只清理仍初始化且需要释放的值；已移出或纯 Copy 值不析构。

MIR lowering 为 `CoreExprKind::Assign` 增加局部目标路径，显式表达 RHS、旧值
释放与新值移交。保留 verifier 对重复初始化、移出后使用和不一致合流的检查；
如需扩展 metadata 或验证规则，必须保持或加强这些不变量，不以放宽检查代替实现。

### D3 控制流与挂起：先构建 SSA，再保存当前值

**当前实现边界。** `src/codegen/environment.rs` 的 `bind_local` / `lookup_local`
只更新和查询 `HashMap<MirLocalId, CompiledValue>`，没有 Cranelift
`def_var` / `use_var`。因此普通分支赋值和无挂起循环也不能靠重复 `Bind` 正确工作。

**阶段 0 的通用局部变量 SSA 转换。** 沿用当前显式 Phi / 块参数体系，在 MIR
控制流确定后计算各程序点的绑定状态与值。调用点拆分等后续 CFG 改写必须维护或
重新计算这些结果，在 continuation metadata 最终生成前保持一致：

- 每次初始化或赋值产生新版本，后续读取、借用、移出和清理使用到达该程序点的版本。
- 为 `if` / `match` 合流、循环头及必要出口生成 Phi 和对应边参数；处理嵌套循环、
  `break`、`continue`，排除不可达前驱。全部前驱都已移出的绑定不生成值 Phi。
- Owned 值沿边转移所有权，Shared 值沿边平衡引用；Phi 不是隐式复制。新的
  SSA 版本仍关联原绑定身份，以便处理作用域清理和源级诊断。
- `Read` / `TakeLocal` / `DropLocal` 及后端环境必须一致地引用当前版本；先验证
  无挂起控制流，再接入 Pending。可以在实现中选择专门的 MIR pass，但不能让
  codegen 根据遍历块的顺序猜测运行时值。

**挂起保存。** Copy / Shared / Owned 局部均可跨挂起，现有 Borrowed 与 Cown
lease 限制不变。`src/mir/lower/functions.rs` 的 `local_values_at_block_entries`
按 CFG 不动点追踪经 SSA 转换后的到达绑定，再计算挂起点前的初始化状态。
例如循环递增后再次挂起，应保存当前轮次的值，不得保存入口的常量。

不能把"有多个 Bind"简单映射为 `MirFrameSlot.value = None`：后端查询并非动态
存储，而且旧的 frame slot 收集会过滤没有显式值的非入口 local，可能连保存都漏掉。
必须一起验证帧槽收集、SSA spill、恢复后的 local 映射及清理 metadata。
跨挂起存活的最新值只保留一份正确的清理义务；已经移出的槽不保存旧值，等待 RHS
返回时新值与旧值的清理责任按 D2 处理。JIT、缓存编译、AOT 使用相同规则。

### D4 捕获：`var` 不能被隐式捕获

捕获边界按源语言构造确定，不根据 HIR 是否物化任务、MIR 是否内联而改变。
下列边界对外层 `var` 的直接引用（读、赋值或移出）均报源级诊断：

| 位置 | 首版规则 | 后续扩展 |
| --- | --- | --- |
| `fn` / `move fn` 闭包 | 阶段 1 均禁止捕获外层 `var` | 阶段 2 仅 `move fn` 可显式移入，见 D6 |
| `@parallel for` 体 | 禁止捕获外层 `var` | 跨 worker 共享修改仍使用 Cown |
| `branch` / `parallel` / `race` 体 | 禁止捕获外层 `var` | 不隐式共享变量槽 |
| handler arm | 无论 handler 类型与执行方式，均禁止捕获外层 `var` | 共享状态通过 Cown 或明确的共享 capability |
| `do` body | 首版统一禁止直接引用外层 `var`；内部声明的 `var` 可正常使用 | 后续先定义状态移交与退出清理，再统一扩展 |

`do` body 和 handler arm 分别检查，handler 参数属于 arm 内部绑定。普通 handler
即使没有私有任务，也可能通过 runtime frame / 独立 thunk 按值捕获，不能把
"没有 `__task_`"等同于"读写同一个外层局部槽"。诊断不能因改为挂起 operation、
增加 abort 或改变编译优化而时有时无。

这些边界内部可以声明自己的 `var`；它们遇到更深一层捕获边界时仍遵守同样规则。
普通嵌套块、顺序 `for` / `while` / `loop` 可以修改外层局部变量。
`when` 是当前任务中的受控访问区，不是按值捕获边界；可读写当前任务的外层 `var`，
但不能因此让 lease 逃逸、跨挂起或被嵌套任务捕获。若 `when` 位于闭包或任务内，
仍先受外层捕获边界约束。

需要只读快照时，用户在边界外显式写 `let snapshot = x`，再捕获 `snapshot`。
该绑定遵守普通 Copy / Shared / Owned 规则，尤其不会因为使用 `let` 就允许重复
复制唯一所有权值。阶段 1 诊断建议显式快照或 Cown，不提示尚未开放的 `move fn`
可变捕获；阶段 2 只在闭包位置提示 `move fn`，不把它当作其他边界的自动豁免。

检查在源级绑定身份与 mutability 可用时完成。可复用 `free_names` 的词法遍历，
但必须区分边界内声明、参数、模式绑定与外层捕获，并单独检查 handler arm。
HIR 的 `task_captures` 与 synthetic handler capture 路径保留一致性断言，
不负责依据物化结果制定新的可见性规则。

### D5 借用交互

现有规则："参数求值时，不得消费仍有借用待使用的对象"（`1-syntax.md:543`）。
释放或替换旧值同样不能使存活借用失效：一个调用的 receiver、callee 或实参
已经借用了 owning local 后，后续实参表达式不得重绑定该 local 或使其被借用
字段失效。复用现有按 owning local 保守判断的策略，并按解析后的绑定身份检查，
不能把同名但被遮蔽的两个局部当成同一对象。

例如 `f(x, { x = other; 0 })` 在 `f` 的第一个参数借用 `x` 时必须拒绝；
函数值作为被借用的 callee 时同理。已经完成的 Copy 读取或独立 Shared 引用
不构成指向原变量槽的借用。`x = transform(x)` 的 RHS 调用先完成，之后按 D2
提交赋值，不因该表达式读取了 `x` 就一律拒绝。

### D6 闭包捕获 `var`：`move fn` 移入，闭包类型不变（阶段 2）

```joky
var n = 1
let read = move fn () -> Int32 { n = n + 1; n }
// 外层 n 已移出，重新初始化前不可读取或再次捕获
read()   // 2
read()   // 3
n = 100  // 重新初始化外层绑定，不改变闭包内的槽
read()   // 4
```

- 捕获 `var` 必须写 `move fn`。这是**消费可变绑定**的显式操作，不等于对所有
  类型沿用普通取值：即使是 Copy 整数或 Shared String，也将外层绑定标为已移出。
  捕获前必须确定已初始化；其后读取、借用或再次捕获报 `UseAfterMove`。
- 外层仍可按 D2 重新赋值，形成独立的当前值；它与闭包环境不共享变量槽。
  Copy 值复制到环境后清空外层的初始化状态，Shared 值转交其持有引用，Owned 值
  转交所有权；代码生成与分析均不得留下第二份旧值清理义务。普通 `let` 捕获和
  普通 Copy/Shared 读取的语义不变。
- 闭包体内捕获的 `var` 可赋值，语义等同 `&self` 方法修改 class 的 `var` 字段
  （"class 的内部可变性不因借用改变"）。闭包内部再次用 `let` / `var` 遮蔽捕获名
  时，得到的是调用内的局部绑定，不改变环境字段。
- **类型仍是 `fn(...) -> R`，不加标记。** 所有函数值本已是 owned、借用调用，
  不需要为这个能力新增 `Fn` / `FnMut`。调用不得与同一环境的另一次调用、移出或
  析构重叠；任务与闭包传参仍执行唯一所有权及借用检查，不能隐式复制到多个任务。
  将来若开放可复制函数值或共享调用能力，需重新评审这项契约。

**阶段 2 的实现范围与限制：**

- 捕获 metadata 保留绑定身份、类型与 `mutable`。闭包函数必须能访问同一个
  持久环境字段，不能只把字段值作为普通参数传入。当前使用私有 class 状态对象，
  `src/codegen/functions/values/closures.rs` 调用 glue 传入状态对象的借用指针，
  复用既有 ABI 与字段 MIR。赋值按 D2 求 RHS、释放旧值、写入字段。
- 捕获环境按借用接收者访问。阶段 2 首版不允许从借用环境移出 Owned 捕获字段后
  再赋值；该能力需要额外的字段初始化与失败清理协议。允许借用该字段，或在不
  消费旧字段的前提下构造新值并整体替换；已有 Drop 捕获限制不因 `mutable` 放宽。
- 消费绑定的 `move fn` 操作只作用于当前调用拥有的局部槽，不能再次消费借用
  环境中的可变捕获槽，包含 Copy / Shared 类型。嵌套闭包若需要独立的可变状态，
  可先用普通取值规则初始化调用内的 `var`，再移入新闭包；不能让外层闭包下次
  调用时面对已经被嵌套捕获清空的字段。
- `closure_bindings` 的排除条件包含两类：可重新赋值的函数绑定，以及**任何具有
  可变捕获的闭包**，包括 `let read = move fn ...`、别名和闭包字面量调用。
  `src/mir/lower/expressions/calls.rs` 当前的内联路径会重新读取外层名字并创建
  调用内副本；在它能保留持久环境语义前，可变捕获必须走实际环境调用路径。
- 阶段 2 首版只开放不会挂起的可变捕获闭包。环境借用跨 continuation 的保存与
  取消协议完成前，拒绝实际可能 Pending 的调用体，包括无外泄 effect 的内部
  task wait；判断依赖实际调用图和挂起分析，不能只看 effect 声明。普通不可变
  捕获闭包的既有挂起能力不变，不允许退回同步阻塞执行以绕过限制。
- 正常返回、失败、闭包移交与最终析构都必须只释放环境的当前值。后续要支持可变
  环境跨挂起或移出字段，需先补齐环境所有者存活、排他调用和未初始化字段清理的
  协议及测试，不能仅增加一个 `Store` 就宣称完成。

## 与备选方案的取舍

- **维持现状，只靠 class / Cown。** 可以表达状态，但简单的任务私有计数、累加
  也需要对象封装。局部 `var` 保留相同隔离原则，减少这类样板代码。
- **`let mut` / `&mut`。** 语言明确不引入 `mut` 和 `&mut`（`1-syntax.md:540`）；
  `var` 与 class 字段用词一致，且不需要引用类型。
- **闭包隐式按快照捕获 `var`。** 这可以安全实现，但容易混淆后续赋值的可见性。
  本设计要求 `let snapshot = x` 显式表示快照，或用 D6 的 `move fn` 移入私有状态。
- **根据 `do` 是否物化任务决定能否访问外层 `var`。** 拒绝；同一源语言规则
  不应依赖优化与内部任务生成策略。首版统一限制，未来统一扩展。
- **赋值用 drop flag 处理可能已移出的 `var`。** 需要运行时标志与 verifier 配套，
  首版在同一存活绑定的状态不一致的合流处诊断，包括之后直接退出的情形。
- **`var` 用新的 `ExprKind::Var`。** 与 `Let` 逻辑几乎全部重叠（遮蔽、注解、
  HIR / MIR lowering、`free_names`），加 `mutable` 字段改动面更小。

## 任务

### 阶段 0：SSA 与生命周期基础

- [x] 建立按绑定身份的初始化状态与到达值分析，处理循环不动点、不可达前驱、
  作用域退出与状态不一致的合流。复用当前所有权类别，不把 Shared 读取误判为 move。
- [x] 实现 D3 的一般局部 SSA 转换：分支、循环头和出口 Phi / 边参数，以及
  与 `Read` / `BorrowLocal` / `TakeLocal` / `DropLocal` 一致的当前值映射。
- [x] 在 MIR 测试中验证重复赋值、自赋值、RHS 移出后返回，以及不同分支的析构；
  不放宽现有 ownership verifier 的安全检查来使测试通过。
- [x] 用真实到达值更新 continuation frame / spill / 恢复与取消清理协议，确保
  非入口局部不会因为 `value = None` 被遗漏。覆盖 Copy / Shared / Owned。

实现位于 `src/mir/local_ssa.rs` 和 `src/mir/lower/functions.rs`。先插入作用域
清理，再按存活局部的到达定义生成必要的 Phi；Owned 在边上 `TakeLocal`，Shared
使用 `Read` / `Dup` / `DropLocal` 转交引用，目标块用 `Bind` 接收。初始化分析
覆盖 Copy / Shared / Owned，并保留现有 Owned 与借用检查。

挂起 metadata 按 CFG 到达绑定保存当前值，spill 以真实定义位置和支配关系判断，
不依赖值编号的先后。验证包括 MIR 序列化往返、debug/release AOT 对象生成、
实际 JIT 循环与多次挂起、自赋值、RHS 移出后重新初始化及取消资源归零。
完整编译器回归与 runtime 测试通过。源语法、源级诊断及四种运行模式的源程序
验收由阶段 1/2 完成。

### 阶段 1：开放局部 `var`

- [x] 增加 `Let.mutable`、`Binding.mutable` 与解析后的绑定身份；接入局部赋值
  语法、固定类型检查、初始化要求和 `ImmutableBinding` 等源级诊断。
- [x] 实现 D2 的 RHS 求值、旧值释放及新值移交；初始化状态分析覆盖所有 `var`，
  为阶段 2 的 Copy/Shared 绑定消费保留语义。将 move 错误提前为有源位置的诊断。
- [x] 接入 D4 的源级捕获边界检查，分别覆盖闭包、任务、并发 `for`、handler arm
  和所有 `do` body；允许普通块、顺序循环与受控 `when` 访问当前任务的局部槽。
- [x] 实现 D5 的借用冲突检查，涵盖 receiver、callee 和实参求值，按绑定身份
  处理遮蔽。`var` 函数绑定不进入依赖固定目标的 `closure_bindings` 优化。
- [x] 同步 `1-syntax.md`、`4-closures.md`、`3-eff.md`，明确 `do` 的统一首版限制，
  不以"是否被物化成任务"描述源语言规则；完成阶段 1 验收后开放语法。

源程序回归覆盖局部控制流、Copy / Shared / Owned 自赋值与重初始化、所有捕获
边界、receiver / callee / 实参借用冲突及源级诊断。跨模块泛型替换、Owned Cursor
与多次挂起、RHS 提前返回和任务取消通过冷 / 缓存 JIT 与 debug / release AOT。
挂起帧按恢复点的局部存活性保存当前值，不保存下一轮才重新绑定的旧 lease。

### 阶段 2：`move fn` 可变捕获

- [x] 实现 D6 的绑定消费语义，覆盖 Copy / Shared / Owned、外层重新初始化、
  重复捕获诊断及按路径的初始化状态合流。
- [x] 增加可变捕获 metadata 和持久环境访问协议，更新 closure call glue、
  MIR、codegen 与环境析构；保留借用环境中 Owned 字段不可移出的首版限制。
- [x] 为所有带可变捕获的闭包禁用或正确改造快照内联路径，覆盖 `let` 保存、
  别名、字面量调用、本地与跨模块传参，保证同一环境的状态持续更新。
- [x] 验证唯一所有权和调用借用，拒绝重复并发捕获及重叠调用；实际挂起分析必须
  拒绝尚不支持的可变环境 Pending 路径，包含泛型实例与内部 task wait。
- [x] 更新闭包文档并完成阶段 2 验收。未来支持环境跨挂起、字段移出或可复制闭包
  时另行修订协议，不计入本阶段完成条件。

实现复用编译器生成的私有 class 状态对象：闭包环境唯一持有它，每次调用借用
同一个对象，通过 `Project` / `Store` 读取、替换字段；现有 call / drop glue
传递并释放状态对象。相比普通闭包多一次受管分配，不改变函数值 ABI。状态类型按
捕获字段的稳定类型标识生成，缓存与跨模块链接可重定位。Copy / Shared / Owned
可变绑定统一通过 `TakeLocal` 消费；普通读取规则不变。

实际挂起分析独立于同签名函数的 Pending ABI 扩展，拒绝真正的挂起路径及内部
任务等待。`CCallback.new` 不接受可变捕获，避免绕过环境的唯一所有权调用约束。

验收覆盖持久计数、Shared / Owned 字段替换、外层重初始化、别名与字面量调用、
本地和跨模块高阶传参与返回、泛型状态、嵌套捕获与遮蔽、任务移交及取消。
六组源程序通过冷 / 缓存 JIT 与 debug / release AOT，并在资源计数开启时排空。
完整回归通过；直接等待、隐藏任务等待和 handler 间接等待均有源级拒绝测试。

已有普通函数值的 move 错误目前可能延后到 MIR verifier；阶段 1/2 涉及的路径
必须报告一致的源级所有权诊断。若顺带修复其他旧路径，可独立提交，不要求改动
与 `var` 无关的语言行为。

## 验收

阶段 0 使用 MIR 级测试验证基础不变量；阶段 1/2 以源程序验证下列行为。涉及
运行行为的代表性场景覆盖冷 / 缓存 JIT 与 debug / release AOT，并检查资源排空。

- 基本：`let` 赋值报 `ImmutableBinding`；参数、struct 字段、模块级 `var`、
  类型值 `var`、无初始值和类型不匹配分别报诊断。`let` / `var` 互相遮蔽、嵌套
  同名变量与字段同名时仍指向正确绑定；`var` 声明与赋值返回 `Unit`。
- 无挂起控制流：`while` 累加正确终止；`if` / `match` 不同路径赋值后的读取
  得到正确值；覆盖嵌套循环、连续覆盖、`break` / `continue` 和不可达分支，
  不得依赖后端遍历块的顺序。
- 赋值生命周期：Copy / Shared / Owned 的 `x = x` 正确；`x = transform(x)`
  只执行 RHS 一次，返回新值后不重复析构旧值；有用户 Drop 的旧值被替换时恰好
  析构一次，effect 正确传播。RHS 中 `?`、abort、挂起与取消都清理实际持有的值。
- 初始化状态：移出后赋值通过，未重新初始化前读取报源级诊断；一个分支移出而
  另一个未移出时在合流处诊断，即使后面没有读取或赋值、只退出作用域也一样。
  覆盖循环回边及多个 `break` 出口；已终止路径和分支局部变量不引起错误合流。
- 手写游标：用户定义的 Owned Cursor 在 `advance` 后重新赋值并遍历正确；
  耗尽、提前退出和取消时元素与游标恰好清理一次，不依赖后续 Map/Set 游标。
- 挂起：局部标量循环在第一次与后续多次挂起后保留最新值；Shared String 和
  Owned class 的反复替换也正确。覆盖分支合流后挂起、RHS 消费旧值后等待返回，
  以及 body 内和交接时取消，帧槽不遗漏、不保存旧值、不重复清理。
- 捕获：阶段 1 的 `fn` / `move fn`、并发 `for`、`branch` / `parallel` / `race`
  及 handler arm 捕获外层 `var` 均报诊断。`do` body 的限制不随 Normal /
  Resumable / Aborts / Pending 路径变化；在边界内部声明的 `var` 正常工作。
  显式 `let snapshot = x` 按通常所有权规则捕获，保留创建快照时的值。
- 同任务访问：普通块、顺序循环与 `when` 可更新当前任务的外层 `var`，lease
  不得因此逃逸、挂起或被任务捕获；嵌套 handler 与闭包仍按各自边界检查。
- 借用：第一个实参借用 `x` 时，`f(x, { x = other; 0 })` 被拒绝；借用 receiver
  或 callee 后在实参中重绑定同一对象也被拒绝。已完成的 Copy 读取和独立 Shared
  引用不误报；同名但不同身份的局部按遮蔽规则检查。
- 可变捕获：D6 的计数器连续调用返回 `2, 3, 4`；`let` 保存、别名、本地与
  跨模块 `fn() -> Int32` 传参后的间接调用使用同一持久环境。含可变捕获的字面量
  调用也使用实际环境，不重读外层名字。
- 捕获后状态：Copy 整数、Shared String 与 Owned class 的外层绑定均需重新
  初始化后才能再读或捕获；外层重新赋值不改变闭包状态。捕获名在闭包内被局部
  遮蔽时不修改环境。Shared / Owned 环境字段反复替换、闭包移交与析构无泄漏。
- 闭包限制：普通 `fn` 捕获 `var` 提示 `move fn`；同一函数值不能移入多个并行
  任务；从借用环境移出 Owned 字段、再次消费可变捕获槽，以及可变捕获体实际
  可能 Pending 时给出诊断。嵌套闭包移入新建的调用内局部 `var` 则按通常规则检查。
  覆盖未声明 effect 但内部 task wait 的闭包，不得仅测试 `time.sleep`。
- 保留现有合法程序行为，运行完整回归；若已有错误程序的诊断阶段或文本改善，
  明确记录并更新对应断言，不以"现有测试零变化"替代兼容性检查。
