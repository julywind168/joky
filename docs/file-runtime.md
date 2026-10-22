# 文件运行时设计与开发计划

状态：FS0–FS5 首版实现于 2026-10-17；正式模块为 `joky/fs`。FS6 便利 API 见第 12 节。
契约与 FS0 探针见第 10 节，实现、验收证据及平台限制见第 11 节。首版不包含全部文件系统 API。
Path 与 IntoPath 的后续设计见第 14 节；FS6P、FS7、FS8 均未实现。

当前实现语义以 [language.md](language.md) 为准；已有资源与线程边界见
[architecture.md](architecture.md)，DNS 的实施经验见
[网络运行时计划](network-runtime.md)。本文记录文件专项的设计选择、实施顺序和验收门槛；
稳定的用户接口已归入 language.md，ABI 与生命周期不变量归入 architecture.md，
这里保留阶段与验收记录，不复制两份长期规范。

## 1. 目标与现状

Joky 脚本应能通过统一的 `joky/fs.File` 使用文件。CLI 可以提供本机文件系统，
嵌入宿主可以提供自己的文件服务，也可以完全不提供文件能力。文件操作不能在语言
调度线程中执行阻塞 I/O；JIT、解释器与独立 AOT 使用相同的资源、错误和取消语义。

| 开发前基础 | 本次文件专项 |
| --- | --- |
| `Type::Resource`、`OwnedResource`、资源工厂与唯一释放责任 | `fs.FileOwner` 的类型绑定、文件操作适配器与宿主实现选择 |
| `tcp.StreamOwner` 和公开记录包装 | `File` 及 `Result` 文件 API，不复用 TCP 类型身份 |
| `ResourceLease`、有界字节请求与宿主快照 | 后台执行期间的文件保活、串行化与取消后关闭 |
| CPS poll/stop、`HostExecutor`、`TaskWaker` | 文件请求的提交、完成交接与非阻塞关闭 |
| DNS 的有界线程池与取消后服务计费 | 独立的文件服务额度、清理准入与宿主可替换实现 |
| 从磁盘加载 `.jk` 用户模块 | 运行中的脚本文件 I/O；源码加载不是本专项的文件能力 |

