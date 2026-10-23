# for 与迭代器分阶段开发计划

状态：2026-10-23，设计基线 `8b30deb`；第一、二阶段及第三阶段的 Array/Slice 借用迭代、Map 单列及成对遍历已完成，
记录见第 6–14 节；第四阶段 A 的 IntoIterator 与借用自动转换、第四阶段 B/C 的普通与借用 enumerate 已完成，
map/filter 待后续实施。
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

### 第四阶段 A：自动转换设计

本轮先完成循环入口转换，组合器单独推进。普通值通过标准库 IntoIterator 协议
转换一次；协议提供关联类型 `IntoIter: Iterator` 和消费 self 的 into_iter 方法。
Item 由返回迭代器的 Iterator 协议确定，不引入重复的关联类型或嵌套关联类型等式。
现有 Iterator 通过泛型实现返回自身，Array 的实现复用已有 into_iter。
用户容器可实现 IntoIterator；仅存在同名固有方法不构成协议实现。

借用入口由编译器按标准容器身份接入第三阶段游标：

| 输入 | 入口 | 循环元素 |
| --- | --- | --- |
| Array<T> | into_iter | T，消费数组 |
| &Array<T> / &Slice<T> | iter | &T |
| &mut Array<T> / &mut Slice<T> | iter_mut | &mut T |
| &Map<K, V> | iter | &Entry<K, V> |
| &mut Map<K, V> | iter_mut | &mut Entry<K, V>，key 仍只读 |
| 已有普通迭代器或借用游标 | 保留原有入口 | 保留原有元素语义 |

这提供 Rust 风格的 `for value in &values` / `&mut values`，但不开放通用引用
trait 实现、GAT、引用聚合存储或任意用户借用迭代器。Map 按值消费继续显式使用
into_entries；固定数组不在本轮范围。显式 iter、iter_mut、into_iter 保留。
输入只求值一次，转换不允许挂起；提前退出、失败与取消沿用已有所有权清理。
借用来源、互斥、跨挂起存储资格和可变游标移动规则均不放宽。

验收覆盖 Array 三种入口、Slice/Map 借用、自定义与泛型 IntoIterator、标准协议
身份、一次求值/转换、空输入、提前退出、错误诊断，以及转换和循环期间的资源清理。
三后端按行为共同覆盖；先执行针对性测试，再执行默认 Debug 检查和相关取消穷举。

### 第四阶段 B：普通 Iterator 的 enumerate 设计

本轮增加 joky/iter.Enumerate<I>、enumerate<I: Iterator>(I)，以及 IntoIterator 的
默认 enumerate(self) 方法。适配器按值保存原迭代器，Item 为 (UInt64, I.Item)，
编号从 0 开始；泛型函数与嵌套 enumerate 沿用普通 Iterator / IntoIterator。
链式入口先执行 IntoIterator 转换，因此也支持 Array；模块函数只包装 Iterator。
借用游标的 Indexed 视图与 map/filter 留待后续设计。方法放在 IntoIterator 而非
Iterator，避免新增按值接收者破坏既有 dyn Iterator 的接口资格。

构造只保存迭代器，不调用 next 或建立结果数组。每次 next 恰好调用一次标准
Iterator.next，得到 Some 时附加编号并推进计数，None 不推进编号；不额外保证融合
（底层在 None 后还能返回 Some 时，显式继续调用可获得下一个编号）。
UInt64 最大值仍是有效编号；之后若再得到 Some，报告 Enumeration index overflow，
不回绕为 0。该次已取出的元素和剩余迭代器按正常失败路径清理。

保持 next 的同步与 @noescape 契约，Item 沿用普通值、拥有值和受支持共享引用的
存储规则；不允许借此把 &mut T 放入元组。拥有迭代器与元素不复制，Copy 迭代器
按语言已有 Copy 规则工作；暂停、提前退出、失败及取消沿用所有权清理。

验收包括惰性、顺序、空输入、泛型与嵌套、默认方法及标准 next 选择、非融合输入、
上限边界、拥有元素移交/清理，以及借用游标拒绝。覆盖 JIT、解释器、独立 AOT，
先做针对性验证，再运行默认 Debug 检查和本适配器的相关取消穷举。

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

