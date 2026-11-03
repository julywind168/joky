# 语言核心审查修复计划

状态：2026-11-03 审查结论，K0、K1-1、K1-2 和 K2 已按实施记录修复；K1-3 解释器回溯已修复，原生同步直调已验证测试原型；K3、K4 已修复；D 节为待决策设计项，未经确认不实施。

本文来自一次针对语言核心（前端、类型、所有权/借用、挂起与结构化并发）的实测审查：
约 500 个探针程序，在 JIT、解释器、AOT 三后端上对比。下列问题均已用最小程序复现，
复现以审查当日的 `main`（`a770cc1`）为准。当前语法与契约仍以 [language.md](language.md)、
[architecture.md](architecture.md) 为准；修复改变行为时同步更新这两份参考文档，不在本文复制现行规则。

审查同时确认以下部分**没有问题**，修复时不应削弱：约 200 个借用/移动探针（经泛型、闭包、
函数值、dyn、分支合流、循环回边、`?`、容器重分配、挂起点、join/scope）未能构造出被接受的
use-after-move、悬垂引用或可变别名；被接受程序的三后端输出一致；race/join/scope 的拥有值
恰好清理一次，副作用不重放；整数与浮点边界（`MIN / -1`、移位越界、NaN、负零、饱和转换）三后端一致。

## 0. 实施约定

- 每项先把“复现”程序写成失败的回归测试，再修复。测试命名、归属与三后端覆盖遵循
  [AGENTS.md](../AGENTS.md) 与 [CONTRIBUTING.md](../CONTRIBUTING.md)：
  生成代码行为至少覆盖 JIT、解释器、AOT 各一次，不在 VM、AOT、CLI 各层重复同一矩阵；
  纯静态拒绝/接受用共享前端测试。
- “根因位置”是审查时的定位线索，不是结论；行号会漂移，以函数名为准，动手前先确认。
- 验证按 AGENTS.md 的改动范围表执行：局部修复先跑针对性测试，再跑 `./scripts/check.sh`；
  K0-2、K1、K3 涉及共享 IR 与生命周期，按影响补跑相关 `exhaustive_` 穷举。不默认使用 Release。
- 每个阶段一个或数个 Conventional Commits 提交；阶段完成后在第 7 节记录实际执行的检查。
- 诊断文本、标识符、测试名用英文；新增资源限额须是受控 `ResourceLimit` 错误，不得 panic 或 abort。

## 1. K0：资源语义漏洞、崩溃与后端不一致（最高优先级）

### K0-1 泛型 Drop 以 Copy 实参实例化后不析构且可复制

```joky
import joky/drop.{type Drop}
struct W<T> { v: T, tag: Int32 }
impl<T> Drop for W<T> { fn drop(&mut self) { println("drop {self.tag}") } }
fn main() {
    let a = W { v: 5, tag: 1 }
    let c = a
    let d = a                  // 被当作 Copy 接受
    let s = W { v: "x", tag: 2 }
    println("end")
}
```

- 现状：三后端只输出 `end`、`drop 2`；`W<Int32>` 可复制、可 `alloc`、可经 `T: Copy` 泛型复制，
  drop 从不执行。`W<Unit>`、`W<Option<Int32>>`、`W<CopyRecord>`、`Array<W<Int32>>` 同样。
  非泛型 `struct G { tag: Int32 } impl Drop for G` 已正确报 “Drop requires an owning record”。
- 根因线索：`src/core/ir/traits.rs` 中 Drop 实现资格检查只看模板目标 `implementation.target.is_owner()`；
  `W<T>` 在 T 未知时为拥有者，具体实例按普通 Copy 记录布局计算，没有复查 Drop。
- 修复方向（推荐）：**有 Drop 实现的名义类型的任何实例都是拥有者**，不满足 Copy，
  不进入 `alloc`/显式 Arena/Copy 存储，离开作用域执行 drop。这与“Drop 记录是资源”的语义一致，
  且不需要在实例化点报错。所有权分类（`is_owner`、`is_copy_storage_value`、Copy 约束检查、
  布局的拥有槽、清理描述）须从同一处查询 Drop 身份，避免各自判断。
  备选：在实例化点拒绝 Copy 实参（报错位置在使用处），表达力较差，不推荐。
