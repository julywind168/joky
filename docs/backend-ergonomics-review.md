# 后端开发易用性审查：语法、命名与标准库

状态：审查意见与改进建议，2026-11-04。本文不是现行规范，已落地项在对应章节标注，其余目标写法尚未实现；
当前规则以 [language.md](language.md) 为准。上一轮容器与借用易用性工作见
[syntax.md §13](syntax.md#13-标准库与示例易用性优化清单)，本文不重复其已完成项。

## 1. 范围与方法

以"用 Joky 写一个普通的 HTTP + PostgreSQL 后端服务"为基准，审查用户直接接触的部分：
关键词与语法手感、命名一致性、标准库缺口与 API 形状。不评价运行时实现、性能或 ABI。

阅读材料：

- [chat](../examples/chat)：TCP 状态域聊天室（host、admin、script、contract）。
- [postgres_orders.jk](../examples/postgres_orders.jk)、
  [postgres_orders_http.jk](../examples/postgres_orders_http.jk)、[http_server.jk](../examples/http_server.jk)。
- `src/stdlib/` 中 prelude、string、option、result、array、map、list、iter、json、json_typed、
  http、postgres、log、process、fs、io、time、task、session、domain 的公开接口。
- language.md 中 `?`/`!`、闭包、join/race/scope/isolate、HTTP、PostgreSQL、String 章节。

用当前 Debug 构建 `target/debug/joky run` 运行了 7 个临时探针（未提交），确认的行为在下文标注【已验证】：

| 探针 | 结果 |
| --- | --- |
| 经 `&mut A` 给拥有型字段赋值 `r.room = Some("y")` | 已支持，替换后释放旧值 |
| `Result<_, Int32>` 在返回 `Result<_, E>` 的函数里用 `?` | 拒绝：要求错误类型相同 |
| `let x = 3` 与 `UInt64` 相加 | 拒绝：`Int32 and UInt64`；`for i in 0 .. n`（n: UInt64）可以 |
| 同模块期望类型下的裸变体 `let c: Color = Green` | 可以 |
| 跨模块期望类型下的裸变体 `[IntValue(n)]`（`Array<postgres.Value>`） | 拒绝：`Undeclared function IntValue` |
| `println(p)`（未实现 Show 的普通 struct） | 拒绝：不满足 Show；`task.Failure` 已在后续修复中补齐 |
| 未导入时用 `Array<T>` 写类型标注 | 默认 prelude 已支持 |

## 2. 总体判断

做得好、应当保持的：

- 字符串插值 `"{id}"`、原始字符串 `r#"..."#`、后缀 `?`/`!` 让常见代码很短。
- 结构化并发（`scope`/`branch`/`join`/`race`/`isolate`）的语义清楚，停机、排空、取消路径在示例中是一等公民。
- 资源上限、幂等键、`OutcomeUnknown` 这类"服务端真问题"在 API 与示例中被正面处理，这一点比多数新语言成熟。
- `#derive(ToJson, FromJson)` + `typed.decode_bytes::<T>` 已经是可用的请求体解码路径。

主要问题：**语言核心偏向"正确但啰嗦"，标准库偏向"有协议、缺便利层"。**
一个 70 行的 HTTP+PG 示例里，约一半代码在做 SQL 参数装箱、列读取、标头线性查找、
手写 JSON 错误体、状态码映射和停机样板；这些在 Go/Rust(axum+sqlx)/TS 中都是一两行。
优先处理参数装箱、路由和字符串基础方法。跨层传播错误时，现有的 `map_err` 可以显式转换错误类型。

## 3. 优先级总表

P0 = 写任何后端服务都会反复撞到；P1 = 明显降低手感或易错；P2 = 一致性与打磨。

| 级别 | 项目 | 章节 |
| --- | --- | --- |
| 已完成 | 经 `&mut` 替换拥有型字段 | §4.2 |
| P0 | String 基础方法：trim / split_once / contains / strip_prefix / replace / lines / 大小写 / parse | §6.1 |
| 已完成 | 标准库公开错误与无载荷枚举实现 Show；Eq／Hash 统一为借用协议，Eq 接入 `==`／`!=` | §4.4 |
| 已完成 | Eq／Hash 派生；Copy／拥有型 struct 的 `..base` 结构更新 | §4.4 |
| 待设计 | 调试输出派生 | §4.4 |
| P0 | PostgreSQL 参数与结果：`ToSql`/`FromSql` 协议、按列名读取、`#derive(FromRow)`、更多类型 | §8 |
| P0 | HTTP：路由、标头/查询查询、响应构造助手、`Status` 命名 | §7 |
| 已完成 | prelude 补齐 `Array`/`Map`/`Set` 类型名 | §4.3 |
| P2 | 跨模块枚举的期望类型裸变体（暂缓） | §4.3 |
| P1 | 停机与信号样板、`race` 只为等停机而写的辅助函数 | §9 |
| P1 | 迭代与集合常用操作：any/all/find/position/retain/collect/enumerate/fold | §6.2 |
| P1 | Option/Result 补 `ok_or`/`expect`/`map_or`/`filter`/`inspect_err` | §6.3 |
| P1 | 结构化日志字段的泛型入口与级别快捷方法 | §6.5 |
| 已核验 | 保留 Int32 默认与显式跨类型转换；表达式内字面量推导已支持，冗余 range 转换已清理 | §5.4 |
| 已完成 | 字符串字面量模式、match 守卫 | §5.5 |
| P2 | `Result?` 的自动错误转换（推迟至泛型 trait 设计） | §4.1 |
| P2 | 关键词：`when`、`branch`、`var`/`mut` 双拼写、`Unit` 写作 `{}` | §5 |
| P2 | 命名一致性：构造器、缩写、访问器族、`release` | §10 |

## 4. 类型系统与错误处理

### 4.1 `?` 的错误类型转换【已验证】

`Result?` 要求调用结果与当前函数使用相同的错误类型。具体类型的借用读取接口返回 `ColumnError`；
泛型 `row.get` / `row.get_opt` 将列错误包装为 `DbError.Column`，可直接写 `row.get::<Int64>("id")?`。
订单服务通过 `query_one_as::<StoredOrder>(...)?` 传播查询和字段映射错误。[订单示例](../examples/postgres_orders.jk)

通用 `From<T>` 降为 P2。一个应用错误类型可能需要分别接受数据库错误和文件错误，而当前 trait 自身不能带类型参数，同一 trait 与目标类型只能有一个实现。等另一个协议也需要按来源类型区分实现时，再一起设计泛型 trait 的实现查找和重叠规则。[当前 trait 规则](language.md#L2182)

届时建议先限于 `Result`：仅在错误类型不同时查找一次显式转换。设计时还需确定拥有型错误的移动、局部资源的清理顺序，以及转换函数本身分配失败时的行为。

### 4.2 经 `&mut` 替换拥有型字段【已修复】

聊天室的 `enter()` 现在直接写 `account.room = Some(name.clone())`。编译器与显式解引用赋值 `(*account).room = next` 使用同一条写入路径，安装新值后释放旧值。需要取回旧值时使用 `mem.replace`。[聊天室示例](../examples/chat/script/main.jk#L172)

### 4.3 名字可见性

默认 prelude 已导出 `Array`、`Map`、`Set` 三个类型名。聊天室脚本可直接声明 `members: Array<Member>`。内嵌标准模块继续显式声明自己的依赖；用户关闭 prelude 后也需显式导入。[聊天室脚本](../examples/chat/script/main.jk)

跨模块枚举的裸变体查找降为 P2。显式选择性导入仍可让 SQL 参数列表写成 `[IntValue(customer), TextValue(key)]`；订单示例现已迁移为 §8.1 的 `postgres.param(&value)`。表达式中的裸构造器须在作用域内唯一；即使是本地枚举，也不会按期望类型消除歧义。以后若扩展这条规则，需同时确定与同名函数、显式导入和泛型推导的优先关系。[订单示例](../examples/postgres_orders.jk)

### 4.4 派生与 Show【Show 已补齐，Eq 运算符及 Eq／Hash 派生已实现】

Show 是面向用户的显示协议，对应 Rust Display：String 原样输出，普通 struct／enum
仍需手写实现。标准库公开错误与无载荷枚举已补齐 Show，包括 `task.Failure`、HTTP、
TCP／TLS、JSON／typed、编码、状态域与 VM 错误；原有文件 I/O、数值解析、日志和 PostgreSQL
错误的输出保持不变。[聊天室](../examples/chat/main.jk) 已直接使用
`eprintln("connection {id}: {failure}")`，不再手写七种失败类别的映射。

错误显示保留底层原因、字节偏移、JSON Pointer、路径及可用的诊断区间；仅借用错误，
显示后仍可检查字段或再次匹配。格式示例与具体约定见 [Show 参考](language.md#showprint--println-与字符串插值)。

Eq／Hash 已统一为共享借用、noescape 协议；Copy 与拥有型记录使用同一套实现，
BorrowEq／BorrowHash 及其 blanket 适配已删除。浮点 Eq 保持 IEEE 行为且不提供 Hash；
Map／Set 的 key 另要求等价关系、相等值同哈希及驻留期间结果稳定。
实现标准 Eq 的类型已支持 `==`／`!=`，比较借用两侧，`!=` 对 Eq.eq 的结果取反。
调用处不依赖 trait 导入或 prelude；详见 [Eq／Hash 参考](language.md#key-协议)。

下标字段借用已修复：`Array<Key<String>>` 的 `values[0].value == "text"` 可直接使用。
单字段拥有型包装的字段投影复用已有句柄，不再把句柄当作存储地址重复读取；多字段记录仍按实际布局投影。

`#derive(Eq, Hash)` 已支持 struct／enum，生成共享借用实现：Eq 逐字段短路比较，enum
先区分变体再比较载荷；Hash 按字段顺序组合，并混入 enum 变体编号。泛型约束按字段及已有
条件 impl 推导，未使用的类型参数不添加约束；可与 Clone 和 struct JSON 派生组合。
浮点字段可派生 Eq，不能派生 Hash；const 泛型、缺少协议实现的元组及容器仍不支持。

Copy／拥有型 struct 已支持 `Options { field: value, ..base }`，可直接基于现有 `Type.default()` 或业务配置
覆盖字段，不依赖 Default。显式字段先求值，base 随后求值一次；类型及泛型实参必须相同，
所有字段须可见。现有 HTTP／session／JSON／文件／聊天配置均可使用。
拥有型 base 整体消费，保留字段转入结果，被覆盖的旧字段在表达式结束前清理；
外层 Drop 的拥有字段仍不能移出，字段自身的 Drop 可随整体字段转移。

以下事项分开设计，尚未实现：

1. 原建议的类型名、字段名及带引号字符串属于 Debug 风格，不直接混入 Show；
   调试协议及其派生另议。
2. 标准 Default trait 和 Default 派生暂缓。
   普通 `Type.default()` 可手写，业务配置的非零默认值不应被逐字段默认派生替代。

## 5. 关键词与语法手感

### 5.1 `when` 与 `aliased`【分支关键字已调整】

`when "say" (mut account, room?: Room = host.rooms[account.room]) { ... } else { ... } aliased { ... }`
是 Joky 最有特色的构造，但关键词选择不利于理解：

- `when` 在 Kotlin/Elixir/Ruby 中是模式匹配或条件分支，读者会以为"当 account 满足……时"。
  它实际表达的是**具名原子操作 / 事务**。建议候选：`atomic "say" (...)` 或 `transact "say" (...)`，
  更直接地传达"要么全部提交，要么都不提交"。
- `else` 分支在最后一个可选路径缺失时执行；写别名分支已由 `alias` 改为 `aliased`，
  与 `DomainAccessError.Aliased` 对齐。仅处理最后一个绑定与前面绑定之间涉及写入的别名；
  只读别名仍正常执行。旧 `alias { ... }` 会报告迁移诊断。
- 返回值 `Outcome<T, Rejection>` 加 `is_committed()` 的组合是对的；建议 `Outcome` 提供
  `committed_or(default)`，减少 `vacated()` 这类只为折叠 Outcome 的函数。

改关键词代价高，若决定改，应在仍是非公开开发阶段时一次完成，并保留旧词的迁移诊断（同 `parallel` 的做法）。

### 5.2 并发关键词【join／race 分隔符已调整】

- `branch` 与业务名词、Git 术语冲突（"branch"在订单、银行、代码仓库领域都是常见词），
  且不如 `spawn` 直观。`scope { spawn { ... } }` 与 Kotlin/Trio/Swift 的心智模型一致。
  按 [syntax.md §3](syntax.md#3-词法与名称) 它是保留字，占用了一个常用业务标识符；`join`、`isolate` 已是上下文词，`branch`/`scope`/`race` 也可照此处理。
- join／race 已统一使用逗号或换行分隔，例如 `race { stopped(stop), accept(&mut listener) }`。
  旧前导 `|` 分支写法已移除并提供迁移诊断，避免与闭包参数和按位或混淆。
- `race` 要求各分支同型，导致示例里大量出现只为凑类型的函数（见 §9）。

### 5.3 可变性拼写

同一概念出现三种拼写：局部 `var x`、参数 `var listener: TcpListener`、
`when` 访问列表 `mut account`，外加引用 `&mut T`。建议统一为：绑定可变性一律 `var`
（含 `when` 访问列表：`when "say" (var account, ...)`），`mut` 只出现在引用类型 `&mut` 中。

### 5.4 数值类型【已核验并清理冗余转换】

- 无约束整数字面量在绑定完成时默认 Int32；长度、下标和容量使用 UInt64。
  同一表达式中的算术、比较及 range 已支持字面量跟随已定型操作数推导，
  如 `3 + items.len()`、`(1 + 2) + items.len()`、`1 < items.len()` 和 `0 .. width`。
- `let x = 3; x + items.len()` 仍报错：x 已在声明时确定为 Int32，后续使用不反向改变绑定类型。
  此处应写 `let x: UInt64 = 3`，或在需要跨类型使用时显式转换。保留现有局部上下文推导边界，
  不引入隐式整数宽化、窄化或跨绑定推导；规则见 [整数类型与字面量](language.md#整数类型与字面量)。
- 保留 Int32 默认。改为 Int64 仍不能与 UInt64 隐式混用，却会改变既有推导结果和溢出边界。
- 已清理 PostgreSQL、HTTP 和数组排序中 6 处 range 起点转换：上界已有 UInt64 类型，
  起点直接写整数字面量即可。两端均为字面量且需要固定 UInt64 索引的 range，以及对已定型值的转换继续保留。
- 类型名 `Int64`/`UInt64` 冗长但一致，可接受；API 中 `get_i64`、`to_i64`、`from_u64`、`to_f64`
  等缩写留到 §10 统一处理。

### 5.5 模式匹配

- 已支持字符串字面量模式与 `pattern if condition => body` 守卫，覆盖按值及借用匹配、嵌套载荷和序列。
  字符串按 UTF-8 内容精确比较；守卫先以共享视图检查绑定，通过后才移动拥有载荷或取得可变引用。
  带守卫的分支不计入穷尽覆盖，需保留无守卫兜底。
- chat 的 `parse()` 与 admin 命令分派已改为字符串 match；附加条件直接放在分支守卫中。
- `if let` / `while let` 的 `&&` 模式链暂缓，当前可用 match 守卫表达附加条件。

### 5.6 其他

- 已支持 `()` Unit 值，可写 `Ok(())`；`{}` 仍是块表达式，空块产生同一个 Unit 值。
  类型位置仍写 `Unit`；有载荷构造器必须显式传参，不加 `Ok()` 特例。
- 非 pub 函数的 `@suspends` 自动推导暂缓：递归调用、泛型、闭包与效果检查需要单独设计。
  当前所有函数仍显式声明挂起效果。
- language.md 开头已用表格说明 `#` 声明属性与 `@` 效果/借用约束标注的分工。

## 6. 基础标准库缺口

### 6.1 String【P0】

已补齐 `contains`、`split_once`、`strip_prefix` / `strip_suffix`、`trim` / `trim_start` / `trim_end`，
同时提供 String 方法与 string 模块函数。输入均为借用，文本结果为独立拥有值；trim 使用 Unicode White_Space。
chat 的 `parse()` 已改用 split_once 和 contains，保留原命令的空格与参数校验规则。
已补齐 ASCII 大小写转换、无文本分配的 eq_ignore_ascii_case，以及按非重叠匹配替换的 replace。

其余建议及本轮完成状态：

| 方法 | 用途 |
| --- | --- |
| `trim()` / `trim_start()` / `trim_end()` | 已完成；处理输入、标头、配置 |
| `split_once(&sep) -> Option<(String, String)>` | 已完成；协议行、`key=value`、`Bearer xxx` |
| `strip_prefix(&p) -> Option<String>` / `strip_suffix` | 已完成；路由、前缀命令 |
| `contains(&needle) -> Bool` | 已完成；替代 `find(x).is_some()` |
| `replace(&from, &to)` | 已完成；普通文本替换，空 from 在每个 Rune 边界插入 |
| `lines()`、`split_whitespace()` | 已完成；返回独立 Array<String>，分别处理 LF / CRLF 行边界与 Unicode 空白 |
| `to_lowercase()` / `to_uppercase()` | ASCII 版本 `to_ascii_lowercase` / `to_ascii_uppercase` 已完成；完整 Unicode 转换暂缓 |
| `eq_ignore_ascii_case(&other)` | 已完成；标头、方法名比较 |
| `text.parse::<T>()` 或保留 `T.parse(&text)` 并统一 | 目前只能 `UInt16.parse(&text)` |
| `repeat(n)`、`pad_start` | 输出格式化 |
| `runes()` 迭代器 | 已完成；借用原文本、惰性产出 Rune，替代手写 `next_rune` 循环 |

`split` 已直接返回 `Array<String>`，空分隔符按 Rune 边界切成独立字符串，不添加首尾空项；
空文本配空分隔符得到空数组。已删除 SplitError，调用方不再需要 `!`。

### 6.2 集合与迭代【P1】

Array 已补齐 `any`、`all`、`find`、`position`、`first` / `last`，同时提供方法与模块函数。
谓词同步借用元素，按顺序短路；find/first/last 返回 Option<&T>，支持 String 等拥有值。
chat 的 Room.has_member 已使用 any，remove_member 使用 position 加 swap_remove，保留原删除语义。
已修复泛型调用中闭包参数的借用效果映射；find 的返回引用只依赖数组，
捕获的查询值可在查询结束后释放，无需保留到结果最后使用。
Array 已补 `retain` 方法与模块函数：同步借用元素判断，原地保留并保持顺序和容量，
支持拥有型元素，无需 Copy / Clone；淘汰项在压紧后的截断阶段销毁。
Array 已补 `dedup` 方法与模块函数：以 Eq 合并相邻重复项，保留每组第一项；
原地保序、容量不变，支持拥有型元素，重复项在截断阶段销毁。
`Array<String>.join(&separator)` 已复用 string.join，借用输入并返回独立字符串。
迭代器已补 count / fold：拥有型入口 iter.count / iter.fold，共享入口 count_shared / fold_shared，
SharedPipeline 提供同名终结方法；支持拥有型累积值，按顺序处理，到第一个 None 停止。

后续分批推进：

- Array：`extend_from(&slice)`，先明确元素移动、销毁与复制约定。
- 迭代器：已有拥有型 Iterator、共享 SharedIterator 与独占游标组合入口；
  后续补 `enumerate`、`skip`、`chain`，再考虑 sum、collect、flat_map；独占游标终结操作另行推进。
  新操作需明确适用的迭代入口；for 的 `index, item` 已提供直接遍历编号。
- 访问器：get 的 Copy 取值与 get_ref 的共享借用语义先保留，文档优先推荐下标及显式借用。
  访问器还区分复制值、共享借用、独占借用，不能仅按“可能没有 / 一定有”合并。
  `Option<&mut T>` 当前尚不支持，get_mut 与 Element 访问器收敛另行设计。

### 6.3 Option / Result【P1】

已补 `Option.expect`、`Result.expect`、`Option.ok_or` / `ok_or_else`、Option/Result `map_or`、
`filter`、`is_some_and`、`Result.inspect_err` 以及 Option/Result `flatten`；回调均为同步 FnOnce，
借用不会逃出调用。

仍缺少的方法：

- `map_or_else`、`unwrap_or_default`（依赖 Default）。
- Result 已有 `map_err`；仍待 `map_or_else`、`transpose`。

### 6.4 配置与进程

- `process.env(name)` 返回 `Result<Option<String>, IoError>`；已补
  `process.env_or(name, default) -> Result<String, IoError>`，适合
  [stdin_lines.jk](../examples/stdin_lines.jk) 和 [testing_inputs.jk](../examples/testing_inputs.jk)
  这类缺失时直接采用默认值的场景。读取一个可选端口仍需解析和自定义错误消息，见
  [chat/host/config.jk](../examples/chat/host/config.jk)。
- `process.env_parse::<T>(name) -> Result<Option<T>, ConfigError>` 暂缓，等待统一的文本解析协议和
  配置错误类型；当前整数、浮点、Uuid 的解析错误类型不同。
- 参数名类型 `String` 迫使调用方 `name.clone()`；只读输入应接受 `&String`。
- 错误消息访问方式不一致：`IoError.message` 是字段，`vm.LoadError.message()` 是方法。统一为 Show + 必要时的方法。

### 6.5 日志

已补 `log.ToField` 与 `log.field(name, value)`，覆盖八种整数、Bool、String、Uuid；
整数和布尔值保留 JSON 类型。Duration 由调用者显式选择单位，例如
`log.field("duration_ms", duration.as_millis())`。

模块、Logger、RequestContext 均提供返回 Unit 的 `debug/info/warn/error`；
Logger 保留等级过滤，Context 自动附加 request_id。HTTP 请求完成日志和订单示例已迁移。
`emit` 保留 Bool，`try_emit` 保留具体错误和 Logger/Context 的 Written、Filtered 区分。
快捷方法忽略日志错误；共享失败计数的归属和并发规则另行设计。

### 6.6 后端常用但缺失的模块

| 模块 | 用途 | 备注 |
| --- | --- | --- |
| URL / 查询串 / 百分号编码 | 已补 `joky/url` 组件编解码、查询键值对及 `Request.path/query_pairs` | 保留重复键；路径不自动解码，完整 URL 解析后置 |
| 时间格式化 | Timestamp 已有 RFC 3339 子集解析与文本输出，缺日期组件（年月日）、日期运算与自定义格式 | 订单、审计常用 |
| 环境配置 | 见 §6.4 | |
| 十进制数 | 金额（chat 用分 + UInt64 回避了这个问题） | 可先与 PG numeric 一起做 |
| 正则或简单模式匹配 | 输入校验 | 可后置，先补 String 方法 |

## 7. HTTP【P0】

参照 [postgres_orders_http.jk](../examples/postgres_orders_http.jk) 与 [http_server.jk](../examples/http_server.jk)：

| 问题 | 现状 | 建议 |
| --- | --- | --- |
| 没有路由 | 已补精确路径 `http.Router`，HTTP/HTTPS 示例已迁移，自动提供 404 和 405/Allow；完成日志统一记录 method/path | 后续补路径参数；当前 `:id` 按字面匹配 |
| 方法是字符串 | 已统一为服务端/客户端共用的 `http.Method`，派生 Eq，补 Options | 挂起函数中可直接比较 request.method；OPTIONS 的业务策略由 handler 决定 |
| target 含查询串 | 已补 `request.path()` 和 `request.query_pairs()`，HTTP 示例已迁移 | 查询保留重复键和错误，由调用者选择取值策略 |
| 标头查找 | 已补 `header` 首值借用、`header_all` 全部值副本，ASCII 忽略大小写 | 订单示例保留重复 Idempotency-Key 拒绝 |
| 响应构造冗长 | 已补 `http.text`、`http.empty`、`http.json<T: ToJson>`、`Response.with_header` | JSON 保留编码错误；标头追加，发送时校验 |
| JSON 错误体手写 | 订单示例已用 `http.json` 与派生 ToJson 的错误/订单结构体 | 编码失败显式返回空 500 响应 |
| 状态码命名 | 已改为 `Status.Ok`、`InternalServerError`，补齐常用成功、重定向及错误码 | `from_code` 与原因短语同步；其他 200–599 保留 Other |
| 请求体解码 | 已补 `request.json<T: FromJson>()`，订单示例已迁移 | 共享借用正文，结果独立拥有，可重复解码；解码错误原样返回 |
| 函数 handler 不能带状态 | 已提供 `with_context(source)` 注入资源句柄，`with_state(handle, source)` 注入域句柄和关联请求上下文 | 订单服务使用池，内存库存服务使用域；见 §7.1 |
| 连接模型 | 每连接一次请求、`Connection: close`、无 chunked | 记为已知限制即可，keep-alive 在压测和反向代理后会成为瓶颈，排入后续计划 |
| 中间件 | 无 | 至少提供请求前后钩子：鉴权、请求日志已内置、CORS |

服务启动样板见 §9。

### 7.1 HTTP 应用状态与关联请求上下文设计

2026-11-06：已实现 `Request<C>`、`with_context` 和 `with_state`。Parallel 主程序可在 handler 中
直接使用静态单域或多域 `when`。本节给出接口节选，完整用例见[共享库存 HTTP 服务](../tests/fixtures/http_state.jk)。

应用有三种不同的状态来源：

| 组成 | 内容与所有权 | 访问规则 |
| --- | --- | --- |
| `#state App` | 跨请求的业务数据，如内存库存、缓存与计数；服务作用域持有唯一域 Owner | handler 取得 `domain.WriteHandle<App>`；通过 `when (app)` 读取、`when (mut app)` 修改 |
| `App.RequestContext` | 应用定义的请求上下文，持有数据库池等资源句柄；每次请求独立拥有包装值 | 普通代码直接使用 `request.ctx.db`；共享的是底层池，包装值和请求局部字段独立 |
| `request.log` / `request.request_id` | 框架提供已绑定请求 ID 的日志对象及 UUID | 直接访问请求字段；HTTP 层在解析和错误响应中也能使用，不依赖应用上下文创建成功 |

数据库池不能放进 `#state App`：域根要求无资源、无用户清理 hook 的受检拥有图，
候选复制和回滚不能复制或撤销一个外部数据库连接池。pool 留在请求上下文中，SQL 在 when 外执行。
when 提交与数据库事务不是同一事务；需要数据库一致性的订单数据继续交给 PostgreSQL 事务。

**关联类型与 handler。** 已实现的 `ContextSource.Context` 由资源来源决定。两种类型可以不同：
来源只保存跨请求资源，关联类型还可以包含每次请求重新初始化的字段。例如连接分享数据库池句柄，
请求命中后才生成 `{ db, current_user: None }`。具体签名见[语言参考](language.md#http-服务端-jokyhttp)。

每种 App 通过 `Application.RequestContext` 选择一种请求上下文类型。不同资源来源可以生成同一种上下文；
工厂检查来源的关联类型，注册路由时再检查 handler 的参数类型。
下例使用内存库存，订单示例的库存仍在 PostgreSQL 中。

```text
pub trait Application {
    type RequestContext
}
#state
struct App { available: UInt64 }
fn new_app(_: Unit) -> App { App { available: 1 } }
impl http.Application for App {
    type RequestContext = Unit
}

fn reserve(app: domain.WriteHandle<App>, request: http.Request<Unit>) @suspends -> http.Response {
    let reserved = when (mut app) {
        if app.available == 0 { false } else {
            app.available -= 1
            true
        }
    }
    request.log.info("inventory.reserve", [])
    match reserved {
        Ok(true) => http.empty(http.Status.Ok),
        Ok(false) => http.empty(http.Status.Conflict),
        Err(_) => http.empty(http.Status.ServiceUnavailable)
    }
}
```

`app` 是权限句柄，不是 App 的副本，也不是跨整个 handler 存活的 `&mut App`。
取得句柄不会锁住状态；只有 when 操作授权后才获得相应访问权，操作提交后释放，再继续 I/O。
闭包式隐式捕获不增加能力：仍按现有 when 捕获契约传入数据，不能把带 pool 的整个 request/ctx
捕获到事务内。应先在 handler 中解码请求，提取 qty、id 等数据，再在 when 中使用。

**请求类型。** 通用记录使用 `Request<C>`，应用上下文字段统一命名为 `ctx: C`。
日志对象和请求 ID 作为 Request 的直接字段，分别通过 `request.log`、`request.request_id` 访问。
`request.log.info/warn/error` 等方法自动附加框架绑定的 request_id，业务无需重复传入；
`request.request_id` 用于读取同一个 UUID，响应仍自动发送 X-Request-Id。
method、target、headers、body 与现有辅助方法保持职责。
状态路由用关联类型建立 `App -> AppContext` 的对应关系，注册时据此检查 handler 参数类型。
使用 `Request<AppContext>`，不要求记录声明直接带 `A: Application` 约束或投影字段。
业务模块可写 `type Request = http.Request<AppContext>` 缩短签名；同一个无泛型 `http.Request`
不能依据另一个参数自动改变 ctx 的类型，不引入隐式类型映射或动态类型查询。

无状态 `Router.new()` 和普通函数服务使用 `Request<Unit>`，Unit 提供空上下文的分享实现；
已有示例相应更新显式类型。当前语言没有泛型类型默认实参，本方案不将其作为前置条件。
只有资源上下文的服务直接使用 `http.with_context(source)`，
注册 `fn(Request<C>) @suspends -> Response`，共享请求实现和路由匹配规则。

**构造与生命周期。** 工厂从句柄和来源推断 A、S，返回 `StateRouter<A, S, A.RequestContext>`。
来源 S 为每次命中的请求构造上下文，因此可以与上下文使用不同类型。StateRouter 与 ContextRouter 共用路由表。

```text
let owner = domain.create(new_app, {})
var router = http.with_state(owner.handle(), {})
router.post("/reserve", reserve)!
http.serve_with(listener, &stop, options, router)!
owner.close()
```

Owner 留在服务作用域；路由器和请求只持有弱访问句柄。正常停机先排空连接，再关闭 Owner；
取消时沿结构化清理顺序先停止子任务，再释放 Owner。主动提前关闭 Owner 后，旧句柄返回现有
Closed 错误，不悄悄创建新 App，也不保证请求一定成功。

保留低层 `Service.share/handle` 协议，协议层接收 `Request<Unit>`，类型化路由适配器在分发前
构造 `Request<C>`。Service.share 仍保证每个连接有独立拥有的资源包装，应用上下文分享和
必要的池别名均是轻量句柄操作，不重新打开池。完整请求读取成功并匹配 handler 后才创建该请求的
应用上下文；解析失败、自动 404/405 不创建请求级上下文，也不调用 handler 或状态操作。
接纳连接时调用来源的 share，只分享长期资源；命中 handler 后才调用来源的 for_request 构造请求值。
上下文分享失败和取消沿已有任务清理通路处理，不允许部分初始化值逃逸。

request_id、method、path 和计时起点由 HTTP 层在调用 handler 前固定；
完成日志使用该快照及响应写入结束时的时间。
request.log 的绑定和完成日志使用同一个原始 request_id；handler 修改自己的 request 或 ctx
不得改变框架绑定的日志身份。

**并行边界。** 主程序已接通直接构造、Owner 关闭和静态访问集 `when`。JIT、解释器及独立 AOT
均复用根任务树的控制代理和 R8 授权器。handler 在请求任务中执行，只在 `when` 处提交受检捕获包。
数据库池与完整请求由调用任务持有。见[库存竞争用例](../tests/fixtures/parallel_single_domain.jk)和[转账用例](../tests/fixtures/parallel_multi_domain.jk)。

动态目录路径与自动周期方法分别验收；直接入口在支持前继续拒绝。

**实施顺序与验收。**

1. 已交付 `with_context` 并迁移订单服务。复用路由 fixture 检查请求局部字段独立、自动 404/405 跳过初始化、隐式 HEAD 正常分发。真实 PostgreSQL 用例验证并发下单和丢失响应后的幂等重试；独立连接观察池会话，确认最后一个路由句柄释放后会话结束。
2. 已接通直接并行静态访问集。同步探针让两个任务争抢同一域的最后一件库存，断言恰有一个成功且最终库存为零。多域转账覆盖反向竞争、不相交的访问集同时计算、业务拒绝和提交前取消。
3. 已交付 `with_state`。两个已准入的 HTTP 请求在停机信号后竞争最后一件库存，分别返回 201 和 409。排空后服务作用域读取最终库存，再关闭 Owner。

**2026-11-05 设计探针。** 临时探针已执行 JIT、解释器、独立 AOT：关联类型选择上下文，泛型请求
持有 `Option<postgres.Pool>` 与 String，适配现有 Service.share/handle，
通过函数指针进入具名 handler，跨挂起并用 when 更新 App。
探针的 pool 值为 None，只验证资源类型存储与类型组合，未声称验证数据库访问或 pool 生命周期。
探针为 Context.share 和内部借用分发方法明确标注 noescape，避免借用包装跨挂起被保守拒绝；
外部 Service.share/handle 沿用现有签名。
另用 check 确认带 Pool 的 #state 被拒绝、记录/别名声明的普通 trait 约束尚不支持；
explain-parallel 确认直接域操作仍为 Local。仅核验现有能力，没有修改编译器、运行时或 HTTP API。

## 8. PostgreSQL【P0】

### 8.1 参数绑定【ToSql 与显式转换已实现】

已提供 `postgres.ToSql` 与 `postgres.param(&value)`，订单示例使用：

```joky
[postgres.param(&customer), postgres.param(&key), postgres.param(&qty)]
```

内置支持 Bool、Int16/32/64、String、Array<UInt8>、Option<T: ToSql>。
整数统一绑定为 bigint；Option 的 None 根据 T 的 `kind()` 生成带类型的 SQL NULL。
文本和字节从借用生成独立拥有的参数副本，不保留输入借用，可跨挂起使用；
这里省去调用方显式 clone，没有消除实际复制。业务类型可实现 ToSql 复用现有四种 SQL 类型。

参数容器仍为 `Array<postgres.Value>`，原有显式变体构造继续可用。
尚未实现异构数组 `[customer, key, qty]` 的隐式转换；不为此引入第二套 Param 容器。
Float64、Uuid、Timestamp 等类型按 §8.3 补齐。Value 变体命名整理仍待处理。

### 8.2 结果读取【泛型读取、FromRow 与标量查询已实现】

Row 持有列名和值，四种现有 SQL 类型均可按名称读取。已提供 `row.get::<T>(name)`、
`row.get_opt::<T>(name)` 和 FromSql 协议，内置支持 Int64、Bool、String、Array<UInt8>、Option<T>。
String 与 bytea 解码为独立拥有值；借用读取使用 `get_text` / `get_bytes`。
具体泛型类型的 trait 实现已支持，FromRow 可映射 Array<UInt8> 与 Option<Array<UInt8>> 字段。

`#derive(FromRow)` 按 struct 字段名映射，支持拥有字段、Option 字段和按字段推导的泛型约束。
Pool 与 Transaction 提供 `query_one_as::<T>`；订单示例通过 StoredOrder 读取调换了列顺序的
`SELECT qty, id`。列号、重名、缺列及 NULL 的规则见[语言参考](language.md#postgresql-数据库-jokypostgres)。

2026-11-06 排查确认，原 HTTP 订单程序已占用 254 个枚举实例，其中 121 个含刚性参数。
泛型读取增加普通 Option/Result 组合后触及总量预算；现将每阶段枚举实例上限调整为与记录一致的
512，并验证满容量缓存命中、超限拒绝与失败后缓存可用。

Pool 与 Transaction 已提供 `query_scalar::<T>`，要求恰好一行一列，复用 FromSql 解码。
Option<T> 允许 SQL NULL，无行仍报 NoRows；列数不符返回带实际数量的 ColumnCount(n)。

剩余命名整理：`get_i64` 与语言类型名 Int64 不一致，见 §10。

### 8.3 类型覆盖

当前只支持 bool、int2/4/8、text 族、bytea。一个普通订单/用户服务至少还需要：
`float8`、`numeric`（金额）、`timestamptz`（映射 `time.Timestamp`）、`uuid`（映射 `uuid.Uuid`）、
`jsonb`（映射 `json.Value` 或 FromJson 类型）、`text[]`/`int8[]`。
建议按 timestamptz → uuid → jsonb → numeric → 数组 的顺序补，前三项都有现成的 Joky 类型可映射。

### 8.4 配置与生命周期

- 连接串只能来自进程环境变量 `JOKY_POSTGRES_URL`，一个进程不能连两个库，也不能从配置文件读取。
  理解这是安全边界的选择（DSN 不进入 Joky 值）；建议增加宿主命名配置
  `postgres.open_named("orders", options)`，由宿主登记多个 DSN，Joky 只看到名字。
- `open(max_connections)` 只接受一个位置参数；建议 `postgres.Options { max_connections, acquire_timeout, ... }`，
  与 `http.Options` 保持同一套构造约定（§10）。
- 事务句柄被丢弃（未 commit/rollback）时的行为需要在 API 上可见：文档已说明弃用连接，
  建议在 language.md 表格中明确写"丢弃即回滚"，并提供 `pool.transaction(|tx| { ... })` 形式，
  闭包返回 Ok 提交、Err 回滚，消除"提前 return 忘记 rollback"的类别错误
  （示例中两处 `tx.rollback()?; return Ok(...)` 就是这种模式）。

## 9. 服务启动与并发样板【P1】

### 9.1 停机与信号

两个 HTTP 示例与 chat 都重复：

```joky
let stop = shutdown.create()
// Keep signal handling active until all admitted requests finish draining.
let signals = stop.subscribe_unix()!
```

`signals` 必须保持绑定直到排空结束，但它看起来是未使用变量；若用户写成 `stop.subscribe_unix()!;`
或改名为 `_`，信号处理会被提前释放。这是一个**静默失效的陷阱**。

建议：

- `shutdown.with_signals() -> Result<Controller, ...>`，控制器自身拥有订阅。
- 一行启动：`http.run("127.0.0.1:8080", router)` 内部完成监听、信号、宽限期与退出码；
  现有 `serve_with(listener, &stop, options, service)` 保留为可组合的底层入口。
- 若保留当前形式，至少对 `subscribe_unix` 返回值加"必须保持存活"的未使用诊断。

### 9.2 只为等待停机而写的 race 辅助函数

[chat/main.jk:59](../examples/chat/main.jk) 与 [admin.jk:34](../examples/chat/host/admin.jk) 为了
"accept 或停机"各写了 2–3 个同型包装函数（`stopped`、`accept`、`stopped_accept`），
admin 还要把 `Result<TcpStream, _>` 改写成 `Result<Option<TcpStream>, _>` 以凑类型。

建议：

- `stop.until(|| { listener.accept() }) -> Option<T>`：停机时返回 None，与 `task.timeout` 形状一致。
- 或让 `race` 支持带绑定的分支：
  `race { s = listener.accept() => Some(s), _ = stop.wait() => None }`，分支各自产生结果，免去同型约束。
- `TcpListener.accept_until(&stop)` 作为最常见组合的直接方法。

### 9.3 session 与连接框架

`session.run_draining(connection, stream, &script, &client, line, closed, &stop)` 有 7 个位置参数，
两个函数指针的签名 `fn(&S, &T, String) @suspends @noescape -> Bool` 用户很难一次写对。
建议与 `http.Service` 对齐，定义 `session.Handler` trait（`on_line`、`on_close`），
`session.serve(listener, &stop, options, handler)` 负责准入、隔离与失败报告；
chat 主文件中的 `serve` 循环（准入、计数 ID、复制脚本、isolate、报告失败）基本都可下沉。

### 9.4 域目录类型

`domain.DirectoryHandle` 不带类型参数，导致每次访问写 `domain.lookup_write::<Account>(host.accounts, id)`，
类型错配只能在运行时发现。建议 `DirectoryHandle<T>`，访问写 `host.accounts.lookup_write(id)`。

## 10. 命名一致性

| 维度 | 现状 | 建议 |
| --- | --- | --- |
| 构造 | `Vm.new()`、`StringBuilder.new()`、`shutdown.create()`、`session.create(opts)`、`admission.create(n, 0)`、`domain.directory(n)`、`postgres.open(n)` | 值对象 `Type.new(...)`；I/O 资源用 `open`/`connect`/`listen`；模块级 `create` 改为 `Type.new` |
| 默认配置 | `http.Options.default()`、`session.Options.default()`、`json.Limits.default()`、`typed.Options.default()`、chat 的 `Config.default()` | Copy／拥有型记录已可用 `..base` 结构更新；已统一 `Type.default()` 普通固有方法；Default trait／派生暂缓 |
| 数值缩写 | `get_i64`、`to_i64`、`from_u64`、`to_f64` 与类型名 `Int64`/`UInt64`/`Float64` 并存 | 跟随类型名：`to_int64()`，或用泛型 `to::<Int64>()`；列读取用 `row.get::<Int64>` |
| 长度 | 容器 `len()`，String `byte_length()`，StringBuilder `byte_length()` | 保留 `byte_length` 明确单位是对的；补 `len()` 作为同义词会更符合直觉，二选一需写入约定 |
| 访问器 | `get`/`get_ref`/`borrow`/`borrow_ref`/`borrow_mut`/`borrow_mut_ref` | 见 §6.2 |
| 释放 | `release()` 提前释放 | 可接受；若引入 `Drop` 用户实现，考虑统一叫 `drop(x)` |
| 完成构建 | `StringBuilder.finish()` | 业界常见 `to_string()`/`build()`，`finish` 可接受，但应与 http 内部 `Wire.finish()`（关闭连接）区分语义 |
| 错误枚举变体 | `DbError.Invalid`、`Limit`、`Closed` 很笼统 | 用具体名词：`InvalidRequest`、`LimitExceeded`、`TransactionClosed` |
| HTTP 状态 | `Success` | `Ok`（见 §7） |
| 参数传递 | 大量只读参数类型为 `String`（`process.env(name: String)`、`tcp.listen(host: String, ...)`、SQL 文本） | 只读输入接受 `&String`；字面量自动借用已可用，调用方少写 `clone()` |

建议把上表的结论写入 CONTRIBUTING.md 的 API 命名约定，新增模块按此执行，旧接口在一次集中迁移中改名。

## 11. 目标写法对照

以下为采纳 §4、§7、§8 建议后的 `place()` 与 HTTP handler 草图，仅用于说明方向：

```text
pub fn place(pool: &pg.Pool, customer: Int64, key: &String, qty: Int64) @suspends -> Result<Order, AppError> {
    if qty <= 0 || key.is_empty() { return Err(AppError.Invalid) }
    pool.transaction(|tx| {
        let inserted = tx.execute("INSERT ... ON CONFLICT (customer, request_id) DO NOTHING", [customer, key, qty])?
        let row = tx.query_one("SELECT id, qty FROM joky_demo_orders WHERE customer=$1 AND request_id=$2", [customer, key])?
        if row.get::<Int64>("qty")? != qty { return Ok(Order.Conflict) }
        if inserted == 1 && tx.execute("UPDATE ... WHERE sku=1 AND available >= $1", [qty])? == 0 {
            return Ok(Order.Insufficient)
        }
        Ok(Order.Confirmed(row.get::<Int64>("id")?))
    })
}

fn create_order(request: http.Request<Orders>) @suspends -> http.Response {
    var keys = request.header_all("Idempotency-Key")
    if keys.len() != 1 { return error_reply(Status.BadRequest, "expected one Idempotency-Key") }
    let key = keys.take_last()
    if key.is_empty() || key.byte_length() > 128 { return error_reply(Status.BadRequest, "invalid Idempotency-Key") }
    let input = match request.json::<Purchase>() { Ok(i) => i, Err(_) => return http.bad_request("expected qty") }
    match orders.place(&request.ctx.pool, 1, &key, input.qty) {
        Ok(Confirmed(id)) => http.json(Status.Ok, &OrderCreated { order_id: id }),
        ...
    }
}

fn main() @suspends -> Int32 {
    let pool = pg.Pool.open(pg.Options { max_connections: 4, ..pg.Options.default() })!
    var router = http.with_context(Orders { pool })
    router.post("/orders", create_order)!
    http.run("127.0.0.1:0", router)
}
```

注：`transaction` 闭包内 `return` 返回闭包本身，与现有闭包规则一致。

## 12. 建议实施顺序

1. **低成本先行**（不改语义）：标准库错误与无载荷枚举实现 Show（已完成）；String 基础方法；Option/Result 补方法；
   Array any/all/find/retain；
   清理标准库内过时写法（`uint64(0) ..`）。
2. **HTTP 便利层**：header/query/path 访问、响应助手、Status 改名、Method 枚举、Router、`http.run`、
   `shutdown.with_signals`。均为标准库源码改动，不涉及编译器。
3. **PostgreSQL 便利层**：Value 变体改名、按列名读取、`get::<T>`、timestamptz/uuid/jsonb、
   `pool.transaction`、Options。
4. **语言特性**：调试协议及派生、Default 派生、字符串字面量模式与守卫（已完成）、
   `ToSql` 隐式装箱、非 pub 函数 `@suspends` 推导。
   每项按 AGENTS.md 的跨模块语义改动要求单独设计与验证。
5. **关键词与命名迁移**（`when`→`atomic`、`branch`→`spawn`、`var`/`mut` 统一、
   构造与缩写命名）：集中一次完成并提供迁移诊断，趁项目尚未公开时做。

每项落地时同步更新 language.md 与相关示例，示例应作为"目标写法"的验收标准：
postgres_orders_http.jk 在完成 1–3 后应缩短到约当前一半，且不再出现手写 JSON 字符串与标头循环。
