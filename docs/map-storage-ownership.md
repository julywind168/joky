# Map 设计与分阶段开发计划

> 2026-11-10 更新：本文涉及自定义 `joky/drop.Drop` 的设计、限制与验收为历史记录；该能力已撤回。当前自动清理规则及迁移见[撤回说明](custom-drop-withdrawal-review.md)。

状态：M0–M7、P-error 与 P-key 均已完成，首版 Map 已通过稳定性与性能基线验收。后续能力见第 6 节。
基线为 `cbe8f30`，Array 已具备拥有型元素、元素与切片借用、
`swap_remove/remove/insert/truncate/clear/swap`。本计划以 **同步、无序的哈希 Map** 为目标，
优先于可变链表推进；按 M0–M7 分阶段验收，每阶段可拆成独立 AI 目标。

完成后的第一版应达到当前 Array 相近的可用程度：源码库实现、泛型实例、Copy 与拥有型 value、
安全借用、容量管理、批量操作，以及三后端一致的失败和清理行为。
这里的“完整”限定为 **无引用 Copy key + Array 已支持的 value 类型**，不代表全部类型都能作为 key。
拥有型 key、动态字符串、Entry、通用迭代器和跨语言挂起属于后续扩展，不阻塞这一版。

已实现能力以 [Array 源码](../src/stdlib/array.jk)、[当前语言语义](language.md)和
[Array 存储方案](array-storage-ownership.md)为依据；本文负责 Map 设计、阶段目标和验收。
阶段完成后，实际接口进入 `language.md`，编译器或运行时不变量进入 `architecture.md`，
本文只更新状态、决策与后续依赖，不复制完整用户手册。

## 1. 设计边界与现有基础

Map 使用 `joky/map` 模块，公开类型最终为 `Map<K, V>`。与 Array 一样，同时提供模块函数
与普通 impl 方法，两种入口复用同一套 Joky 算法；无需关键字、索引语法或宿主 HashMap 包装。
Map 本身始终是不可隐式复制的拥有者，默认作用域清理，也提供消费式 `release`。

| 基础 | 当前状态 | 对计划的影响 |
| --- | --- | --- |
| Array 泛型存储、扩容、交换、不保序删除 | 已实现 | 直接复用；Map 不新建专用堆或裸指针体系 |
| 多拥有字段记录、拥有型 Option/元组、自动清理 | 已实现 | 可组合多个数组；移出的 value 可以用 Option 返回 |
| `mem.replace(&mut T, T) -> T` | 已实现 | 可替换拥有字段；不能据此假设所有数组元素均可直接取得 `&mut T` |
| Array 元素访问 | Copy 使用 get/Element，拥有型使用 borrow_ref/borrow_mut_ref | Map 保持相同区分；先补统一的按索引替换便利方法 |
| 静态 trait、条件 impl | 已实现 | 可做泛型 Copy key；不依赖 trait 特化或关联类型 |
| 抽象 trait 的引用参数 | 保守地可能保留普通引用 | 第一版 key 协议采用按值 Copy 参数，避免把局部 key 借用交给未知实现 |
| 资源引用放入 Option、记录或枚举 | 尚不支持 | 不承诺 `get_ref() -> Option<&V>` 或 Entry |
| 可复用部分记录移出 | 尚不支持 | 不通过依次移出多个字段实现 Map 解构；用受检替换或逐项消费 |
| 拥有容器跨 `@suspends/join/race` | 尚不支持 | 同步 helper 可返回普通结果；容器及视图不能跨语言挂起 |

“Copy”在本文表示编译器现有的类型分类，**不是已存在的 `Copy` trait 约束语法**。
泛型库必须沿用实例化后的移动/存储检查；用户定义一个同名标记 trait 不能证明类型可复制。
类型声明不带 trait 约束，约束放在需要它们的函数和 impl 上。

## 2. 存储与算法决策

### 2.1 哈希桶索引与紧凑载荷分开

初版采用开放寻址、线性探测和删除标记，桶数为 2 的幂。桶只有三种状态：
`Empty`、`Deleted`、`Full(entry_index)`。建议布局如下，所有字段均私有：

| 字段 | 内容 |
| --- | --- |
| buckets | `Array<Bucket>`，只保存桶状态和 entry 索引 |
| keys | `Array<K>`，紧凑排列的 key |
| values | `Array<V>`，与 keys 同下标的 value |
| hashes | `Array<UInt64>`，每个 entry 插入时计算的哈希 |
| positions | `Array<UInt64>`，entry 对应的桶下标 |
| deleted | 当前删除标记数量 |

这个布局增加了索引内存和多次数组访问，但能复用当前 Array：删除时各载荷数组同步
`swap_remove`，通过 positions 修正被搬到空位的末尾 entry 的桶；重建哈希索引只需要
新 buckets 和 positions，不复制或析构 value。载荷数组自身扩容仍会移动元素，不能宣称地址稳定。
初版接受这一取舍，性能证据不足时不改成更复杂的槽位存储、SIMD 探测或链式桶。

正常返回时必须满足：

1. `keys.len == values.len == hashes.len == positions.len == map.len`。
2. 每个 entry 恰好有一个 Full 桶，且 `buckets[positions[i]] == Full(i)`；不存在重复逻辑 key。
3. Empty 终止未命中查找，Deleted 不能终止查找；插入记住首个 Deleted 后仍继续查找已有 key。
4. 每次探测至多访问桶数个位置，包括恶意碰撞或不合法的用户协议实现，不会形成无限循环。
5. keys 只读，不对外提供可变 key 引用；value 修改不影响索引。
6. 每个拥有值在任何可中断点都有唯一清理责任，不以四个数组长度暂时相等作为清理前提。

### 2.2 扩容、删除标记与容量

空表允许零个桶，首次需要存储时至少 8 个桶；最大负载为桶数的 3/4。
桶下标使用 `hash & (bucket_count - 1)`，不依赖当前语言尚未提供的 `%`。
探测回绕也使用掩码。容量加法、倍增、向上取 2 的幂和各数组字节数必须先检查溢出。