## 9. 第三阶段续：Slice 与 Map 单列遍历（2026-10-23）

实现基线 `b87a51b`。本次交付 `joky/slice.iter(&Slice<T>) -> &Iter<T>` 和
`iter_mut(&mut Slice<T>) -> &mut Iter<T>`，使 Map 现有 keys/values/values_mut 切片
可以直接参与 for 单列遍历。公开语法及示例见 [语言参考](language.md#slice-与-map-单列遍历)。

两个 Slice 构造原语使用独立受检签名，共用原 Array 游标 runtime helper。
切片头已有子区间地址、长度及元素布局，因此不新增运行时描述符或改变 Buffer/CPS ABI。
子切片只遍历自身区间，仍通过既有来源分析保留原容器借用；共享/独占元素资格、
游标位置、耗尽、重借用、挂起和清理均沿用第 8 节契约。

Map 只开放现有列视图：keys 始终只读，values_mut 可修改值，不允许通过活动借用结构性修改
Map。keys/values 相同下标在共享借用期间对应同一项，遍历顺序仍未指定；删除重插不保序。
本次没有提供 key/value 借用元组、Map 专用成对游标、IntoIterator 或组合器。

验收覆盖：

- [共享 fixture](../tests/fixtures/slice_borrow_iter.jk)：泛型转发、非零起点和嵌套子切片、
  空切片、Unit/String 元素、只修改指定区间、游标重借用及耗尽、元素超过局部视图绑定存活；
  Map 空表、删除重插、列间对应、共享/可变值遍历与拥有记录的提前返回和失败清理。
- [VM 测试](../src/vm/tests/containers/slice_borrow_iter_tests.rs)：两种 VM 执行，
  子切片拥有元素跨多次 Pending/恢复/取消、分配失败预算扫描与析构唯一性、arena 归零；
  共享前端拒绝原容器/父视图冲突、可变键借用、含引用元素及局部来源逃逸。
- [独立 AOT 测试](../tests/cli/containers.rs)：相同 fixture 的同步行为、真实 yield、
  提前返回及失败清理。相关取消穷举覆盖 Slice → 子切片 → Iter → 元素的来源链。

实际验证均使用 Debug：

| 检查 | 结果 |
| --- | --- |
| `cargo test slice_borrow_iter` | 5 项 VM 测试及 1 项独立 AOT 测试通过；逐指令取消默认忽略 |
| `cargo test exhaustive_slice_borrow_iter_instruction_cancellation --lib -- --ignored` | 1 项通过；取消后无重复析构，arena 归零 |
| 语言参考新增示例 | `cargo build --lib` 后关闭 prelude，在 JIT、解释器、独立 AOT 均输出 35 |
| `./scripts/check.sh` | `Checks passed (quick, Debug)`；fmt、Clippy、1550 项库测试、298 项 CLI 测试及其他默认集成测试通过，while/Map 三后端短输入正确性通过 |
| 文档本地链接与 `git diff --check` | 通过 |

默认库测试忽略 120 项、CLI 忽略 8 项；本轮额外运行上述相关的逐指令取消穷举。
未运行 Release、无关压力、Godot、性能测量或其他平台验证。默认检查后仅补充文档记录，未再改代码。

## 10. 第三阶段续：Map 成对借用游标（2026-10-23）

实现基线 `cb720be`。本次交付 Map.iter/iter_mut、Iter/Entry 借用目标，以及
advance/current/current_mut、entry_key/entry_value/entry_value_mut 模块接口。
for 每轮给出 Entry 视图，具体规则见 [语言参考](language.md#map-成对借用遍历)。

实现继续保留普通 Iterator.next 的 @noescape 契约和引用聚合限制。Entry 保存对应项的两个地址，
键始终只读，独占视图允许修改值；不构造 `(&K, &mut V)` 元组，也不放宽原 Map 的整体借用规则。
因此修改值前需结束 key 的借用，或先计算所需 key 信息。共享 Entry 可跨推进保留原项身份。

构造原语从具体拥有记录的受检 Array 字段布局取得两列，模板阶段允许尚未具体化的类型参数，
实际生成代码前检查两列类型与偏移；运行时检查长度一致。前端及所有后端使用同一类型和布局契约。
现有 Int32 存储原语测试夹具排除新的多类型参数原语；它们由专门的 Map 行为 fixture 覆盖。

创建游标和每次取项均 O(1)。为了保证共享 Entry 的稳定身份，当前每次取项分配一个任务 arena
快照，逐项累计 O(n) 描述符空间；仅保存地址，不复制或消费 key/value，描述符不承担析构责任。
分配计入原任务预算，失败/取消沿用 Map 的唯一清理责任。没有改变 Map、Buffer 或 CPS 帧布局，
没有优化或测量遍历性能。IntoIterator、组合器与范围运算符仍属后续工作。

验收覆盖：

- [共享 fixture](../tests/fixtures/map_borrow_iter.jk)：空表、删除重插后的配对、共享游标别名、
  Entry 跨推进稳定、泛型转发、独占重借用、耗尽、String key、拥有记录和嵌套 Array value，
  提前返回、运行时失败和真实挂起；断言不依赖 Map 的遍历顺序。
- [VM 测试](../src/vm/tests/containers/map_borrow_iter_tests.rs)：JIT/解释器执行、分配预算扫描、
  多次 Pending 后恢复或取消、析构唯一性与 arena 归零；共享前端拒绝结构修改、键修改、
  冲突借用、Entry/Iter 裸值与非法存储、局部来源逃逸和引用元素。
- [独立 AOT 测试](../tests/cli/containers.rs)：同一 fixture 的同步/CPS 行为、真实 yield、
  Entry 及元素读写、提前返回、无有效当前项和失败清理。

实际验证均使用 Debug：

| 检查 | 结果 |
| --- | --- |
| `cargo test map_borrow_iter` | 6 项 VM 测试与 1 项独立 AOT 测试通过；逐指令取消默认忽略 |
| `cargo test exhaustive_map_borrow_iter_instruction_cancellation --lib -- --ignored` | 1 项通过；取消后无重复析构，arena 归零 |
| `cargo test buffer_storage` | 4 项相关存储测试通过；新的多类型参数原语由上述 Map fixture 覆盖 |
| 语言参考新增示例 | `cargo build --lib` 后关闭 prelude，在 JIT、解释器、独立 AOT 均输出 36 |
| `./scripts/check.sh` | `Checks passed (quick, Debug)`；fmt、Clippy、1556 项库测试、299 项 CLI 测试及其他默认集成测试通过，while/Map 三后端短输入正确性通过 |
| 文档本地链接与 `git diff --check` | 通过 |

默认库测试忽略 121 项、CLI 忽略 8 项；本轮额外运行上述相关的逐指令取消穷举。
未运行 Release、无关压力、Godot、性能测量或其他平台验证。默认检查后仅补充文档记录，未再改代码。

## 11. 第四阶段 A：IntoIterator 与借用自动转换（2026-10-23）

设计提交 `3feebd1`。本次交付公开 IntoIterator 协议、普通 Iterator 的恒等转换、
Array 消费转换，以及 Array/Slice/Map 引用的自动借用转换。
接口与示例见 [语言参考](language.md#intoiterator-与自动转换)。组合器留待下一阶段。

解析器先用隐藏绑定保存输入，再生成编译器专用的转换标记。这样输入只求值一次，
其中的提前结束仍属于外层控制流，独占引用按普通绑定规则移动。
类型规划将普通值转成固定标准库 into_iter 调用；容器引用按声明身份接入既有借用接口，
已存在的借用游标保持原样。类型推断与实际生成代码共用同一转换选择，随后复用第三阶段的
借用循环步骤和第一阶段的 next/match 循环。不新增后端指令、ABI、存储类型或引用豁免。

IntoIterator 只声明 IntoIter: Iterator，Item 由返回迭代器决定，未增加嵌套关联类型等式。
借用入口是编译器支持的标准容器转换，不能用作 I: IntoIterator 的实参；
自定义普通容器可实现公开协议，用户同名 trait 或固有 into_iter/iter 方法不能替代标准身份。
Map 按值循环仍显式使用 into_entries，固定数组与通用引用 trait 实现不在本轮范围。

主要证据：

- [协议 fixture](../tests/fixtures/for_into_iter.jk)：输入与转换各执行一次、标准 trait
  优先于同名固有方法、用户容器、泛型 IntoIterator/Iterator、空数组、显式转换与转换失败。
- [VM 测试](../src/vm/tests/language/for_iterator_tests.rs)：JIT/解释器、关闭 prelude、
  跨模块泛型调用、假协议、非法关联类型/效果、借用冲突、独占引用移动及 Map key 只读。
- 复用 Array 消费与 Array/Slice/Map 借用 fixture，将部分循环改为自动转换，保留显式 API
  与重借用覆盖。既有资源边界、分配失败、Pending/恢复/取消、析构唯一性和 arena 归零断言
  同时验证新入口，避免再复制一套生命周期矩阵。
- [独立 AOT 测试](../tests/cli/language.rs) 验证导入模块中的自定义转换及转换失败；
  [容器 AOT 测试](../tests/cli/containers.rs) 验证上述共享 fixture 的同步/CPS 行为与清理。

实际验证均使用 Debug：

| 检查 | 结果 |
| --- | --- |
| `cargo test --lib iter -- --nocapture` | 63 项通过；7 项压力测试默认忽略 |
| `cargo test for_into_iter` | 最终 3 项 VM 测试与 1 项独立 AOT 测试通过 |
| `cargo test --test cli iter_` | 7 项通过；1 项网络压力测试默认忽略 |
| 相关逐指令取消穷举 | for_iterators、array_into_iter、array_borrow_iter、slice_borrow_iter、map_borrow_iter 共 5 项通过 |
| 语言参考新增/调整示例 | 关闭 prelude，在 JIT、解释器与独立 AOT 分别输出 15、60 |
| `./scripts/check.sh` | `Checks passed (quick, Debug)`；fmt、Clippy、1559 项库测试、300 项 CLI 测试及其他默认集成测试通过，while/Map 三后端短输入正确性通过 |
| 文档本地链接与 `git diff --check` | 通过 |

逐指令取消使用 `cargo test --lib -- --ignored` 并逐一传入上述五组的完整
`exhaustive_*_instruction_cancellation` 测试名。未运行 Release、无关压力、Godot、
性能测量或其他平台验证。

默认库测试忽略 121 项、CLI 忽略 8 项；本轮额外运行上述相关取消穷举。
默认检查后仅补充文档验收记录，未再改代码。

## 12. 第四阶段 B：惰性 enumerate（2026-10-23）

设计提交 `25b2966`。本次增加 Enumerate<I>、模块函数 enumerate，以及 IntoIterator 的
默认 enumerate 方法；普通 Iterator 通过已有 blanket impl 获得链式入口，Array 和自定义
IntoIterator 输入也可直接使用。具体接口见 [语言参考](language.md#enumerate-带编号遍历)。

按值消费的链式方法放在 IntoIterator，保留 Iterator 原有的 dyn 接口资格；
普通适配器仍只包装 Iterator。IntoIterator 转换立即执行，之后的适配器构造不预读元素。
每步固定调用标准 Iterator.next，Some 增加编号，None 不改变编号且不额外融合底层。
UInt64 最大编号可以成功返回，后续再出现 Some 才报溢出并清理已取出的元素。
无新后端指令、ABI、运行时类型或借用豁免；借用编号视图和 map/filter 不在本轮范围。

实现同时修复默认方法已替换 Self 后的跨模块名义类型校验：生成的 trait 实现使用
当前编译模块验证类型身份，包括仍带刚性 impl 参数的默认方法；源码名称、私有函数和
成员查找继续使用 trait 定义作用域。回归覆盖泛型/具体外部 Self 调用私有泛型 helper，
以及调用方同名 helper 不得劫持解析。

主要证据：

- [普通 fixture](../tests/fixtures/iterator_enumerate.jk)：惰性、零基顺序、部分消费后包装、
  空输入、None 后继续取项、泛型/嵌套、Array 直接转换、dyn Iterator 兼容、Unit/共享引用、
  同名固有 next、拥有元素移交、continue/break/return/失败和挂起。
- [VM 测试](../src/vm/tests/language/iterator_enumerate_tests.rs)：JIT/解释器、关闭 prelude、
  未启动即丢弃、已取出项独立存活、分配失败、Pending/恢复/取消、编号不重放、
  析构唯一性与 arena 归零；共享前端拒绝借用游标、重复移动和私有状态访问。
- [边界 fixture](../tests/fixtures/iterator_enumerate_boundaries.jk) 与
  [仅测试探针](../tests/fixtures/iterator_enumerate_invariants.jk)：在隔离标准库副本中设置起始编号，
  用生产 next 验证最大编号、None 后恢复及溢出清理；正式标准库不导出此探针。
- [独立 AOT](../tests/cli/language.rs) 验证同步/CPS、真实 yield、提前退出和失败清理；
  [AOT 边界测试](../src/backends/aot/tests/iterator_enumerate.rs) 验证相同上限与拥有元素清理契约。

实际验证均使用 Debug：

| 检查 | 结果 |
| --- | --- |
| `cargo test iterator_enumerate` | 8 项库测试（包含 AOT 边界）及 1 项独立 AOT CLI 测试通过；逐指令穷举默认忽略 |
| `cargo test --lib trait_defaults` | 7 项通过；逐指令穷举默认忽略 |
| 相关逐指令取消穷举 | enumerate 与 trait 默认方法关联拥有值两组通过 |
| 语言参考新增示例 | 关闭 prelude，在 JIT、解释器和独立 AOT 均输出 63 |
| `./scripts/check.sh` | `Checks passed (quick, Debug)`；fmt、Clippy、1568 项库测试、301 项 CLI 测试及其他默认集成测试通过，while/Map 三后端短输入正确性通过 |
| 文档本地链接与 `git diff --check` | 通过 |

取消穷举使用 `cargo test --lib -- --ignored exhaustive_iterator_enumerate_instruction_cancellation exhaustive_trait_defaults_associated_owners_instruction_cancellation`。
未运行 Release、无关压力、Godot、性能测量或其他平台验证。

默认库测试忽略 122 项、CLI 忽略 8 项；本轮额外运行上述两组相关取消穷举。
默认检查后仅补充文档验收记录，未再改代码。

## 13. 第四阶段 C 设计：借用 enumerate（2026-10-23）

本轮给 Array/Slice 的 Iter 和 Map 的 Entries 增加惰性编号适配器。沿用
advance/current 协议，每轮直接返回借用编号视图，不将独占引用放入元组或 Option。
普通 IntoIterator.enumerate 与其 dyn Iterator 兼容性保持不变。

公开接口计划放在 joky/iter：

- enumerate_ref / enumerate_mut 接收共享 / 独占 Iter，返回对应的 EnumerateIter 借用。
- enumerate_entries / enumerate_entries_mut 接收 Entries，返回 EnumerateEntries 借用。
- EnumerateIter 每轮产生 Indexed<T> 视图，index 返回 UInt64 快照，value / value_mut
  借出元素。EnumerateEntries 每轮产生 IndexedEntry<K,V>，entry_index 返回编号，
  entry / entry_mut 借出原有 Entry；键仍通过只读 entry_key 访问。
- 两种适配器都提供显式 advance/current/current_mut 操作，并接入 for 的借用步骤展开。
  容器先调用已有 iter / iter_mut，Slice 使用 slice_iter / slice_iter_mut。
  本轮不增加借用类型上的 trait 实现或重载普通 enumerate。

构造只分配任务内描述符，不推进原游标，也不预生成编号数组。适配器借用原游标并共享
其推进状态；在部分消费后包装，从接下来成功取出的元素重新编号为零。不同共享别名
推进原游标时，适配器只计算自己的成功 advance 次数。共享编号视图保存编号与元素地址
快照，后续 advance 不改变已返回的视图；独占视图继续阻止重叠可变访问。

编号游标与编号项均为编译器拥有的 borrow-only 类型，分别保留单列元素与 Map 键值的
类型身份，不能按值构造、存储进容器或返回悬空借用。返回引用继承原容器的 provenance，
保守的整容器借用、重借用、挂起和取消检查继续生效。元数据由任务 arena 管理，描述符
分配失败与运行失败遵循既有终止清理；元素所有权始终属于原容器。

UInt64 编号从零开始，只有成功推进才增加；底层借用游标长度有 isize::MAX 上限，
正常使用不会达到 UInt64 溢出。运行时仍检查溢出，防止内部不变量被破坏后回绕。
耗尽后 current 报错，空输入与重复耗尽不产生编号视图。Map Entry 与编号视图的快照
都由任务 arena 计费，本轮不改变已有逐项视图的 arena 生命周期。

实现按共享前端、公共运行时、解释器及 JIT/AOT 两路 lowering 同步接入。验证覆盖
Array/Slice/Map 的共享和可变行为、空输入、部分消费、快照、提前退出与拒绝矩阵；
拥有元素的挂起恢复、失败、分配失败和逐指令取消使用共享 fixture。先运行针对性测试，
再运行默认 Debug 检查及相关取消穷举。不涉及 map/filter、闭包存储或一般引用聚合。


## 14. 第四阶段 C：借用编号视图实现（2026-10-23）

设计提交 `9dec37a`。本次交付 Array/Slice 借用游标的 enumerate_ref/enumerate_mut、
Map 的 enumerate_entries/enumerate_entries_mut，以及编号读取、项借用与显式步进 API。
完整接口见 [语言参考](language.md#借用-enumerate编号视图)，类型与运行时边界见
[架构说明](architecture.md)。

四种新类型仅作借用目标，沿用既有元素存储资格、来源分析和跨挂起规则。运行时的
advance 保存最近成功推进的编号与元素地址，current 生成稳定的编号项快照；Map Entry
内嵌于编号项同一次 arena 分配中。共享别名推进原游标不会改变包装器 current 或旧项。
独占项仍禁止冲突访问；没有新增引用聚合、key/value 分离借用或一般引用 trait 实现。

共享运行时新增六个私有 helper，源语言共享/独占签名映射到同一实现；解释器直接调用
相同操作，JIT/AOT 共用原生 lowering。容器继续持有全部元素，所有权清理不转移到
编号适配器；描述符分配计入任务预算，任务结束一并释放。普通 enumerate 无行为变更。

主要证据：

- [共享 fixture](../tests/fixtures/borrowed_enumerate.jk)：Array/Slice/Map 共享与可变遍历、
  空输入、Unit、String、嵌套 Array、泛型包装、惰性构造、部分消费与重新编号、别名推进、
  稳定快照、显式步进、独占重借用、continue/break/return、失败与挂起。
- [VM 测试](../src/vm/tests/language/borrowed_enumerate_tests.rs)：关闭 prelude 的 JIT/解释器、
  current 前置条件、拥有元素析构唯一性、arena 归零、分配预算失败扫描、Pending/恢复/取消，
  以及借用冲突、逃逸、聚合存储、按值视图和 Map 键修改的共享前端拒绝矩阵。
- [独立 AOT](../tests/cli/containers.rs)：同一 fixture 的同步与 CPS、真实 yield、提前返回、
  运行失败清理和非法 current。文档示例在三个后端均输出 63。
- 单个函数叠加多个复杂借用循环仍受现有引用流分析预算限制；本轮按独立行为拆分测试
  函数，没有提高编译预算或绕过借用分析。

实际验证均使用 Debug：

| 检查 | 结果 |
| --- | --- |
| `cargo test borrowed_enumerate` | 6 项 VM/前端测试与 1 项独立 AOT CLI 测试通过；逐指令穷举默认忽略 |
| 相关逐指令取消穷举 | borrowed_enumerate、array_borrow_iter、slice_borrow_iter、map_borrow_iter 共 4 项通过 |
| 语言参考新增示例 | 关闭 prelude，在 JIT、解释器和使用当前运行库的独立 AOT 均输出 63 |
| `./scripts/check.sh` | `Checks passed (quick, Debug)`；fmt、Clippy、1574 项库测试、302 项 CLI 测试及其他默认集成测试通过，while/Map 三后端短输入正确性通过 |
| 文档本地链接与 `git diff --check` | 通过 |

取消穷举使用 `cargo test --lib -- --ignored`，传入上述四组完整的
`exhaustive_*_instruction_cancellation` 测试名。默认库测试忽略 123 项、CLI 忽略 8 项；
本轮额外执行上述四组。未运行 Release、无关压力、Godot、性能测量或其他平台验证。

默认检查后仅补充文档记录，未再改代码。手动 AOT 首次使用了旧的本地运行库；
改用当前 Cargo 产物后示例通过，并更新本地 Debug 运行库以保持日常命令可用。