- 验收：上例输出 `end`、`drop 2`、`drop 1`（顺序按现有作用域清理规则），`let d = a` 报 use-after-move；
  `dup::<W<Int32>>`、`alloc(W { v: 0, .. })`、`Array<W<Int32>>` 的 push/pop/release 各有测试，
  drop 恰好一次，三后端一致。现有 `W<FixedArray>` 的 “FixedArray storage does not support custom Drop” 保持。

### K0-2 并发块内写闭包导致编译器 panic

```joky
fn main() @suspends {
    let r = join { | { let g = || -> Int32 { 1 }; g() } | { 2 } }
}
```

- 现状：`joky check` 即 panic：`src/core/ir/closures.rs` 中 `prepare_closure` 的 `.unwrap()`。
  `join`/`race`/`branch`/`isolate` 四种块，普通闭包、`move ||`、`move || @suspends` 均触发；只定义不调用也触发。
- 根因线索：块分支编译为合成入口 `__joky_task_N`，`self.source_function` 指向该合成名，
  在 `functions` 与 `generics.generated_functions` 中都查不到。
- 修复方向：块分支入口登记到闭包准备可见的函数表，或让 `source_function` 解析回词法外层函数
  （闭包的 noescape/保留推导需要的是外层信息，确认后择一）。无论哪种，`unwrap` 改为受控内部错误。
- 验收：四种块内的同步闭包、move 闭包、挂起 move 闭包可编译并在三后端运行；
  在块内闭包捕获块外引用时沿用现有 branch 捕获的借用规则（加一条拒绝用例）。

### K0-3 取消/失败清理时兄弟分支 Drop 顺序：解释器与 JIT/AOT 相反

```joky
import joky/task
import joky/drop.{type Drop}
import joky/array.{type Array}
struct R { id: Int32, d: Array<Int32> }
impl Drop for R { fn drop(&mut self) { println("drop {self.id}") } }
fn hold(r: R, ms: UInt64) @suspends -> Int32 { task.sleep_ms(ms); r.id }
fn main() @suspends {
    let w = race { | hold(R { id: 1, d: [] }, 1) | hold(R { id: 2, d: [] }, 50) | hold(R { id: 3, d: [] }, 50) }
    println("winner {w}")
}
```

- 现状：JIT/AOT 与解释器 `--task-workers` 输出 `drop 1, drop 2, drop 3`；解释器 Local 输出 `drop 1, drop 3, drop 2`。
  join 中一个分支失败后取消兄弟、scope 主体失败取消多个 branch、isolate 内 scope 失败同样不一致。
- 根因线索：`src/backends/interp/execute.rs` 的 `release_subtrees` 用 `pending` 栈 `extend` 后 `pop`，兄弟按 LIFO 释放。
- 修复方向：规定“取消的兄弟分支按登记（源码）顺序清理，每个分支内部按既有规则”，
  解释器改为保持该顺序（例如兄弟逆序入栈）。将该规则写入 architecture.md 的结构化并发小节。
- 验收：race 取消、join 失败取消、scope 失败取消、isolate 内失败四个场景各一个三后端测试，
  输出一致；解释器时间片取消仍能遍历未停止子树（现有测试保持通过）。

## 2. K1：编译器健壮性（深度与复杂度）

### K1-1 表达式解析与 IR 规划无深度上限，栈溢出 abort

- 复现：`python3 -c "n=2000;print('fn main() { let x = '+'('*n+'1'+')'*n+' }')" > p.jk; joky check p.jk`
  （Release 下 rc=134；Debug 下约 200 层即溢出）。Debug 下同样会溢出的还有：约 300 个分支的 `else if` 链、
  约 400 项平铺 `1+1+…+1`（溢出在 IR 的 `member_call`）、约 400 个连续一元 `-`（`explicit_trait_call`）、200 层元组嵌套。
  `joky fmt` 先完整解析，也会崩溃。