`capacity()` 表示现有桶负载上限和各载荷数组容量共同允许的 entry 数，不能直接返回桶数。
`reserve(additional)` 以 `len + additional` 为目标，空间足够时避免增长；若删除标记使目标
插入量超过探测负载上限，则在本次 reserve 中重建索引。成功后，在没有删除等中间修改的前提下，
接下来 additional 次新 key 插入无需再次分配容器缓冲区；用户 Hash/Eq 自身的分配和后端拥有值
清理快照不在此承诺内。这不是总运行时零分配或保证不会发生资源错误的承诺。

新增插入使 `len + deleted + 1` 超过负载上限时，若清除删除标记已足够则同容量重建，
否则增长桶表。实现可在已确定复用 Deleted 时省略不必要的重建，但不能改变正确性。
因此删除再插入可能在 len 小于 capacity 时分配临时索引；文档必须说明这一点。
替换已有 key 不增加长度、不扩容、不重建。`clear` 保留容量，并把桶重新变为 Empty。

重建时先构造完整的新 buckets/positions，再通过已有拥有字段替换机制安装。
重建只使用缓存哈希，不调用用户 Hash/Eq；旧索引和新索引同时存在的峰值内存计入任务预算。
载荷 reserve 逐字段完成即可，无需引入可恢复事务：当前分配失败是终止性运行时失败，
要求完整清理，不承诺失败后继续使用旧 Map。未来若开放可恢复 OOM，需要另行设计原子提交。

### 2.3 key 协议

M2–M3 先用内部 `Int32` key 原型验证算法和资源行为，不发布需要长期兼容的 `IntMap`。
M4 再公开统一的 `Map<K, V>`，移除重复的原型算法，不能依赖当前不支持的
`impl<V> Map<Int32, V>` 部分特化。

第一版在 `joky/eq` 与 `joky/hash` 提供普通静态 trait，契约分别为：

```joky
pub trait Eq { fn eq(self, other: Self) -> Bool }
pub trait Hash { fn hash(self) -> UInt64 }
```

按值协议只用于这一版无引用 Copy key。最少支持四种整数、Bool、Unit，以及实现协议的
无引用 Copy 名义记录/枚举。元组 key 需要现有 impl 目标规则的额外支持，首版可用记录包装。
不自动按对象原始字节哈希：padding 不属于值语义。整数混合规则及测试向量在 M0 确定，
不同位宽和负数转换在三后端必须一致；hash 输出不作为持久化格式承诺。

Eq 应构成等价关系，且相等 key 的 hash 必须相等；驻留期间两者结果应保持稳定。
实现违规可能导致查不到或出现重复逻辑 key，但不能导致越界、无限探测、双重清理或类型安全破坏。
用户方法可以失败，查找/插入在修改表之前完成所需 Hash/Eq 调用；重建及按位置移除不调用它们。
首版不承诺抵御不可信输入的哈希碰撞攻击；给自定义 hash 再混入种子也不能消除相同 hash 的碰撞。

后续拥有型 key 需要借用式协议与查询生命周期设计，不能直接改变已公开的按值协议签名。
String 是否可用还取决于容器元素布局及内容访问能力，不能仅因语言存在 String 就标记支持。

## 3. 第一版 API 契约

下表列出 M2/M4/M5/M6 已实现的接口。`key: K` 均按值传入；
查找方法需要 `K: Eq + Hash`。
生命周期和纯容量操作不应无故要求 Hash/Eq。所有下标、长度和容量使用 UInt64。

| 阶段 | 接口 | 成功、未命中及拥有权语义 |
| --- | --- | --- |
| M2/M4 | `new()`、`with_capacity(n)` | 创建空表；with_capacity 至少容纳 n 项，不要求分配量恰好为 n |
| M2/M4 | `len()`、`capacity()`、`is_empty()` | 查询 entry 数、entry 容量和空状态 |
| M2/M4 | `contains_key(key)` | 返回是否存在，不转移 value |
| M2/M4 | `get(key) -> Option<V>` | 仅 Copy V；未命中返回 None，拥有型 V 实例必须拒绝复制读取 |
| M2/M4 | `insert(key, value) -> Option<V>` | 新增返回 None；已有 key 时保留原 key、装入新 value，返回旧 value；调用者负责旧值 |
| M2/M4 | `remove(key) -> Option<V>` | 未命中不修改；命中移出 value，长度减一，允许内部顺序变化 |
| M2/M4 | `reserve(additional)`、`clear()`、`release(self)` | reserve 契约见上；clear 析构内容并保留容量；release 消费整个容器 |
| M5 | `borrow(key)`、`borrow_mut(key)` | Copy V 的 `&Element<V>` / `&mut Element<V>`，与 Array 一致 |
| M5 | `borrow_ref(key)`、`borrow_mut_ref(key)` | 拥有型 V 的 `&V` / `&mut V`；可以原地更新或 mem.replace |
| M6 | `keys()`、`values()`、`values_mut()` | 分别返回只读 key 切片、只读/独占 value 切片；不提供 keys_mut |
| M6 | `pop() -> Option<(K, V)>` | 移出任意一项；空表返回 None，不调用 Hash/Eq |
| M6 | `into_entries(self) -> Array<(K, V)>` | 消费 Map，将所有 key/value 所有权转移到数组；顺序未指定 |
| M6 | `from_entries(entries)`、`extend(entries)` | 消费 `Array<(K, V)>`；按输入次序处理，重复 key 的最后一个 value 生效 |

四种 borrow 方法未命中均产生带源码位置的终止性运行时错误，不能伪造空引用。
调用者可先 contains_key，再借用；成本是两次查找。这是现有引用规则下的明确取舍，
不把 `Option<&V>` 当作已经具备的能力。

Map 借用保守依赖整个 Map；活动 view 期间拒绝冲突的 insert/remove/pop/extend/reserve/clear、into_entries、移动和释放。
最后一次使用后恢复权限，不承诺不同 key 的同时独占借用。即使某次重建只移动索引，规则仍一致。
切片顺序只在借用期间有效，keys 与 values 的相同下标对应同一项；不承诺插入顺序或稳定迭代顺序。

