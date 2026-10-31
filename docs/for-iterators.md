# for 与迭代器分阶段开发计划

状态：2026-10-24，设计基线 `8b30deb`；第一、二阶段及第三阶段的 Array/Slice 借用迭代、Map 单列及成对遍历已完成，
记录见第 6–11 节；第四阶段 A 的 IntoIterator 与借用自动转换已完成。
第 12–13 节的 for 可选编号与 Map 键值解构已完成，取代 enumerate。
第 14 节记录 Map 直接二元解构的逐项描述符分配消除。
第 15–16 节记录泛型 Range、闭区间及 `..` / `..=` 表达式的设计与实现。
第 17–19 节记录已完成的有界范围切片与整数范围模式。
第 20–21 节记录普通迭代器 take/zip/map/filter 组合器的设计与实现。
第 22 节记录共享数组遍历的值状态协议及组合器。
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

## 12. 循环编号与 Map 解构设计（2026-10-24）

用 for 自身的可选编号取代库级 enumerate：for value in input 保持不变，
for index, pattern in input 增加从循环体用法推导的整数编号。顶层逗号区分编号和元素绑定，
括号内的逗号仍是元素解构；Map 可写 for (key, value) in &table，或
for index, (key, value) in &mut table。编号只能是单个名称或下划线；重复绑定报错。

编号由当前循环的隐藏局部计数器维护，每次执行该 for 从零开始，输入与 IntoIterator
转换仍只执行一次。成功取项后、执行用户循环体前绑定编号，continue 不漏计，嵌套循环
计数独立；挂起与恢复不重放编号。推导整数类型的最大值可用，后续若再成功取项则报编号溢出，
正常耗尽不报溢出。只生成普通局部变量与控制流，不创建编号适配器或逐项编号快照。

编号在同一循环中只有一个整数类型，根据整个循环体的赋值、运算与函数参数等上下文
推导；支持 Int8/16/32/64 与 UInt8/16/32/64，无约束时默认 Int32。不同位置要求
不兼容的类型时报错，不按每处使用自动转换。需要混用整数类型时显式转换。

Map 的两个绑定是 &K 和 &V / &mut V。对 Entry 的二元解构直接生成编译器检查的
两列投影，不构造引用元组；名称与下划线绑定均支持。两个投影沿用原容器来源，使用不同
的结构路径证明键列与值列不重叠，因此循环体可以读取 key 同时修改 value。
键不获得可变权限，不区分不同迭代步的值地址；重复可变 value、推进游标、容器变更和
悬空引用继续被拒绝。既有 entry_key/entry_value 接口保持其保守的整 Entry 借用行为。
同一二元 Entry 解构也可用于 let，保证绑定语义一致；本轮不新增一般引用聚合存储。

删除普通与借用 enumerate 的公开 API、专用类型、编号运行时 helper 和测试探针；
Iterator、IntoIterator、Array/Slice/Map 基础游标保持。旧功能的独有生命周期测试迁移到
新语法；与基础游标重复的快照/手动推进责任继续由已有测试负责。

使用 git revert --no-commit 撤销 8af0a0f、9dec37a、3993a7d、25b2966，并保留
3993a7d 中独立的 trait 默认方法跨模块名义类型修复及回归测试。撤销与新实现一起验证，
最终作为一个实现提交，保留历史。当前文档单独先提交。

验证覆盖编号顺序、输入单次求值、模式歧义、重复绑定、嵌套、continue/break/return、
整数类型推导、约束冲突与溢出边界、Map 共享/可变解构、String 键与拥有值、借用拒绝、挂起恢复、失败与取消。
执行三后端针对性测试，再执行默认 Debug 检查及受影响的逐指令取消穷举；不运行 Release。

## 13. 循环编号与 Map 解构实现记录（2026-10-24）

已实现第 12 节方案。循环体保留一个编译器内部节点，待元素绑定类型确定后，
结合参数、赋值、运算、别名、分支、记录/枚举构造及闭包上下文筛选整数类型。
推导有编译预算限制，默认 Int32，不产生逐处隐式转换。编号随后降低为普通局部变量、
溢出检查及递增；不增加编号运行时描述符，也不改变 CPS ABI。