- 根因线索：`src/core/parser/expressions.rs`（`parse_range`、`parse_unary` 等）与 `atoms.rs` 的 `parse_atom` 无深度计数；
  `core::ir` 规划递归遍历 AST。类型（256 层）、模式（256 层）、插值（64 层）、属性（32 层）已有受控上限。
- 修复方向：
  1. Parser 增加统一的表达式/块嵌套深度计数，超过上限报带位置的解析错误。上限须保证 Debug 构建
     在默认主线程栈上也不会溢出（以 Debug 实测为准，留足余量；当前 200 层已溢出，说明单层栈帧很大）。
  2. 左结合二元链、`else if` 链在 parser 中迭代构造；IR 对这两类长链改为迭代规划或在深度上限内工作，
     使“平铺 1000 项加法”“200 分支 else if”这类生成代码正常编译。
  3. 评估在独立大栈线程中运行编译（`stacker` 或 `std::thread::Builder::stack_size`），作为纵深防御而非替代深度上限。
- 验收：每类输入一个测试——超过上限得到受控错误码，链式输入（≥1000 项加法、≥300 分支 else if）在 Debug 下编译并正确运行；
  `fmt` 同样不崩溃。嵌入 API `VmEngine::load_source` 至少一条测试证明深嵌套返回 `Err`。

### K1-2 嵌套泛型调用与泛型方法链的类型推导指数复杂度

```joky
fn id<T>(x: T) -> T { x }
fn main() { let y = id(id(id(id(id(id(id(id(id(id(id(1))))))))))) }
```

- 现状：Release 下嵌套 9/10/11 层 `check` 耗时 1.5s / 4.5s / 19s（每层约 ×4）；
  `Some(1).or(None).or(None)…` 链 8–10 次、`.and(Some(2))` 链、嵌套 Option 的 `.unwrap()` 链同样卡死。后缀 `!` 或拆成多条 `let` 则正常。
- 根因线索：`src/core/ir/inference.rs` 的 `hint`/`type_hint`/`bound_hint`，`generics.rs` 的 `generic_signature_expecteds`，
  `members.rs` 的 `member_call`；每层对接收者和实参重复调用推导，结果未缓存。
- 修复方向：对单次规划内的表达式定型提示按（表达式身份，期望类型）做记忆化，或改为自底向上一次求出
  每个子表达式的候选类型再向下传递期望类型。与 K2-2 同处代码，建议一起设计。
  同时为推导增加工作量预算（受控 `ResourceLimit`），确保任何剩余的病态输入不会卡死。
- 验收：嵌套 30 层 `id(...)`、30 次 `.or(None)` 链在 Debug 下数秒内完成（只断言正确性，不计时；
  若需防回归，用推导步数计数器断言线性规模）；结果类型不变。

### K1-3 原生后端深递归栈溢出崩溃

- 现状：`rec(10_000_000)`（同步或 `@suspends` 递归）在 JIT abort（rc=134）、AOT SIGSEGV；
  解释器在 `max_frames = 1024`（`src/vm/limits.rs`）给出受控错误，CLI 已折叠连续重复的回溯帧。
- 修复方向：
  1. 解释器回溯折叠重复帧（如 `at rec (repeated 1021 times)`），立即可做。
  2. 原生后端：在生成函数入口加入栈界检查（比较栈指针与每线程记录的下限，越界经运行时错误通道返回受控错误），
     或依赖 guard page + 信号处理转换为受控失败。方案涉及 ABI 与浮点环境保护入口，需先写设计小节并评估开销，
     确认后再实施；在此之前于 architecture.md “已知边界”明确记录“原生深递归会终止进程”。
- 验收：回溯折叠有测试；原生方案按其设计另定验收。

## 3. K2：推导与过度拒绝

以下均为编译器拒绝了文档允许或应当允许的程序；修复须保持现有拒绝用例不变。