`extend`/`from_entries` 覆盖重复 key 时，负责析构返回的旧 value；不能把旧值遗漏在未受控临时量中。
如果旧值析构失败，则停止普通执行并清理其余输入和 Map。`clear` 正常结束后为空；
析构失败时继续必要的资源清理并保留首个错误。Map 不承诺业务可依赖的逐 key 析构顺序。
这些规则在 M3/M6 各自测试，不要求用户写自定义 Map Drop 来拼接清理。

## 4. 阶段与可独立执行的目标

主依赖顺序为 `M0 → M1 → M2 → M3 → M4 → M5 → M6 → M7`。
M0 验证后增加两个门槛：P-error 在 M2b 的容量错误路径、M5 的未命中借用接口之前完成；
P-key 在 M4b 发布泛型 Map 之前完成。两者不阻塞 M1。
每个子目标单独验证和提交；阶段只在全部子目标通过时标记完成。
当前 M0–M7、P-error、P-key 均已完成；首版范围已验收。

| 阶段 | 可交付结果 | 完成后的能力 |
| --- | --- | --- |
| M0（验证完成） | 可执行可行性用例与冻结的核心契约 | 布局、替换及借用可表达；类型准入和错误入口须补前置机制 |
| M1（已完成） | 通用源码 `Array.replace` | 不扩容地替换并移出旧元素 |
| M2（已完成） | 内部 Int32/Copy value Map | 基本字典、增长、删除和容量循环 |
| M3（已完成） | 拥有型 value 与故障清理 | `Map` 原型可管理嵌套 Array 和资源记录 |
| M4（已完成） | 泛型 Copy key、Hash/Eq、公开模块 | 发布统一 `Map<K, V>` 核心接口 |
| M5（已完成） | 元素借用与共享引用 value | 达到 Array 对应的访问和来源检查能力 |
| M6（已完成） | 视图、逐项消费、批量构造与合并 | 可遍历、导入、导出并与 Array 组合 |
| M7（已完成） | 压力验证、示例与发布验收 | 第一版 Map 完成，后续功能另立目标 |

### M0：验证表示与冻结契约

**目标：** 用最小可执行用例验证方案，不实现完整 Map。

- 验证多数组拥有记录的构造、字段重借用、读取 Copy 元数据、返回/移动及退出清理。
- 验证 Array 替换的源码序列：检查下标，swap(index, last)，take_last，push(next)，
  再 swap(index, last)。中间 push 必须使用原有空余容量，不增加元素缓冲区容量；
  运行时临时内存需求单独验证。
- 验证按值 Hash/Eq 泛型调用、无引用 Copy key、拥有型 V 实例，以及 Map 包装的引用来源检查。
  缺少编译器能力时记录最小失败用例，先形成窄范围修复子目标，不能绕过检查。
- 冻结桶枚举布局、整数 hash 规则及向量、容量/溢出规则、缺失 key 诊断策略；验证源码的错误报告能力。
  若缺少合适的通用错误入口，记录复现并单独安排受检机制；依赖它的接口在补齐前不得验收，
  不借用无关 Array 越界伪装 Map 错误。

**验收：** 三后端运行正例；负例证明非法复制、视图逃逸与冲突修改仍被拒绝。
提交可长期保留的回归用例和本文决策更新；未证明的能力明确留作后续依赖。
不增加公开 Map，不实现 String、Entry 或借用协议扩展。

#### M0 验证结果与冻结决策（2026-10-02）

下表保留 M0 验证时的结果；相应缺口现由 M1、P-error 和 P-key 补齐。
当时的 Storage 探针只验证表示，不进行哈希寻址；正式实现完成后已删除这层重复测试。
当前回归直接覆盖 [Array 替换](../src/vm/tests/containers/array_replace_tests.rs)、
[标准 key 协议](../src/vm/tests/containers/key_protocol_tests.rs)、
[生产 Map 的类型与布局](../src/vm/tests/containers/generic_map_tests.rs)、
[拥有值清理](../src/vm/tests/containers/owned_map_tests.rs)、[借用](../src/vm/tests/containers/map_borrowing_tests.rs)与
[三后端 CLI](../tests/cli.rs)。

| 检查 | 结果 |
| --- | --- |
| 五数组拥有记录 | 构造、字段重借用、Copy 元数据、返回、移动、释放和索引字段安装均通过 |
| 泛型替换 | Copy/拥有型实例可运行；中间位置和尾部替换保持长度、容量及其他元素，旧值归调用方 |
| 泛型 key 协议 | 四种整数、Bool、Unit、自定义无引用 Copy 记录均可按值调用 Hash/Eq；拥有型 key 重复使用被拒绝 |
| 资源与引用 | 拥有型 value 可经跨模块 helper 返回共享/独占引用；局部绑定后可进一步重借用其拥有字段 |
| 内容来源 | 共享引用 value 在容器释放后仍可读取；栈借用写入、释放容器后修改已保留来源均被拒绝 |
| 权限 | 移动后使用、拥有值复制 get、冲突修改、局部视图逃逸及资源引用装入 Option 均被拒绝 |
| 故障清理 | JIT/解释器分配预算扫描覆盖部分构造和索引安装；解释器逐指令取消覆盖替换中间状态；资源恰好一次清理，存储全部回收 |
| key 类型准入 | 普通 trait 无法保证 key 无引用：含共享引用的 Copy 记录仍可进入探针，见 P-key |
| 主动报错 | 当前无通用源码错误入口，`panic("Map key not found")` 在三后端均报 Undeclared function，见 P-error |

桶表示固定为普通 `enum Bucket { Empty, Deleted, Full(UInt64) }`，不使用整数哨兵或自制打包 ABI。
现有布局以 4 字节 tag 开始：UInt64 对齐为 8 时 Full 载荷偏移 8、总大小 16、对齐 8；
UInt64 对齐为 4 的布局模型下分别为 4、12、4。两种布局已用 MemoryLayout 验证，实际分配
始终查询目标布局，不能硬编码 16 字节。各数组之间的对应关系沿用第 2 节。

整数 hash 冻结为以下 UInt64 运算（乘法 wrapping，右移为逻辑右移）：

```text
a = (x ^ (x >> 30)) * 0xbf58476d1ce4e5b9
b = (a ^ (a >> 27)) * 0x94d049bb133111eb
h = b ^ (b >> 31)
```