Map Entry 二元绑定生成受检的键/值投影，键与值具有不同的来源路径。
同一组引用可以同时用于读键、修改值；整 Entry 显式访问和容器借用限制继续保留。
非二元模式不获得新的投影规则，普通下划线仍可丢弃整个 Entry。

普通和借用 enumerate 的 API、专用类型、运行时 helper 与测试探针已撤回。
保留独立的 trait 默认方法跨模块类型修复；其回归测试也重新通过。

主要证据：

- [编号 fixture](../tests/fixtures/for_indexed.jk) 与 [VM 测试](../src/vm/tests/language/for_indexed_tests.rs)：
  八种整数类型、默认类型、约束冲突、别名/分支/构造/闭包、嵌套、编号与元素解构区分、
  continue、重新遍历游标、Map 共享/可变键值绑定、拥有元素溢出清理与旧 API 拒绝。
- Int8/UInt8 边界直接执行；UInt64 边界仅在测试 AST 中设置隐藏计数器初值，
  JIT、解释器与[独立 AOT](../src/backends/aot/tests/for_indexed.rs) 执行最终两项及溢出，
  不为此提供源码入口或运行时测试接口。
- [普通 for fixture](../tests/fixtures/for_iterators.jk) 与 [Map fixture](../tests/fixtures/map_borrow_iter.jk)
  的既有生命周期测试迁移部分循环到编号/解构语法，继续验证输入转换单次执行、
  break/return、析构唯一性、挂起/恢复、失败、分配预算与取消清理。
- [CLI 独立 AOT](../tests/cli/language.rs) 执行相同编号 fixture，覆盖同步、挂起与溢出失败；
  Map 的独立 AOT 继续复用已有共享 fixture。

实际验证均使用 Debug：

| 检查 | 结果 |
| --- | --- |
| `cargo test for_indexed -- --nocapture` | 4 项库测试与 1 项 CLI 独立 AOT 测试通过 |
| `cargo test --lib -- for_indexed for_into_iter for_iterators map_borrow_iter trait_defaults_pass_substituted_self` | 19 项通过；2 项穷举默认忽略 |
| 当前 Debug 测试二进制的 `--ignored exhaustive_for_iterators_instruction_cancellation exhaustive_map_borrow_iter_instruction_cancellation` | 2 项逐指令取消穷举通过 |
| `./scripts/check.sh` | `Checks passed (quick, Debug)`；fmt、Clippy、1564 项库测试、301 项 CLI 测试及其他默认集成测试通过，while/Map 三后端短输入正确性通过 |
| 语言参考新增示例 | 关闭 prelude，在 JIT、解释器与独立 AOT 均输出 63、34 |
| 文档本地链接与 `git diff --check` | 通过 |

默认库测试忽略 121 项，CLI 忽略 8 项；本轮额外执行上述两项相关穷举。
未运行 Release、无关压力、Godot、性能测量或其他平台验证。
默认检查后仅补充验收文档，未再修改代码。

## 14. Map 直接二元解构消除逐项分配（2026-10-24）

`for (key, value)` 与带编号的同形模式现在在成功推进配对游标后直接取得两列引用，
覆盖共享、可变、显式游标重借用、单列忽略与双列忽略。每次遍历只需创建游标的
O(1) 描述符空间；循环体自身的分配另计。

类型规划识别解析器生成的直接二元绑定，生成内部配对绑定标记。元素类型仍参与编号推导，
绑定前重借用游标，再复用 EntryProject 的两列来源路径；借用检查不放宽。
JIT/AOT 共用一个无分配的地址投影 helper，解释器调用相同的运行时投影逻辑。
投影只在刚成功 advance 后执行，键和值地址都在用户循环体之前捕获，挂起沿用普通引用保存。

`for entry in ...`、显式 current/current_mut 和随后对 entry 的 let 解构仍使用稳定快照。
共享 Entry 可跨推进保留身份的语义不变；原有遍历 n 项累积 O(n) 描述符的成本仅适用于此路径。
当前参考与运行时不变量分别更新在 language.md 和 architecture.md。

验收新增：

- 两种 VM 通过宿主探针比较游标创建后的存活分配数；空表、1 项与 64 项的共享、
  带编号可变遍历和双列忽略均不增加分配数，结束后任务分配归零。
