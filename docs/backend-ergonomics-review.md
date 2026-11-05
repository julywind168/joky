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
| P1 | 整数默认类型与长度/下标 UInt64 的摩擦 | §5.4 |
| P1 | 字符串字面量模式、match 守卫 | §5.5 |
| P2 | `Result?` 的自动错误转换（推迟至泛型 trait 设计） | §4.1 |
| P2 | 关键词：`when`/`alias`、`branch`、`var`/`mut` 双拼写、`Unit` 写作 `{}` | §5 |
| P2 | 命名一致性：构造器、缩写、访问器族、`release` | §10 |

## 4. 类型系统与错误处理

### 4.1 `?` 的错误类型转换【已验证】

`Result?` 要求调用结果与当前函数使用相同的错误类型。订单示例的 `number()` 先用 `map_err` 把 `ColumnError` 包进 `DbError.Column`，再用 `?` 提前返回错误。读列成功后得到 `Option<Int64>`，由 `number()` 将 SQL `NULL` 解释为该列的 `UnexpectedNull(i)`。错误转换使用无捕获闭包，可在 HTTP 服务的 worker 内直接调用。[订单示例](../examples/postgres_orders.jk#L10)

通用 `From<T>` 降为 P2。一个应用错误类型可能需要分别接受数据库错误和文件错误，而当前 trait 自身不能带类型参数，同一 trait 与目标类型只能有一个实现。等另一个协议也需要按来源类型区分实现时，再一起设计泛型 trait 的实现查找和重叠规则。[当前 trait 规则](language.md#L2182)

届时建议先限于 `Result`：仅在错误类型不同时查找一次显式转换。设计时还需确定拥有型错误的移动、局部资源的清理顺序，以及转换函数本身分配失败时的行为。

### 4.2 经 `&mut` 替换拥有型字段【已修复】

聊天室的 `enter()` 现在直接写 `account.room = Some(name.clone())`。编译器与显式解引用赋值 `(*account).room = next` 使用同一条写入路径，安装新值后释放旧值。需要取回旧值时使用 `mem.replace`。[聊天室示例](../examples/chat/script/main.jk#L172)

### 4.3 名字可见性

默认 prelude 已导出 `Array`、`Map`、`Set` 三个类型名。聊天室脚本可直接声明 `members: Array<Member>`。内嵌标准模块继续显式声明自己的依赖；用户关闭 prelude 后也需显式导入。[聊天室脚本](../examples/chat/script/main.jk)

跨模块枚举的裸变体查找降为 P2。订单示例用 `import joky/postgres.{IntValue, TextValue}` 声明所需变体，SQL 参数列表即可写成 `[IntValue(customer), TextValue(key)]`。表达式中的裸构造器须在作用域内唯一；即使是本地枚举，也不会按期望类型消除歧义。以后若扩展这条规则，需同时确定与同名函数、显式导入和泛型推导的优先关系。[订单示例](../examples/postgres_orders.jk)

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

Copy／拥有型 struct 已支持 `Options { field: value, ..base }`，可直接基于现有 defaults/new 或业务配置
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

### 5.1 `when` 与 `alias`

`when "say" (mut account, room?: Room = host.rooms[account.room]) { ... } else { ... } alias { ... }`
是 Joky 最有特色的构造，但关键词选择不利于理解：

- `when` 在 Kotlin/Elixir/Ruby 中是模式匹配或条件分支，读者会以为"当 account 满足……时"。
  它实际表达的是**具名原子操作 / 事务**。建议候选：`atomic "say" (...)` 或 `transact "say" (...)`，
  更直接地传达"要么全部提交，要么都不提交"。
- `else` 分支在可选路径缺失时执行，含义尚可推断；`alias` 分支（两个路径解析到同一实例）
  读不出意思。建议改为 `same { ... }` 或 `aliased { ... }`，并在诊断中说明触发条件。
- 返回值 `Outcome<T, Rejection>` 加 `is_committed()` 的组合是对的；建议 `Outcome` 提供
  `committed_or(default)`，减少 `vacated()` 这类只为折叠 Outcome 的函数。

改关键词代价高，若决定改，应在仍是非公开开发阶段时一次完成，并保留旧词的迁移诊断（同 `parallel` 的做法）。

### 5.2 并发关键词

- `branch` 与业务名词、Git 术语冲突（"branch"在订单、银行、代码仓库领域都是常见词），
  且不如 `spawn` 直观。`scope { spawn { ... } }` 与 Kotlin/Trio/Swift 的心智模型一致。
  按 [syntax.md §3](syntax.md#3-词法与名称) 它是保留字，占用了一个常用业务标识符；`join`、`isolate` 已是上下文词，`branch`/`scope`/`race` 也可照此处理。
- `join { | a | b }`、`race { | a | b }` 的前导竖线与闭包 `|x|` 视觉冲突，
  `race { | stopped(stop) | accept(&mut listener) }` 第一眼像闭包参数列表。
  建议接受逗号或换行分隔的形式：`race { stopped(stop), accept(&mut listener) }`，前导 `|` 作为兼容写法保留。
- `race` 要求各分支同型，导致示例里大量出现只为凑类型的函数（见 §9）。

### 5.3 可变性拼写

同一概念出现三种拼写：局部 `var x`、参数 `var listener: TcpListener`、
`when` 访问列表 `mut account`，外加引用 `&mut T`。建议统一为：绑定可变性一律 `var`
（含 `when` 访问列表：`when "say" (var account, ...)`），`mut` 只出现在引用类型 `&mut` 中。

### 5.4 数值类型

- 无约束整数字面量默认 Int32，而 `len()`、下标、`byte_length()`、容量全部是 UInt64【已验证】。
  `let x = 3; x + items.len()` 报错，用户被迫在业务代码里写 `uint64(...)`。
  建议之一：无约束字面量在同一表达式内跟随另一操作数定型（已对 range 生效，可推广到二元运算与比较）；
  或者把默认整数改为 Int64。后者是全局语义改动，需单独评估。
- 标准库自身仍有过时写法，如 `postgres.jk` 的 `for i in uint64(0) .. width`，
  range 已能推导字面量，应顺手清理以免成为用户模仿的样板。
- 类型名 `Int64`/`UInt64` 冗长但一致，可接受；但 API 里混用 Rust 风格缩写
  （`get_i64`、`to_i64`、`from_u64`、`to_f64`），见 §10。

### 5.5 模式匹配

- 不支持字符串字面量模式与 match 守卫（language.md「枚举与模式匹配」「条件模式与模式循环」）。
  后端代码大量按字符串分派：HTTP 方法、命令动词、标头值、配置项。chat 的 `parse()` 与 admin 的命令
  分派只能写成 `if verb == "join" && ... else if line == "balance"` 链。建议优先支持
  `match verb { "join" => ..., "say" => ..., _ => ... }` 与 `Some(k) if k.byte_length() <= 128 => ...`。
- `if let` 不支持 `&&` 链；可与守卫一同设计。

### 5.6 其他

- `Unit` 字面量是 `{}`，于是到处出现 `Ok({})`，读起来像返回空对象。syntax.md 曾决定第一版不加 `()` 别名，
  从后端代码的出现频率看值得重新评估：至少允许 `Ok(())`，或为 `Result<Unit, E>` 提供 `Ok()` 零参构造。
- `@suspends` 有传染性：任何间接做 I/O 的函数都要标注，等同于 async 着色。
  建议对**非 pub 函数**推导 `@suspends`，只要求公开签名、函数类型和 trait 方法显式标注；
  诊断已能定位挂起调用，推导不会降低可读性。
- 声明属性用 `#`（`#derive`、`#state`、`#every`），效果标注用 `@`（`@suspends`、`@noescape`），
  两个符号可以接受，但应在 language.md 开头用一张表说明分工，避免用户猜测"为什么不是 `@derive`"。

## 6. 基础标准库缺口

### 6.1 String【P0】

现有：find、starts_with、ends_with、substring、split、join、concat、rune_count、next_rune。
[chat 的 parse()](../examples/chat/script/main.jk) 用 `find` + `substring(...)!` + 两次 `find(...).is_none()`
才完成"按第一个空格切成动词和参数、参数不含空白"。

建议补齐（均为借用输入、返回新 String 或借用切片）：

| 方法 | 用途 |
| --- | --- |
| `trim()` / `trim_start()` / `trim_end()` | 处理输入、标头、配置 |
| `split_once(&sep) -> Option<(String, String)>` | 协议行、`key=value`、`Bearer xxx` |
| `strip_prefix(&p) -> Option<String>` / `strip_suffix` | 路由、前缀命令 |
| `contains(&needle) -> Bool` | 替代 `find(x).is_some()` |
| `replace(&from, &to)` | 模板、转义 |
| `lines()`、`split_whitespace()` | 文本协议、CSV 粗处理 |
| `to_lowercase()` / `to_uppercase()`（ASCII 版本 `to_ascii_lowercase` 先行） | 标头名、大小写无关比较 |
| `eq_ignore_ascii_case(&other)` | 标头、方法名 |
| `text.parse::<T>()` 或保留 `T.parse(&text)` 并统一 | 目前只能 `UInt16.parse(&text)` |
| `repeat(n)`、`pad_start` | 输出格式化 |
| `runes()` 迭代器 | 替代手写 `next_rune` 循环 |

`split` 返回 `Result<Array<String>, SplitError>`（仅因空分隔符）导致每次调用都要 `!`；
建议空分隔符改为按 Rune 切分或 panic，返回值直接为 `Array<String>`。

### 6.2 集合与迭代【P1】

`iter` 模块只有 take/zip/map/filter。chat 的 `Room.has_member` 和 `remove_member`
（[script/main.jk:192](../examples/chat/script/main.jk)）都是手写循环。建议：

- Array 方法：`any`、`all`、`find`、`position`、`retain`、`dedup`、`first`/`last`、`extend_from(&slice)`、`join`（对 String 元素）。
- 迭代器：`enumerate`（for 已有 `index, item` 语法，组合器链中仍需要）、`fold`、`sum`、`count`、
  `collect::<Array<_>>()`、`skip`、`chain`、`flat_map`。
- 访问器族收敛：Array/Map/List 同时存在 `get`（Copy）、`get_ref`（Option<&T>）、
  `borrow`（&Element）、`borrow_ref`（&T，未命中失败）、`borrow_mut`、`borrow_mut_ref`。
  六个名字对用户来说只有两件事："可能没有"和"一定有"。建议文档推荐面只保留
  `get`（Option<&T> 或 Copy 值）、`get_mut` 与下标 `xs[i]`（未命中失败），其余降为内部或兼容入口。

### 6.3 Option / Result【P1】

现有方法齐全度约为 Rust 的一半。后端代码中最常用而缺失的：

- `Option.ok_or(err)` / `ok_or_else`：把"找不到"转成业务错误，配合 `?`。
- `expect("message")`：`!` 失败时没有上下文，日志里只看到"unwrap None"。建议 `expect` 或 `!` 支持附带消息。
- `map_or` / `map_or_else`、`filter`、`is_some_and`、`unwrap_or_default`（依赖 Default）。
- Result 已有 `map_err`，补 `inspect_err`、`transpose`、`flatten`。

### 6.4 配置与进程

- `process.env(name)` 返回 `Result<Option<String>, IoError>`，读取一个可选端口要 12 行
  （[chat/host/config.jk](../examples/chat/host/config.jk)）。建议补
  `process.env_or(name, default)` 与 `process.env_parse::<T>(name) -> Result<Option<T>, ConfigError>`。
- 参数名类型 `String` 迫使调用方 `name.clone()`；只读输入应接受 `&String`。
- 错误消息访问方式不一致：`IoError.message` 是字段，`vm.LoadError.message()` 是方法。统一为 Show + 必要时的方法。

### 6.5 日志

现状：`context.emit(log.Level.Warn, "order.commit_unknown", [log.text("route", "/orders")]);`
需要为每种值选一个构造函数（`log.text`/`uint`/`integer`/`boolean`），返回的 Bool 还要用 `;` 丢弃。

建议：

- `log.ToField` 协议 + 泛型 `log.field(name, value)`，常见类型（整数、Bool、String、Uuid、Duration）实现。
- 级别快捷方法：`context.warn("order.commit_unknown", [...])`、`log.info(...)`。
- `emit` 默认不返回值（失败计数由 logger 维护），需要结果时用 `try_emit`。

### 6.6 后端常用但缺失的模块

| 模块 | 用途 | 备注 |
| --- | --- | --- |
| URL / 查询串 / 百分号编码 | 解析 `request.target` 中的路径与查询 | HTTP 路由的前提 |
| 时间格式化 | Timestamp 已有 RFC 3339 子集解析与文本输出，缺日期组件（年月日）、日期运算与自定义格式 | 订单、审计常用 |
| 环境配置 | 见 §6.4 | |
| 十进制数 | 金额（chat 用分 + UInt64 回避了这个问题） | 可先与 PG numeric 一起做 |
| 正则或简单模式匹配 | 输入校验 | 可后置，先补 String 方法 |

## 7. HTTP【P0】

参照 [postgres_orders_http.jk](../examples/postgres_orders_http.jk) 与 [http_server.jk](../examples/http_server.jk)：

| 问题 | 现状 | 建议 |
| --- | --- | --- |
| 没有路由 | handler 内手写 `if method == "POST" && target == "/orders"`，`route()` 再写一遍同样的判断 | `http.Router`：`router.post("/orders/:id", handler)`，自动提供 route 标签、405/404 |
| 方法是字符串 | `Request.method: String`，客户端却用 `Method` 枚举 | 服务端也用 `http.Method`，与客户端统一 |
| target 含查询串 | 需要用户自己拆 `?` | `request.path()`、`request.query("page") -> Option<String>` |
| 标头查找 | `for field in &headers { if field.name == "idempotency-key" ... }`（示例第 28 行） | `request.header("idempotency-key") -> Option<&String>`，`header_all` 处理重复 |
| 响应构造冗长 | 每次写 `http.Response { headers: [], status, content_type, body: text.into_bytes() }`，示例各自封装 `reply()` | `http.json(status, &value)`（ToJson）、`http.text(status, s)`、`http.empty(status)`；`Response.with_header(name, value)` |
| JSON 错误体手写 | `r#"{"error":"not found"}"#` 遍布示例 | 配合 `http.json` 与 `#derive(ToJson)` 错误结构体 |
| 状态码命名 | `Status.Success` 表示 200，`InternalError` 表示 500 | 用标准原因短语：`Status.Ok`、`InternalServerError`、`UnprocessableContent`、`TooManyRequests`、`Unauthorized`、`Forbidden`、`MethodNotAllowed`；补齐常用码 |
| 请求体解码 | `typed.decode_bytes::<Purchase>(body)` 可用 | 补 `request.json::<T>()`，失败返回可直接转成 400 的错误 |
| 函数 handler 不能带状态 | 有状态时必须实现 `Service`，含 `share`、`route`、`handle` 三个方法 | 路由器持有共享状态 `Router.with_state(state)`，handler 签名 `fn(&S, Request)` |
| 连接模型 | 每连接一次请求、`Connection: close`、无 chunked | 记为已知限制即可，keep-alive 在压测和反向代理后会成为瓶颈，排入后续计划 |
| 中间件 | 无 | 至少提供请求前后钩子：鉴权、请求日志已内置、CORS |

服务启动样板见 §9。

## 8. PostgreSQL【P0】

### 8.1 参数绑定

现状（[postgres_orders.jk:21](../examples/postgres_orders.jk)）：

```joky
[IntValue(customer), TextValue(key.clone()), IntValue(qty)]
```

建议：

- 新增 `postgres.ToSql` 协议，Int16/32/64、Bool、String、`&String`、Array<UInt8>、Option<T>、
  Float64、Uuid、Timestamp 实现；`execute(sql, params)` 的参数改为 `Array<postgres.Param>`，
  由期望类型自动装箱，目标写法 `[customer, key, qty]`。若首版做不到隐式装箱，退一步用短构造函数
  `[pg.int(customer), pg.text(&key), pg.int(qty)]`。
- 变体名去掉冗余后缀：`Value.IntValue` → `Value.Int`，`TextValue` → `Text`。
- 文本参数接受借用，避免 `key.clone()`。

### 8.2 结果读取

现状：Row 是 `Array<Value>`，只能按列号读取；`postgres.get_i64(row, i)` 返回
`Result<Option<Int64>, ColumnError>`，非空列需要再写一个 `number()` 包装。

建议：

- 方法形式与按列名读取：`row.get::<Int64>("id")?`、`row.get_opt::<String>("note")?`；
  需要驱动返回列名元数据（Row 改为带列描述的记录，而非裸数组）。
- `#derive(FromRow)`：`let order: Order = tx.query_one_as::<Order>(sql, params)?`，按字段名映射。
- 标量便利：`query_scalar::<Int64>(sql, params)`。
- 命名：`get_i64` 与语言类型名 `Int64` 不一致，见 §10。

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
| 默认配置 | `http.defaults()`、`session.defaults()`、`json.Limits.defaults()`、`typed.Options.defaults()`、chat 的 `defaults()` | Copy／拥有型记录已可用 `..base` 结构更新；`Type.default()`／Default 派生暂缓 |
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

```joky
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

fn create_order(state: &Orders, request: http.Request) @suspends -> http.Response {
    let key = match request.header("idempotency-key") {
        Some(k) if !k.is_empty() && k.byte_length() <= 128 => k.clone(),
        _ => return http.json(Status.BadRequest, &ApiError.new("missing Idempotency-Key"))
    }
    let input = match request.json::<Purchase>() { Ok(i) => i, Err(_) => return http.bad_request("expected qty") }
    match orders.place(&state.pool, 1, &key, input.qty) {
        Ok(Confirmed(id)) => http.json(Status.Ok, &OrderCreated { order_id: id }),
        ...
    }
}

fn main() @suspends -> Int32 {
    let pool = pg.Pool.open(pg.Options { max_connections: 4, ..pg.Options.default() })!
    let router = http.Router.with_state(Orders { pool }).post("/orders", create_order)
    http.run("127.0.0.1:0", router)
}
```

注：`Some(k) if ...` 依赖 §5.5 的 match 守卫，当前未支持；`transaction` 闭包内 `return` 返回闭包本身，与现有闭包规则一致。

## 12. 建议实施顺序

1. **低成本先行**（不改语义）：标准库错误与无载荷枚举实现 Show（已完成）；String 基础方法；Option/Result 补方法；
   Array any/all/find/retain；
   清理标准库内过时写法（`uint64(0) ..`）。
2. **HTTP 便利层**：header/query/path 访问、响应助手、Status 改名、Method 枚举、Router、`http.run`、
   `shutdown.with_signals`。均为标准库源码改动，不涉及编译器。
3. **PostgreSQL 便利层**：Value 变体改名、按列名读取、`get::<T>`、timestamptz/uuid/jsonb、
   `pool.transaction`、Options。
4. **语言特性**：调试协议及派生、Default 派生、字符串字面量模式与守卫、
   `ToSql` 隐式装箱、非 pub 函数 `@suspends` 推导。
   每项按 AGENTS.md 的跨模块语义改动要求单独设计与验证。
5. **关键词与命名迁移**（`when`→`atomic`、`alias`→`same`、`branch`→`spawn`、`var`/`mut` 统一、
   构造与缩写命名）：集中一次完成并提供迁移诊断，趁项目尚未公开时做。

每项落地时同步更新 language.md 与相关示例，示例应作为"目标写法"的验收标准：
postgres_orders_http.jk 在完成 1–3 后应缩短到约当前一半，且不再出现手写 JSON 字符串与标头循环。