当前 DNS 每份运行时共享固定 4 个工作线程，最多 64 个排队请求、128 个未回收请求；
取消不保证终止已经开始的系统解析。这些是 DNS 的现有参数，不直接作为文件服务参数。
可核对 [DNS 实现](../src/runtime/dns.rs) 和
[DNS 线程边界与关闭](architecture.md#dns-线程边界与关闭)。

首版包括打开、有限字节读取、部分写入、定位、显式同步与关闭。目录遍历、文件删除和重命名、
元数据、文件监听、映射文件、句柄复制、读写拆分及原子替换写入留待后续；文本便利接口由 FS6 补充。
不新增语言关键字，不开放普通语言引用跨线程，不承诺硬实时或阻塞系统调用的有界终止。

## 2. 类型与宿主能力分层

公开 `File` 是拥有型记录，唯一资源字段为内部 `fs.FileOwner`，沿用
[TcpStream 的包装方式](../src/stdlib/tcp.jk)。该资源非 Copy、没有用户构造器，不能从
整数或 `Ptr` 伪造；移动、聚合、State 存储及作用域清理沿用既有资源规则。

`FileOwner` 表示由宿主管理的文件资源，不规定它一定包含操作系统 fd。运行时包装负责
语言侧身份与清理，宿主文件状态负责实际句柄、操作状态、释放实现和后台保活。
后台只取得专门设计的线程安全状态，不能直接把现有 `OwnedResource` 或 `ResourceLease`
当作可跨线程对象使用。

### 2.1 类型始终存在，能力显式安装

标准库声明及受检内部入口始终可解析。没有文件实现时，完整的缺省适配器返回
`Err(Unsupported)`，不构造空句柄或伪造成功结果。仅导入模块、声明含 File 的类型，
或者加载包含可选文件分支的程序，不应因为缺少文件能力失败。

| 运行环境 | 默认行为 |
| --- | --- |
| Joky CLI、CLI 生成的独立程序 | 显式安装本机文件实现 |
| Rust/C 嵌入 VM 或嵌入 AOT | 默认缺省实现，宿主显式安装文件服务 |
| Godot | 仅用于嵌入验证，不提供文件服务；合法 open 返回 Unsupported |
| 测试 | 安装内存、可控阻塞或故障注入实现 |

能力选择属于执行上下文，不使用可变的进程全局“当前文件实现”。多个 VM/执行器可以
显式共享同一服务及额度，但不能意外继承另一宿主的权限。嵌套 VM 的继承规则须显式配置；
首版默认不自动传播文件能力。

首版在执行上下文启动前固定文件服务配置，避免运行中替换的额外语义。
每个已打开资源仍须保留原实现及代码拥有者，直到所有请求和最终关闭结束；模块重载或 VM
销毁不能把旧资源交给另一实现回收。未来开放配置替换也必须保留这个规则。

### 2.2 缺少能力与错误注册不同

无实现、或已选实现不支持某项操作，返回 `Unsupported`。访问被拒绝返回
`PermissionDenied`；路径不存在返回 `NotFound`。宿主描述符版本、签名或生命周期约定
不匹配，应在注册/加载阶段拒绝，不能用 `Unsupported` 隐藏 ABI 错误。

缺省实现也必须具备完整绑定和 AOT 链接符号；运行时错误分支不能补救缺失 extern。
是否能打开具体路径以操作结果为准，首版不需要单独的能力查询 API。

路径按宿主命名空间解释。首版输入为拥有型 UTF-8 String，拒绝内嵌 NUL，并设输入长度上限；
不承诺表示所有平台上的非 UTF-8 路径。本机相对路径的基准目录应在服务创建时确定，
不依赖其他线程改变进程工作目录。自定义宿主的命名空间规则由该宿主定义与验证；
这不包含新增 Godot 文件适配。后续 Path 的本机词法规则与宿主命名空间边界见第 14 节。
本机路径基准不是安全沙箱；限制目录权限需要独立设计，不能用字符串前缀检查宣称完成隔离。

## 3. 首版语言接口契约

以下保留 FS0 的接口规格；可执行声明见 [fs.jk](../src/stdlib/fs.jk)，用法见 [language.md](language.md#内置文件模块-jokyfs)。
模块为 `joky/fs`，类型为 `File`、`OpenOptions`、`SeekFrom`、`IoError`、`ErrorKind`。

| 操作 | 输入与结果 | 语义 |
| --- | --- | --- |
| `open` | 拥有型 String 路径、OpenOptions → Result<File, IoError> | 创建资源并原子交接给语言拥有者 |
| `File.read` | `&mut self`、UInt64 上限 → Result<Array<UInt8>, IoError> | 返回至多上限个字节；正上限下空数组表示 EOF |
| `File.write` | `&mut self`、拥有型 Array<UInt8> → Result<UInt64, IoError> | 允许部分写入，返回实际字节数 |
| `File.seek` | `&mut self`、SeekFrom → Result<UInt64, IoError> | 成功返回新的绝对位置 |
| `File.sync_all` | `&mut self` → Result<Unit, IoError> | 请求宿主同步数据及相关元数据，不能实现时返回 Unsupported |
| `File.close` | 消费 `self` → Result<Unit, IoError> | 等待宿主关闭结果；任何结果下都不恢复 File 的使用权 |

以上操作全部为 `@suspends`；缺省实现或内存实现可以立即完成，但调用约定保持一致。
`@suspends` 只允许挂起，不能自动把宿主阻塞函数变成非阻塞函数。

OpenOptions 包括读、写、追加、创建、排他创建及截断意图；合法组合表见第 10.1 节，
互相冲突的选项在发起副作用前返回 `InvalidInput`。不以先查询存在性再创建模拟排他创建。
SeekFrom 区分从起点的 UInt64 位置和从当前位置/末尾的 Int64 偏移，负结果及超出实现
可表示范围的定位必须报错，不静默截断。

首版单次字节请求硬上限为 1 MiB，与既有字节请求边界一致；零读取上限返回
`InvalidInput`，空写入返回 0，非空写入零进度返回 `WriteZero`。写入输入按值消费并复制
为宿主快照，失败时也不隐式还原输入数组。有限 read/write 不承诺读满或写满。
普通 I/O 失败也不承诺没有副作用；FS6 的 write_all 部分进度与重试规则见第 12 节。

首版业务错误包括 `Unsupported`、`PermissionDenied`、`NotFound`、`AlreadyExists`、
`InvalidInput`、`Busy`、`Closed`、`LimitExceeded`、`WriteZero` 和 `SystemFailure`。
错误消息必须有长度上限；未知宿主错误映射为 SystemFailure，不能映射成 EOF。
任务预算耗尽、协议错误和运行时不变量破坏继续使用终止性错误通道。

close 成功不替代 sync_all；自动析构不隐式同步落盘，也不承诺报告关闭错误。
持久化要求及各宿主对 sync_all 的保证要在实现文档中明确，不能把 write 成功当作持久化成功。

## 4. 非阻塞执行与线程边界

本机实现使用有界阻塞工作线程池；宿主也可接自己的异步服务。统一要求 submit、poll、
stop 和资源释放回调不执行阻塞 I/O、不等待工作线程、不 join 活跃线程。
线程创建与服务初始化发生在进入任务 poll 之前。

```text
任务线程：校验 → 预留额度/复制有界输入 → 提交 → Pending
工作线程：取得宿主状态 → 执行文件操作 → 发布有界完成包 → wake
任务线程：领取完成包 → 构造语言结果 → 完成或失败清理
```

后台只持有拥有型路径/字节、线程安全的文件状态、请求身份、服务计费及 TaskWaker。
不得持有 Buffer、arena、CpsFrame、Rc、VM、实例 State 或未经线程契约许可的引擎对象。
只有任务线程能创建语言结果、写错误输出槽和操作语言预算。

系统调用期间不持有调度器、请求完成状态或服务队列的锁。锁只保护有界状态迁移；
轮询不得等待由工作线程跨整个 I/O 持有的文件锁。单文件互斥通过请求状态保证。
结果先发布再唤醒，完成与撤销交付共享一致的状态协议，避免丢失唤醒与重复交付。

DNS 与文件服务可以复用队列、请求身份、完成通知等基础设施，但首版使用独立工作额度
和队列，避免慢文件占满 DNS 工作线程。是否抽取公共 Rust 模块由实现复用程度决定，
不为交付文件功能先重写已经稳定的 DNS 服务。

宿主实现必须声明线程亲和性和完成方式。不能把任意引擎文件 API 直接放入线程池；
若操作只能在主线程执行且可能阻塞，就不满足本接口契约，应暂时返回 Unsupported，
或提供符合契约的宿主服务后再开放该操作。

## 5. 所有权、串行化与取消

### 5.1 两层生命周期

语言 FileOwner 保存不可伪造的身份和唯一逻辑关闭责任；宿主文件状态可以被后台请求保活。
语言侧销毁与物理句柄关闭不是同一时刻：语言拥有者释放后，后台仍可能完成已开始的工作，
但不得再访问语言内存或交付给已取消任务。

open 取得句柄后立即归操作守卫所有，语言接管成功前的任何失败或取消都转入后台关闭。
成功接管必须连续转移唯一清理责任；arena 分配失败、结果包丢弃和未领取成功结果都不能泄漏。

同一文件首版最多一个活动操作，覆盖提交至完成交接/取消处理。语言侧使用独占借用，
宿主状态额外检查活动请求身份。身份不直接采用可复用 fd；迟到完成不能命中新文件。
既有语言 ResourceLease 只保护任务侧包装，不能代替后台文件状态的独立保活和访问控制。

### 5.2 首版取消契约：在途取消使文件逻辑关闭

文件读取也会改变游标，写入可能已部分生效。仅在取消时释放语言借用、随后允许下一个
操作直接开始，会造成游标竞争和不确定的重试。FS0 的可控探针验证并冻结以下规则；
这是文件服务独立的取消契约，不改变现有 TCP 或其他资源的取消语义。

| 取消时状态 | 首版处理 |
| --- | --- |
| 未提交或仍在排队，成功阻止工作开始 | 撤销请求，不执行副作用；已有 File 保持可用 |
| open 已开始或成功结果尚未交接 | 撤销交付；若取得句柄，由后台关闭，不创建语言 File |
| 已有文件的 read/write/seek/sync_all 已开始，结果尚未交接 | File 逻辑关闭，拒绝后续操作；后台结束后执行最终关闭 |
| close 已提交 | 保留关闭责任直到完成；取消只撤销结果交付 |

“是否开始”必须由工作线程取件与取消共享的原子状态迁移决定，不能先检查再无保护地取消。
已经完成但尚未交接的操作也按在途取消处理，因为调用方没有取得其结果。
逻辑关闭后的 read/write/seek/sync_all 返回 Closed；消费式 close 可以等待最终回收结果，
不能重新发起一次物理关闭。后台关闭结果需有界保留到语言包装释放或显式 close 领取。

这意味着 race 超时可能让原 File 不再可用，但不会撤销已经发生的写入或截断。
任务恢复后不得自动重试写入；重新 open 也不等于恢复上次进度。
未来若需要取消后继续使用，可单独设计 Busy/draining、显式恢复或带偏移操作；
首版不同时引入这些模式。

### 5.3 stop 与 Drop

stop 同步撤销结果交付与对语言内存的访问，解除任务侧请求和借用；不等待系统调用结束。
后台继续执行的请求只拥有宿主数据、服务额度与资源保活引用。
因此保留现有同步 stop 契约，不要求同步 stop 结束不可中断的宿主工作。

资源释放回调及语言 Drop 同样不执行最终阻塞 close，只把状态标为关闭并移交回收责任。
不得在最后一个 Arc 意外落到任务线程时通过 Rust 析构直接关闭底层文件。
宿主文件状态的最终销毁也必须遵循指定的回收执行位置。

## 6. 容量、清理准入与服务关闭

文件服务分别限制工作线程、普通排队请求、未回收请求、打开/正在打开的文件数、
宿主缓冲字节、路径及错误消息长度。未回收计数包括执行中、完成未领取、取消后仍在执行
以及等待最终关闭的状态。FS0 冻结的默认值见第 10.2 节；宿主可在初始化时配置，运行中不热改。

普通队列满或瞬时准入不足返回 Busy，不在调度线程等待空位；单项输入超过硬上限返回
LimitExceeded。输入快照、读取存储和完成包在提交前预留额度，禁止先分配大块再记账。

任务侧用 ExternalCharge 记录请求及交接峰值；后台服务独立限制真实存储。
取消可解除任务侧计费，但后台对应额度须保持到真实存储释放，不能靠反复取消绕过上限。
正常读取交接时覆盖宿主缓冲与语言数组并存的峰值；工作线程不释放非 Send 的 arena 计费对象。

关闭不能与普通 I/O 争抢唯一的满队列空位。每次 open 准入时必须同时预留句柄名额、
可供打开失败/取消/析构使用的清理记录和关闭准入；预留失败则不执行 open。
首版使用预分配的每文件清理记录与独立关闭队列，普通请求饱和时仍可非阻塞移交关闭责任。
关闭队列与普通队列均有工作时交替取件，空队列不占轮次；工作线程全被阻塞时允许回收延迟，
但保留额度并拒绝新准入，不能无限积压。

服务关闭按以下顺序进行：

1. 关闭普通请求准入，撤销排队操作，保留所有打开结果和文件的最终回收通道。
2. 取消任务时撤销通知与交付，后台请求保留自己的状态与代码依赖。
3. 已开始的操作返回后执行关闭，回收未领取结果、句柄、缓冲和额度。
4. 所有拥有者已释放、后台操作与清理均完成后，才确认服务彻底关闭并允许卸载实现代码。

服务关闭入口不 join 活跃工作，返回可查询的完成状态；不能承诺有界关闭时间。
共享服务由宿主协调关闭，销毁单个 VM 不得关闭其他 VM 正在使用的服务。
释放线程安全句柄还不够，后台回调和释放函数所在动态库也必须保持加载。
不能假设已有 DNS 的全局服务关闭入口自动覆盖新的文件服务。

## 7. 三后端、嵌入与 ABI 接线

文件声明及类型身份属于标准库，具体文件服务属于执行上下文，私有受检适配器连接两者。
JIT/解释器/AOT 必须共享操作语义和 poll/stop 核心，不能在解释器中另写同步文件路径。
独立 AOT 使用可重定位符号及运行时上下文，不嵌入编译进程的服务指针。

可借鉴现有资源工厂及 ResourceIoV1，但不能直接宣称它们已经覆盖路径 String 输入、
文件错误结果、seek/sync/close 和取消后后台保活。FS0 决定新增独立的 FileServiceV1
描述符及受检文件操作布局，见第 10.3 节；不放宽普通 extern 聚合 ABI 或跨挂起视图规则。

宿主描述符必须说明版本、签名、上下文所有权、线程亲和性、请求/资源释放责任、
结果发布顺序及关闭条件。缺省、本机和测试实现共用同一契约；错误注册必须可诊断。
若增加公开 C 接口，同步维护头文件、符号导出、安装布局和静态库链接测试。

Godot 仅用于嵌入验证，不追加引擎文件功能；验证缺少文件实现时仍可编译与执行，合法
open 返回 Unsupported。其他宿主若提供自己的文件服务，应自行验证路径语义、线程规则、
实例销毁、重载及代码保活；通用 VM 测试不代替宿主集成验收。

## 8. 实施阶段与完成门槛

| 阶段 | 工作 | 完成门槛 |
| --- | --- | --- |
| FS0：契约与可控探针 | 冻结 API、选项表、错误、取消规则、默认额度及描述符；验证线程状态与清理准入 | 可控宿主证明慢操作不阻止其他任务，取消后无语言内存访问，满队列仍可清理；所有待定项有结论 |
| FS1：类型与可选宿主 | FileOwner、File、完整缺省绑定、执行上下文选择及资源交接 | 无文件实现的三后端程序可导入/加载，调用返回 Unsupported；注册错误拒绝；拥有者与实现代码恰好释放 |
| FS2：文件服务基础 | 有界队列、后台状态、完成唤醒、取消、计费和非阻塞关闭 | 排队/执行/完成/交接/取消状态矩阵通过；服务关闭与迟到结果隔离；无取消绕过额度 |
| FS3：本机文件 API | 接入 open/read/write/seek/sync_all/close 与 CLI/AOT 安装 | 三后端执行真实临时文件读写、错误和取消；独立 AOT 等待与安装布局验证 |
| FS4：嵌入 | 公开宿主配置、共享服务隔离、缺省 Unsupported | 未提供/已提供实现均在通用宿主验证；实例关闭、重载与后台保活验证；Godot 仅验证缺省能力 |
| FS5：收尾与文档 | 受影响 Debug 穷举、示例、稳定接口文档和平台记录 | 逐项记录证据与未验证平台，更新 language/architecture；未实现能力继续保留设计状态 |
| FS6：字节与文本便利 API | 默认/受限完整读取、完整 write_all、严格 UTF-8 read_text/write_text；具体契约见第 12 节 | 三后端短读/短写、超限、解码与部分进度；取消穷举及真实文件场景 |
| FS6P：路径抽象（待实现） | 最小 `joky/path.Path`，随后将文件入口接入 `IntoPath`；见第 14 节 | 三后端字符串/Path 调用与所有权验证；本机路径边界及无文件服务时的纯路径操作 |
| FS7：目录与元数据（待实现） | 在路径契约确定后增加 `create_dir_all` 等操作；逐项冻结接口与错误 | 递归创建、已存在目录、中间文件、部分成功、并发与取消；慢操作不阻塞调度器 |
| FS8：原子替换写入（待实现） | 同目录临时文件、完整写入、原子替换；单独定义持久化保证 | 读者观察旧内容或新内容；失败/取消清理、平台替换语义、能力缺省及持久化边界 |

FS1/FS2 可以用确定性内存宿主推进，但不能跳过 FS3 的真实文件操作。
FS4 的文件服务能力由通用 Rust/C 宿主验证，Godot 不作为引擎文件适配的开发目标。
首版开发验收目标为 macOS/aarch64；Linux、Windows 不在本次已验证范围内。
单平台验证不扩写为跨平台保证。
后续顺序为 FS6P 的最小 Path 模块 → 文件入口接入 IntoPath → FS7 → FS8。
后三阶段的设计与验收门槛不代表当前已经提供这些能力。

## 9. 验收矩阵与验证范围

| 范围 | 必须覆盖的行为 |
| --- | --- |
| 能力与隔离 | 未安装、安装内存/本机实现、两个 VM 使用不同实现、显式共享服务、嵌套 VM 不隐式继承 |
| 基本 I/O | 二进制字节、EOF、空写、部分读写、非空零进度、权限/不存在/重复创建、选项冲突、定位边界、同步失败 |
| 所有权 | Result/聚合/State 中的 File、移动后拒绝使用、显式 close 消费、自动清理、打开成功后语言分配失败 |
| 取消 | 排队移除、开始竞争、阻塞中取消、完成未领取取消、open 迟到成功、close 等待取消、逻辑关闭后拒绝复用 |
| 调度 | 慢文件操作期间其他任务/定时器继续推进，重复 poll 不重放副作用，唤醒先后竞争和迟到通知无害 |
| 容量 | 每个上限的边界、取消后仍保留服务额度、交接复制峰值、普通队列满时析构和关闭仍可提交 |
| 关闭与代码保活 | 任务/实例/VM 销毁、模块重载、共享服务、后台未结束时禁止卸载、最终关闭及额度恰好归还 |
| 后端与部署 | JIT、解释器和独立 AOT 执行生成代码；宿主描述符校验；有公开 ABI 改动时验证头文件与安装布局 |
| Godot 嵌入边界 | 不安装文件服务时仍可导入/执行，合法 open 返回 Unsupported |

并发测试以可控阻塞点、完成闸门和资源账本证明顺序，不靠真实磁盘恰好慢或 sleep 时间碰运气。
验证调度不阻塞时，在文件工作被闸门保持未完成的情况下证明其他任务已经推进，
使用超时仅防测试挂死，不把耗时阈值当性能验收。真实文件测试使用独立临时目录并可靠清理。

默认测试保留短状态矩阵与关键资源边界；逐指令取消、交接和容量组合使用相关
`exhaustive_` Debug 用例，不自动运行无关穷举。
仅探针/测试改动按仓库规则执行受影响测试、Rust 格式及 Clippy；实现、ABI 或生命周期
改动先跑针对性行为测试，再运行 `./scripts/check.sh`，并补相关 Debug 穷举。
只有用户明确要求发布验收、性能测量或优化构建问题时才扩大到 Release。

初始文档提交仅检查内容、链接与差异格式。FS0 新增独立探针及 fixture，按测试改动范围
运行三后端探针、Rust 格式与 Clippy；具体记录见下文。后续各阶段记录实际执行的检查、
平台、结果与未验证项，不把计划中的验收矩阵记为通过。


## 10. FS0 契约冻结与探针记录

### 10.1 API、字节与文本

第 3 节的六项操作为首版范围；字节类型固定使用 `Array<UInt8>`，不增加 Bytes 类型。
路径使用 String，文件内容不隐式解码或编码。FS0 预留的文本便利接口要求：有界整文件读取后
严格校验 UTF-8；文本写入按 UTF-8 处理完整输入并明确覆盖/追加策略；分块及按行读取
必须保留跨块的 UTF-8 状态。不能把每次 read 的结果独立转成字符串。
这些便利接口的函数名与组合错误未纳入 FS0 的首版 API；现由第 12 节 FS6 定义。

OpenOptions 是六个公开 Bool 字段的 Copy 记录，依次为 `read`、`write`、`append`、
`create`、`create_new`、`truncate`。`OpenOptions.new()` 返回 read=true、其他=false。
内部 flags 按上述顺序占用位 0–5，其他位非零为 InvalidInput。

| 访问组合 | 合法的创建/截断组合 |
| --- | --- |
| 仅 read | 全 false |
| 仅 write，或 read + write | 全 false；仅 create；仅 create_new；仅 truncate；create + truncate |
| 仅 append，或 read + append | 全 false；仅 create；仅 create_new |

其余组合全部返回 InvalidInput：无访问权限、write 与 append 同时启用、create 与
create_new 同时启用、只读创建/截断、追加截断及 create_new + truncate 均拒绝。
共有 17 个合法位组合。append 自带写权限，每次写入请求追加；不承诺跨多个 write 的
原子性。create 不隐式截断，create_new 要求宿主提供排他创建，否则 Unsupported。

SeekFrom 为 `Start(UInt64)`、`Current(Int64)`、`End(Int64)`。内部分别编码为
origin=0/1/2；signed offset 保留 Int64 位表示，Start 使用 UInt64 原值。
IoError 拥有 `kind: ErrorKind` 和 `message: String`。错误编号依次冻结为：
1 Unsupported、2 PermissionDenied、3 NotFound、4 AlreadyExists、5 InvalidInput、
6 Busy、7 Closed、8 LimitExceeded、9 WriteZero、10 SystemFailure；0 仅表示成功。
错误编号是文件服务契约，不复用现有 TCP/DNS 的编号。

入口先校验路径、选项及单项硬上限，再选择服务；合法请求在缺省服务上返回 Unsupported。
已有文件先校验逻辑关闭状态，再处理零长度等快捷分支，因此空写也不能绕过 Closed。
宿主队列 Busy 表示未开始操作，可由调用方决定稍后重试；系统错误不保证零副作用。

### 10.2 首版服务额度

| 项目 | 默认值 | 约束 |
| --- | --- | --- |
| 本机工作线程 | 2 | 初始化时创建，独立于 DNS |
| 普通排队请求 | 64 | 不包括已经取出的工作及独立关闭记录 |
| 未回收普通请求 | 128 | 包含取消后在途与完成未交接请求 |
| 文件名额 | 256 | 覆盖正在打开、已打开、逻辑关闭及物理关闭未结束 |
| 宿主请求存储预算 | 16 MiB | 包括快照、结果、请求记录与计入的描述符存储，按实际容量计费 |
| 单次字节请求 | 1 MiB | 固定硬上限，宿主可配置更小的服务限制 |
| 路径 UTF-8 字节数 | 4096 | 固定硬上限；长度超限为 LimitExceeded，空路径/NUL 为 InvalidInput |
| 错误消息 UTF-8 字节数 | 1024 | 宿主有界生成并在字符边界截断，非法 UTF-8 为协议错误 |
| 清理记录 | 每文件名额 1 个 | 预分配关闭状态及有界错误存储，独立于普通请求额度，保留到物理关闭及结果回收结束 |

初始化配置中线程/请求/文件数必须为正，排队数不得大于未回收请求数；所有容量计算
检查整数溢出并在初始化完成前分配固定清理存储。初始化失败不发布半成品服务。
文件控制记录及其清理槽使用独立的文件名额预算，不能被普通请求耗尽。
固定服务开销、工作线程栈及内核缓存单独报告为宿主开销，不声称包含在任务 arena 限额中。

### 10.3 独立文件服务描述符与内部结果

FS0 发现 [ResourceIoV1](../src/runtime/byte_request.rs) 明确要求 stop 撤销访问并 join
相关 worker；它的读写回调也没有文件业务错误和路径/定位参数。不能把仍在运行的文件
请求塞入旧接口并重新解释其安全约定。正式实现新增 `FileServiceV1`，旧接口保持不变。

以下为 FS0 冻结并在 FS1–FS5 实现的字段与协议；C 声明见 [joky_fs.h](../include/joky_fs.h)。
采用 repr(C) 定宽整数、显式指针和长度；`abi_version=1`、`struct_size` 与保留位在注册时
校验。没有可选回调：不支持某个操作时由完整实现返回 Unsupported。

| 描述符成员 | 责任 |
| --- | --- |
| context + retain/release | 不透明服务上下文；线程安全保活引用覆盖任务、文件、结果包和后台工作；最后释放不得在任务线程隐式 join 或关闭文件 |
| submit(context, input, wake, out_operation, out_result) | 非阻塞校验/准入并创建独立操作；返回前复制所有输入视图，不能保存 input 或其载荷地址 |
| poll(context, operation, out_result) | 非阻塞查询；Pending 不写结果，Ready 恰好转交一次完整结果 |
| stop(context, operation, reason) | 完成/失败/取消各路径恰好一次；撤销交付并释放任务侧 operation，后台副本可继续运行；不 join |
| release_result(context, result_token) | 释放已交出的消息/字节存储；不再拥有已交出的 file_token |
| release_file(context, file_token) | 逻辑关闭及非阻塞回收移交；已安排的物理关闭不重复执行 |
| shutdown(context) / is_quiescent(context) | 关闭准入并查询全体拥有者、请求与清理是否结束，不等待活跃线程 |

submit 的 input 字段固定为 operation_code、file_token、path 指针/字节数、flags、
bytes 指针/字节数、limit、origin、offset_bits；无关字段必须清零。operation_code/origin
为 UInt32，flags/limit/offset_bits 及所有长度为 UInt64，载荷指针为只读字节指针，
token/context 为不透明指针；长度转换为平台 usize 前必须验证可表示范围。
operation_code 固定为 1 Open、2 Read、3 Write、4 Seek、5 SyncAll、6 Close。
Open 使用 path/flags，Read 使用 file_token/limit，Write 使用 file_token/bytes，
Seek 使用 file_token/origin/offset_bits，SyncAll/Close 只使用 file_token。
file_token 是宿主不透明身份，任务线程只转交，不解释或跨线程解引用。
外部宿主拥有排队/执行中的取消阶段与逻辑关闭状态，因此文件操作参数可能包含无效的
read limit、超限 write 长度或非法 origin。submit 必须先检查 Closed，再按首版上限验证，
无效请求不得复制载荷或产生副作用；否则运行时的本地空写/参数快捷路径会隐藏宿主的关闭
状态。Open 的路径和 flags 仍在选择服务前由运行时验证。

submit 返回 0 Accepted、1 Rejected 或 2 ProtocolFailure；只有 Accepted 可以产生非空 operation。
out_operation/out_result 在调用前清零，Accepted 必须返回 operation 且不同时交付结果。
Rejected 带有有界业务错误结果且无副作用；所有非空输出的回收责任都必须立即由运行时
守卫接管，即使同时出现协议错误。poll 返回 0 Ready、1 Pending 或 2 ProtocolFailure。
stop 的 reason 为 0 Completed、1 Failed、2 Cancelled；shutdown 无返回值，
is_quiescent 返回 UInt32 的 0/1。回调不得 unwind 或重入 Joky。
Ready 结果字段为 result_token、error_code、message 指针/长度、file_token、bytes 指针/长度、
count；error_code 为 UInt32，count 为 UInt64。Open 成功只交文件，Read 成功只交字节，
Write/Seek 成功只交 count，其他成功为空。
错误结果不交文件或字节；运行时仍须回收违约实现产生的非空拥有输出。
结果内的消息/字节内存由 result_token 保活；非空 file_token 在输出时独立转交唯一关闭责任，
由运行时结果守卫持有，release_result 不再关闭它。语言提取文件时从结果守卫移走该责任，
未提取则由守卫调用 release_file。release_result 是有界非阻塞的释放，不能暗中执行最终物理 close。

wake 采用独立保活、可失效的通知对象；工作线程只发布自己的结果并调用通知，不写 Joky
地址。stop 与结果交付的竞争由宿主同步；返回后不再访问 operation 的任务侧包装。
后台副本持有自身的 context 和结果，取消后的迟到成功自行回收，不回调已经释放的指针。
描述符的函数代码必须随 context 保活；C 宿主承担相同义务。

编译器私有操作统一返回内部非 Copy `fs.OutcomeOwner`，其存储承载结果守卫；
语言包装通过受信任的即时 CPS 提取操作取得 kind/message、文件、字节或 count，再构造 Result。
FS1 将同步提取调整为首次 poll 即完成的 CPS 适配器，以共用三后端的受检资源交接路径；提取不启动 I/O。
只有成功结果允许提取对应拥有值，提取转移责任，重复或错类型提取为运行时协议错误。
OutcomeOwner 退出时回收未取走的结果，因此语言分配失败和交接取消均保留连续释放责任。
错误文本复制成语言 String 后才释放宿主结果；复制峰值计入任务预算。
该内部结果叶不对普通源码开放构造或宿主注册，不放宽普通 extern 的聚合返回规则。

私有操作的源码级参数采用第 3 节类型：Open 的 OpenOptions 编码成 UInt64 flags；
Read/Write/Seek/SyncAll 借用 FileOwner，Seek 的 origin/offset 使用两个 UInt64；
Close 的公开包装消费 File，但私有入口借用 owner，结果返回或取消时由包装清理 owner。
所有私有操作返回 OutcomeOwner，均为 @suspends。完成后不再由后台直接写任何错误引用槽。
这些受检绑定及三后端结果收养在 FS1/FS2 独立实现，现有资源工厂接口不因此开放该布局。

Close 是两阶段责任：submit 时逻辑关闭，文件仍由原 owner 保活；完成结果提供关闭状态。
Close 的请求与完成存储使用文件预留清理记录，不占普通请求额度，不因普通队列已满
返回 Busy。每文件只有一个关闭等待者；已有关闭工作时接入其结果，不重复物理关闭。
物理关闭失败不自动重试裸句柄，
实现须终结该 token 并保留错误，避免复用后的句柄被重复关闭。自动析构只保留计数/诊断，
不新增可能无限增长的错误消息队列。

### 10.4 可控探针的证据范围

探针文件： [共享宿主模型](../tests/fixtures/host_file_io_host.rs)、
[Joky 程序](../tests/fixtures/host_file_io.jk)、[三后端入口](../tests/host_file_io.rs)。
宿主模型只有一个工作线程、一个普通排队位置、两个未回收请求和三个文件槽，
使用固定四字节快照及完成闸门缩小容量边界；不访问真实磁盘。

生成代码使用既有资源工厂创建 ProbeFile/ProbeRequest，由请求资源的析构撤销交付；
标量 CPS 等待回调只访问宿主状态，不保留 frame 或语言引用。测试中的整数 ID 仅用于
控制路由，不是正式文件能力或公开句柄方案。它验证资源析构与调度的可行性，
不证明新的 FileServiceV1 或内部 OutcomeOwner 已实现。

共同场景覆盖：后台被闸门保持阻塞时独立任务和虚拟时钟定时器完成；正常结果仅交付一次；
提交后修改来源不改变后台快照；普通队列满时第三个请求被拒绝但析构仍可提交关闭；
排队取消不启动副作用，保留 File 并允许新请求取得新身份；在途及完成未领取取消使 File
逻辑关闭；取消后后台额度保持，迟到结果不恢复旧任务或新任务；关闭只在工作线程执行且
每文件恰好一次；关闭服务准入后仍可等待后台退出，调用方不在语言取消路径中 join。

模型通过后台持有的数据类型和资源账本检查隔离，不把它当作内存安全工具的替代品。
真实 open/read/write/seek/sync/close、错误映射、生产计费、结果分配失败、动态库卸载、
公开描述符及通用宿主的正式验收见第 11 节。旧 ResourceIoV1 的协议不作任何修改。

### 10.5 验证记录

2026-10-17，macOS / aarch64，FS0 开发验证完成：

- `cargo test --test host_file_io`：2 项测试通过。第一项执行 JIT 和解释器，第二项编译并运行
  独立 AOT 可执行程序；三后端共用上述 6 组受控生命周期场景。
- `cargo fmt --check` 及 `rustfmt --edition 2021 --check tests/fixtures/host_file_io_host.rs` 通过。
  include 的共享 fixture 单独检查格式。
- `cargo clippy --all-targets -- -D warnings` 通过。
- 本文 10 个本地链接和 `git diff --check` 通过；新增未跟踪文件另行检查差异格式。

范围为独立测试/fixture 和文档，不改生产代码；按 AGENTS.md 未运行全套检查脚本、
无关穷举或 Release。没有验证真实磁盘或引擎文件服务，不声明其他平台可用。
以上记录仅为 FS0 的契约与可行性验证；正式实现的证据如下。


## 11. FS1–FS5 实现与验收

| 阶段 | 实现与证据 |
| --- | --- |
| FS1 | fs.FileOwner / fs.OutcomeOwner、公开 File 和 Result API、精确内置签名、缺省 Unsupported；`tests/fs.rs` 覆盖三后端及非法声明/伪造拒绝 |
| FS2 | 独立工作池、请求/存储/文件额度、预留关闭队列、输入快照、任务复制峰值计费；`src/runtime/fs/worker_tests.rs` 使用闸门覆盖排队/执行/待领取取消、满队列关闭和 shutdown |
| FS3 | 本机 open/read/write/seek/sync_all/close；真实临时目录的 JIT/解释器/CLI/AOT 往返、选项及 UTF-8/字节边界；C 布局和独立嵌入缺省服务测试；安装脚本包含 joky_fs.h |
| FS4 | Rust 阻塞宿主 trait 与非阻塞 FileServiceV1、显式共享与快照保活、嵌套 VM 隔离；宿主错误输出拥有者回收；Godot 仅验证缺省文件能力（引擎 FileAccess 适配已移除，见第 13 节） |
| FS5 | 语言/架构/Godot 文档及 files.jk 示例；逐指令取消穷举和默认 Debug 检查，执行记录见下文 |

当前实现选择：Rust 阻塞宿主复用有界工作池；异步宿主通过独立 FileServiceV1 接入，
自身负责同等的额度和关闭协议。所有非空返回 token 都先纳入守卫，再验证字段与长度；
非法结果按 SystemFailure 交给语言，指针有效性仍是 unsafe 注册契约。
内部提取由同步调用改为即时 CPS，不增加 I/O 等待；这一调整避免另造一套资源聚合返回 ABI。

Godot 不提供文件服务，合法 open 返回 Unsupported。通用文件能力仍由其他宿主按需接入。
首轮原生平台为 macOS/aarch64。Linux、Windows、移动/Web 未在本次环境执行；
Windows CloseHandle 仅有条件编译实现，不宣称验证通过。
目录/删除/重命名、元数据、文件监听、映射、句柄复制、读写拆分、
文本流便利接口和性能/实时性保证仍未实现。read_all/write_all 与完整文本读写由 FS6 补充，
不计入 FS0–FS5 的首版交付。

### 验证记录

正式实现的针对性用例：`cargo test --test fs`、`cargo test --test host_file_io`、
`cargo test runtime::fs::worker::tests --lib`。FS0 可控宿主继续覆盖三个后端的慢操作与
调度并存；正式 worker 用例直接观察队列、请求与物理关闭账本，避免磁盘速度相关断言。
`tests/fs.rs` 使用独立进程执行静态链接 AOT，验证真实文件、宿主描述符与 C 布局，
同一宿主 fixture 在 JIT/解释器/AOT 验证成功和违约输出的恰好一次回收。

本次实际通过：

- `./scripts/check.sh`：默认 Debug 格式、Clippy、单元/集成测试和三后端短输入基准正确性。
  完成收尾调整后重跑受影响的文件行为、worker、格式与 Clippy 检查。
- `cargo test --test fs --test host_file_io`：正式文件接口、描述符、C 布局/入口、安装布局、示例，
  以及 FS0 可控宿主的 JIT/解释器/独立 AOT 行为。
- `cargo test runtime::fs::worker::tests --lib`：10 个确定性状态/额度/失败回收用例。
- `cargo test --test fs exhaustive_filesystem_cancel_every_instruction -- --ignored`：
  正常结果与错误结果的逐指令取消。
- `cargo test exhaustive_resource_handoff_cancel_and_exhaust_at_every_instruction --lib -- --ignored`：
  共用资源交接与耗尽路径的 Debug 穷举。
- `git diff --check` 和新增文档相对链接核对。

没有运行 Release、完整 stress、性能计时或非本机平台验收。

## 12. FS6：字节与文本便利 API

状态：2026-10-17 已实现并通过本机验收。此阶段仅组合既有异步操作，不增加内部资源类型、工作池或宿主 ABI。
2026-10-18 调整为默认无参数读取与显式 `_limited` 读取；旧的带参数调用迁移到 `_limited`。
正式接口放在 `File` 上，均借用 `&mut self` 且标记 `@suspends`：

| 接口 | 结果 |
| --- | --- |
| `read_all()` | `Result<Array<UInt8>, IoError>` |
| `read_all_limited(max_bytes: UInt64)` | `Result<Array<UInt8>, IoError>` |
| `write_all(bytes: Array<UInt8>)` | `Result<Unit, WriteError>` |
| `read_text()` | `Result<String, ReadTextError>` |
| `read_text_limited(max_bytes: UInt64)` | `Result<String, ReadTextError>` |
| `write_text(text: String)` | `Result<Unit, WriteError>` |

`WriteError` 的公开字段为 `written: UInt64` 与 `error: IoError`；written 是此前成功
write 返回的字节数之和，不计失败请求中无法获知的副作用，不是可安全重试的偏移保证。
`ReadTextError` 为 `Io(IoError)` 或 `Utf8(joky/string.Utf8Error)`，保留解码原因与字节偏移。

### 12.1 读取与存储上限

read_all 与 read_all_limited 从当前游标开始，不隐式 seek、不依赖元数据、不建立文件快照。
每次最多请求 64 KiB，短读继续，只有空结果表示 EOF。read_all 不设置额外内容上限，
也不将某个固定大小作为隐式默认值；它仍受实际存储能力及宿主配置的任务内存预算约束。

read_all_limited 的 max_bytes 是返回内容的字节上限，允许 0；读到上限后再读 1 字节，
空结果表示恰好 EOF，否则返回 LimitExceeded。因此超限失败可能推进游标 max_bytes + 1
字节；不退回探测字节，也不返回部分内容。算法先减剩余额度，不对 UInt64.MAX 执行加一。
两种读取在 I/O 失败时均丢弃累积内容，不回滚游标。持续增长的文件不保证让无参数版本到达 EOF；
需要限定内容大小时使用受限版，需要持续处理时使用分块 read。

两组接口共用接受 Option 上限的私有读取循环，不按 max_bytes 预分配；Array 随实际内容
增长，其扩容容量、暂时的新旧缓冲区和当前块仍计入任务内存预算。内容上限与任务总内存
额度分别控制返回内容和任务资源，不能互相替代；分配或预算失败保持现有运行时错误语义。

read_text 与 read_text_limited 分别完成对应字节读取，再整体严格验证 UTF-8，支持字符
跨读块，保留 BOM、NUL 与换行，不做替换或规范化；I/O 与受限版的超限失败优先于解码。
UTF-8 错误偏移相对于本次读取开始处，而非文件绝对位置。宿主 ABI、后台工作池和取消规则不变。

### 12.2 完整写入与部分成功

write_all 消费输入，按最多 64 KiB 的拥有型块调用 write；成功短写后继续未写入的后缀。
后续块大小收缩到短写返回的数量，避免持续微小短写时反复复制大后缀。
非空零进度为 WriteZero；任何错误立即返回，不自动重试 Busy 或其他失败。
空输入仍调用一次底层空 write，保留 Closed 与宿主能力检查，不绕过文件状态。
write_text 将整个 String 作为 UTF-8 字节交给 write_all；written 始终按字节计数。

两者均从现有游标/追加模式写入，不自动创建、截断、定位、sync 或 close；覆盖和追加
由 open 选项决定。成功只表示所有输入字节已由 write 确认，不保证持久化或多块原子性。
追加模式下，多次写入之间允许其他写入者插入。失败消费输入，调用方需要重试时应自行
保留原始数据，并处理失败请求可能已产生的副作用。

### 12.3 取消与验收

组合方法持有普通语言拥有值，逐次复用既有请求。取消不产生 WriteError，不返回进度，
按当前底层请求的阶段适用 FS0–FS5 规则；在请求间取消不会撤销已完成的读写，外层若仍
保有 File，其游标停在已发生效果之后。取消整条任务仍自动释放文件与所有累积缓冲区。

验收覆盖 JIT、解释器、独立 AOT：空文件、零上限、恰好上限、超限探测、短读直到 EOF、
大于单次 read/write 上限的输入、任意二进制、多字节 UTF-8 跨块、非法/截断 UTF-8、
短写成功、零进度、部分成功后的错误、错误进度与实际宿主副作用、缺省宿主和真实文件。
可控宿主还应验证取消后的服务额度归零，解释器保留逐指令取消穷举。
本阶段不包含路径便利函数、文本流/按行读取、目录/元数据或原子替换写入。


### 12.4 实现与验收记录

实现位于 [fs.jk](../src/stdlib/fs.jk)。[文件示例](../examples/files.jk)使用完整文本读写。

2026-10-17，macOS/aarch64，Debug：

- `cargo test --test fs_convenience`：JIT、解释器及独立 AOT 共用
  [可控宿主](../tests/fixtures/fs_convenience_host.rs)与[语言用例](../tests/fixtures/fs_convenience.jk)，
  两项测试通过；覆盖 1 MiB 以上的二进制往返、64 KiB 边界上的 UTF-8、UInt64.MAX 内容上限、
  非零起点、短读/短写、零进度、Busy 不重试及失败请求的未知外部副作用；关闭后服务额度归零。
- `cargo test --test fs`：14 项通过、1 项穷举默认忽略。既有三后端描述符用例扩展为
  read_text 的排队/执行中取消，并验证取消后空 write_text 的 Closed 优先级；
  文件示例在 CLI JIT、解释器和独立 AOT 中读写真实文件。
- `cargo test --test fs exhaustive_filesystem_cancel_every_instruction -- --ignored`：通过。
  在既有探针之外新增完整文本读写的逐指令取消，检查文件、请求及结果拥有者计数全部归零。
- `./scripts/check.sh`：默认 quick Debug 通过，包含 fmt、Clippy、默认测试和 while/Map
  三后端短输入正确性。测试补充后另跑 fmt、Clippy 及受影响用例和取消穷举，均通过。
- `git diff --check` 与相关文档相对链接目标检查通过。

本轮未运行 Release、完整 stress、性能测量或其他操作系统验收；
FS0–FS5 已记录的平台范围继续适用。

### 12.5 默认读取与受限读取拆分

2026-10-18：移除普通完整读取的必填内容上限，原有上限语义保留在 `_limited` 接口。
无参数读取与受限读取共用私有读取循环和 UTF-8 解码，不新增宿主回调或运行时特例。
普通 CLI 示例改用 `read_text()`；明确限制大小的测试改用 `_limited`。
写入接口及部分进度语义保持不变。

本机 Debug 验证：

- `cargo test --test fs_convenience --test fs`：16 项通过，1 项穷举默认忽略。
  共用宿主用例在 JIT、解释器、独立 AOT 验证两组文本读取的短读、跨块 UTF-8、非法/截断
  UTF-8 与 I/O 错误；无参数字节读取覆盖空文件、非零游标、EOF 和超过 1 MiB 的内容，
  受限版保留零上限、恰好上限、超限探测及 UInt64.MAX 用例。关闭后服务额度归零。
- `cargo test --test fs exhaustive_filesystem_cancel_every_instruction -- --ignored`：通过；
  同一任务分别执行无参数与受限文本读取，逐指令取消后文件、请求与结果拥有者计数归零。
- `./scripts/check.sh`：默认 quick Debug 全部通过，包括 fmt、Clippy、默认测试与三后端
  while/Map 短输入正确性；`git diff --check` 和相关文档相对链接目标检查通过。

平台范围仍为 macOS/aarch64；没有新增其他平台、Release 或性能验收。

## 13. Godot 文件适配移除

2026-10-18：Godot 定位为嵌入验证，移除引擎 FileAccess 文件服务、公开能力开关、
编译快照中的服务注入及 Scene 退出的文件服务 drain。删除对应文件示例、测试资源和
文件专属引擎验收场景。先前 FS4/FS6 的 Godot 文件功能不再属于当前能力或后续开发目标。

保留 Joky 通用的 FileService / FileServiceV1、CLI 本机实现及三后端文件测试。
Godot 仅保留一个嵌入回归：通过适配器的 Program 编译并执行实际 Joky 代码，验证
res://、user:// 和普通路径的合法 open 均返回 Unsupported。脚本资源加载器自身的
FileAccess 不变，不向脚本授予文件能力。现有其他嵌入验证功能不因本次范围调整而扩展。

本次验证：

- `cargo test --locked --manifest-path integrations/godot/Cargo.toml embedded_scripts_keep_filesystem_capability_unavailable`：通过，执行缺省文件能力回归。
- `integrations/godot/check.sh`：Godot 适配器格式、Clippy、10 个 Rust 测试、Debug 构建及全部
  保留的真实引擎场景通过；包括资源加载/保存、场景重开、异步任务、重载、退出及既有网络场景。
  使用 macOS/aarch64、Godot 4.7.2；测试入口已不包含文件专属场景。
- `./scripts/check.sh`：默认 quick Debug 全部通过，包含格式、Clippy、默认测试、通用文件三后端用例及短输入基准正确性。
- `git diff --check` 和相关文档相对链接目标检查通过；未运行 Release、额外穷举或其他平台验收。

## 14. 路径抽象与后续文件能力设计

2026-10-18：本节为待实现设计，阶段名为 FS6P，先于 FS7/FS8。当前 `fs.open` 仍只消费
String；`joky/path`、Path 和 IntoPath 尚未提供。本节签名是接口草案，不是可运行示例。
实施完成后，将实际用户接口归入 language.md，并在本文记录验证证据。

### 14.1 拥有型 Path

新增 `joky/path` 模块，导出拥有型记录 `Path`，首版私有存储为一个 UTF-8 String。
Path 非 Copy，沿用普通记录的移动、借用与清理规则；显式 clone 复制文本。
首版只提供 Path 和普通 `&Path` 借用，不引入独立的 PathBuf 或新的借用类型。
路径只保存数据，没有需要关闭的系统句柄，不引入 PathOwner 一类内部资源。

| 拟议接口 | 契约 |
| --- | --- |
| `Path.from(text: String) -> Path` | 接管文本，保留原始拼写；不检查存在性或操作系统是否接受该路径 |
| `clone(&self) -> Path` | 复制路径文本，原值保持可用 |
| `into_string(self) -> String` | 消费 Path，移出文本 |
| `to_string(&self) -> String` | 返回文本副本 |
| `join(&self, child: &String) -> Path` | 按本机路径规则生成新路径，不修改输入 |
| `parent(&self) -> Option<Path>` | 返回父路径；无父路径时返回 None |
| `file_name(&self) -> Option<String>` | 返回最后的文件名部分；不存在时返回 None |
| `extension(&self) -> Option<String>` | 返回扩展名；不存在时返回 None |
| `is_absolute(&self) -> Bool` | 按本机规则判断是否为绝对路径 |
| `is_empty(&self) -> Bool` | 判断原始文本是否为空 |

以上操作均同步，不访问文件系统、不提交工作池请求，也不要求安装文件服务。
分配继续受语言任务存储预算约束。纯路径构造允许空文本；NUL、长度和具体操作的输入限制
在文件请求边界统一检查。现有 open 的空路径/NUL/4096 字节限制不因增加 Path 而改变。

### 14.2 IntoPath 与文件入口

`joky/path` 拟提供静态 trait `IntoPath`，方法为 `into_path(self) -> Path`。
为 String 和 Path 分别提供实现：String 移入 Path，Path 返回自身；这两种转换都不要求
复制文本。文件操作用普通泛型接受 `P: IntoPath`，不增加语言隐式转换或函数重载规则。

拟议 open 签名为 `open<P: IntoPath>(path: P, options: OpenOptions) @suspends -> Result<File, IoError>`。
字符串字面量、String 变量和 Path 变量都可以直接传入；按值传参消费拥有者，保持现有
open 的所有权习惯。调用后仍需使用原值时显式传入 clone 的结果。
首版不承诺直接接收 `&String` 或 `&Path`；当前挂起函数也不提供同步调用的自动共享借用。

实现先通过 IntoPath 得到拥有型 Path，再将其文本移交现有内部 open 入口。
后台继续只持有独立拥有的路径数据，不能保留语言借用。静态 trait 与泛型包装不改变
FileServiceV1 的 UTF-8 传输形式、资源交接、取消计费或缺省 Unsupported 规则。
宿主需要具体签名的导出入口时使用具体类型包装，不把泛型模板直接当作宿主 ABI。

后续 create_dir_all、metadata 和原子替换写入沿用同一个路径约束，避免维护字符串与
Path 两套同义入口。文件入口转换必须保留原始文本，不能顺便规范化自定义宿主路径。

### 14.3 平台语法与词法边界

默认 Path 使用执行平台的本机路径语法；AOT 使用产物运行目标的规则，不使用构建机器
的规则。跨平台表示同一程序可使用当地路径规则，不保证同一个绝对路径文本在各平台有效。

- 不统一把反斜杠替换成斜杠；分隔符、根、盘符和前缀按对应平台处理。
- join 是结构化拼接，绝对子路径可以替换前面的路径；Windows 根路径、盘符相对路径、
  UNC 与特殊前缀需要分别定义和测试，不能只用一个“是否以分隔符开头”的判断处理。
- 构造和文本转换保留原始拼写；join、parent 等操作生成结果时的分隔符处理须有明确测试。
  不自动折叠 `..`、转换大小写、解析符号链接或查询当前工作目录。
- 空路径、根、单个相对文件名、末尾分隔符、重复分隔符、`.`、`..`、隐藏文件及末尾点
  都属于接口冻结前的边界矩阵，尤其区分 None 与空字符串/空父路径。
- parent、join 和字符串前缀判断不能证明路径被限制在某个目录内；目录隔离属于独立的
  文件服务安全设计，不由 Path 的词法操作提供。

如需 canonicalize，应放在 fs，采用挂起接口，通过文件服务查询实际文件系统。
它与纯词法路径处理分开，不作为 FS6P 的最小接口。首版不增加显式解析其他平台路径的公共 API。

### 14.4 UTF-8 与嵌入宿主边界

首版 Path 保持 UTF-8，与当前 String 路径及宿主描述符兼容；它不能无损表示所有平台的
原生文件名。把 String 包装成 Path 不会消除这一限制。
在增加 read_dir 之前，必须冻结无法表示的目录项如何报告错误；不得用替换字符制造一个
可能指向其他文件的可操作路径，也不得静默跳过目录项。错误粒度与类型在该阶段确定。
如果要求完整支持任意原生文件名，应先设计原生路径表示及相应宿主 ABI，再实现目录遍历。

Path 词法操作不依赖 FileService，嵌入宿主未提供文件能力时仍可使用；合法文件请求才在
缺省实现上返回 Unsupported。默认 Path 不识别 `res://` 等自定义命名空间。
自定义宿主仍可接收直接传入 fs 的 String，并自行解释原始文本；不得假定本机 Path.join
适用于该命名空间。本阶段不增加 Godot 文件功能或引擎路径适配。

### 14.5 与 FS7/FS8 的衔接和验收

FS6P 分两步：先实现最小 Path 模块与词法测试，再将 open 接入 IntoPath。
生成代码的行为测试覆盖 JIT、解释器和 AOT，验证 String/Path 输入、移动后拒绝使用、
显式复制后复用、文本往返、非法输入错误及无服务时的纯路径操作和文件 Unsupported。
现有文件宿主 ABI 与取消行为须保持回归覆盖；有新增生命周期影响时补相关 Debug 穷举。
平台记录区分词法测试与真实操作系统文件测试，未运行 Windows/Linux 时不得宣称已验证。

FS7 优先加入 create_dir_all：递归创建缺失父目录，已有目录成功，中间组件为文件时失败；
失败或取消不保证回滚已经创建的目录。空路径、并发创建、错误映射及宿主不支持的行为
需在该阶段冻结。目录与元数据系统调用仍在文件工作线程执行，不阻塞调度器。

FS8 在目标同目录创建独占临时文件，完整写入后执行宿主支持的原子替换，使通过目标路径
打开文件的读者观察到完整旧版本或新版本。现有 write_all/write_text 不具有此保证。
原子可见性与断电持久化分别约定，不能把替换成功等同于文件和目录都已持久化；
临时文件清理、替换提交点、取消后结果不确定性、权限/元数据以及各平台能力在 FS8 单独冻结。
不支持原子替换的宿主返回 Unsupported，不能用先删除再重命名模拟原子成功。