| 编号 | 复现 | 现状 | 根因线索 / 方向 |
| --- | --- | --- | --- |
| K2-1 | `const A: Int64 = 5` 后 `println(A)`、`id(A)`、`Some(A)` | Cannot infer type parameter `T` | `inference.rs` `hint()` 的 `Expr::Ident` 只查局部 `vars`，补查模块常量（关联常量已有独立分支） |
| K2-2 | `let a: Int64 = id(id(1))`；`let o: Option<Int64> = Some(id(1))`；`first::<Int64, Bool>(id(1), true)`；`id(3000000000) + wide` | Int64 与 Int32 冲突 | 内层泛型调用在严格轮（`default=false`）也把字面量默认成 Int32；严格轮应保留未定型字面量，由外层期望类型定型。与 K1-2 一起改 |
| K2-3 | `1 + 1.5`、`[1, 1.5]`、`pair(2, 1.5)` | 被拒，而 `1.5 + 1`、`[1.5, 1]`、`pair(1.5, 2)` 通过 | 统一规则须与书写顺序无关。已确认：缺少具体类型上下文时，两种顺序都拒绝。`if c { 1 } else { 1.5 }` 随同一规则 |
| K2-4 | `fn h(p: &P) -> Int32 { let g = \|\| { p.x }; g() }`；`var g = \|\| { p.x = 5 }`（p: &mut P）；`move \|\| { p.x }` | Type &P has no fields | 闭包捕获生成的 `Binding { place: Some(..) }` 在 `records.rs` `place()`→`field_target` 路径未对引用绑定自动解引用；`(*p).x`、`p.get()` 已可用 |
| K2-5 | `import app/n as n.{type Open}` 后 `let Open { a, b } = s`；`type O = n.Open` 后 `let O { .. } = s` | Unknown record pattern `app/n.Open` | `record_patterns.rs` `resolve_record_pattern` 用已规范化名查找 `named_type`/`scope.types` 失败；与构造表达式共用解析 |
| K2-6 | `fn h<S: Src<Item = U>, U>(s: &S) -> U`，调用 `h(&i)` | “Type 'I' does not satisfy trait bounds…”；加 `let b: Int32 =` 才通过 | 已知 S 时应正向从关联类型等式确定 U（`types/projections.rs`），不属于文档排除的“由 Item 反推 Self”；诊断不应声称不满足约束 |
| K2-7 | `let o: Option<fn(Int32) -> Int32> = Some(\|x: Int32\| -> Int32 { x })`；泛型记录字段同理 | Conflicting types: fn(..) and `<closure:1>` | 泛型推导遇到期望类型为 fn 且实参为无捕获闭包时执行与非泛型位置相同的转换 |
| K2-8 | `let v: &Int32 = a.alloc(3)`；`fn mkg<T: Copy>(t: T) -> &mut T` 后 `let w: &Int32 = mkg(3)`；`Array.new::<dyn Get>()` 后 `xs.push(S { .. })` | expected &mut T / Conflicting types | 泛型调用结果和实参在期望位置未执行 `&mut T → &T` 收窄与拥有型 dyn 擦除；非泛型位置已支持 |
| K2-9 | 挂起函数 `fn bump(r: &mut P) @suspends { task.yield_now(); inc(&mut r.x) }` | Local borrowing is only supported in synchronous functions | `locals.rs`（及 `memory.rs` 同一诊断）只接受 Copy 绑定或 suspend-safe owner 字段；经 `&mut` 引用投影出的字段不是局部存储，应走引用重借用路径。同步函数与 `&r.x` 已可用 |
| K2-10 | 挂起函数内 `Some(41).map::<Int32>(\|x\| { x + 1 })` | Only reference-free move closure environments can enter suspending functions | 不跨挂起点、或零捕获的非 move 闭包应允许；至少诊断提示改用 `move`。需先确认放宽条件（闭包活跃区间内无挂起点）与借用分析的配合 |
| K2-11 | `let f = move \|\| @suspends { .. }` 后 `race { \| f() \| { .. } }` | Undeclared function 'f' | 调用分支应解析局部可调用值（函数值已可用，块分支 `\| { f() } \|` 已可用） |
| K2-12 | 两个模块各有**私有** `impl Int32 { fn twice(self) .. }`，同时导入 | Duplicate associated member `Int32.twice` | `members.rs` 只按（目标名, 成员名）去重；私有成员只在其模块内可见，不应跨模块冲突。公开成员冲突的规则见 D3 |

