# for 与迭代器分阶段开发计划

状态：2026-10-23，设计基线 `8b30deb`；第一、二阶段及第三阶段的 Array 借用迭代已完成验收，
记录见第 6–8 节。Map 迭代游标与第四阶段仍待实施。
当前能力以 [语言参考](language.md#for-循环与迭代器) 为准；第 1–5 节保留设计与阶段划分。

## 1. 目标与边界

通过标准库 Iterator 协议和 for 语法统一逐项遍历。复用 trait 关联类型、静态分派、
while/match、所有权清理与 CPS，不新增后端迭代指令或改变运行时 ABI。
首阶段交付同步 next、整数 range 和自定义迭代器；容器消费、借用迭代与组合器分阶段实施。

## 2. 协议与循环语义

拟议协议位于 joky/iter，包含关联类型 Item 和同步方法
`fn next(&mut self) @noescape -> Option<Self.Item>`。
Item 沿用既有泛型与存储资格，支持普通值和拥有值；不借此放宽引用或视图存储。

`for pattern in expression { body }` 的 expression 只求值一次，放入独立作用域的
可变隐藏绑定。按标准库 Iterator 身份调用 next，Some 进入本轮绑定和循环体，
第一次 None 退出。用户同名 trait、固有 next 方法或 Option/Some 名称不能劫持生成调用。
隐藏绑定不可与用户标识符冲突；错误保留源循环、表达式和模式的位置。

- 首版 pattern 支持变量、`_` 与现有不可失败解构；可失败模式报错，不隐式过滤。
- 循环返回 Unit；循环体正常结束时丢弃末值，绑定仅在本轮有效。
- continue 清理本轮后再次 next；break 退出最近循环；return 退出函数。
- 拥有迭代器按值移动，Copy 迭代器按现有规则复制。提前退出清理迭代器和未移交元素。
- 失败与取消沿用原清理顺序，每项只清理一次。循环体可挂起，迭代器及元素须满足
  既有跨挂起存储资格；恢复不得重放取项或输入求值。next 首版不能挂起。
- 不自动将容器转换为迭代器；IntoIterator 后续独立设计。

解析与类型检查保留足够的 for 信息以验证标准协议和模式，随后降低为共享循环与匹配。
模块装载应自动包含语法所需标准协议，包含无 prelude 场景；range 函数仍通过显式导入使用。

## 3. 整数范围与未来范围模式

第一阶段 range 提供递增半开区间，start 包含、end 不包含；start >= end 时为空。
先提供明确类型的整数接口，具体首版类型在实施核对后记录，不隐式进行有损转换。
递增必须在 current < end 时进行，确保最大整数边界不溢出；耗尽后继续 next 保持 None。
首版不含步长、倒序、无界或闭区间。

未来目标语法为表达式 `a..b` / `a..=b`，分别构造半开/闭区间迭代器；
match 中的同形模式直接比较值，不创建或消费迭代器。range 保留为普通库接口。
范围模式另行处理端点常量、同类型检查、空区间、重叠及穷尽性；不能将函数调用
`range(a, b)` 当成普通模式。闭区间迭代显式记录结束状态，不能使用 end + 1 实现。
本计划第一阶段只预留上述方向，不交付范围运算符或 match 范围模式。

## 4. 后续容器与组合接口

第二阶段 Array.into_iter 消费容器并按索引顺序逐项移交值；不得以反向 pop 替代。
正常完成、提前退出、失败和取消均清理剩余元素，不移动已移交的元素。

第三阶段 iter/iter_mut 需要先补齐借用迭代器的存储与来源模型：保存容器借用、
元素有效期、前一项存活时是否允许 next、跨挂起保活和取消释放。
现有视图聚合与独占引用泛型限制不能用语法糖绕开；不预先承诺 GAT 或显式生命周期语法。
Map 迭代沿用未指定顺序与禁止可变 key 的规则。

第四阶段再评估 IntoIterator 自动转换、enumerate、map/filter 和其他惰性组合器，
包括 callable 能力、效果及清理契约。

## 5. 第一阶段实施与验收

1. 核对 AST、模块依赖注入、trait 身份绑定和泛型模板检查入口。
2. 增加 joky/iter 的 Iterator 与整数 range，接入内嵌标准库。
3. 增加 for/in 解析、作用域和不可失败模式检查；共享现有循环规划。
4. 执行生成代码验证 range 空/倒序/负数/最大值边界、自定义 Iterator、泛型约束、
   嵌套循环、解构、遮蔽、只求值一次、break/continue/return 与 Unit 结果。
5. 验证拥有 Item 和迭代器的完成/提前退出/失败清理、循环体多次 Pending/恢复/取消，
   以及非法协议、可失败模式、非法存储和效果的前端拒绝。三后端按行为共同覆盖。
6. 先运行针对性测试，再执行默认 Debug `./scripts/check.sh`；按新增取消路径补相关
   Debug 穷举，不运行无关压力或 Release。更新 language、syntax 和本文验收记录后提交。

设计提交仅检查内容、本地链接与 `git diff --check`；以上均为待实施契约，非现有可执行示例。

## 6. 第一阶段实施与验收（2026-10-23）

首版交付 `joky/iter.Iterator`、协议入口 next、Copy Range 与 Int32 半开区间 range，
以及 `for pattern in iterator`。Int32 之外的整数范围留待后续明确接口后扩展。
范围表达式与 match 范围模式、容器 into_iter/iter/iter_mut、IntoIterator 和组合器未实施。

实施核对后选择与 while let 一致的解析期展开，而非增加独立 For AST：
普通 Let/Destructure 已能检查不可失败绑定，标准库泛型 helper 能强制 Iterator 约束，
不可拼写别名能固定协议和 Option 身份；原始 span 保留在输入、模式和循环节点。
这使现有泛型、闭包、模块限定、借用和所有权遍历自动覆盖 for，无需增加第二套节点遍历。
未修改后端、运行时 ABI、引用存储准入或任务生命周期机制。

主要证据：

- [行为 fixture](../tests/fixtures/for_iterators.jk)：Int32 空/反向/负数/极值区间、耗尽、
  只求值一次、泛型协议、同名固有 next、元组/记录解构、嵌套与遮蔽、Unit、闭包、
  String 与用户拥有记录、完成/continue/break/return/失败清理。
- [VM 测试](../src/vm/tests/language/for_iterator_tests.rs)：JIT/解释器执行；关闭 prelude、
  标准名称遮蔽、插值内部隐式依赖、非法协议/模式/作用域/移动；两种 VM 的多次 Pending、
  恢复不重放 next、取消清理与 arena 归零；解释器资源边界和逐指令取消。
- [独立 AOT 测试](../tests/cli/language.rs)：同一 fixture 的同步/CPS 入口、真实 yield、
  输出/析构事件与失败清理，构建时关闭 prelude。

实际验证均使用 Debug：

| 检查 | 结果 |
| --- | --- |
| for_iterators VM 针对性测试，最终版本由默认检查再次执行 | 5 项通过，逐指令取消默认忽略；覆盖 JIT 与解释器 |
| `cargo test --test cli for_iterators` | 1 项通过，实际构建并执行同步/CPS 与失败路径的独立原生程序 |
| `cargo test exhaustive_for_iterators_instruction_cancellation --lib -- --ignored` | 1 项通过，逐指令取消后无重复析构，arena 归零 |
| `./scripts/check.sh` | `Checks passed (quick, Debug)`；fmt、Clippy、1534 项库测试、295 项 CLI 测试及其他默认集成测试通过；while/Map 三后端短输入正确性通过 |
| 文档本地链接与 `git diff --check` | 通过 |

默认库测试忽略 117 项，CLI 忽略 8 项；本轮只额外运行相关的迭代器取消穷举。
非法迭代对象的 CLI 人工探针指向 in 右侧原始表达式；外层循环中的输入提前跳转探针通过。
未做 Release、无关压力、性能测量、Godot 或其他平台验证。检查通过后仅补充本文记录，未再改代码。

## 7. 第二阶段：Array 消费式迭代（2026-10-23）

实现基线 `6504148`。新增 `joky/array.IntoIter<T>`，包含私有 Array<T> 字段，
实现标准 Iterator<Item = T>；模块函数 into_iter 与 Array.into_iter 方法共用算法。
无需 T: Copy，按原数组下标正序逐项移交元素，空数组及耗尽后返回 None。
未开始、部分消费、提前 return/break、失败及取消均沿普通字段清理释放剩余元素。
已取出的元素可比迭代器活得更久；不承诺未消费元素的析构顺序。

现有存储只允许完整的已初始化前缀；本阶段采用转换时原地反转一次，再逐次 take_last。
交付顺序仍是原数组正序，初始化 O(n)、取项 O(1)，不另建元素缓冲区。
每次 swap 保持全部元素初始化，take_last 同步缩短前缀并移交清理责任，因此复用
三后端既有交换、移出与清理原语；没有新增存储布局、ABI、后端指令或借用准入。
若将来需要 O(1) 构造，再单独设计支持已消费前缀的游标存储，不把当前实现称为零成本构造。

验收证据：

- [共享 fixture](../tests/fixtures/array_into_iter.jk)：0–6 项奇偶长度正序与耗尽，Unit、String、
  共享引用、嵌套数组、带 Drop 的拥有记录，完整遍历、continue/break/return、失败、
  未开始即丢弃，以及已交付元素在迭代器释放后继续使用。
- [VM 测试](../src/vm/tests/containers/array_into_iter_tests.rs)：JIT/解释器一致性，
  分配预算失败扫描至首次完整成功，多次 Pending 不重放、取消时当前与剩余元素清理，
  资源边界与逐指令取消覆盖构造、反转及移交；所有可观察创建均析构且无重复，arena 归零。
  拒绝原数组/迭代器重复使用、活动借用下移动、私有存储访问及直接 for Array。
- [AOT 测试](../tests/cli/containers.rs)：独立构建运行同一 fixture 的同步和真实 yield 路径，
  核对交付顺序、移动后读取和析构次数；失败时保留原错误并清理所有元素。
- 标准库导出身份测试同步登记 IntoIter 与 into_iter，保留 Array 私有存储限制。

默认检查暴露一项既有 dyn 约束边界测试依赖 prelude 的泛型记录实例数量：新增 IntoIter
使该用例先触及记录实例上限，未进入它应验证的 256 个约束边界。将该独立用例改为
不加载 prelude 的共享前端检查，保留 256 成功、257 拒绝及错误位置断言；未修改编译器限额。

第二阶段结束时尚未实现 Array.iter/iter_mut、Map 迭代、IntoIterator 自动转换与组合器；
Array 借用迭代的后续交付见第 8 节。

实际验证均使用 Debug：

| 检查 | 结果 |
| --- | --- |
| `cargo test array_into_iter` | 5 项 VM 测试、1 项独立 AOT 测试通过；取消穷举默认忽略 |
| `cargo test exhaustive_array_into_iter_instruction_cancellation --lib -- --ignored` | 1 项通过，涵盖构造、反转、取项和剩余元素清理 |
| `cargo test borrowed_dyn_qualification_shares_exact_obligation_boundary --lib` | 隔离 prelude 后 1 项通过，原约束边界保持不变 |
| `./scripts/check.sh` | 修正上述旧测试后重跑通过：fmt、Clippy、1539 项库测试、296 项 CLI 测试及其他默认集成测试通过，while/Map 三后端短输入正确性通过 |
| `cargo test array_into_iter_rejects --lib`，`cargo fmt -- --check`，`cargo clippy --all-targets -- -D warnings` | 最后将静态拒绝矩阵改为共享前端后，补跑受影响测试及 Rust 检查，均通过；未再改实现 |
| 文档本地链接与 `git diff --check` | 通过 |

默认库测试忽略 118 项、CLI 忽略 8 项；本轮仅额外运行相关的数组迭代器取消穷举。
未运行 Release、无关压力、Godot、性能测量或其他平台验证。检查结束后仅补记本文验收结果。

## 8. 第三阶段：Array 借用迭代（2026-10-23）

实现基线 `fc775cc`。本次交付 Array.iter/iter_mut，以及其所需的借用游标存储、
来源追踪和跨挂起规则。Map 的迭代游标尚未交付，其 key/value 接口仍须保持顺序未指定、
key 不可修改的约束。IntoIterator 与组合器仍属第四阶段。

普通 Iterator.next 的 @noescape 契约不能返回来源于接收者的元素借用，
`Option<&mut T>` 也不满足既有存储资格。因此本阶段采用独立的借用游标接口，
引入仅可借用的 `joky/iter.Iter<T>`：iter 返回 `&Iter<T>`，iter_mut 返回 `&mut Iter<T>`。
手动接口分为 advance 与 current/current_mut；for 根据已解析的游标类型完成两步操作，
每项引用直接绑定在本轮局部变量中。普通 Iterator、关联 Item 与 @noescape 保持原契约，
不新增 GAT、生命周期参数或通用引用聚合资格。

存储及来源契约：

- 每个游标只分配一个任务 arena 描述符，保存非拥有数据指针、长度、步幅与位置；
  构造和取项均 O(1)，取项不分配。原 Buffer 布局和 CPS 帧 ABI 不变。
- 共享游标副本共享位置，独立调用 iter 才创建独立游标。首次推进前、耗尽后的 current
  受检失败；耗尽状态稳定。共享元素引用可以跨后续 advance，仍指向原元素。
- 独占元素引用仍活跃时，推进和冲突访问均由原有借用分析拒绝；以整个数组为借用单位，
  不因不同下标放宽冲突。独占游标进入 for 时移动；`&mut *cursor` 可显式重借用并在循环后继续使用。
- 游标和取项均沿同一来源链追溯至数组；不得返回局部数组的游标/元素引用，不得在活动借用下
  扩容、移动或释放数组。游标不能存入普通聚合、容器或通用类型实参。
- 可挂起引用仍要求符合原有存储资格；取消只清理数组的元素所有权，不重复清理借用项。
  描述符不持有元素清理责任，由任务 arena 统一回收，失败预算包含描述符分配。

实现使用 Type::BufferIterator 和既有 BufferOp::Storage；五个受信任源码原语共用三个私有
runtime helper，JIT/AOT 共用原生降低，解释器使用同一描述符实现。
解析期展开的条件 Match 增加 iteration 标记，定型后由共享 IR 选择借用分支，
生成调用仍使用不可拼写的标准库别名。没有按用户方法名猜测 iter 或绕过来源分析。

拥有记录的字段访问测试发现一处既有检查遗漏：挂起函数在判定字段是否可安全借用时，
需先通过 pointee 解析浅名义引用目标，随后再检查字段的 is_suspend_safe_owner。
该路径与实际字段借用降低统一；未放宽存储资格或借用冲突，也未调整编译器分析预算。

验收证据：

- [共享 fixture](../tests/fixtures/array_borrow_iter.jk)：普通值、空数组、Unit、String、
  嵌套数组、拥有记录，泛型转发/遍历、输入只求值一次、顺序、耗尽、共享游标别名、
  元素跨推进/局部游标作用域存活、独占游标重借用、原地修改与 continue/break/return。
- [VM 测试](../src/vm/tests/containers/array_borrow_iter_tests.rs)：两种 VM 的完成、失败、
  分配预算扫描、多次 Pending/恢复/取消、析构唯一性和 arena 归零；共享前端拒绝借用冲突、
  局部来源逃逸、引用元素及非法视图存储；取消穷举覆盖新游标和嵌套拥有字段借用。
- [独立 AOT 测试](../tests/cli/containers.rs)：关闭 prelude，执行同一 fixture、真实 yield、
  独占与共享元素跨挂起读取、受检 current 错误和失败清理。

实际验证均使用 Debug：

| 检查 | 结果 |
| --- | --- |
| `cargo test array_borrow_iter` | 6 项 VM 测试与 1 项独立 AOT 测试通过；逐指令取消默认忽略 |
| `cargo test exhaustive_array_borrow_iter_instruction_cancellation --lib -- --ignored` | 1 项通过；取消后无重复析构，arena 归零 |
| `./scripts/check.sh` | `Checks passed (quick, Debug)`；fmt、Clippy、1545 项库测试、297 项 CLI 测试及其他默认集成测试通过，while/Map 三后端短输入正确性通过 |
| 文档本地链接与 `git diff --check` | 通过 |

默认库测试忽略 119 项、CLI 忽略 8 项；本轮额外运行相关的借用迭代器取消穷举。
未运行 Release、无关压力、Godot、性能测量或其他平台验证。默认检查后仅补充文档记录，未再改代码。