Int32 先符号扩展到 Int64 再 wrapping 转 UInt64；Int64 直接 wrapping 转 UInt64；
UInt32 零扩展，UInt64 保持原值。Bool 映射 false=0 / true=1，Unit 映射 0 后使用同一混合函数。
冻结向量由独立整数计算获得，并由 Joky 程序在三个后端核对：

| 输入 | UInt64 hash |
| --- | --- |
| 0 | 0 |
| 1 | 6238072747940578789 |
| 42 | 12058926934050108962 |
| Int32.MAX | 8876943580449041804 |
| Int32.MIN | 11926543287740443804 |
| Int64.MAX | 6514504133438201533 |
| Int64.MIN | 2720858781877447050 |
| UInt32.MAX | 10030294862651378044 |
| -1 / UInt64.MAX | 13029008266876403067 |

容量先检查 `additional <= UInt64.MAX - len`，再检查 required 不超过 K、V、UInt64 载荷数组
各自的元素数量上限。桶从 8 开始，比较 `required <= count - count / 4`；
只有 `count <= bucket_limit / 2` 时才能倍增。bucket_limit 来自目标 Bucket 布局/存储上限，
不假设必为 2 的幂；required=0 可返回零桶。测试覆盖 6→7、12→13 的增长边界、非 2 的幂上限、
UInt64.MAX 加法和倍增边界；数学上可表示不等于任务预算允许实际分配。

**替换的内存承诺已收紧。** Copy 探针在准备完成后禁止后续 arena 分配仍能完成替换。
拥有型探针在 JIT 下也能完成，但解释器会在拥有参数 `next` 的清理快照登记处需要 arena 空间；
耗尽时正常报告 ResourceLimit，四个已构造资源仍全部清理。
这来自已有 `src/backends/interp/memory.rs::snapshot`，无需为 M0 更改后端。
M1 及 Map 替换只承诺不增长、不新建元素缓冲区，不能承诺零临时分配；故障测试允许受控资源失败，
不把当前快照的分配次数变成永久 API。

**P-error：补通用受检失败入口（已完成）。** 当前复现为 `fn main() { panic("Map key not found") }`，
结果是 UnknownName，而非可执行的业务失败。建议独立目标提供 `joky/check.assert(Bool, String)`：
true 正常返回 Unit，false 终止当前普通执行、发布 RuntimeError 并走现有清理；消息复制到错误对象，
定位 assert 的真实源码调用位置，并保留后端已有的调用轨迹，不虚构原生栈信息。
Map 未命中借用使用 `Map key not found`；容量算术或布局上限失败使用 `Map capacity exceeds the supported limit`。
检查后再取已确认存在的索引，失败不执行后续访问，因此无需先引入 never 类型。
该目标必须覆盖三后端、清理、嵌入调用、析构中失败和参数求值顺序；不借助宿主自定义绑定
或 Array 越界实现错误。完成前不能将 M2b/M5 的相应错误路径标为已完成。

P-error 验收（2026-10-02）：已提供编译器绑定的 `joky/check.assert(Bool, String)`。
最小缺口由 M0 的 UnknownName 复现证明；实现采用通用 IR 检查，不依赖 Map 或宿主注册。
[行为用例](../tests/fixtures/checked_failure.jk)、[VM 测试](../src/vm/tests/language/checked_failure_tests.rs)
与 CLI 三后端测试覆盖成功、终止失败、顺序求值、参数求值失败、直接/别名/跨模块及函数值调用、
嵌入/CPS 调用、析构失败和首错保留；消息在 VM 释放后仍有效，所有 arena 存储回收。
直接调用定位到源码调用处，函数值调用定位到绑定定义；保留后端已有轨迹能力。
quick 与完整检查均通过。实际接口与 IR/ABI 边界分别见 language.md、architecture.md。

**P-key：补无引用 Copy 类型资格检查（已完成）。** `probe_reference_key_admission` 证明
`ReferenceKey { target: &Int32 }` 可实现按值 Eq/Hash 并进入普通泛型 Storage；标记 trait 也不能排除它。
独立目标应提供可供源码容器复用的内部类型资格检查，在构造/写入方法实例化时检查实际 K：
必须可存储、无引用、无拥有资源，并递归检查记录/枚举/元组的内容；不引入 Map 专用运行时分支。
至少验证直接与嵌套引用、拥有型、String、别名及跨模块实例；合法整数、Bool、Unit 和无引用 Copy
聚合继续通过。裸类型名称出现在未使用签名中是否拒绝不作为这一门槛的承诺。
该机制完成后再发布 M4b；探针接受引用 key 是缺口证据，不是首版 Map 开放引用 key 的决定。

P-key 验收（2026-10-02）：内部 `joky/internal/types.require_plain_copy<T>()` 使用编译器真实
Copy、存储布局和引用分类，具体实例化时递归检查；未确定的泛型参数延后检查。
[资格用例](../src/core/imports/sources/qualification_tests.rs)执行合法聚合（含 AOT 原生执行），
三后端拒绝直接/嵌套引用、拥有型、别名与跨模块非法实例，String 保留已有泛型准入诊断。
直接使用固定非法类型的资格调用即使位于未使用泛型体中也拒绝；普通用户同名函数无特权，
内部模块不能直接导入。检查从 IR 擦除，不增加运行时 ABI 或 Map 专用分支。
M0 的 ReferenceKey 准入是此机制的最小必要性证据；quick 与完整检查均通过。

M0 完成的是验证、契约冻结和前置任务界定；两个前置机制均已验收。

### M1：补齐返回旧值的数组替换

**目标：** 在 Joky 源码库添加模块/方法两种 `Array.replace(index, next) -> Option<T>`。

有效下标返回 Some(old)，长度、容量及其他位置不变；越界返回 None，原数组不变，
传入的 next 按现有拥有参数规则消费并清理。有效替换不析构 old 或 next，也不需要新元素缓冲区；
拥有值清理快照仍可能占用临时内存，按 M0 的验证结果覆盖资源失败。
先复用 M0 验证的交换序列；只有真实测试证明现有通用机制不足，才另立底层改动目标。