验收：每行一条三后端运行测试（或共享前端接受测试 + 一次运行），并保留/补充相邻的拒绝用例
（例如 K2-4 的闭包仍不能逃逸、K2-9 的独占重借用冲突仍被拒、K2-10 跨挂起的借用闭包仍被拒）。

## 4. K3：借用分析的规模

- 修复前复现：同一函数内约 250 行 `t = t + get(&p)`，或约 200 行 `println("line")`，即报
  `Reference flow analysis exceeds its compilation budget`；约 300 行时活跃性分析也超预算
  （`Reference liveness exceeds its compilation budget`）。预算按操作计数，Debug/Release 结果相同；诊断范围是整个函数，无提示。
  同类：300 个整数分支的 match 报 `Match coverage analysis exceeds 65536 steps`；1000 个插值的字符串约 9 秒后才报超预算。
- 根因线索（`src/core/ir/borrowing.rs`）：
  - 流分析每条指令的成本按当前 `State` 全部引用条目累加，而 `State.slots` 中已死的临时引用不随活跃性移除，成本对函数长度二次增长；
  - 活跃性分析每轮成本为“指令数 × 引用槽数”，预算固定 2,000,000；另有 `types.len() > 4096 || n > 100_000` 的硬上限。
- 修复方向：
  1. 在流分析的每个程序点按已计算的 `live` 集合裁剪死引用条目（注意 `retained`/`regions`/`arena_retained`
     记录的是需持续到释放点的借用，不能按值活跃性裁剪，只裁剪 `slots`/`contents` 中不再活跃且未被保留的项）；
  2. 活跃性改用位集与按基本块迭代，避免逐指令重复构造 `BTreeSet`；
  3. 预算改为随函数规模线性缩放（如“基础值 + k × 指令数”），保留对病态输入的上限；
  4. 超预算诊断指出最可能的原因并建议拆分函数。
  该项影响借用证明的核心路径：裁剪规则须先在设计小节写清哪些状态可裁剪，再实施；
  全部现有借用拒绝测试必须保持，并补跑借用相关 `exhaustive_`。
- 验收：2000 行 `t = t + get(&p)`、1000 行 `println`、1000 行 `x = x + 1` 在 Debug 下通过检查并正确运行；
  match coverage 对 300 个整数字面量分支通过（纯字面量列不应需要区间划分的完整预算）。

## 5. K4：诊断质量

以下为审查时的复现。诊断由共享前端回归核对，负号与后缀优先级另有三后端运行用例。

- **内部名外泄**：`__joky_task_N`（如 race 分支类型不一致报 `Return type mismatch for '__joky_task_3'`）、
  `joky/member/joky/impl/500/joky/time.Duration.add`（`1s + 1`、用户 `impl Add` 后 `V{..} + 2`）、
  `joky/trait/1/mk`、`trait#1`、`<const:A>`、`Cyclic associated constant dependency`（实为模块常量环）。
  统一在渲染诊断前把内部符号映射回源码名（运算符写成 `+` 运算，合成分支写成 “join branch” 等）。
- **术语不符**：用户记录/泛型值/FnOnce 闭包重复移动报 “moved or uninitialized buffer owner”；
  join 结果为引用报 “scope results must be reference-free”；显式 Arena 引用 push 进 `Array<&Int32>`
  报 “cannot be retained by the task arena”；局部值活不到块外时有的写法报 “conflicts with a live borrow”。
- **期望/实际方向颠倒**：`let x: Int32 = 0..3` 报 “expected joky/iter.Range<T>, got Int32”；泛型返回值与注解不符的情况均如此。
- **脱糖参数名外露**：`a..b` 混用整数类型报 “generic parameter `T`”，`for x in 5` 报 “for 'I'”。
- **吞掉真实原因**：`inference.rs` `hint()` 以 `.ok()` 丢弃资源限额与资格错误，统一报 “Cannot infer”。
  例如 `grow((x, x), n - 1)` 的真实原因是 “Tuple layout exceeds 65536 scalar slots”。`ResourceLimit` 与资格错误应直接传播。