- 共享 fixture 验证游标别名推进后的投影稳定性、嵌套共享遍历、独占游标重借用、
  Unit 两列及返回源 Map 元素引用；由两种 VM 和独立 AOT 执行。
- 增加冲突推进、重复可变借用与局部 Map 引用逃逸的拒绝用例。
- 复用拥有值的提前返回/失败、挂起恢复、分配失败预算和逐指令取消覆盖。

未进行性能计时；分配验证属于资源行为回归，不代表已测得某个加速比例。

实际验证均使用 Debug：

| 检查 | 结果 |
| --- | --- |
| `cargo test --lib -- for_indexed map_borrow_iter` | 11 项通过；1 项取消穷举默认忽略 |
| `cargo test --test cli -- map_borrow_iter for_indexed` | 2 项独立 AOT 用例通过，覆盖同步与挂起执行 |
| `cargo test --lib -- --ignored exhaustive_map_borrow_iter_instruction_cancellation` | 1 项通过；资源恰好清理一次，arena 归零 |
| `./scripts/check.sh` | quick Debug 通过；fmt、Clippy、1565 项库测试、301 项 CLI 测试、其他默认集成测试及 while/Map 三后端短输入正确性通过 |
| `git diff --check` | 通过 |

未运行 Release、无关压力或其他平台验证。默认检查后仅补充验收记录，未再修改代码。

## 15. 整数范围与范围表达式设计（2026-10-24）

本轮将 Range 改为 `Range<T>`，提供 `range(start, end)` 与 `range_inclusive(start, end)`，
分别对应半开、闭区间。两者保持递增、两端有界；逆序区间为空，闭区间两端相等时产出一项。
类型标注中的旧 `Range` 迁移为 `Range<Int32>`，已有 `range(0, n)` 调用保留。

范围是标准库 Copy 值，通过 Iterator/IntoIterator 使用。标准库 Step 协议提供有序比较和
受检后继，首批实现 Int8/16/32/64、UInt8/16/32/64；泛型调用声明 `T: Copy + Step`。
具体整数的后继在最大值返回 None，闭区间在产出终点时记录耗尽，不构造 end + 1。
端点按普通泛型调用推导为同一类型，无具体约束的整数字面量默认 Int32；不隐式混合整数类型，
不从 for 循环体反向推导端点。Step 可用于用户定义的 Copy 类型，比较与后继必须保持递增契约。

`a..b` 与 `a..=b` 是表达式，解析为固定标准库 range/range_inclusive 调用；无需显式导入，
同名局部函数、模块别名或关闭 prelude 不改变协议身份。端点从左到右各求值一次，
可保存范围、作为参数/返回值或直接用于 for。范围运算低于逻辑运算、高于赋值，不允许无括号连写。

词法新增连续的 `..` / `..=`，同时迁移数组展开、序列 rest、记录 rest 的解析，保留原有用途。
不在本轮引入省略端点、步长、倒序、切片索引范围或 match 范围模式；后者还需独立设计
常量端点、重叠、覆盖与穷尽性检查。

验收覆盖八种整数及极值、空/逆序/闭区间、耗尽、端点类型推导与冲突、单次有序求值、
前导负号与浮点/元组点号词法、优先级、标准身份、泛型/别名、无 prelude、展开/rest 兼容性，
以及端点和循环体挂起、失败与取消。执行三后端针对性用例后运行默认 Debug 检查，
相关取消路径运行 Debug 穷举；不进行性能测量或 Release 验收。

## 16. 整数范围与范围表达式实现（2026-10-24）