**验收：** 空、单项、首/中/末、越界、零大小、对齐聚合、共享引用和拥有型值；
用 Drop 事件验证旧值交给调用者、无效 next 恰好清理一次；覆盖逐指令取消及预算边界。
更新 Array 实际接口文档。本阶段可独立交付，不夹带哈希算法。

#### M1 验收结果（2026-10-02）

已加入 `array.replace` / `Array.replace`，复用交换、take_last、push，不增加底层机制。
[行为用例](../tests/fixtures/array_replace.jk)覆盖空/单项/首中末/越界/UInt64.MAX、
Unit、对齐记录、共享引用与拥有型资源；三后端核对输出及 Drop 事件。
[VM 测试](../src/vm/tests/containers/array_replace_tests.rs)逐指令取消、扫描分配预算、禁止后续分配、
检查所有已构造资源恰好一次清理与 arena 全部回收；借用冲突及消费后使用继续拒绝。
替换契约由正式 Array API 的专项测试维护，不再保留 M0 探针包装。
`./scripts/check.sh --quick` 与 `./scripts/check.sh` 均通过（Debug/Release 与基准正确性）。
实际接口与临时内存边界已写入 language.md。

### M2：完成 Int32 key、Copy value 的算法原型

**M2a：** 在内部/测试模块实现桶状态、整数哈希、有界查找、new/with_capacity、len/is_empty、
contains_key/get/insert；通过 M1 完成替换。原型使用独立内部类型，避免泛型部分特化。

**M2b：** 先完成 P-error，再完成 reserve、重建索引、删除标记、remove、clear、capacity/release；
删除修正被搬移末项的 positions 和桶引用。所有容量由受检算术产生，内存全部走 Array 预算。

**验收：** Int32 的 0、负数、MIN/MAX；反复替换长度不变；碰撞簇首/中/尾删除后仍可找到后续项；
探测环绕、删除标记复用、全删后重插、多次扩容、同容量重建、reserve(0)、极大 reserve 失败。
以固定种子操作序列和 Rust 标准 Map 作为测试 oracle，逐步比较内容/长度，不比较迭代顺序。
Rust Map 只用于测试对照，不能成为 Joky Map 的实现。

**完成条件：** 所有核心操作在三个后端行为一致；测试检查双向桶索引不变量。
空查找不分配，已有 key 替换不增长；尚不把内部原型写入公开能力表。

#### M2 验收结果（2026-10-02）

M2 提交的内部 `joky/internal/int_map` 原型实现了本阶段核心接口（不作长期公开 API）。
该算法现迁入 [统一源码](../src/stdlib/map.jk)，不保留重复原型；实现仅组合 Array；桶/载荷分离、有界探测、
缓存哈希重建、删除末项修正与受检容量算术均在 Joky 中实现。
[用例](../tests/fixtures/int_map.jk)与 [VM 测试](../src/vm/tests/containers/int_map_tests.rs)覆盖整数边界、
首中尾碰撞删除、环绕、删除标记复用、全删重插、反复替换、多次扩容、同容量重建、
reserve(0)、非 2 的幂布局上限、加法/倍增上限与失败诊断。
固定种子模型验证已收敛到 [Map 独立模型](../tests/fixtures/map_stress_oracle.rs)与
[生产 Map 状态及所有权测试](../src/vm/tests/containers/map_stress_tests.rs)：每步比较返回值、长度、内容与
资源账本，并检查双向桶索引；CLI 继续核对三后端的基础、碰撞、容量边界及失败诊断。
禁止后续 arena 分配时，空查询、Copy 替换和 reserve 后 20 次新增仍成功。
小栈解析问题通过浅层源码 helper 拆分解决，既有 256 KiB 栈测试继续通过，未增加底层机制。
更新后的 quick 与完整检查均通过。

### M3：拥有型 value、嵌套容器与失败安全

**M3a：** 开放已有 Array 支持的无引用拥有型 V，包括 Array、混合/多拥有字段记录、
拥有型 Option/元组及用户 Drop。新增、替换、删除、返回、移动、显式释放和作用域退出均可用。
公开 get 仍限 Copy V；不能为了复用算法隐式复制拥有者。

**M3b：** 针对构造、各载荷 reserve、插入提交、swap_remove 和索引安装之间的每个中断点，
验证所有权登记。准备临时数组失败、Hash/Eq 以外的运行时失败、用户 Drop 失败、解释器时间片取消、
预算耗尽及重载后的旧任务清理都不能泄漏或重复析构。未完成的 Map 构造也要清理已完成字段。

**验收：** 用唯一资源 ID 记录构造/移动/析构，所有已创建资源最终恰好清理一次；
重建索引不触发 value Drop，remove/insert 返回的旧值能在 Map 释放后继续使用。
验证 clear 后容量保留并可重新插入，释放后预算可复用，迁移峰值超预算能受控失败。
利用现有故障注入与执行预算设施，不新增生产用的测试逃生接口。

#### M3 验收结果（2026-10-02）

沿用 M2 的同一份 Joky 算法，无新增编译器/运行时机制。
[拥有型用例](../tests/fixtures/owned_map.jk)覆盖嵌套 Array、多拥有字段资源、
拥有型 Option/元组、返回/移动/显式释放、clear 保留容量与重新插入。
替换和删除返回的旧值可在 Map 释放后继续读取；资源事件证明重建不触发 value Drop。
[VM 故障测试](../src/vm/tests/containers/owned_map_tests.rs)扫描分配预算（含迁移峰值失败）、
逐指令取消、执行指令预算耗尽、重载后旧任务继续/取消，检查已创建 ID 恰好清理一次与 arena 全回收。
CLI 三后端核对正常操作、Drop 失败、旧值消费时 Drop 失败和普通运行时失败的清理输出。
clear/release 后可连续复用分配预算；Copy get、重复使用已消费拥有者仍拒绝。
用例按 value 类型分别实例化，不放宽现有 256 泛型实例上限。
quick 与完整检查（含 Debug/Release、基准正确性）均通过；原型仍不作公开兼容接口。

### M4：泛型 key 与公开 Map 模块