- **引用丢失**：`a.borrow_ref(0).unwrap()` 报 “Type 'Int32' has no method 'unwrap'”，应显示接收者类型 `&Int32`。
- **Token 名**：“Expected LBrace, got Ident("b")”、“got Fn”、“IntLit(2)” 改为源码拼写；
  或模式 `1 | 2 =>` 明确说明“不支持或模式”。
- **其他**：
  - 少写 `}` 时指向未闭合的开括号；
  - `println!(..)` 提示 Joky 没有宏；
  - 未知属性 `#tset` 报 “unknown attribute”，错误码不用 `abi_error`；
  - 重复导入函数不称 “constructor”；
  - `[0, ..5]` 的 “expects a sequence of Int32, got Int32”；
  - 重叠 impl 时提示 `Trait.v(x)`，照做仍报 Overlapping，提示应与实际可行写法一致；
  - 关联类型 `type I: Copy` 报 “Unknown trait bound 'Copy'”；
  - trait 实现签名不符时指出具体哪一项；
  - 给外部模块的记录写固有 impl 时说明“类型不在本模块定义”；
  - 分支块内给外层 `var` 赋值时说明“捕获是快照”，而不是提示 “declare it with var”；
  - 分支块内 `?` 的诊断指向缺失的分支结果类型。
- **负号与后缀优先级**：`-5.method()` 按 `-(5.method())` 解析，`-1[0]` 按 `-(1[0])` 解析。
  字面量没有后缀时才直接折叠负号，继续支持最小有符号整数。

## 6. D：待决策设计项（未确认前不实施）

以下涉及语言语义取舍，列出现状与推荐，由维护者决定后再拆为实施阶段。

### D1 隐式 task arena 的回收粒度

- 现状：`fn work(i: Int32) -> Int32 { sum(make_point(i)) }`（内部 `alloc`，引用不外逃）在 `main` 循环中调用约数百万次后，
  因 64 MiB 预算失败。隐式分配只在根任务结束时回收，长期运行的服务循环会隐性耗尽。
- 推荐：利用已有的返回引用/保留效果固定点，对“结果与参数都不携带 arena 来源、也不向参数所指 arena 对象写入引用”的调用，
  由调用方建立区域并在返回后回收（region inference 的保守子集）。至少先在 language.md 显著说明该行为，并在 `explain` 类诊断中提示。

### D2 枚举布局复用载荷区

- 现状：每个变体独占载荷区，宽度 `1 + Σ payload`。4 变体枚举嵌套 7 层超过 65536 槽上限；多变体消息枚举的内存是各变体之和。
- 推荐：保持 SSA/标量槽表示不变，在**内存布局**（Array 元素、arena、CPS 帧聚合）中按最大变体复用载荷区，构造时清零未选载荷。
  需评估与 Parallel 快照（只遍历活动载荷）、宿主边界和 FixedArray 的交互。

### D3 内建类型扩展方法与标准库的命名冲突

- 现状：用户 `impl Int32 { fn to_u64 .. }` 报重复，定位在 `joky/integer.jk:90`；标准库为内建类型新增任何方法都可能破坏用户代码。
- 选项：（a）禁止用户为内建类型写固有 impl，引导使用 trait；（b）允许，但按模块可见性解析，局部定义遮蔽标准库并给警告；
  （c）维持全局唯一，但诊断定位在用户代码。推荐 (a) 或 (b)，K2-12（私有成员不冲突）独立于此决策先做。

### D4 换行分隔与被丢弃的表达式值

- 现状：`let total = a` 换行 `- b` 实际返回 `a`；`[10, 20` 换行 `-5, 30]` 漏逗号得到 3 个元素，类型检查通过。
  `[]`、`()` 内换行不分隔，记录初始化 `{}` 内换行分隔；无任何“丢弃非 Unit 值”的提示。
- 推荐：对非末项、非 Unit、无副作用（字面量、一元/二元运算、取借用、解引用）的表达式报错或警告；
  `[]`/`()` 内“换行 + 行首一元运算符”至少给出诊断。项目目前没有警告机制，需先决定是否引入警告等级。

### D5 整数算术默认环绕