第 15 节的范围功能已实现，当前语义与类型推导规则见
[范围表达式](language.md#范围表达式)。公开类型改为 Range<T>，旧的显式 Range 类型标注
已迁移为 Range<Int32>；原有 range(0, n) 调用继续可用。

范围表达式直接降低为标准库调用，三后端复用现有泛型、调用、循环及 CPS 路径。
标准库提供八种整数的 Step 实现，且允许用户为 Copy 类型实现该协议。
沿用普通泛型调用推导：函数返回类型或接收变量的 Range<UInt64> 标注不会反推端点，
需给一个端点明确类型，或显式调用 range::<UInt64>；不在本阶段扩展整个语言的结果上下文推导。

新增共享 fixture 覆盖八种整数及最小/最大值、半开与闭区间、逆序/空区间、重复耗尽、
泛型与自定义 Step、已定型端点、元组点号、编号循环和展开/rest 兼容性。
两种 VM 与独立 AOT 执行相同 fixture，检查端点顺序、一次求值、挂起恢复及失败清理；
逐指令取消穷举检查拥有值恰好清理一次、任务 arena 归零。
新示例为 [examples/ranges.jk](../examples/ranges.jk)，原 for 示例已改用范围表达式。

实际验证使用 Debug：

- `cargo test --lib -- ranges_ range_expressions range_tokens for_iter for_indexed`：19 项通过，2 项穷举默认忽略。
- `cargo test --test cli -- ranges_execute for_into_iter`：2 项独立 AOT 用例通过。
- 当前 Debug 库测试二进制的 `--ignored exhaustive_ranges_instruction_cancellation`：逐指令取消穷举通过。
- `./scripts/check.sh`：格式与 Clippy 通过；库测试 1570 项通过，1 项旧数组展开语法断言失败，122 项默认忽略。
  该旧断言把 `[..a ..b]` 当作解析错误；范围语法加入后它表示展开范围，改为验证类型错误，并保留连续范围的语法拒绝。
  修改后 `cargo test --lib array_spread` 的 4 项测试全部通过，1 项穷举默认忽略。
- 最终 `cargo test --lib ranges_reject_mixed_or_unsupported_endpoints_and_incomplete_syntax`、
  `cargo fmt -- --check` 与 `cargo clippy --all-targets -- -D warnings` 通过。
- 两个 examples 与语言参考中的范围示例均关闭 prelude，实际运行 JIT、解释器和独立 AOT，输出符合预期。
- 默认脚本在旧断言处停止后，单独补跑其余检查：`cargo test --bins --test '*'` 的 302 项 CLI 与其他默认集成测试全部通过，
  `cargo test --doc` 通过；`cargo test --bench while -- --iterations 0,1000` 与
  `cargo test --bench map -- --iterations 0,32` 的三后端短输入正确性通过。
- `cargo build --bin joky --lib` 刷新本地 Debug 可执行文件与静态库；文档本地链接、`git diff --check` 通过。

上述检查合计覆盖默认 Debug 检查项；修正旧断言后只重跑受影响的测试、格式与 Clippy，未重复整个库测试集。
未运行 Release、无关压力或性能测量。

## 17. 范围切片与整数范围模式设计（2026-10-24）

先实现 Array、FixedArray、Slice 的 `&xs[a..b]` / `&mut xs[a..b]`，同时支持闭区间。
索引位置直接读取范围语法的两个端点，以 UInt64 定型，不构造或推进 Range 迭代器；
暂不接受保存的 Range 值或省略端点。基值、起点、终点按顺序各求值一次，借用覆盖整个源存储，
返回既有 Slice 视图，不复制元素，也不额外证明不同区间互不重叠。
半开区间沿用 start <= end <= len；闭区间要求 start <= end < len，检查后转换为半开区间，
必须拒绝 UInt64 最大值的终点而不能环绕。共享与可变借用、挂起恢复、取消和逃逸沿用现有契约。

随后实现八种整数的 match 范围模式，端点首版仅允许整数字面量及直接负号。
支持半开与闭区间、嵌套载荷/元组/序列中的范围；按被匹配类型检查端点，不创建 Range 或调用 Step。
空/逆序范围静态拒绝，部分重叠按源码顺序选择，完全覆盖的后续分支拒绝；
通过区间边界划分检查覆盖与穷尽，不枚举整数域，保留既有覆盖分析预算及各列相关性。
Rune、命名常量、开放端点、模式守卫与字符串切片不在本轮范围。

两个实现阶段分别附带 JIT、解释器和独立 AOT 行为验证，运行默认 Debug 检查，
借用与取消变化补充相关 Debug 穷举；阶段完成后分别提交。

## 18. 有界范围切片实现（2026-10-24）

第 17 节的切片部分已实现，见[范围切片语法](language.md#范围切片语法)。
Array、FixedArray、Slice 共用原索引引用入口；对解析器生成的范围表达式直接取得 UInt64 端点。
来源引用和端点都在后续表达式执行前快照，防止重新绑定来源或修改起点改变已经捕获的值。
闭区间检查逆序与 UInt64 最大值后加一，所有容器继续使用既有运行时边界检查。
for 编号在切片端点中使用时也参与 UInt64 约束推导。

新增共享 fixture 覆盖空切片、闭区间、嵌套视图、可变子视图、拥有型元素、允许存储的共享引用元素、
泛型返回视图、基值及端点求值顺序、编号推导、运行时边界错误与清理。
静态拒绝矩阵覆盖错误端点类型、Range 变量、裸视图值、共享转可变、冲突借用与局部来源逃逸。
拥有值跨端点与使用处挂起，逐指令取消验证清理一次且 arena 归零。
可运行示例为 [examples/range_slices.jk](../examples/range_slices.jk)。

实际验证使用 Debug：

- `cargo test --lib range_slices`：3 项通过，1 项穷举默认忽略。
- `cargo test --test cli range_slices`：1 项独立 AOT 用例通过，含挂起及最大值终点失败。
- 当前 Debug 库测试二进制的 `--ignored exhaustive_range_slices_instruction_cancellation`：通过。
- 新示例与语言参考中的切片片段均在关闭 prelude 时运行 JIT、解释器及独立 AOT，输出符合预期。
- `./scripts/check.sh`：quick Debug 全部通过；fmt、Clippy、1574 项库测试、303 项 CLI 测试、
  其他默认集成测试及 while/Map 三后端短输入正确性通过。
- 直接在 for 输入中使用共享/可变范围切片也在三后端验证；本地 Debug 可执行文件及静态库已刷新。
- 文档本地链接及 `git diff --check` 通过。未运行 Release、无关压力或性能测量。

## 19. 整数范围模式实现（2026-10-24）

第 17 节的整数模式部分已实现，当前规则见[整数范围模式](language.md#整数范围模式)。
解析器保留范围模式及其源码范围；类型检查按输入整数类型验证两个字面量，拒绝空区间和逆序区间。
执行只生成同型比较，闭区间不会对运行时终点加一，支持 UInt64 最大值与 Int64 最小值。
元组模式的最外层解析与类型准入也已接通，复用已有元组载荷匹配、借用与清理实现。

覆盖分析对出现范围的整数列按端点切分等价区段，再递归检查剩余字段，保留元组、载荷和序列各列间的相关性。
使用 i128 保存分析边界，能表示 UInt64.MAX 之后的哨兵；不会枚举整数域。
每个区段的矩阵扫描计入既有分析预算，纯字面量列保持原路径。
部分重叠遵循首个匹配分支；被多个前序分支共同覆盖的范围也报告不可达。

新增共享 fixture 覆盖八种整数边界、UInt8 全域、半开/闭区间、普通字面量与范围混用、
部分重叠、元组字段相关性、枚举与序列中的范围、if let/while let、共享/可变及消费匹配。
拥有型元组跨匹配前求值和选中分支挂起，检查单次求值、失败清理与逐指令取消。
静态矩阵覆盖缺失取值、完全覆盖、空/逆序/越界端点、非法类型与语法；另验证区间分析预算。
新示例为 [examples/range_patterns.jk](../examples/range_patterns.jk)。

实际验证使用 Debug：

- `cargo test --lib -- range_patterns range_tests pattern`：42 项通过，3 项穷举默认忽略。
- `cargo test --test cli range_patterns`：1 项独立 AOT 用例通过，含拥有值、挂起与失败清理。
- 当前 Debug 库测试二进制的 `--ignored exhaustive_range_patterns_instruction_cancellation`：通过。
- 示例与语言参考中的范围模式片段均关闭 prelude，在 JIT、解释器和独立 AOT 输出符合预期。
- 八种整数的最小/最大值闭区间作为首个分支、后接 `_` 的执行探针在三后端通过，
  验证实际上下界比较，不依赖末分支省略检查。
- `./scripts/check.sh`：quick Debug 全部通过；fmt、Clippy、1578 项库测试、304 项 CLI 测试、
  其他默认集成测试和 while/Map 三后端短输入正确性通过；库默认忽略 124 项，CLI 默认忽略 8 项。
- `cargo build --bin joky --lib` 已刷新本地 Debug 可执行文件和静态库；本地文档链接及 `git diff --check` 通过。

本轮两阶段均完成针对性三后端验证、相关逐指令取消穷举与默认 Debug 检查。
未运行 Release、无关压力、其他平台或性能测量；最终只补充验收文档，没有再修改实现。

## 20. 普通迭代器惰性组合器设计（2026-10-24）

接入 take、zip、map、filter，输入按值转换一次 IntoIterator，构造时不调用 next 或回调。
首版使用 joky/iter 的模块函数，可嵌套组合或用中间绑定串联；暂不增加 blanket 扩展方法语法。
适用于 Range、Array 消费迭代和自定义 Iterator，不包括独立的借用游标协议。

- take(input, count)：count 为 UInt64，最多交付 count 项，零次不推进源。
- zip(left, right)：每轮先左后右，任一源首次 None 后不再推进；右源结束时丢弃已取出的左项。
- map(input, transform)：FnMut(Item) -> T，按需转换，支持拥有值输入和输出。
- filter(input, predicate)：FnMut(&Item) -> Bool，借用当前项判断，拒绝项及时清理，接受项原样移动。

组合器字段私有，不创建中间数组；回调使用同步函数指针或无借用 move 闭包。
回调环境沿用既有所有权、失败清理和禁止跨挂起规则。take/zip 不引入额外挂起限制。
耗尽策略统一为首次 None 后保持耗尽，避免反复调用具有副作用的源。

实现时补齐拥有记录内 Copy 迭代器字段的借用能力，验证投影引用仍受根所有权与冲突检查约束。
filter 不能绕过借用逃逸检查；先确认 callable 参数的借用契约，必要时补充可验证的契约表达。
保持原有借用捕获逃逸、FnOnce 重复调用、闭包容器存储与挂起限制。

验证覆盖按需推进、FnMut 状态、空输入和不等长 zip、重复耗尽、提前退出、拥有元素及回调环境
的正常/失败清理；JIT、解释器与独立 AOT 执行共享 fixture。实现后运行默认 Debug 检查，
涉及所有权路径补跑相关取消穷举。当前节为设计，完成状态另行记录。


## 21. 普通迭代器惰性组合器实现（2026-10-24）

第 20 节的四个组合器已实现，当前 API 见[惰性组合器](language.md#惰性组合器)。
Take、Zip、Map、Filter 使用普通私有字段记录及 Iterator 实现，构造只转换输入，
next 才推进和调用回调；四者首次 None 后保持耗尽。zip 按左后右顺序取项，
右源结束时清理本轮已取出的左项；没有中间数组或专用运行时指令。
首版使用模块函数组合，未增加 blanket 方法语法或借用游标协议。

编译器补齐三个已有路径：拥有记录及引用绑定内的无引用 Copy 字段可通过 Project 重借用；
刚性 callable 移动后结束旧抽象位置，同时保留捕获来源；借用函数指针调用先读取指针，
再沿用普通间接调用的 noescape 契约。拥有记录仍保守地按整个根检查冲突，未开放字段分离。
filter 的闭包逃逸检查在具体实例沿用现有来源分析，普通函数指针谓词须声明 @noescape；
没有把所有 FnMut 参数一概视为不逃逸。共享引用元素的二次借用仍不支持。

共享 fixture 验证范围与 Array、协议转换一次、构造惰性、FnMut 状态、编号与元组解构、
空输入、UInt64 最大 take 数量、不等长 zip、重复耗尽、拥有项及回调环境的清理、提前 return、
回调失败和 take/zip 挂起。静态矩阵覆盖借用捕获、参数逃逸、FnOnce、重复移动、
共享转可变、局部字段引用逃逸以及闭包跨挂起拒绝。
取消用例覆盖挂起的 take/zip 与从挂起入口调用同步 map/filter，逐指令检查重复清理和 arena 回收。
新示例为 [iterator_adapters.jk](../examples/iterator_adapters.jk)。

标准库新增模板使已有多容器模块导入用例超过旧记录缓存总量；记录实例每阶段上限改为 512，
仍将导入的刚性模板实例计入预算。枚举/别名预算、256 层深度与布局槽预算保持不变，
增加满容量重复实例命中以及下一实例拒绝的边界测试。

实际验证使用 Debug：

- 首轮组合器、闭包及混合拥有记录专项 30 项通过，2 项穷举默认忽略。
- `./scripts/check.sh` 的 fmt、Clippy 通过；库测试 1581 项通过、4 项失败、125 项默认忽略。
  三项失败来自旧记录实例预算，另一项来自仍把引用字段投影视为非法的旧断言。
  调整预算、更新拒绝矩阵，并补上字段投影执行验证及预算边界验证。
- 最终 `cargo test --lib -- generic_record local_field_borrows suspending_borrows fixed_array_f5_rejects iterator_adapters`：
  24 项通过，2 项穷举默认忽略，覆盖上述失败及最终字段投影路径。
- 最终 Debug 库测试二进制的 `--ignored exhaustive_iterator_adapters_instruction_cancellation exhaustive_closure_interpreter_cancellation_every_instruction`：
  2 项逐指令取消穷举通过。
- `cargo build --bin joky --lib` 已刷新本地 Debug 编译器与静态库；新增示例及语言参考片段
  均关闭 prelude，在 JIT、解释器及独立 AOT 输出符合预期。
- 默认脚本在库测试失败处停止后，补完剩余检查：最终 fmt 与 Clippy 通过；
  `cargo test --bins --test '*'` 的 305 项 CLI 测试及其他默认集成测试通过，
  `cargo test --doc` 通过；while（0、1000）与 Map（0、32）短输入正确性均覆盖三个后端并通过。
  CLI 包含最终共享 fixture 的独立 AOT 正常执行、map/filter 失败清理和挂起执行。
- 本地文档链接与 `git diff --check` 通过。

上述运行合计覆盖默认 Debug 检查项；修正预算与旧断言后，重跑受影响的专项及剩余默认检查，
未重复整套库测试。未运行 Release、无关压力、其他平台或性能测量。

## 22. 共享遍历组合：值传递状态

E6 采用独立的 SharedIterator 协议，普通 Iterator.next 与既有 advance/current 游标继续保留。
Array.iter_shared 返回保存数组共享引用和下标的局部 Copy 状态，step 消费一份状态并返回
后继状态与 Option<Item>。for 和显式 step_shared 都调用同一标准协议；泛型 helper 使用
`I: Copy + SharedIterator`，不是仅给循环语法增加特例。取到的 &T 依赖原数组，
不依赖游标变量，允许在下一次 step 后继续读取；全部状态和元素最后使用后才能修改数组。

这样不需要开放可变借用含引用聚合，也不需要引用 trait impl、GAT 或新生命周期语法。
Copy 约束明确表示可复制；容器、arena、持久存储仍另外检查引用资格，不因 Copy 而准入。
聚合布局只将实际拥有字段统计为拥有槽，共享引用始终携带原始来源。

交付 take_shared、filter_shared、zip_shared，均惰性并在首次 None 后永久耗尽。
take(0) 不调用底层 step；zip 先左后右，右侧先耗尽时允许左侧多推进一项，与普通 zip 一致。
filter 只接受共享元素和同步 noescape 函数指针，回调失败沿原任务失败清理路径传播。
同步例程中的解释器切片与取消可用；普通语言挂起、并发、保存回调引用和局部数组逃逸仍拒绝。

本阶段评估后暂缓三项：共享 map 返回拥有值时会把借用状态和拥有结果放进同一聚合，
需要额外的混合存储设计；独占元素不能复制，不能直接套用本协议；通用方法链会增加
扩展方法规则，暂由明确的模块组合函数承担。Slice 的组合视图也留待视图聚合规则确定后增加。
现有拥有型 map 和独占 for/游标仍可使用，不作迁移要求。

验证覆盖三后端、泛型 for、编号、嵌套组合、元素跨推进、耗尽、take(0)、不等长 zip、
回调失败、提前退出、来源冲突与存储拒绝，以及 Debug 逐指令取消。示例见
[iterator_adapters.jk](../examples/iterator_adapters.jk)，协议参考见
[语言参考](language.md#共享遍历组合器)。

## 23. 流水线包装与有限共享 map

SharedPipeline 使用普通泛型固有方法提供 filter/map/take/zip。SharedSource 以按下标共享查询
连接 Array 与 iter，避免模块循环导入；SharedView 在首次缺失后终止。
map 的输入可以是共享引用或前级转换结果，输出通过既有 plain_copy 资格检查，
不把 Copy 约束当作无引用存储资格。回调只接受同步 noescape 函数指针。
这一步不开放混合拥有/借用聚合，也不新增通用扩展方法或 trait 泛型方法。

验证：`cargo test --lib shared_pipeline` 两项通过；
`cargo test --test cli shared_pipeline_executes_in_standalone_aot_without_prelude` 通过。
覆盖惰性顺序、编号、复制状态、Copy 记录与 Unit 结果、map 后筛选、zip、take(0)、
空输入、提前返回、失败清理，以及非法输出、捕获、逃逸、存储与源失效的拒绝。

## 24. 独占流水线：转移游标，独立更新适配器状态

ExclusivePipeline 以局部只可转移的游标包连接 ExclusiveSource 与 ExclusivePlan。
源提供 &mut Iter，take/filter 的计数器、同步 noescape 函数指针及嵌套计划均为普通无引用 Copy 值。
for 入口消费一次包装值并解构游标/计划；推进只借用计划与游标，当前元素通过原 current_mut 获得。
因此无需把后继游标和 &mut 元素放在同一返回值，也不需要循环体结束后的隐式写回。
continue、break 和 return 直接沿已有循环控制流处理。

本阶段仅准入 &mut Iter 加无引用 Copy 元数据的局部记录、元组和泛型传递；整个包的值读取
执行转移，Copy 约束、枚举载荷、容器/arena 存储、整体取借用与普通语言挂起继续拒绝。
共享视图、其他 &mut 引用、拥有值和借用的混合聚合不由此放开。
回调不能保留元素引用或挂起；独占 zip/map、捕获回调及 Slice 流水线继续暂缓。

公开 into_parts 与 advance_exclusive 供显式推进及泛型代码使用，for 不提供额外权限。
验证包括嵌套与次序、take(0)、空来源、编号/continue/break/return、拥有元素替换、
失败清理，以及重复使用、活跃元素时推进、别名、逃逸、存储和 Copy 约束拒绝。

阶段验证：独占行为与拒绝矩阵两项通过，独立 AOT fixture 和示例三后端通过；
共享/独占回调失败均在独立 AOT 验证。
`cargo test --lib exhaustive_pipeline_instruction_cancellation -- --ignored` 通过，
包括两类流水线各自的正常遍历、continue/break 与提前返回入口。
完整示例为 [borrowed_pipelines.jk](../examples/borrowed_pipelines.jk)。

## 25. 共享流水线的只读捕获回调

共享 filter/map 使用普通泛型方法接收 `F: Copy + Fn(Item) @noescape -> R`。
匿名函数按输入和返回路径推导类型，可直接读取外层阈值、偏移量或字符串；
编译器为具体闭包体验证输入借用不逃逸，命名函数指针仍须显式声明 noescape。
抽象 callable 调用也保留该契约，不能从普通 Fn 参数伪造更强约束。

无拥有槽、无独占捕获的只读 Fn 环境满足局部 Copy，可组成借用记录与元组并经同步 helper 传递。
复制保留所有捕获来源，副本使用独立环境存储；所有副本最后使用之前，被捕获对象不能修改或失效。
共享流水线的状态协议仍是按值 step，保留复制、惰性、次序、编号、take(0) 与耗尽语义。
已有命名回调、显式共享协议和普通消费式组合器继续可用。

本阶段不开放可变捕获、拥有型 move 回调、共享 map 的拥有/引用输出、
独占流水线捕获、Slice 流水线或挂起回调。Copy 借用环境仍不能进入枚举、容器、arena 或宿主 ABI。
示例见 [captured_pipeline.jk](../examples/captured_pipeline.jk)，阶段验证记录见
[语法计划 §13.12](syntax.md#1312-共享流水线的捕获回调)。