**M4a：** 实现并导出按值 Eq/Hash 协议及基础 key 实现；验证导入别名、条件 impl、
跨模块解析、泛型体约束及固定的 trait 方法选择。无引用 Copy 分类由编译器真实规则保证，
不能只靠标记 trait；若当前边界不能表达该限制，先提交有负例的通用限制修复。

**M4b：** 先完成 P-key，再把原型收敛成一份 `Map<K, V>` 算法，增加标准模块加载和方法包装；
生命周期操作不附加多余 key 约束。添加 Map 嵌套于 Array、Array 嵌套于 Map、Map 作为 Map value 的用例。
公开前确认目标布局、模块可见性和自动清理不要求 Map 专用运行时 ABI。

**验收：** 基础 key、自定义 Copy 记录/枚举、强制所有 hash 相同的合法 key；
不同表示但 Eq 相等的 key 替换后保留原 key；用户 Hash/Eq 失败正确传播且资源完整清理。
缺少协议、签名错误、拥有型或含引用 key、String key、私有字段访问均有针对性诊断测试。
新增 `examples/maps.jk`，更新 `language.md` 和 README 的实际能力；内部原型不再保留重复实现。

#### M4a 验收结果（2026-10-02）

已发布按值 `joky/eq.Eq`、`joky/hash.Hash` 和六种基础实现。
[协议用例](../tests/fixtures/key_protocols.jk)在三后端核对冻结向量；
[VM/原生测试](../src/vm/tests/containers/key_protocol_tests.rs)验证别名、跨模块、条件 impl、
泛型体约束、固定 trait 选择和错误签名诊断。真实资格检查仍由 P-key 提供。
可信源码容器改为显式 pub 导出，Array/Element/Slice 的既有接口保持公开；
最小私有 helper 回归在旧行为下失败，修正后内部调用可执行、外部访问拒绝。
quick 与完整检查均通过；M4b 验收见下。

#### M4b 验收结果（2026-10-02）

已公开 `joky/map.Map<K, V>` 的 new/with_capacity、len/is_empty/capacity、contains_key/get、
insert/remove、reserve/clear/release；模块函数与方法共用 [唯一 Joky 算法](../src/stdlib/map.jk)。
内部 IntMap 源码与模块注册已移除。构造/写入以 P-key 的真实分类限制 K，
生命周期操作不要求 Eq/Hash，查找/修改使用普通静态协议；没有 Map 专用运行时 ABI。

[泛型用例](../tests/fixtures/generic_map.jk)与 [诊断/布局/模块测试](../src/vm/tests/containers/generic_map_tests.rs)
覆盖六种基础 key、自定义记录/枚举、完全碰撞、Eq 相等的不同表示保留原 key、
别名与跨模块条件 impl、固定 trait 选择、生命周期无协议约束，以及 Array<Map>、Map<Array>、Map<Map>。
Hash/Eq 失败在修改前传播，传入和驻留拥有值均恰好清理；资源事件证明重建不调用用户协议。
三后端拒绝缺少约束、错误签名、拥有型/含引用/String key、私有字段/桶/helper、冲突借用和非法复制。
默认标准库不导出测试检查；测试加载同一生产源码并附加双向索引检查，JIT/解释器与 AOT 原生均执行。

M2 的 192 步全内容 oracle、容量边界、禁止后续缓冲区分配，与 M3 的分配/迁移峰值失败、
逐指令取消、指令耗尽、重载后继续/取消、Drop 失败及预算复用均已迁移并重新通过。
M4 的[公开示例](../examples/maps.jk)当时在三后端输出三行 42；language.md、README 与 architecture.md
分别补齐实际接口、能力表和实现不变量。quick 与完整检查（fmt、Clippy、Debug/Release、
三后端集成和基准正确性）均通过；未进行性能测量。
M4 整体验收完成。后续元素借用与共享引用 V 来源见 M5 验收，遍历视图与消费/批量接口见 M6 验收；M7 压力/性能验收见下文。

### M5：安全借用与共享引用 value

**M5a：** 增加四种元素借用接口。验证 Copy Element 读写、拥有型字段读取/原地修改、
嵌套容器访问以及 mem.replace 返回旧值；未命中错误有 Map 调用位置。
共享借用允许共享读取，独占借用阻止冲突访问，最后使用后恢复权限。

**M5b：** 开放 Array 已支持的共享引用及含共享引用 Copy 聚合作为 V，追踪实际内容来源。
get、remove、insert 返回的旧值以及后续导出操作都不能因 Map 提前释放而失去来源保活。
扩展仅允许既有存储契约，不缩短任务期引用保留，也不开放含任意引用的拥有型聚合。

**验收：** 正例跨同步 helper/泛型/别名/模块返回视图；负例覆盖视图冲突、栈引用写入、
移动后使用、局部引用逃逸、视图装入 Option/聚合、宿主 ABI 及语言挂起。
Map 移动/释放后已复制出的普通共享引用仍按原契约有效，原来源的冲突修改仍被拒绝。
不为了 ergonomics 放宽整 Map 借用，不加入多个 key 的同时独占访问。

#### M5 验收结果（2026-10-02）

四种元素借用均已提供模块函数与方法，查询后复用 Array 借用，保持整 Map 的权限边界。
[借用用例](../tests/fixtures/map_borrowing.jk)覆盖 Copy Element 读写、拥有字段和嵌套容器访问、
通过 mem.replace 取回旧拥有值、最后使用后恢复修改权限，以及 Map 返回/移动/释放后的引用读取。
共享引用与含引用 Copy 记录/Option/元组沿用现有 contents/retained 分析，无需放宽所有权或引用规则。

[VM 验证](../src/vm/tests/containers/map_borrowing_tests.rs)与[跨模块 helper](../tests/fixtures/map_views_library.jk)
覆盖泛型、别名、跨模块返回视图与复制内容；三后端拒绝冲突视图、不同 key 的同时独占借用、
栈引用写入、移动后使用、局部视图逃逸、视图聚合、宿主 ABI 和语言挂起。
普通共享引用复制出来后不依赖 Map 存储，原来源的任务期冲突修改仍被拒绝。
分配预算扫描、解释器逐指令取消及指令预算耗尽验证拥有值恰好清理一次、arena 全回收。

