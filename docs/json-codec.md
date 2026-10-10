# JSON 编解码设计与分阶段验收

状态：JSON0–JSON3 已完成。起点为 `a0c8f8d`。
`joky/json` 提供 Value、Number、受检数字转换、Limits 与严格编解码入口；TCP JSON 示例与 Godot 联合验收已接入。
JSON0 本身不修改编译器、运行时或标准库语义。

本轮承接 [NET5 双向会话](network-runtime.md#15-net5-tcp-读写拆分)：先提供独立 JSON 值与
编解码，再组合长度前缀 TCP 消息及 Godot 所属任务。使用现有 Result、String、Rune、
Array、Map 和拥有权规则；不会引入脚本求值、后台线程或另一套宿主专用 JSON 值。
实现完成后，公开用法归入 [language.md](language.md)，编译器与清理不变量归入
[architecture.md](architecture.md)；本文保留方案、阶段和验收记录。

## 1. 范围与规范基线

语法基线为 [RFC 8259](https://www.rfc-editor.org/rfc/rfc8259.html)。实现完整的值语法，
顶层允许标量；不提供注释、尾逗号或额外字面量扩展。以下互操作策略是 Joky 的首版选择，
不是把 RFC 的所有许可都解释成强制要求：

- 字符串只表示 Unicode 标量序列。输入字节必须是严格 UTF-8；解码转义时拒绝孤立代理项。
- 对象键按解码后的字符串比较，拒绝重复键；例如 `"a"` 与 `"\u0061"` 是同一个键。
- 开头 BOM 返回错误，编码不输出 BOM。完整解码只接受一个值及前后 JSON 空白。
- 数字保留已验证的原始拼写；需要机器数值时显式转换。
- 对象无顺序契约，数组保留顺序。编码器不排序键、不承诺逐字节复现整个输入。

自动 struct/enum 派生、反射、schema、JSON Pointer、流式 token 接口、增量解码、
pretty print、规范化签名格式和任意精度数学运算不属于首版。
JSON 不负责 TCP 分帧；TCP 超时、重连、HTTP、TLS 和 UDP 不属于此轮。

## 2. JSON0 探针与实际缺口

[最小探针](../tests/fixtures/json0_primitives.jk)及 [CLI 接线](../tests/cli.rs)检查现有能力，
探针的有限 Atom/Group 不是 JSON Value 实现，也不验证 JSON 语法。

| 能力 | JSON0 实测 | 后续处理 |
| --- | --- | --- |
| 有限的拥有枚举、Array、String key Map | 借用匹配、原地替换、跨 yield、race 输家取消后复用及外层 Drop 均可用 | 复用现有规则，不增加隐式复制 |
| `Value` 经 Array 自递归 | JSON0 时报告递归声明错误 | JSON1a 已支持 |
| `Value` 经 Map 自递归、记录/容器互递归、别名间接递归 | JSON0 时均拒绝 | JSON1a 已覆盖，并包括有限泛型递归 |
| 直接内联自递归 | 当前拒绝 | 必须继续拒绝 |
| Int64/UInt64 十进制输出、Float64 输出/分类 | 可用；2^53+1 转 Float64 与 2^53 相同，负零输出 `-0`，极小/极大有限浮点的定点输出超过 256 字节 | 保留数字文本，显式转换 |
| 文本到整数/Float64 | JSON0 时没有文本解析入口 | JSON1b 已提供 Number 的受检整数/浮点转换；通用数值 from_text 未开放 |
| 严格 UTF-8、Rune、StringBuilder | 可用；拒绝 UTF-8 编码的代理项，支持 NUL 与非 BMP 标量 | JSON2 复用并补 JSON 转义逻辑 |

负向探针记录当前缺口，不是永久要求。JSON1a 已将合法间接递归用例改成行为测试；
仍保留直接内联递归的拒绝测试。文本转换探针也随相应交付更新，不为保持绿灯维持旧限制。

## 3. JSON1a：拥有型递归容器前置

首版采用自然的拥有值树。数组/对象可移入或移出任意深度的子值，整个树统一自动清理，
不以只读 graph、公开节点索引或宿主 opaque 对象代替公共 Value。

当前已实现的类型形状如下；Bool/String 是保留名，变体采用 Boolean/Text：

```text
enum Value {
    Null,
    Boolean(Bool),
    Number(Number),
    Text(String),
    Array(Array<Value>),
    Object(Map<String, Value>),
}
```

类型环必须经过受检拥有容器的间接存储边界。Array/Map 的包装记录或别名不改变是否合法，
不能简单对名称为 Array 的声明跳过环检查。`enum Bad { Next(Bad) }`、通过 Option/元组
形成的纯内联环仍非法；不断改变类型实参而产生无限实例的泛型递归也仍拒绝。
无需先提供公开 Box、Rc、独占引用字段或可变共享循环图。

前置不仅是移除 resolver 的错误，还需验证：

- 名义身份、泛型实例、布局、Copy/拥有分类与含引用分类都能在有限类型图上终止。
  布局查询到达容器控制块即结束内联展开；元素布局独立求解。
- 元素清理描述、活跃枚举标签和 Drop 描述必须支持间接回边；不能无限展开 descriptor，
  也不能让编译器元数据的引用环泄漏。运行时释放按实际拥有树执行，不遍历非活跃载荷。
- 用户可以构造比 JSON 深度上限更深的普通树。移动、失败、取消与清理不能依赖 JSON 限制
  才安全；深树清理避免随树深度增长的原生栈递归，工作存储继续受预算约束。
- 借用元素时不能同时移动、替换或扩容原容器；消费式匹配、mem.replace、删除、提前 release
  和作用域退出转移或释放每个唯一拥有者一次。清理失败仍继续释放其他拥有者。
- 同步调用、CPS 挂起、解释器时间片、预算耗尽、任务取消和重载保活都沿用现有责任链。
  不开放新的宿主聚合 ABI；持久实例准入仍按其自身边界另行检查。

实现入口包括 [类型解析](../src/core/types.rs)、[布局](../src/core/types/layout.rs)、
[元素存储布局](../src/core/types/memory_layout.rs)、[清理规划](../src/core/ir/cleanup.rs)、
[Buffer](../src/runtime/buffer.rs)和[清理运行时](../src/runtime/cleanup.rs)。
这是跨模块语言基础修改，需要独立完成三后端行为测试、默认 Debug 检查及受影响取消穷举。

## 4. JSON1b：值模型与数字

模块为 `joky/json`，不自动加入 prelude。Value 为非 Copy 的拥有枚举，Number 的文本字段私有。
数组使用 Array<Value>，对象使用 Map<String, Value>；String key 沿用 BorrowEq/BorrowHash。
对象 value 不要求 Eq/Hash，Value 不自动获得数字意义上的相等、哈希或隐式深拷贝。

构造和修改使用现有枚举构造、容器方法、共享/独占 match、mem.replace 等显式操作。
对象查询先 contains_key_by_ref 再 value_ref，数组先检查 len 再 borrow_ref；借用越界沿用现有
终止性错误规则。便利查询逐项验收，不先承诺当前容器没有提供的 Option<&Value> 接口。
移除对象键或数组元素返回拥有子值；键缺失与 Value.Null 必须可区分。

### 4.1 Number 的不变量与转换

Number 内部保存严格验证过的 ASCII 数字拼写，外部不能注入未验证文本。
解码和重新编码保留该拼写，包括 `-0`、指数的大小写与符号、小数尾零；
`18446744073709551616`、`1e400` 在长度限制内也可作为 Number 保存。
这提供文本精度保留，不承诺任意精度运算或与采用 Float64 的对端无损互操作。

| 已实现接口 | 契约 |
| --- | --- |
| `Number.from_text(input: &String)` | 返回 Result；检查完整数字语法并复制输入，不保留借用 |
| `Number.from_i64` / `from_u64` | 十进制构造，使用现有整数格式化 |
| `Number.from_f64` | 返回 Result；非有限值拒绝，有限值使用现有最短往返格式，保留负零 |
| `to_i64` / `to_u64` | 返回 Result；首版只转换没有小数/指数部分的整数拼写，逐位检查范围 |
| `to_f64` | 返回 Result；允许 IEEE 舍入，结果须有限，溢出报错；不承诺精确转换 |

整数转换不先经过 Float64，不截断小数。`1.0` / `1e0` 的整数转换返回 NotInteger，
Int64 的 `-0` 返回 0；UInt64 的负号拼写（包括 `-0`）返回 OutOfRange。
Float64 转换按目标精度舍入；下溢可产生保留符号的零，须在接口文档和边界测试中明确。
NumberError 区分 InvalidSyntax、NotInteger、OutOfRange、NonFinite、NumberLimit，并提供文本内字节 offset。
Number 构造统一应用 1024 字节的硬数字上限，长度检查在复制或扫描前完成；
机器数格式化也必须满足这一不变量。

数字语法扫描和受检整数累积在 Joky 中实现。Float64 文本转换不能用简单的
“十进制逐位累加再乘幂”替代正确舍入；JSON1b 已引入私有 `parse_float64(&String) -> Float64` 原语，
三个后端共享 Rust 的十进制解析。原语不分配、不转移拥有权、不保留借用，输入上限 1024 字节；
非法/超限输入返回 NaN，溢出返回 Infinity，下溢返回有符号零。Number 的源码包装先验证
语法与长度，转换后检查有限性，将溢出返回 OutOfRange；无效拥有句柄仍走终止性 ABI 错误通道。
通用公开数值 from_text 未开放。

### 4.2 JSON1b 交付记录

实现与公开用法分别见 [json.jk](../src/stdlib/json.jk) 和 [language.md](language.md#json-值与数字)。
number.as_text() 返回共享借用，不能修改私有文本；整数转换先检查小数/指数标记，
再检查符号和范围。NotInteger.offset 指向小数点或指数标记，OutOfRange.offset 指向
第一个使整数超范围的数字，UInt64 负号为 0，浮点溢出为 0；NonFinite.offset 为 0，
NumberLimit.offset 为 1024。构造的 InvalidSyntax.offset 指向首个违反语法的字节或 EOF。

[三后端行为 fixture](../tests/fixtures/json1b_values.jk)覆盖文本保真、边界/错误位置、
正确舍入、溢出、下溢、负零、最小次正规数和最大有限数的格式化往返，以及所有 Value 类别的
构造、共享查询、独占替换、删除、消费匹配和跨 yield 保存。私有字段/原语的拒绝测试防止绕过不变量。
[VM 验收](../src/vm/json_tests.rs)补充部分构造预算失败、取消边界、逐指令取消、重载与 VM 销毁后的保活清理。
编解码和 TCP JSON 组合仍留到 JSON2/JSON3。

验证：针对性三后端行为与拒绝测试、默认 `./scripts/check.sh`（Debug）、JSON1b 逐指令取消穷举均通过。
Godot 扩展格式/Clippy、8 项 Rust 测试与全部真实引擎场景通过完整复跑；首轮 NET5 的 150ms
截止时间用例在前置通信阶段超时，原样单独复跑与完整复跑均通过，保留该用例的时间敏感性记录。
未运行 Release、无关压力穷举或性能计时。


## 5. JSON2：解码、编码与错误

以下公共入口均已实现。
带 limits 的版本与默认 limits 包装复用同一算法。

| 入口 | 拥有权与结果 |
| --- | --- |
| `decode(input: &String) -> Result<Value, DecodeError>` | 完整解码，结果拥有全部字符串/数字文本，输入可在返回后释放 |
| `decode_with_limits(input: &String, limits: Limits)` | 同上，使用指定上限；不保留输入借用 |
| `decode_bytes(input: Array<UInt8>) -> Result<Value, DecodeError>` | 消费字节数组，先检查长度及严格 UTF-8；失败也按普通拥有值清理 |
| `decode_bytes_with_limits(input: Array<UInt8>, limits: Limits)` | 字节入口的上限版本 |
| `encode(value: &Value) -> Result<String, EncodeError>` | 紧凑输出，借用值树；输出独立拥有 |
| `encode_with_limits(value: &Value, limits: Limits)` | 同上，使用指定上限 |

编解码首版是同步源码库，不使用 eval。解码用受限状态栈推进，编码用显式遍历栈推进；
不按未限制的输入深度递归调用语言函数。未完成的容器和字符串各自保持拥有清理责任，
语法/上限错误不返回半个 Value；内存不足和执行预算耗尽仍是终止性运行时错误。
同步接口不保证主动让出：解释器在所属异步入口内可受现有时间片限制，
JIT/AOT 同步 helper 不会自动被抢占。字节上限不等于引擎帧时间保证；
JSON3 已检查真实消息负载下的任务进展，未承诺帧耗时；若要协作式 codec 则另外明确挂起接口。

JSON 转义与 Joky 源码字符串转义分别实现：不能把 `\u{...}` 当成 JSON 的 `\uXXXX`。
解码组合代理对生成 Rune；字符串中的 NUL 保留为数据。编码转义引号、反斜线和控制字符，
其余 Unicode 标量直接输出 UTF-8；斜杠可直接输出。空字符串、空数组、空对象各自保留。
重复键在解码后的键上检查，不经 Unicode 规范化；不接受多个相邻顶层值或有效值后的垃圾。

### 5.1 Limits

Limits 是 Copy 配置，`Limits.defaults()` 返回以下默认值。首版这些值同时是硬上限，
调用者可缩小但不能增大；超过硬上限先返回 InvalidLimits，不开始编解码。
零上限合法，但相关内容不能超过零；所有边界按 `实际值 <= 上限` 接受。

| 字段 | 默认/硬上限 | 计数方式 |
| --- | --- | --- |
| max_input_bytes | 1048576 | 原始输入的完整 UTF-8 字节数，含空白 |
| max_output_bytes | 1048576 | 实际编码字节，含转义、引号和分隔符；追加前检查 |
| max_depth | 128 | 标量根深度 0；每进入一个数组/对象加 1，空容器也计入 |
| max_nodes | 65536 | 每个 Value 计 1，包含容器和 Null；对象键不另计 Value |
| max_string_bytes | 262144 | 每个解码后字符串/对象键的 UTF-8 字节数，或编码源字符串的长度 |
| max_number_bytes | 1024 | 每个 Number 的完整原始数字拼写字节数 |

数字上限选为 1024 字节，覆盖现有 Float64.to_text 对次正规数与最大有限值的定点输出；
不能默认把“最短往返有效数字”理解成“总文本短于 256 字节”。

解码/编码只应用本操作相关字段，Limits 自身仍统一校验。长度、节点和扩容计算不能环绕；
检查在相关分配或输出增长前执行。限额不等于实际总内存预算，Map 元数据、栈和临时拥有者
仍按运行时计费。max_depth 与普通语言的类型深度、函数帧数、析构重入限制互相独立。
编码后转成 TCP 载荷仍需满足 write_message 的 limit，JSON 上限不能绕过传输上限。

### 5.2 错误位置与恢复边界

DecodeError 包含 kind 和零基原始输入字节 offset，不采用 UTF-16 单元或字符索引。
EOF 位置为 input.byte_length；InvalidEscape/转义产生的 InvalidUnicode 指向起始反斜线，
原始 InvalidUtf8 沿用 Utf8Error.offset；无效数字指向违反语法的首字节或 EOF，
重复键指向第二个键的起始引号。输入超限指向第一个超限字节，InvalidLimits 的 offset 为 0。

首版 kind 至少区分 UnexpectedEof、UnexpectedToken、InvalidEscape、InvalidUnicode、
InvalidUtf8、InvalidNumber、DuplicateKey、TrailingData、InputLimit、DepthLimit、NodeLimit、
StringLimit、NumberLimit、InvalidLimits。字节入口先做长度和 UTF-8 校验，再扫描语法，
因此先发现 UTF-8 错误不是“按语法从左到右的首个错误”。
所有入口先校验 Limits；数字 token 先受限扫描，再验证语法，因此同时包含语法错误和超长内容时
可先返回 NumberLimit。DepthLimit/NodeLimit 指向当前值的起始字节；StringLimit 指向
使解码后长度超限的原始标量或转义的起始反斜线，NumberLimit 指向首个超限 token 字节。
代理对的第二段含非法十六进制字符时 InvalidEscape 指向第二段反斜线；缺少合法低代理项
返回指向第一段的 InvalidUnicode，已经开始但未完成的四位转义返回 EOF。

EncodeError 是枚举，区分 InvalidLimits、OutputLimit、DepthLimit、NodeLimit、StringLimit、NumberLimit，
不伪造输入 offset 或承诺 JSON 路径。Number 的不变量让正常 Value 不包含非法数值文本。
失败只清理本次输出/工作栈，原输入树保持不变。

TCP read_message 成功后，非法 JSON 是完整消息内的业务错误；应用可选择回复错误或结束会话。
TCP 分帧截断、取消或长度超限仍遵循 NET4/NET5 的协议结束规则，不能用 JSON 解码失败掩盖它们。

## 6. 分步交付与验收

每步完成相关 Debug 验证后独立提交，不把计划中的能力计作已实现。

| 阶段 | 交付 | 必须验收 | 状态 |
| --- | --- | --- | --- |
| JSON0 | 契约、前置/缺口探针、数字与错误策略 | 已有原语三后端执行；递归/数字转换缺口诊断一致 | 已完成 |
| JSON1a | 经拥有容器间接存储的递归类型基础 | 自递归、互递归、别名、有限泛型；非法内联/无限实例继续拒绝；布局、借用、深树移动/清理、预算失败、CPS 取消和重载 | 已完成 |
| JSON1b | Value/Number、构造修改、借用查询与受检数字转换 | 大整数文本、整数边界、浮点舍入/溢出/下溢/负零、非有限拒绝、移出子树和键缺失/Null 区分 | 已完成 |
| JSON2a | 解码、转义、重复键、错误位置与上限 | 全部值类别；原始 UTF-8/代理对/孤立代理项/NUL；语法与限额边界、错误返回和清理 | 已完成 |
| JSON2b | 紧凑编码与往返 | 文本精度、数组顺序、对象无序语义、转义输出增长、输入保留及失败清理 | 已完成 |
| JSON3 | JSON over TCP 示例与联合验收 | 真实请求/回复/推送，分片/背压、业务错误；三后端、Godot 两实例/暂停/重载/销毁/迟到事件 | 已完成 |

JSON1a 是语言基础，不与 codec 混成一次巨大修改；JSON1b 到 JSON2 主要使用 Joky 源码，
仅为必要的受检数值转换引入共享原语。JSON3 保持现有标量 Godot 导出 ABI：
拥有 JSON 值留在任务局部，State 通过短同步调用取得标量控制或更新计数。

后续测试矩阵需要覆盖：

- 输入：所有顶层类别、任意容器组合、空内容、合法空白、控制字符、转义键、非 BMP 字符。
- 拒绝：不完整 token/转义/容器、错误逗号/冒号、未转义控制字符、重复键、BOM、非法数字、尾随数据。
- 数字：Int64/UInt64 边界及前后一位、2^53±1、超出机器范围的合法拼写、极端指数、舍入边界与负零。
- 资源：每个 limit 的 N-1/N/N+1、零上限、超硬上限、低预算、部分构造失败、逐指令取消和旧代码保活。
- 往返：值的语义比较忽略对象键顺序，Number 拼写精确保留；输出可由独立 JSON 实现接受。
  不把 JSON 文本的空白/转义拼写完全复现或字典序排序作为通过条件。

验证按 [AGENTS.md](../AGENTS.md#按改动范围验证)选取范围。核心递归类型修改需要默认检查
及相关 Debug 穷举；独立探针不自动运行全套或 Release；Godot 场景以真实引擎检查为准。

## 7. JSON0 验证记录

- `cargo test --test cli json0_` 通过 3 项探针：已有原语在 JIT、解释器和独立 AOT 实际执行；
  6 种递归类型形状及 3 个文本数值转换入口在 run、run --interpreter 和 build 下均报告当前缺口。
  新增极端浮点输出长度探针后重跑同组通过，有限拥有值的外层 Drop 各执行一次。
- `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings` 及 `git diff --check` 通过。
- README 与本文共 76 处本地链接/锚点检查通过；规范来源直接链接 RFC 8259。
- 本轮只增加探针、CLI 测试接线与文档，未运行主仓库整套检查、压力穷举、Release 或 Godot。
  JSON0 当时尚未实现 JSON 模块、递归拥有树和文本数值转换；不能将前置探针通过报告成 JSON 编解码验收。

## 8. JSON1a 实现记录

- 递归基础采用现有 Buffer 间接存储与名义身份，支持 Array/Map、自递归、记录/枚举互递归、
  别名、泛型节点及 Array<Option<Tree>>；未引入 Box、共享可变环、节点索引或宿主 JSON 对象。
- 元素布局及清理按完整定义建立，描述符止于直接子拥有句柄；释放沿控制块迭代，状态计入分配预算。
  三后端执行 10,000 层拥有树；VM 两模式还验证构建后预算降为零仍完成 10,000 层带 Drop 树的释放。
- 覆盖借用查询、mem.replace、消费 match、Array pop/reserve/truncate、Map 子值修改，以及初始化失败、
  析构失败继续清理、CPS 取消、旧任务重载和持久实例完整元素图准入。
  两 VM 模式还将 1,000 层带 Drop 树保持到 Pending，在源码重载和 VM 销毁后取消并核对全部析构顺序。
- 内联递归、无限实参展开、Copy 读取拥有元素与真实活跃借用冲突保持拒绝。
- 下一步 JSON1b 实现公开 Value/Number 与数字转换；此阶段仍没有 JSON 编解码接口。

验证记录：`RUST_TEST_THREADS=8 ./scripts/check.sh` 完成格式、Clippy、默认 Debug 测试与短输入基准正确性；
新增深树 Pending/取消探针另行执行，并复核相关 fixture 的 JIT、解释器、AOT。
Debug 逐指令取消穷举覆盖递归树、挂起拥有者、NET5a 分离拥有者和异步拥有者借用。
`RUST_TEST_THREADS=2 ./integrations/godot/check.sh` 单独完成适配器检查与真实 Godot 联合验收。
文档检查覆盖本轮五份文档的 262 个本地链接与 `git diff --check`；未运行 Release 或无关穷举。

## 9. JSON2a 实现记录

- 四个解码入口均采用 Joky 源码状态机；私有 Decoder 与 Array<DecodeFrame> 保存未完成容器、
  待插入对象键和已完成子树。处理输入深度不递归调用语言函数，不新增宿主 codec 原语或 ABI。
  step/helper 划分保持编译器现有引用分析预算，未放宽预算阈值。
- 解码复用严格 UTF-8、Rune、StringBuilder、Array/Map 与 Number 的范围语法检查；原始数字
  验证后复制完整拼写。对象键按解码后 UTF-8 比较，保留 Unicode 规范化形式的区别。
- 无效配置、输入长度和字节入口 UTF-8 检查先于语法扫描；容器深度/节点在构造前检查，
  字符串在追加标量前检查，数字在扫描及复制时受限。解码不应用 max_output_bytes，但仍校验其硬上限。
- DecodeErrorKind 在独立源码模块声明，再由 `joky/json` 重导出类型，以保留它与
  NumberErrorKind 各自的 NumberLimit 标签。公开使用 `DecodeErrorKind.NumberLimit`，
  不依赖同名平铺构造器。
- [严格行为 fixture](../tests/fixtures/json2a_decode.jk)覆盖全部值类别、语法拒绝、精确错误位置、
  每项相关限额的 N-1/N/N+1 和零值、全部配置硬上限、128/129 层容器、UTF-8/代理对/NUL、
  转义后重复键与不同规范化形式的键。解码输入释放后继续查询拥有树。
  [独立语义样本](../tests/fixtures/json2a_model.jk)在 JSON2a 时包含固定种子生成的 40 组输入与期望指纹，
  由 Python JSON 解析结果产生基准，忽略对象顺序并精确保留数字拼写；两组 fixture 均执行三后端。
- [VM 验收](../src/vm/json_tests.rs)检查正常/错误路径的分配预算失败、解析中取消、完成树跨 yield、
  重载和 VM 销毁后的完成/取消清理；逐指令取消另以 ignored Debug 用例执行。
- Godot 回归暴露并修正 [NET5 截止时间 fixture](../integrations/godot/demo/tests/network_duplex.gd)
  的准备顺序：旧用例在请求前暂停树，空闲发送端的 next_frame 按节点暂停，可能一直等到会话超时。
  现在先完成命令握手并发送半帧，再暂停及缩放时间，仍验证真实时间截止与双向取消清理；
  截止时间改为 1000ms、真实等待为 1.2s，给准备通信留出余量，不修改网络或 Godot 等待语义。

验证：JSON0–JSON2a 针对性回归、三后端严格解码/独立语义样本、默认 Debug 检查及
JSON2a 逐指令取消穷举通过。默认检查包含 1139 项单元测试、148 项 CLI 测试、
19 项模块常量测试、23 项源码模块测试和三后端短输入基准正确性。
Godot 首轮完整回归、原样单独复跑与临时计时探针均复现旧 NET5 准备顺序问题；
修正后单独 NET5 和完整 Godot 检查均通过（含格式、Clippy、8 项 Rust 测试及真实引擎场景）。
文档本地链接/锚点与 `git diff --check` 通过；未运行 Release、无关穷举或性能计时。

下一步 JSON2b 实现紧凑编码与独立实现交叉验证的往返；本阶段不提供编码或 TCP JSON 示例。

## 10. JSON2b 实现记录

- `encode(&Value)` 和 `encode_with_limits(&Value, Limits)` 借用完整树，返回独立拥有的紧凑 String。
  不新增宿主 codec 原语、反射或共享引用容器。EncodeError 在独立源码模块声明，由 `joky/json`
  重导出名义类型，与 DecodeErrorKind/NumberErrorKind 的同名限额标签通过类型限定区分。
- 源码 Encoder 保存 StringBuilder、节点数、访问模式与 Array<UInt64> 索引路径；每步从共享根
  重新借用当前节点。路径不存引用、不复制子树、不修改输入，也不按树深度递归调用函数。
  代价是节点访问最多重复遍历 128 层祖先，时间为 O(节点数 × 深度 + 文本字节)，额外路径空间 O(深度)。
  输出和路径存储继续按任务 arena 计费。
- 编码直接输出原始 Number 拼写，保留大整数、极端指数和负零；数组保序，对象不排序键。
  引号、反斜线及控制字符按 JSON 转义，b/f/n/r/t 使用短转义，其余控制字符用小写 `\u00xx`；
  其他 Unicode 标量直接输出 UTF-8，斜杠不转义。键使用相同规则。
- 所有配置先校验硬上限；编码忽略 max_input_bytes 的操作限制，但仍校验该配置。
  深度/节点在输出当前值前检查，字符串/数字先检查源长度；每次追加前检查实际 UTF-8 输出增长，
  包括转义、引号、逗号、冒号及容器闭合符。不同限额同时违反时，除配置预检外按遍历及追加顺序返回。
- [编码 fixture](../tests/fixtures/json2b_encode.jk)覆盖紧凑拼写、全部控制字符、Unicode、键转义、
  数字保真、相关限额的 N-1/N/N+1、零限额与全部配置硬上限、128 层成功及 1024 层树的受限拒绝。
  成功和可恢复限额错误后均继续检查原输入树。
  [共享语义样本](../tests/fixtures/json2a_model.jk)扩为 45 组，并检查 decode/encode/decode 的独立期望指纹；
  五个新增边界模型覆盖所有控制字符及键、机器范围外数字、不同 Unicode 规范化形式和空内容。
- [VM 验收](../src/vm/json_tests.rs)覆盖部分输出的分配预算失败、限额错误清理、解析/编码过程中取消、
  输入树与输出跨 yield、重载及 VM 销毁后的完成/取消；逐指令取消另行执行 ignored Debug 用例。

独立互操作验收：三后端分别编码 45 组样本，再用 Python 标准 JSON 解析器读取全部输出，
拒绝非有限扩展及重复键，并按原始数字 token、数组顺序和对象无序语义核对独立指纹，135 组均通过。
该验收不向默认 Cargo 测试引入 Python 或额外 JSON 依赖。

验证：JSON2a/JSON2b 针对性测试、45 组往返模型、输入修改/释放后的独立输出检查、
JSON2b Debug 逐指令取消穷举通过。默认 `RUST_TEST_THREADS=8 ./scripts/check.sh` 通过格式、
Clippy、1142 项单元测试、149 项 CLI 测试、19 项模块常量测试、23 项源码模块测试及
三后端短输入基准正确性；最后增加的五个模型与输入修改用例另行复跑受影响测试通过。
`RUST_TEST_THREADS=2 ./integrations/godot/check.sh` 完成格式、Clippy、8 项 Rust 测试及全部真实引擎回归，
包括修正后的 NET5 暂停截止时间用例。236 处本地链接/锚点和 `git diff --check` 通过。
未运行 Release、无关穷举或性能计时。

下一步 JSON3 组合长度前缀 TCP 消息与真实 Godot 任务；编码不自动分帧，也不绕过传输长度上限。

## 11. JSON3 TCP 组合与联合验收

[tcp_json.jk](../examples/tcp_json.jk) 演示长度前缀 JSON 请求、回复及独立启动推送。
协议和上限的用法归 [语言文档](language.md#json-over-tcp-示例)；
[network_json.jk](../integrations/godot/demo/network_json.jk) 使用现有 Godot 标量 State ABI，
JSON 树、编码结果与 socket 所有者只留在任务局部。本阶段不新增 stdlib、运行时原语或后端分支。

完整消息内的 JSON/UTF-8 和信封错误转换为 error 回复；之后继续处理有效请求。
帧截断或长度超限直接结束会话，取消关闭两个方向，不能从丢失的前缀恢复。
编码完成且通过输出限额后才开始发送长度头，JSON 上限与传输上限分别检查。

[原生对端](../tests/support/json_tcp.rs) 拆开发送头、UTF-8 和 token，并合并错误/后续请求；
核对大整数、极端指数和 NUL 的原始输出字节，另验收截断、超长声明和无错误回复的连接结束。
背压场景保持 receive window 不被读取，VM 确认实际无可运行进展且完整发送数介于 0 与 512 之间；
期间树与借用编码字符串保活。重载并销毁 VM 后，旧任务既能在解除背压后完成，也能被取消并释放。
三后端 CLI 核对全部完整帧及错误恢复；VM 核对取消前缀与所有者释放。解释器另以七个时间片预算抽样取消，
检查 JSON 构造/编码/socket 组合的清理与完成结果，不将抽样写成逐指令穷举。

[Godot 对端](../integrations/godot/demo/tests/network_json.gd) 使用独立 JSON 实现生成和解析，
覆盖两个实例、嵌套值与 Unicode、错误后恢复、分片/合包、写背压期间的推送、暂停/缩放下真实截止时间、
重载后的新旧快照、脚本移除/节点退出树/销毁，以及旧连接迟到数据与新实例隔离。
Godot JSON 数字为浮点表示，预期模型也经独立解析再按语义比较；精确大数字与 NUL 交叉验证留给原生字节对端。

响应验收暴露两点实现边界：同步成功的密集发送不一定主动让出，应在有限批次后 `yield_now`；
`write_all`/`write_message` 的源码 `write_chunk` 当前逐字节构造分块，在解释器大消息重复发送时占用很多时间片。
Godot 背压探针一次预组装固定 ASCII 帧，使用 `TcpWriter.write` 累计部分写入，核对 512 条独立 JSON 帧；
原始树和编码字符串跨整个发送会话保活，结束前再编码验证输入保留。普通会话和主仓库三后端仍使用 `write_message`。
这验证真实 socket 背压与读写进展，不以时间断言性能，也不把字节上限当作帧耗时保证。
通用分块复制优化与协作式 codec 不在本阶段引入。

验证：最终 `cargo test json3_` 通过 3 项 VM 与 2 项 CLI 测试，覆盖 JIT、解释器和 AOT。
`RUST_TEST_THREADS=8 ./scripts/check.sh` 通过格式、Clippy、1145 项单元测试、151 项 CLI 测试、
19 项模块常量测试、23 项源码模块测试及三后端短输入 while/Map 基准正确性。
`RUST_TEST_THREADS=2 ./integrations/godot/check.sh` 通过格式、Clippy、9 项 Rust 测试与全部真实引擎场景。
246 处本地文档链接/锚点及暂存 diff 检查通过。未运行 Release、额外压力或逐指令穷举、性能计时。
最终复跑修正了原生测试对端的 macOS 非阻塞继承标志；未修改 codec、TCP 实现或宿主 ABI。