- 现状：`+ - *` 溢出静默环绕，`Duration` 加法溢出失败；最短的 `int32()` 是 wrapping 转换，受检版本为 `to_int32()`。
- 选项：（a）维持环绕，补充 `checked_*`/`wrapping_*` 方法与文档说明；（b）默认溢出失败（与“受检”风格一致），
  提供显式 `wrapping_add` 等。(b) 是不兼容变更且影响性能与三后端 lowering，需单独设计。

### D6 解释器帧与并发分支上限

- 现状：解释器把动态分支计入 `max_frames = 1024`，原生后端 `MAX_DYNAMIC_TASK_CHILDREN = 65536`（`src/runtime/cps.rs`）；
  1100 个 `branch` 或 join 写的 `fib(13)` 仅在解释器 Local 失败。约 1000 并发连接的服务无法在解释器下运行。
- 推荐：将分支计数与调用深度分开限额，默认值与原生后端对齐，或在 language.md 写明差异；与 K1-3 的原生深度限额一起决定。

### D7 词法安全与源码兼容

- 双向控制字符（U+202E、U+2066–2069）在注释和字符串中被接受，存在 Trojan Source 风险；推荐默认拒绝（字符串内可用 `\u{..}` 写出）。
- UTF-8 BOM 被拒（Unexpected character `\u{feff}`）；推荐文件开头允许并忽略。
- U+2028、换页符按空白处理但编辑器渲染为换行，与 D4 叠加会产生视觉误导；推荐拒绝或按换行处理。
- 仅 CR 换行的文件，诊断标题行号与渲染位置不一致，属 bug，可并入 K4。

### D8 其他

- 拼错的载荷变体名变成绑定：`if let Some(Rde) = o` 总是匹配，而顶层裸标识符一律按变体处理，两处规则不一致；
  新增变体还可能改变已有 `Some(x)` 的含义。可选：载荷内绑定与已知变体名“近似”时警告，或要求变体以 `Enum.` 限定。
- `"{let y = 1}"` 输出 `()`，与“Unit 写作 `{}`、不加 `()` 别名”的规定不一致，Show 输出应统一。
- 固定启动开销：Release 下对空程序 `check --no-prelude` 约 0.55 秒，疑为每次重新准备标准库，原因未定位；
  先 profile，再决定是否缓存标准库的准备结果。
- 拥有型或借用型 `dyn Read<Item = Int32>` 不满足泛型约束 `T: Read`，属于设计限制，需要时另立计划。

## 7. 实施与验证记录