四种未命中路径均报 Map key not found，并验证用户 Map 调用位置及驻留资源清理。
最小诊断回归证明原生错误原先只有标准库 assert 位置，因此增加通用同步调用失败的源码调用点记录：
JIT 显示文件/行号，AOT 显示来源 ID/字节范围；不新增 Map 专用 ABI，也不伪造原生栈或解释器 PC。
普通调用、函数值调用、首错保留、重载后诊断保活及 AOT/JIT 共享运行时桥接均有行为测试。

[CLI 测试](../tests/cli.rs)在三后端执行相同用例；[示例](../examples/maps.jk)新增借用修改和共享引用，输出五行 42。
quick 与完整检查（fmt、Clippy、Debug/Release、三后端集成及基准正确性）均通过，未测量性能。
M5a/M5b 整体验收完成；M6 的实现与验收见下节。

### M6：遍历视图、消费与批量组合

**M6a：** 通过既有 Slice 增加 keys/values/values_mut；加入 pop 和 into_entries。
value 切片上的 Copy/拥有型限制与 Array 一致。into_entries 通过逐项移动或受检字段替换实现，
不依赖尚不存在的可复用部分移出；不公开内部桶下标或可变 key 切片。

**M6b：** 增加消费式 from_entries/extend，保持输入处理顺序及最后 value 生效的规则。
允许以输入长度作为 reserve 上界，但要检查溢出和资源预算；不承诺重复 key 输入只按最终大小分配。
明确被覆盖旧值的清理点；输出顺序未指定，批量失败时按终止性错误清理全部剩余资源。

**验收：** 空/单项/多项、重复 key、任意内部顺序、Copy/拥有型/引用 value；
导出后资源仍可使用、输入消费后不能复用、values_mut 不能改变 key；
批量中途分配失败/析构失败/取消不遗漏尚未处理的输入。
本阶段提供可用的 while + Slice 遍历，不引入闭包、for 或通用 Iterator。

#### M6 验收结果（2026-10-02）

[src/stdlib/map.jk](../src/stdlib/map.jk) 已提供 keys/values/values_mut、pop、into_entries、
from_entries/extend 的模块函数与方法。切片复用 Array 的完整范围和整个 Map 借用来源；
不公开可变 key、桶下标或 Iterator。pop 移出紧凑尾项并更新删除标记，导出循环移动各项；
二者及切片接口均无 Hash/Eq 约束，不调用用户 key 协议。

批量接口消费输入 Array，按输入长度受检 reserve，再原地反转并从尾部处理，保持原输入顺序。
重复 key 保留原 key、最后 value 生效；每轮作用域在继续下一项前清理 insert 返回的旧值。
失败按终止性错误清理 Map、当前拥有临时量及尚未处理的输入，未增加编译器机制或运行时 ABI。
导出循环展开嵌套表达式后，既有 256 KiB 小栈加载/执行回归保持通过，无需放宽测试限制。

[遍历与批量测试](../src/vm/tests/containers/map_iteration_tests.rs)及[语言用例](../tests/fixtures/map_iteration.jk)
覆盖空/单项/多项、只读和独占切片、完全碰撞与重复 key、Copy/拥有型/共享引用 value，
并拒绝修改 key、冲突借用、局部视图逃逸和消费后复用。资源导出后继续使用，
跨模块别名和泛型 helper 保持实际内容来源；反复 pop、重建和重用逐次检查双向索引。
[CLI 测试](../tests/cli.rs)在 JIT、解释器和独立 AOT 上验证同一行为及旧值析构失败的中止/清理。
JIT/解释器分别扫描分配预算，并在导出入口单独收紧预算以覆盖导出失败；
解释器另逐时间片取消并扫描指令预算，检查已创建资源恰好清理一次且 arena 无残留。

[示例](../examples/maps.jk)新增批量导入、while + Slice 遍历及消费导出，三后端输出六行 42。
quick 与完整检查（fmt、Clippy、Debug/Release、三后端集成及基准正确性）均通过。
Debug/Release 各通过 718 个单元测试（既有 1 个忽略项）、74 个 CLI 测试和 22 个源码模块测试。
M6 阶段未测量性能。M6a/M6b 整体验收完成；后续稳定性与性能记录见 M7。

### M7：第一版稳定性与发布验收

扩大固定种子差分测试，覆盖长时间插入/替换/删除/clear/reserve 混合序列、
低位相同 hash、完全碰撞、删除标记高占比、容量阈值和连续预算复用。
逐次检查内容和资源计数，三后端使用同一组语言用例；解释器专有时间片、取消和重载测试独立列明，
不能把它们写成原生后端也具备的相同调度能力。

检查源码库确实只用 Array/通用机制；审查借用诊断、跨模块实例化、隐藏字段和未初始化读取。
示例至少覆盖频次统计、自定义 key、嵌套拥有 value、借用修改与批量导入/导出。
按 `benches/README.md` 单独记录查找、插入、删除重插、遍历和内存占用，注明规模与碰撞分布；
性能测试不混入正确性检查，也不把未经测量的均摊 O(1) 当作所有 key 下的保证。
预期平均查找/插入/删除 O(1)，完全碰撞最坏 O(n)；重建平均 O(b + n)、完全碰撞最坏
O(b + n²)，其中 b 为目标桶数。紧凑载荷遍历 O(n)，clear 还需 O(b) 重置桶状态。

**完成条件：** M0–M6 全部验收；完整检查通过；示例和文档只描述已经实现的边界；
没有未解释的资源泄漏、三后端差异或所有权检查绕过。再选择后续目标，不无限追加接口。

#### M7 验收结果（2026-10-02）

[长序列用例](../tests/fixtures/map_stress.jk)使用两个固定种子（123456789、0xfedcba98）、
标准整数 Hash / 低 12 位相同 / 完全碰撞三种分布，每组 4,096 步，每个后端共 24,576 步。
每 256 步先填满 64 个 key，再删除 56 项并预留空间以触发同桶数重建；其余步骤混合替换、
删除、查询、clear 和 reserve。载荷是带自定义 Drop 及嵌套 Array 的拥有值。
每一步将 key/value 切片与独立 Array 模型逐项比较，检查唯一性、完整性及资源内容；
[Rust BTreeMap 参照与资源账本](../tests/fixtures/map_stress_oracle.rs)另逐步核对操作结果、长度、
内容摘要和存活资源 ID 集合，不依赖 Map 的内部排列或析构顺序。

