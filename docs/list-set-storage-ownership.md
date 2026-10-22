# List / Set 设计与分阶段开发计划

状态：LS0、List L1–L4、Set S1–S3 与 LS1 联合开发验收已完成。LS0 起点为 `e75787e`。
本文将同步可变链表 `List<T>` 与无序哈希集合 `Set<T>` 纳入同一轮容器开发；
LS0 已冻结首版接口、表示和类型准入；List 和 Set 已实现，公开用法见 [List](language.md#可变-list) 与 [Set](language.md#无序-set)。

首版以现有 Array、Map、拥有型记录、借用和 Drop 为基础，容器算法用 Joky 源码实现。
不新增容器关键字，不要求先实现 Box、Rc、通用迭代器或独占引用字段。
List 使用内部索引连接节点，Set 复用 Map 的哈希与清理实现。两者完成后仍保留 `graph`。

本文负责表示选择、阶段目标与验收；实现完成后，公开接口归入 [language.md](language.md)，
新增的编译器或运行时不变量归入 [architecture.md](architecture.md)。
现有基础见 [Array 方案](array-storage-ownership.md)、[Map 方案](map-storage-ownership.md)
和 [容器扩展规划](arena-borrowing-ffi.md#61-容器与所有权扩展)。
接口核对以当前 [array.jk](../src/stdlib/array.jk)、[map.jk](../src/stdlib/map.jk)、
[eq.jk](../src/stdlib/eq.jk) 和 [hash.jk](../src/stdlib/hash.jk) 为准，避免套用历史阶段的能力限制。

## 1. 范围与依赖

| 项目 | 首版交付 | 暂不包含 |
| --- | --- | --- |
| List | 可变双向链表；两端增删、按逻辑位置插入/删除/替换、元素借用、反转与批量转移 | 公开节点句柄、游标、稳定元素地址、跨容器 O(1) 拼接、引用式可变循环图 |
| Set | 无序哈希集合；插入/删除/查询、只读元素视图、批量转换、集合关系与消费式集合运算 | 有序集合、驻留元素可变借用、异构查询、隐式克隆、通用 Iterator |
| 共同边界 | 同步源码库、自动清理、提前 release、JIT/解释器/AOT 一致 | 容器或视图跨语言挂起、直接进入宿主 ABI、扩展持久实例支持范围 |

解释器时间片暂停、预算耗尽、任务取消和旧代码重载保活属于首版验收；这些不是语言级
`@suspends` 容器支持。挂起入口可以调用现有规则允许的同步 helper。
模块分别为 `joky/list`、`joky/set`，默认不加入 prelude；提供模块函数及转发到同一算法的 impl 方法。
类型声明不带约束，只在实际需要协议的方法或函数上添加约束。

| 已有基础 | 使用方式与限制 |
| --- | --- |
| Array 的 replace、swap_remove、reserve、元素/切片借用 | List 使用受检下标和完整元素移动；不读取私有 Buffer 或裸指针 |
| Map 的 BorrowEq / BorrowHash、String key、借用查询 | Set 直接复用，不能照旧版 Map 的 Copy-only 初始阶段重新设计协议 |
| 拥有记录、枚举、元组与清理 | List/Set 都是不可隐式复制的拥有者；返回 T 转移拥有权 |
| Option 借用查询、take、unwrap_or_else | 可减少分支代码，不提供节点地址稳定性或新的借用权限 |
| Copy 分类与存储类型检查 | Copy 是现有类型分类，不引入虚构的 Copy trait；非法实例必须被编译器拒绝 |

List 的 T 以当前 Array 元素准入为上限：支持 Copy 值、普通共享引用及其 Copy 聚合，
以及已支持的无引用拥有型元素，如 String、拥有记录和嵌套容器。
Set 的 T 以当前 Map key 准入为上限：必须可存储且递归无普通引用；涉及查询或插入时要求
`BorrowEq + BorrowHash`。支持 String 及满足协议的受支持拥有类型，不能据此承诺所有类型都可作 key。
现有 Eq / Hash 的 Copy 类型通过已有适配实现借用协议，不重复实现适配层。

## 2. List：容器拥有节点，索引表示连接

### 2.1 表示与不变量

List 采用三组私有数组，把逻辑节点位置与紧凑元素存储分开，复用 Map 已采用的索引修正思路。

| 私有字段 | 内容 |
| --- | --- |
| values: Array<T> | 紧凑存放所有活动元素，长度即 List.len |
| owners: Array<UInt64> | 与 values 同下标，记录该元素对应的节点槽号 |
| slots: Array<NodeSlot> | Copy 元数据；Used 保存 prev、next、value_index；Free 保存 next_free |
| head / tail | 首尾节点的可选槽号，均用 Option<UInt64> |
| free | 空闲槽链表的可选首槽号 |

链接只保存数值，不保存 `&Node` 或 `&mut Node`。所有字段私有，不暴露槽号，也不把槽号当地址。
values 扩容或 swap_remove 可以搬动元素；稳定的是内部连接的含义，不是元素的物理地址。
首版不先抽取通用 SlotMap，以免同时承担公开句柄身份和代际回收协议。

正常返回时必须满足：

1. `values.len == owners.len == 活动节点数`，且不超过 slots.len。
2. 每个 Used 槽唯一对应一个 value，下标互逆：`owners[value_index] == slot_index`。
3. 从 head 沿 next 恰好访问全部活动节点一次；从 tail 沿 prev 得到相反顺序。
4. 首节点无 prev，尾节点无 next；相邻节点的 prev/next 相互对应；空表的 head/tail 均为 None。
5. 每个槽恰好属于活动链或空闲链；两条链不重叠且各自无环，无丢失槽、重复槽和指向 Free 的活动边。
6. 槽号和载荷下标都受边界检查；内部不变量失败产生诊断，不能以默认下标 0 掩盖损坏。

删除先定位节点，保存邻接关系和载荷下标；用 swap_remove 移出 T 并同步移出 owners 项。
若末尾载荷搬入空位，修正其对应 Used 槽的 value_index。随后修复邻接关系，把原槽接入空闲链。
slots 本身不因删除使用 swap_remove，避免让所有连接失效。
新增节点优先复用空闲槽，无空闲槽时才增加 slots 的已初始化长度。
删除后的空闲槽供后续插入复用，已分配容量不会随 len 缩小；空间占用可能反映历史峰值。
首版不提供槽位压缩或 shrink，不能把摘除节点描述为立即归还全部相关存储。

跨数组操作可能在中途失败。清理责任依赖各数组的已初始化元素及局部拥有值，不依赖链结构仍完整，
也不能通过遍历活动链决定释放哪些 T。普通代码只观察正常返回后的完整结构。
不增加自定义 Drop 来重复释放 values 中的元素。

### 2.2 容量、次序与复杂度

公开 index 是从 head 起的零基逻辑位置，类型为 UInt64；插入或删除会改变后续位置。
定位从较近的一端开始。索引不是可跨修改保留的节点身份。

| 操作 | 算法复杂度目标 |
| --- | --- |
| len / is_empty / 首尾 Copy 查询 | O(1) |
| push_front / push_back | 摊销 O(1)，扩容可能 O(n) |
| pop_front / pop_back | O(1)，不包含用户析构成本 |
| 按位置查询、插入、删除、替换 | 定位 O(min(index, n-index))，定位后的连接修改 O(1) |
| reverse / clear / into_array / append | 线性；append 的节点转移对源元素数线性，扩容可能额外搬迁目标载荷，不承诺 O(1) 拼接 |

复杂度按元素搬移、节点访问计数，不包含用户 Drop、分配器及后端清理快照成本。
三组数组增加了空间和间接访问成本；不得宣传 List 的一般访问性能优于 Array。
若业务需要拿到节点后 O(1) 插入/删除，则需要后续句柄或游标方案，不能把本版位置接口描述成已满足。

`capacity()` 是三组数组容量的最小值，表示可容纳的活动节点数，包含可复用空闲槽。
`reserve(additional)` 检查 `len + additional` 溢出，并使三组容量至少达到这一目标；
给 slots 计算新增空间时须扣除空闲槽，不能把 slots.len 当作活动长度。
没有中间破坏容量承诺的操作时，随后 additional 次插入不再扩容内部数组；
这不承诺整个运行时零分配。clear 清空三组已初始化内容及链头，保留各数组容量。

### 2.3 API 与拥有权

以下为 LS0 冻结并由 L1–L4 实现的方法，模块函数提供等价入口；self 省略处按查询共享借用、修改独占借用处理。

| 接口 | 语义 |
| --- | --- |
| new / with_capacity / len / capacity / is_empty | 沿用 Array 的命名与容量约定 |
| push_front(value: T) / push_back(value: T) | 消费 value，加入相应端点 |
| pop_front() / pop_back() -> Option<T> | 移出首尾元素；空表返回 None |
| insert(index, value: T) -> Bool | 允许 index == len；越界返回 false，传入 value 仍按普通按值规则被消费并清理 |
| remove(index) -> Option<T> | 命中则移出元素；越界返回 None，List 不变 |
| get(index) / front() / back() -> Option<T> | 仅 Copy T；拥有型 T 的复制读取必须编译失败 |
| replace(index, value: T) -> Option<T> | 命中移出旧值并装入新值；越界返回 None 并清理传入值 |
| borrow(index) / borrow_mut(index) | Copy T 的 Element 视图，沿用 Array 现有准入，不增加新的引用类型 |
| borrow_ref(index) / borrow_mut_ref(index) | &T / &mut T，沿用 Array 对无引用 Copy 或拥有型 T 的支持边界 |
| reverse() | 翻转连接及首尾，不重排、复制或析构 T |
| reserve(additional) / clear() / release(self) | 容量预留、清理内容或消费整个容器 |
| from_array(values) / extend(values) | 消费 Array<T>，保持数组顺序，extend 追加到尾部 |
| into_array(self) -> Array<T> | 消费 List，按链顺序转移元素，不能直接返回物理排列的 values |
| append(&mut other) | 按 other 的链顺序移入 self 尾部，正常返回后 other 为空且保留容量 |

借用越界产生带源码位置的终止性运行时错误，不伪造引用，也不承诺 `Option<&T>` 资源视图。
List 的逻辑内容不连续，首版不提供逻辑 slice 或按物理 values 顺序的公开视图。
遍历可用 Copy 位置读取；需要移走所有元素时用反复 pop_front 或 into_array。
反复按位置 get 遍历可能 O(n²)，文档必须说明，通用借用迭代另行设计。

元素借用保守约束整个 List。借用存活期间拒绝冲突修改、删除、扩容、反转、移动或释放；
最后使用后恢复权限。即使某个操作只改连接，也不额外开放同时独占访问不同节点。
append 不能让两参数别名同一 List；与嵌套拥有容器有关的来源检查沿用现有规则。
普通共享引用元素被复制、移出或删除时仍保留原来源，删除节点不销毁被引用对象。

## 3. Set：复用 Map<T, Unit>

### 3.1 表示、相等性与元素访问

Set 是只有一个私有 `Map<T, Unit>` 字段的拥有型记录。复用 Map 的插入、探测、删除、
容量与重建逻辑，不复制哈希算法或创建第二套桶布局。Unit value 保持零大小值语义；
Map 的控制元数据仍有成本，首版不据此宣称与专用 HashSet 相同的内存占用。

相等性由 BorrowEq 定义，相等元素必须有相同 BorrowHash；协议在驻留期间必须稳定。
保留 Map 对恶意碰撞、错误协议、编译限额与运行时失败的安全边界，不承诺抗碰撞攻击。
Set 不保证插入次序、迭代次序或稳定元素地址；集合关系测试不能依赖桶或载荷顺序。
不提供元素可变借用，因为修改相等性或哈希相关状态会破坏集合。

### 3.2 API 与重复元素

查询统一使用借用式参数，冻结为 `contains(&self, value: &T)`、`remove(&mut self, value: &T)`，
并显式保留查询参数的 `@noescape(value)` 契约。已有的隐式共享借用适用时可直接传值表达式，
不另外引入 `_by_ref` 与按值查询两套新接口。方法到模块函数到 Map 的传递不能丢失该契约。

| 接口 | 语义 |
| --- | --- |
| new / with_capacity / len / capacity / is_empty | 不无故要求 Hash/Eq；容量单位为可容纳的不同元素数 |
| contains(value: &T) -> Bool | 查询不消费调用者的元素，不克隆 String |
| insert(value: T) -> Bool | 新增返回 true；重复返回 false，保留已驻留元素，清理新传入的重复值 |
| remove(value: &T) -> Bool | 命中则移除并清理驻留元素，返回 true；查询参数不被消费；未命中返回 false |
| pop() -> Option<T> | 移出任意一个驻留元素；空集合返回 None；不调用 Hash/Eq |
| elements() -> &Slice<T> | 复用 Map.keys 的只读视图及类型边界，不提供 elements_mut |
| reserve(additional) / clear() / release(self) | 沿用 Map 的容量、删除标记、清理和消费语义 |
| from_array(values) / extend(values) | 消费输入数组，按输入次序处理；等价元素保留首次驻留的实例 |
| into_array(self) -> Array<T> | 消费 Set，转移所有元素；输出顺序未指定 |
| is_subset(other: &Self) / is_disjoint(other: &Self) -> Bool | 只读借用两边，允许两个参数指向同一集合，不复制元素 |

insert 用 Map.insert 的 Option<Unit> 区分新增与已有；remove 用 remove_by_ref 的结果区分命中。
into_array 可通过 Map.into_entries 再消费 (T, Unit) 元组完成，不直接移出 Map 私有字段。
首版不提供 `take(value) -> Option<T>`：现有 Map.remove_by_ref 返回 value 并析构 key，
不能把它包装成移出驻留 key；若以后增加 take，必须先提供可复用的 Map entry 移出能力。

集合运算包含在 S3，选择明确的消费语义以支持 String 和拥有型元素：

| 接口 | 元素来源与重复处理 |
| --- | --- |
| union(self, other: Self) -> Self | 消费两边；等价元素保留 self 的实例，other 中的重复值清理 |
| intersection(self, other: &Self) -> Self | 消费 self，仅保留 other 中也存在的 self 元素；other 不变 |
| difference(self, other: &Self) -> Self | 消费 self，保留 other 中不存在的元素；other 不变 |
| symmetric_difference(self, other: Self) -> Self | 消费两边，仅保留恰好属于一边的元素；重复对中的两个实例都清理 |

不能对拥有型元素提供“两边只读借用却返回新拥有集合”的通用接口，除非另行引入显式克隆契约。
每次借用查询必须结束后才能移动待处理元素；消费 self 与同时借用同一 self 的调用应被拒绝。
集合运算复用 pop、查询、insert/remove，不依赖通用迭代器；在正常哈希分布下目标为线性期望成本，
最坏碰撞成本沿用 Map。扩容或删除标记整理可能分配，不能额外许诺原地零分配。

## 4. 共同的失败与清理契约

运行时分配、用户协议及 Drop 失败仍是当前的终止性错误；不伪装为越界 None、重复 false 或空集合。
操作失败后要求完整清理，不承诺容器可以恢复执行，也不为首版引入跨数组回滚事务。

- 普通实参照常求值。插入被拒、重复或越界都不代表传入拥有值没有被构造或消费。
- 任意内部指令边界，每个拥有值必须有唯一清理责任：来源数组、目标数组或受追踪局部值。
  不能依赖活动链、哈希桶或多个数组长度暂时一致才能回收。
- 清理失败后继续必要清理并保留首个错误。正常 API 不承诺按 List 逻辑次序或 Set 迭代次序运行 Drop。
- 构造、extend、append、集合运算及 into_array 可能同时持有输入、输出和扩容旧缓冲区，峰值内存计入任务预算。
- 所有权与借用限制对直接调用、模块别名、泛型包装和旧任务重载保持一致；不能只靠私有字段隐藏违规引用。

## 5. 分阶段交付

主依赖为 `LS0 → L1 → L2 → L3 → L4` 与 `LS0 → S1 → S2 → S3`，两线完成后进入 LS1。
“一起做”指共用一份范围与验收计划，不要求混在一个提交。建议先落地 S1–S2，
再推进 List 和 S3；每阶段交付可评审的实现、行为测试和准确的状态记录。

| 阶段 | 交付内容 | 必须通过的门槛 |
| --- | --- | --- |
| LS0：可行性与契约冻结（已完成） | 最小源码探针验证 List 三数组组合、拥有型 swap_remove 修正、借用返回来源；Set 包装、String noescape 查询与重复清理；冻结方法拼写和类型准入 | JIT/解释器/AOT 均执行；没有新增存储原语、放宽私有边界或借用规则。证据与验证范围见下节 |
| L1：连接与 Copy 元素（已完成） | 模块接线、构造/容量、两端操作、位置查询/插入/删除、空闲槽复用、反转 | Rust 序列模型逐步对照；首尾/唯一/中间节点、搬迁载荷索引修正、正反遍历和空闲链全部不变量 |
| L2：拥有型与失败路径（已完成） | String、Drop 记录、嵌套容器、replace、clear、release；完善转移过程 | 返回旧值后仍可使用；元素恰好一次清理；失败与取消不靠链完整性；非法复制实例拒绝 |
| L3：元素借用与共享内容（已完成） | Element、直接引用、外层拥有者依赖、共享引用元素 | 冲突修改/移动/逃逸拒绝；最后使用恢复权限；移动或删除元素不丢共享内容来源 |
| L4：批量与使用文档（已完成） | from_array、extend、into_array、append、示例和准确的复杂度说明 | 保持逻辑顺序，遍历不误用物理排列；自别名拒绝；部分转移失败/取消清理；不把位置接口宣传为稳定句柄 |
| S1：集合核心（已完成） | Map<T, Unit> 包装、构造/容量、insert/contains/remove/pop/clear/release | 整数、Unit、String、自定义相等值和碰撞；重复元素身份保留与查询不消费 |
| S2：借用与批量（已完成） | elements、from_array、extend、into_array，导入与泛型覆盖 | 无可变驻留 key 通道；视图约束整个 Set；拥有元素批量去重与清理；无额外 key 克隆 |
| S3：关系与集合运算（已完成） | 两种关系查询与四种消费式运算 | 模型集合对照；空/相等/包含/相交/不相交；同实例只读查询；元素来源优先级与消费别名拒绝 |
| LS1：联合验收（已完成） | 完整文档、示例、默认回归与专项 Debug 穷举，移除重复原型 | 两条线均完成；三后端、CLI、错误与取消覆盖；没有新增容器专用 unsafe 通道或借用检查豁免 |

LS0 的探针与生产代码应区分。通过探针不等于 List/Set 已实现；后续生产回归接管后，
移除重复算法及无独立价值的探针，不永久保留 IntList、IntSet 两套实现。
不在本轮修改 graph 语义、实现公开节点句柄或顺带升级其他容器 API。

### 5.1 LS0 验收记录

本节记录 `37fd5f0` 时的 LS0 验收，不追溯改写当时的范围。原探针作为普通用户模块 probe 加载，
当时没有注册 joky/list 或 joky/set。L1–L4、S1–S3 完成后，重复的 ListProbe 与 SetProbe
均已移除；原始探针可在 LS0 提交 `37fd5f0` 查看，下面记录其历史证据。生产测试接管现有回归。

| 探针入口 | 已验证事实 |
| --- | --- |
| list_moves | 三个数组及首尾/空闲元数据可以组成泛型拥有记录；移出首节点后修正搬迁载荷的反向索引、相邻连接和空闲头；剩余节点可正确借用，移出值在容器释放后仍有效 |
| list_borrows | 拥有型元素共享/独占借用经过包装仍指向实际载荷；最后使用后可以删除节点；连续删除至空表保持索引与首尾状态，三个资源各清理一次 |
| list_types | Int32 的 Copy 读取与 Element 读写、Unit、普通共享引用、嵌套 Array 的移动与借用均可表达；移出内容保留其实际来源 |
| set_strings | Map<T, Unit> 包装支持 String 去重；方法到模块函数到 Map 的 noescape 查询/删除可以接受局部 String；调用后仍能读取、替换查询值；只读切片、Int32 和 Unit key 可用 |
| set_duplicates | 相等但 id 不同的拥有值保留首次驻留实例，新重复值立即清理；删除清理驻留值，查询拥有者一直由调用者持有 |
| set_transfer | pop 可从 Map 的 (T, Unit) 返回值移出 T；释放 Set 不影响已移出的拥有值 |

两组正例均关闭 prelude，使用显式导入，分别执行于 JIT、解释器与独立 AOT 程序。
三后端核对相同的结果和构造/析构事件；VM 另检查 arena 存活计数归零。
事件中相邻数组元素的析构次序是当前底层实现的观测，不增加 List 的逻辑顺序析构承诺。

九组负例在两种 VM 加载路径及 AOT 编译路径拒绝：拥有型 T 经 get 复制、活动元素引用下删除、
返回局部容器的元素引用、访问 List 私有存储、List 移动后使用、活动 Set 切片下删除、
通过只读 Set 切片获取可变引用、访问 Set 私有 Map、用普通引用作 Set key。
没有为探针添加类型白名单、特殊编译器路径或运行时存储接口。

冻结及实现约束：

- List 采用三数组表示，公开 API 仍使用逻辑位置。探针为了直接检验映射而使用槽号定位借用，
  该测试接口不进入生产 API；位置查找、空闲槽复用与完整增删算法仍由 L1 验收。
- 类型准入以第 1 节定义的 Array 元素/Map key 规则为上限；get 的 Copy 限制与各借用入口的
  类型边界继续由现有实例化检查保证，不新增 Copy trait 或容器专用豁免。
- 修改方法先读取所需 Copy 元数据，再开始独占数组调用。不能在 self.slots.set 的后续实参里
  读取 self.free；这符合现有接收者先于实参建立借用的规则，无需前置语言修改。
- Set 查询使用 contains/remove 加借用参数及显式 noescape 契约；重复保留旧实例、pop 移出元素、
  无可变驻留元素通道的决策保持不变。没有发现阻塞 S1 或 L1 的新通用能力缺口。
- 合并全部探针类型曾触及现有泛型实例/依赖预算。测试按 List/Set 分组，每组每后端编译一次，
  没有提高 256 个实例或 32 层依赖的限额；后续生产类型矩阵也须尊重该预算。

LS0 当时实际通过 `cargo test list_set_probe`（3 个测试、6 个正例入口及 9 组拒绝用例）、
`cargo fmt --check`、`cargo clippy --all-targets -- -D warnings` 与文档链接/diff 检查。
改动仅为独立探针、测试模块接线与文档，没有运行 quick/full/stress 或 Release 穷举。
分配/Drop 失败、逐指令取消、重载、完整序列模型、性能与集合运算尚未由 LS0 验证，
List 与 Set 的后续生产覆盖见下节。

### 5.2 L1–L4 验收记录

生产实现位于 [list.jk](../src/stdlib/list.jk)，显式模块为 joky/list；实现只调用公开 Array API。
三数组、连接元数据和 helper 保持私有，没有新增存储原语、ABI、容器专用 unsafe 或借用检查豁免。

| 阶段 | 实现与证据 |
| --- | --- |
| L1 | 构造、容量、首尾及位置增删、空闲槽复用、较近端定位和反转；[model](../tests/fixtures/list_model.jk) 的每步结果与完整逻辑序列对照 Rust Vec；[invariants](../tests/fixtures/list_invariants.jk) 在测试专用定义作用域检查双向链、全部 Used/Free 槽、互逆载荷索引及历史峰值槽数 |
| L2 | String、带 Drop 的拥有记录、嵌套 List、replace、clear、release；移出值在释放容器后仍可用，拒绝的插入/替换值立即清理；不同 Drop 错误验证保留首个错误并完成其他清理 |
| L3 | Element 及直接共享/独占引用、最后使用恢复权限、普通共享引用和 Copy 引用聚合；38 组负例在 JIT/解释器加载及 AOT 编译路径拒绝，包括冲突修改、视图逃逸、私有存储、栈引用、拥有型复制和 append 自别名；跨模块泛型别名保持来源 |
| L4 | from_array、extend、into_array、append 按逻辑顺序移动拥有值；源 List 清空且保留容量；[lists.jk](../examples/lists.jk) 和跨文件别名程序在三种 CLI 后端运行；公开接口及复杂度归入 language.md |

行为与资源 fixture 见 [list.jk](../tests/fixtures/list.jk)，VM 测试见
[list_tests.rs](../src/vm/tests/containers/list_tests.rs)，独立原生程序见
[lists.rs](../src/backends/aot/tests/lists.rs)。19 个行为/失败入口在 JIT、解释器与 AOT 执行；
默认顺序模型在两种 VM 后端及 AOT 使用两个种子、各 256 步，逐步检查完整序列和内部不变量。
负例覆盖直接 API 及用户模块包装；正例和 CLI 示例关闭 prelude，使用显式导入。

资源验收包含：

- VM arena 预算按 16 字节递增扫至首次成功，分别观察 values、owners、slots 的扩容失败调用点，
  以及已构造批量元素之后的转移失败；每次核对资源事件和 arena 存活计数。
- 原生错误路径覆盖借用越界、容量整数溢出、布局容量上限、移出后失败和 Drop 失败；
  正常/失败退出均核对拥有值析构，不以单个返回值代替清理验证。
- 解释器资源边界取消、指令预算耗尽、重载后丢弃 VM 的旧任务取消/继续完成，均核对资源事件。
  构造完成回调前的指令切点可能已经拥有该记录，允许唯一被中断构造的未记录析构；
  已记录构造必须恰好析构一次，完整运行另外核对所有预期构造和析构。
- Debug 专项穷举覆盖 transitions 与 bulk 的每个解释器指令切点；长模型覆盖两个种子各 4096 步。
  不把解释器指令穷举描述为原生后端逐指令取消；性能和跨语言挂起不在本轮验收范围。

实际验证：`cargo test list_`、`cargo test exhaustive_list_ -- --ignored`、
`./scripts/check.sh`，以及文档链接和 `git diff --check`；均采用 Debug，未运行 Release 或无关 stress。

### 5.3 S1–S3 与 LS1 验收记录

生产实现位于 [set.jk](../src/stdlib/set.jk)，显式注册 joky/set；单一私有 Map 字段复用
公开 Map/Array 接口，没有新增桶布局、ABI、unsafe 通道、编译限额或借用检查豁免。

| 阶段 | 实现与证据 |
| --- | --- |
| S1 | 构造/容量、借用查询、插入/删除/pop/clear/release；整数、Unit、String 与全碰撞拥有 key；不同 id 的相等实例证明重复插入保留原实例、删除不消费查询拥有者 |
| S2 | 只读 elements、from_array/extend/into_array；输入次序决定首次代表实例，输出只检查成员；30 组拒绝用例覆盖引用 key、无协议查询、可变元素通道、视图冲突/逃逸、消费别名及跨模块包装 |
| S3 | 子集/不相交查询及四种消费式集合运算；身份断言区分左右元素，右侧借用不被结果保留；独立 Rust BTreeSet 状态模型与位集合关系矩阵对照 |
| LS1 | List/Set 文档与三后端 CLI 示例、默认 Debug 回归、相关 Debug 穷举；LS0 重复原型已移除，graph 范围保持不变 |

行为 fixture 见 [set.jk](../tests/fixtures/set.jk)，VM 与独立原生测试见
[set_tests.rs](../src/vm/tests/containers/set_tests.rs)、[sets.rs](../src/backends/aot/tests/sets.rs)。
24 个正常/失败入口在 JIT、解释器、AOT 执行，包含 Hash/Eq/Drop/容量失败、重复析构失败、
批量和四种运算的 Eq 失败；每个拥有实例的析构事件单独核对。
[跨模块包装](../tests/fixtures/set_views.jk)验证 noescape 查询、别名导入与借用右侧离开作用域后的结果。

[状态模型](../tests/fixtures/set_model.jk)在三个后端采用两个固定种子、各 256 步，
每步对照操作返回值、长度与全部成员，不依赖顺序；默认关系矩阵覆盖三元素域的全部 64 对集合。
Debug 专项长模型各 4096 步，八元素域矩阵覆盖全部 65,536 对集合。
穷举矩阵按行调用并结束根任务，避免任务期切片头部在单一根调用内无界积累。
模型的单步消费转移放在独立 helper，遵守现有引用流分析预算。

资源回归覆盖 transitions、bulk 与四种拥有元素运算：VM 分配预算按 16 字节扫至成功，
验证部分构造及转移失败后唯一清理；解释器资源边界取消、逐指令取消、指令预算耗尽和
重载后旧任务继续/取消均核对析构事件与 arena 存活数。逐指令取消不代表原生后端支持该能力。
不把本轮开发验收描述为 Release 发布验收、跨语言挂起或性能验证。

实际验证：`cargo test set_`、`cargo test exhaustive_set_ -- --ignored`、
`cargo test exhaustive_list_ -- --ignored` 与 `./scripts/check.sh` 全部通过。
默认检查包含 fmt、Clippy、Debug 测试及三后端 while/Map 短输入基准正确性；
Set 专项穷举 3 个测试、List 专项穷举 2 个测试通过，文档链接与 diff 检查通过。
本轮使用 Debug，未运行 Release、全部无关 stress 或性能测量。

## 6. 验证矩阵与完成标准

共享 fixture 按泛型实例预算分组，每组在每个后端编译一次，再逐入口核对结果与资源事件。默认测试使用短确定性模型，
长序列和逐指令取消用 `exhaustive_` 命名并默认忽略；模型保留种子与可重现的操作记录。

| 维度 | List | Set |
| --- | --- | --- |
| 基本模型 | 与 Rust Vec 的逻辑顺序对照，检查全部内部索引不变量 | 与独立集合模型对照成员关系，不比较遍历顺序 |
| 重点边界 | 空/单节点、头尾中间、反转后修改、空闲槽重复复用、最大下标/容量溢出 | 空、重复、全碰撞、删除标记、同容量重建、容量溢出、重复实例身份 |
| 类型 | Unit、整数、记录/枚举、String、Drop、嵌套容器、普通共享引用 | Unit、现有协议支持的整数、String、无引用自定义 Copy/拥有 key；非法 key 拒绝 |
| 来源与权限 | 拥有值被复制、活动视图下修改、视图逃逸、栈引用存入、append 别名均拒绝 | 可变驻留元素通道、查询引用逃逸、活动切片下修改、消费运算别名均拒绝 |
| 错误 | 扩容峰值、每组数组的分配失败、移出后失败、Drop 失败、批量半途退出 | Hash/Eq/Drop 失败、重复清理、rehash 预算、集合运算半途失败 |
| 生命周期 | 两端/中间删除、替换、反转、clear、append 的取消与重载保活 | insert/remove/pop/clear、批量和集合运算的取消与重载保活 |
| 集成 | 模块/方法混用、别名/跨模块、关闭 prelude、JIT/解释器/独立 AOT/CLI | 同左，额外验证借用查询的 noescape 契约跨包装保持 |

资源测试使用构造/转移/析构事件和存活计数；只检查输出值或总 arena 归零不足以证明没有重复析构。
Set 的代表元素测试需用“相等但有不同 id 的拥有值”，从而区分保留旧实例与误替换成新实例。
性能测量与正确性验收分开，不能把哈希平均复杂度或索引连接的 O(1) 修改等同于已测得性能提升。

阶段检查遵循 [AGENTS.md](../AGENTS.md#按改动范围验证)：纯文档检查内容、链接及 diff；
独立 Rust 测试运行 fmt、Clippy 与受影响入口；局部实现先针对性测试再 quick；
跨模块所有权、存储或生命周期修改运行默认 Debug 检查，并补相关 Debug 穷举。
LS1 联合开发验收执行 `./scripts/check.sh` 与相关 Debug stress；用户明确要求发布验收时
才运行 `./scripts/check.sh --full --release` 与 `./scripts/check.sh --stress --release`，报告实际结果。
不因提交动作重复全套检查，也不把针对性通过写成全量通过。

## 7. 与 graph 的关系及后续边界

List 内部的双向索引连接并未开放语言级循环引用。List 独占其存储，Set 独占其 Map；
现有 graph 则创建可以共享、带环、封存后通过 `&Node` 遍历的任务期对象图。
可变链表完成不能替代 graph 的类型和发布契约，本轮保留 graph 及其全部回归测试。

若后续确有公开节点操作需求，再单列 NodeStore/节点句柄方案。至少要明确容器身份、
槽号与代际、删除/复用/clear 后的失效、错误容器句柄、计数溢出、借用与迁移规则；
仅一个 UInt64 下标无法提供这些保证。公开游标若需要携带容器借用，还依赖现有聚合存储限制之外的设计。
跨容器拼接也不能在这些问题未定时承诺 O(1)。

只有通用节点库进一步覆盖共享节点、循环、构造期间不可观察、只读发布、失败清理及
跨挂起保活，并给出现有 `&Node` 程序的迁移方案后，才评估 graph 降为库能力或删除语法。
Box 所有链、Rc 共享所有权、可变循环图、通用迭代器和 Set.take 均作为后续独立目标。