| 阶段 | 状态 | 提交 | 实际执行的检查 |
| --- | --- | --- | --- |
| K0-1 | 已修复 | `fix: preserve ownership of generic Drop specializations` | Debug 定向测试 23 项通过，覆盖 JIT、解释器、独立 AOT 和共享前端拒绝用例；另跑 `exhaustive_parallel_drop_value_budgets` 通过。fmt、Clippy、diff 检查通过。按本次要求未跑全量。 |
| K0-2 | 已修复 | `fix: resolve closure metadata inside concurrent blocks` | Debug 定向测试 14 项通过；另跑 `exhaustive_concurrent_blocks_cancel_every_instruction` 通过。原始复现通过 CLI 检查，四种并发块各调用三种闭包的探针在 JIT、解释器、独立 AOT 均通过。fmt、Clippy 和 diff 检查通过，未跑全量。 |
| K0-3 | 已修复 | `fix: clean up cancelled interpreter siblings in registration order` | Debug 定向测试 8 项通过。race、join 失败、scope 主体失败及 isolate 内失败四种场景均核对 JIT、解释器、独立 AOT 的完整析构序列。另跑结构化结果、并发块、race、scope 和 isolate 五项取消穷举，全部通过。fmt、Clippy、diff 检查通过，未跑全量。 |
| K1-1 | 已修复 | `bda88494`、`fix: plan flat operator and conditional chains iteratively` | Debug 定向库测试 135 项、CLI 回归 2 项、加法逐指令取消穷举 1 项通过。Dev CLI 对 2000 层括号在 check 和 fmt 均返回带位置的 resource_limit；1000 项加法和 300 分支条件链在 JIT、解释器、独立 AOT 输出一致。fmt、Clippy、diff 检查通过，未跑全量。 |
| K1-2、K2-2 | 已修复 | `fix: bound nested generic inference and preserve literal context` | Debug 定向库测试 79 项、CLI 回归 2 项通过。30 层 id、or、and、unwrap 链通过 JIT、解释器及独立 AOT；比较 10 与 30 层的推导步数，另验证上下文定型、变量遮蔽、同位置 AST 和预算诊断。fmt、Clippy、diff 检查通过，未跑全量。 |
| K1-3 | 解释器回溯已修复，原生同步直调已验证测试原型 | `fix: fold adjacent identical interpreter trace frames in CLI diagnostics`、`test: prototype native stack preflight for direct owning calls` | macOS arm64 的 JIT 和 AOT 小栈回归验证了拒绝位置、拥有实参析构一次及失败后线程可复用。常规构建的原生栈保护仍待实施。见[原生栈保护提案](architecture.md#k1-3-原生栈保护提案)。 |
| K2-1 | 已修复 | `fix: infer generic argument types from module constants` | 模块常量及同名局部参数在 JIT、解释器和 AOT 的定向回归通过。 |
| K2-3 | 已修复 | `fix: reject mixed numeric literal defaults consistently` | 算术、数组、同型泛型调用和条件分支的双向拒绝用例及显式浮点上下文的三后端运行通过。补充提交 `fix: preserve numeric literal defaults in nested expressions` 覆盖泛型实参中的表达式和连续运算。 |
| K2-4 | 已修复 | `fix: dereference captured references for field access` | 三后端验证借用捕获、move 捕获及独占引用写入。 |
| K2-5 | 已修复 | `fix: preserve resolved record pattern identities` | 两个 CLI 回归、九个记录模式库测试通过，覆盖三后端、别名及私有字段拒绝。 |
| K2-6 | 已修复 | `fix: infer type arguments from known associated projections` | 10 个关联类型定向测试通过，含无结果注解调用的三后端执行。 |
| K2-7 | 已修复 | `fix: contextualize function pointers in generic arguments` | Option 与泛型记录的无捕获闭包在三后端执行，捕获闭包转换继续报错。 |
| K2-8 | 已修复 | `fix: apply reference and owning dyn coercions in generic calls` | Arena.alloc、泛型可变引用返回和 Array.push 的拥有 dyn 擦除在三后端执行通过。 |
| K2-9 | 已修复 | `fix: reborrow referenced fields in suspending functions` | 三后端均验证挂起后独占投影写入，共享前端继续拒绝重叠的独占借用。 |
| K2-10 | 已修复（零捕获） | `fix: infer capture-free callbacks in suspending callers` | 三后端运行无捕获 map 回调，借用捕获仍受原限制且诊断提示 move。未放宽有捕获闭包的活跃区间。 |
| K2-11 | 已修复 | `fix: prepare suspending closure calls for concurrent branches` | 三后端执行 race 和 join，拒绝 FnOnce 重复使用，挂起闭包取消穷举通过。 |
| K2-12 | 已修复 | `fix: scope overlapping private members to their defining modules` | 三后端验证模块私有方法和依赖私有常量的解析，同模块重复和公开冲突仍拒绝。另跑常量测试 19 项、源模块可见性测试 6 项通过。 |
| K3 | 已修复 | `7d4d4b60`、`b36c033d`、`3b2dc97b` | 2,000 次短借用、1,000 次打印/自增/字符串插值和 300 臂整数 match 在 JIT、解释器和独立 AOT 上输出正确。借用相关库回归 384 项、CLI 定向回归 8 项通过；另跑 27 个库穷举和 1 个 CLI 穷举通过。fmt、Clippy、diff 检查通过，未跑全量。见[借用分析的规模与边界](architecture.md#k3-借用分析的规模与边界)。 |
| K4 | 已修复 | `8b716635`、`cd5e9bd1` | 29 个共享前端诊断用例核对错误码和消息，并对关键调用、类型布局和分支核对源码范围。解析、类型推导、trait、模块导入、常量及 CLI 定向回归通过；负号后缀和上下文推导在 JIT、解释器及独立 AOT 运行通过。fmt、Clippy、diff 检查通过，未跑全量。 |
| D1–D8 | 待决策 | | |