[VM 测试](../src/vm/tests/containers/map_stress_tests.rs)在 JIT/解释器的全部长序列中检查私有双向索引及删除计数；
AOT 另执行同定义域的 320 步索引验证。[CLI 测试](../tests/cli.rs)在独立 AOT 程序上运行相同
六组公共行为与资源账本；仅测试专有索引断言由 VM 测试注册表提供，生产标准库没有测试导出。
512 KiB 预算下连续六次 512 步调用均完整清理，LIVE 归零；
既有 owning Map 的 96 轮 1 MiB 分配/clear 回归继续验证单任务累计分配超过 64 MiB 预算时的退款复用。
解释器另以单指令时间片运行，
在 1/64/120/128/256/300 个完整检查点后重载并销毁 VM，再分别完成旧任务或取消。
该时间片/重载检查只描述解释器能力，不声称原生后端具有相同调度边界。

源码边界审查：Map 存储仍是五个私有 Array 和删除计数，哈希、探测、重建、消费及批量策略均在
[src/stdlib/map.jk](../src/stdlib/map.jk)，运行时和后端没有 Map 专用操作或 ABI。
K 资格使用通用递归 PlainCopy 检查；协议调用在修改前完成，重建复用缓存 hash。
所有载荷访问受 Array 初始化长度保护，视图范围使用 len；失败只进入既有终止清理，
不允许继续读取半更新状态。M4 的隐藏字段/类型与协议资格拒绝测试、M5/M6 的借用诊断、
跨模块泛型/别名和实际引用来源测试继续作为门槛；审查未发现需放宽规则或增加公开接口的问题。
[现有示例](../examples/maps.jk)覆盖频次、自定义 key、拥有 value、借用修改及批量导入/导出。

新增 [Map 专项基准](../benches/README.md#map-专项基准)，正确性接入检查脚本，计时及内存扫描独立运行。
quick 与完整检查均通过：Debug/Release 各 722 个单元测试通过（2 个显式忽略项）、
75 个 CLI 测试和 22 个源码模块测试通过，while/Map 三后端基准正确性检查通过。
随后独立运行两轮计时（N=0/32/128、15 样本、目标批次 30 ms），核对全部 3,240 条原始样本；
显式运行的内存扫描也通过，覆盖 26 组工作负载及成功/失败预算边界。
[性能实验记录](interpreter-performance.md#map-首版基线)保存方法、精简结果、构建身份与本地产物位置。
碰撞显著增加当前实现的执行成本；数据是本机小规模同步基线，不作版本提速或所有 key 的 O(1) 保证。
M7 整体验收完成，首版 M0–M7 目标闭环；未添加范围外的公开接口。

## 5. AI 目标设置与完成标准

一个目标通常只处理一个阶段或一个子目标。M0–M7 与两个前置任务均已完成；
首版验收结束，后续按第 6 节选择独立目标。
M3a 不能把基本资源安全留给 M3b，后者负责扩大故障覆盖。
不要一次设置“实现完整 Map”，也不要在基础能力未验证时并行开发依赖它的接口。

可复用目标模板：

> 完成 docs/map-storage-ownership.md 的阶段【编号与名称】。先检查前置阶段状态与当前仓库，
> 沿用已完成代码，完成本阶段交付物和验收用例。容器算法写在 Joky；新增编译器或运行时机制
> 必须由最小失败用例证明必要，并保持 JIT、解释器和 AOT 语义一致。不得通过复制拥有者、
> 裸指针、隐藏宿主容器或放宽借用规则绕过问题。按 AGENTS.md 的改动范围选择验证，
> 满足本阶段验收要求，补齐文档，按 Conventional Commits 提交。
> 最后报告已完成接口、实际测试、提交号和剩余边界；只在全部验收通过后更新阶段状态。
> 如遇必要前置能力缺失，记录复现、影响与最小修复目标，不将受阻阶段标为完成。

每阶段共同检查以下事项：

- 行为测试执行生成代码；算法不是只有 Rust 单元测试通过。
- 正常、未命中、边界、重复操作及本阶段涉及的错误/资源路径有验证。
- 拥有值和视图的正例、拒绝非法程序的负例共同覆盖，不能只测输出数字。
- 新增模块/方法/泛型/别名共用同一语义；测试运行前重新编译内嵌 stdlib。
- 按 AGENTS.md 选择的检查及本阶段验收用例通过后再提交；若因环境不能完成，报告未验证项，不声称阶段已验收。
- 本文阶段状态、实现文档和代码一致；提交中不包含下一阶段的未完成公开接口。

**首版验收已完成。** M0–M7、P-error 与 P-key 均已验收；后续能力另设独立目标。

## 6. 第一版之后的独立目标

| 后续能力 | 必须先解决的问题 |
| --- | --- |
| `get_ref/get_mut -> Option<引用>` 与 Entry | 资源引用在聚合中的来源、独占性、分支清理和逃逸检查；Entry 还要处理插入后失效 |
| 拥有型 key、String key、借用查询 | 可存储布局、按内容 Hash/Eq、借用式协议的保留效果、查询类型与 key 的一致性及 key 清理；后续实施见[字符串计划 S6](string-storage-ownership.md#s6借用式-key-协议与-string-map-key) |
| drain/retain/回调组合与通用迭代器 | 中途退出/失败的剩余项清理、闭包或函数值契约、视图与结构修改冲突 |
| shrink_to_fit / 紧凑槽位布局 | 通用数组缩容或载荷迁移支持、临时内存峰值、所有权登记和性能证据 |
| 跨语言挂起 | 容器与 view 的任务保活、取消、恢复和代码重载协议 |
| 防碰撞策略、有序 Map、多 key 独占借用 | 分别建立安全目标、顺序语义或不相交借用证明，不能从当前实现推定支持 |

这些目标不纳入 M7 的完成条件。可变链表继续作为节点所有权和链接更新的独立验证项目。
