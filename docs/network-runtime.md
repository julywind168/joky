# 网络运行时开发计划与验收记录

状态：NET0–NET4 已完成本机 Debug 开发验证与 Godot 联合验收；NET5a 已完成拆分契约与独立拥有者借用前置，NET5b–NET5d 待实现。已提供共享 TCP 驱动、有界接收缓冲、读满、分隔符、UTF-8 行读取和长度前缀消息读写。
起点为 `f76a407`。
首轮目标是 CLI、独立 AOT 程序与 Godot 中的 Joky 脚本共用异步 TCP 能力，
复用现有任务树、执行器、错误值与清理规则。NET0 交付契约、行为探针和明确的能力缺口；
它不注册 joky/net，不提供可用于业务代码的 socket 类型或 ABI。

## 1. 已有能力与前置缺口

现有 @suspends、parallel/race、单调定时器、HostExecutor、TaskWaker 及
BorrowingHostV1 的同步 stop 协议可以复用。Godot 的 tasks.drive 通过
VmEngine.poll_tasks 推进同一执行器；NET1b 已将 Mio 就绪与外部唤醒接入阻塞/逐帧驱动。
参见 [现有调度与驱动](architecture.md#continuation-与调度)、
[异步宿主借用](arena-borrowing-ffi.md#54-挂起宿主接口阶段-6已实现)。

| 边界 | 当前事实 | 生产网络所需工作 |
| --- | --- | --- |
| 外部资源拥有者 | Resource 叶、同步/异步工厂、Result/聚合、容器、持久 State 和唯一回收责任已实现 | TCP 类型、业务 Result 与连接准入额度已接入 |
| 字节数据 | ResourceIoV1 已实现有界宿主请求、发送快照、接收 Array 交接与峰值计费 | 已接入平台 I/O、错误值和 write_all |
| 拥有型聚合跨挂起 | 无引用拥有记录、枚举、元组及资源叶已支持 CPS 存储、参数和返回 | 复用现有清理，不扩大普通容器视图准入 |
| 借用 | 整个无引用拥有对象的共享/独占引用可跨挂起；元素/Slice 视图仍限同步 | BufferedTcp 复用每连接一个活动操作的权限规则 |
| race | 分支要求 @suspends 且返回 Unit，胜出会停止其他分支 | 可直接验证超时取消；返回 Result 的超时包装还需设计，不能假定已有带结果的 race |
| 就绪事件 | Mio Reactor 已将就绪、定时器、外部唤醒接到统一驱动，具备失效身份与有界推进 | TCP 公共模块与引擎网络联合场景已接入 |

把 token 放入附带空数组或空字符串的记录，不作为“通用外部资源已实现”的证据。
临时宿主持有 socket，也不等于语言已经能返回 Result<TcpStream, IoError>。

## 2. 首版运行与所有权契约

### 2.1 资源

目标 TcpStream/TcpListener 是非 Copy 拥有者，作用域结束自动清理，可消费式显式关闭。
内部资源身份不可由用户构造或转换；关闭后身份失效，旧完成事件不能命中新资源。
资源表若复用槽位，必须区分代次并处理代次耗尽，不能仅使用可复用的 OS fd。

创建和接受连接必须满足连续清理责任：

1. 外部资源取得后立即归宿主操作状态管理。
2. 在语言拥有者成功登记前，失败、预算耗尽或取消均由操作状态负责关闭。
3. 成功交接后，操作状态失去清理权，语言拥有者取得唯一清理权。
4. 交接不能留下“宿主已交出裸 token、语言尚未登记拥有者”的指令切点。

NET0 只验证步骤 1 的操作所有权和 stop 清理、同步 helper 内的拥有型 Result 转移，
以及独立 Array 跨挂起移动。通用外部资源叶、拥有型聚合的 CPS 帧存储、创建/接收的
原子交接及持久 State 准入是 NET1a 的先决工作。
正常 Drop 必须同步、不可挂起；显式关闭的错误报告与 Drop 的无重复清理另行区分。

### 2.2 字节与读写

网络层传输任意字节，不隐式解码 UTF-8。String 转换由文本层负责。
首版采用受预算约束的宿主请求缓冲区：发送先取得快照，接收先写宿主存储，完成时
把已初始化字节交给语言 Array。平台层不得跨 Pending 保留未经授权的语言载荷指针。
缓冲区、队列和连接均有明确上限，宿主暂存与语言副本并存的峰值也要计入额度。

目标 read 返回至多请求上限的字节；TCP 是字节流，一次 read 不保证得到完整消息。
正上限读取返回空数组表示 EOF；零上限应单独规定，不能与 EOF 混淆。
write 允许部分成功，write_all 在库层保留进度。零进度写入不能形成无限重试。
额外的消息分帧、read_exact、编码和协议解析基于这些语义构建。

首版每个连接最多一个活动 I/O 操作，权限覆盖整个等待区间；未来读写拆分需要显式能力，
不能通过复制句柄绕过。NET1a 已锁定下文的资源读写原语，支持 `&mut Resource` 接收者和消费式发送 Array；
整个 Array 对象的借用已可跨挂起；Slice/元素视图仍未开放。TCP 的 Result/IoError 公共包装在 NET2 实现。

### 2.3 完成、错误与取消

连接拒绝、连接重置、普通 I/O 失败和超时用 Result 表达；EOF 单独表达。
WouldBlock 只表示继续等待，不是业务错误。宿主协议错误与运行时故障保持终止性失败，
不能把它们伪装成 EOF、零字节或成功。

每个已启动操作恰好一次 stop，覆盖完成、失败、部分启动失败和取消；未启动分支不调用 stop。
stop 返回前撤销就绪登记、结束全部外部访问并释放操作状态；它先于父任务恢复、任务数据清理
和所属 State 析构。若平台操作只能异步撤销，必须由宿主拥有的缓冲区和关闭状态吸收迟到完成，
不得继续访问语言内存，也不能破坏现有同步 stop 契约。

取消只停止后续操作，不撤销已发生的 I/O。发送取消前可能已有字节抵达对端；read_exact
被取消前可能已消费前缀。每种组合操作要明确部分进度、连接是否仍可用及重试策略。
超时使用单调真实时间；Godot 的暂停和 time_scale 不改变网络截止时间。暂停期间脚本是否继续
推进须服从实例驱动策略，恢复后检查已到期请求，不能凭暂停时长重置网络超时。

## 3. 驱动与 Godot

平台 I/O 与任务执行器分层；就绪来源仅排队唤醒，脚本在所属执行器线程恢复。
不允许网络回调重入正在运行的脚本，或在后台线程访问 Node、帧、arena 和 State。

| 环境 | 驱动要求 |
| --- | --- |
| CLI / 独立 AOT | 没有就绪任务时等待 I/O、最近截止时间或外部唤醒；有任务则有界推进；消除登记与睡眠之间的丢失唤醒窗口 |
| 嵌入 VM | 暴露非阻塞推进及宿主等待接入点，宿主继续掌控事件循环 |
| Godot | 每帧有界、非阻塞收取网络事件并调用 poll_tasks；节点释放、脚本替换及实例关闭走所属任务取消路径 |

不能只给 CLI block_on 增加 socket 等待就宣布 Godot 已支持网络。
首轮生产验收必须在 Godot 实际场景中验证帧继续运行、连接收发、超时、节点删除和脚本替换。
NET0 的实例测试使用 Godot 同样调用的 VM 入口，只证明该生命周期边界，不能替代引擎集成测试。

NET1b 已选用 Mio 1.2.4，使用跨平台可读/可写就绪接口及内核 Waker，
不改变语言任务执行器。必须处理虚假就绪，并在需要时读写至 WouldBlock，或把尚未处理完的
就绪工作保留到下一轮，兼顾事件模式与公平预算。
参见 [Mio Poll 官方契约](https://docs.rs/mio/latest/mio/struct.Poll.html)。NET0 不增加该依赖。

## 4. 阶段与门槛

| 阶段 | 工作 | 完成门槛 |
| --- | --- | --- |
| NET0：可行性与契约 | 现有能力探针、真实 loopback 字节读取、取消和失效唤醒；记录前置缺口 | 三后端执行；明确已验证事实和未实现能力；无网络专用借用豁免 |
| NET1a：资源与请求基础 | 通用外部资源拥有叶、拥有型聚合 CPS 帧存储、原子交接、有界字节请求、挂起期间接收者权限 | Result/聚合跨挂起及 State 存储、创建/接收所有指令切点、失败和关闭清理；锁定公开 API 与 ABI |
| NET1b：事件驱动 | 网络就绪与定时器/外部唤醒整合，CLI 等待与宿主非阻塞入口 | 虚假就绪、丢失/迟到事件、槽位复用、公平性与空闲不忙轮询；Godot 驱动接线 |
| NET2：TCP 核心 | IPv4/IPv6 地址、bind/listen/accept/connect、字节 read/write、shutdown/close | 真实本地客户端/服务器；部分 I/O、EOF、拒绝/断连、结果错误；JIT/解释器/AOT |
| NET3：组合与联合验收 | write_all、超时及并发连接示例、背压与额度；Godot 场景 | 取消前缀语义、重载/销毁、帧不中断、泄漏与过期事件；开发默认 Debug 验证 |
| NET4：缓冲与消息边界 | 有界接收缓存、完整读取、文本行和长度前缀消息 | 三后端真实程序、背压及协议取消、Godot 联合验收；已完成 |
| NET5：TCP 读写拆分 | 独立拥有的接收端/发送端，同一连接同时读写 | 方向权限、独立就绪与取消、半关闭、共享资源计费及 Godot 联合验收；分步推进见下文 |
| 后续 | DNS、TLS、HTTP、WebSocket；UDP 不在当前推进范围 | 每项另定协议、取消与平台范围；DNS 不允许阻塞执行器 |

NET1a 是探针确认后补出的依赖，先完成它再推进 NET1b/NET2。仅有 NET0 成功测试不足以跳过它。
公开首版平台范围须在 NET1b 选型时列明；NET0 的跨后端通过不等于所有操作系统已验证。

## 5. NET0 探针与验收记录

源码见 [net0.jk](../tests/fixtures/net0.jk)，共享宿主见
[net0_host.rs](../tests/fixtures/net0_host.rs)，VM 验收见
[net0_tests.rs](../src/vm/net0_tests.rs)，独立 AOT 程序由 [CLI 测试](../tests/cli.rs)链接执行。

宿主在测试驱动中创建 127.0.0.1 临时端口的真实 TCP 连接，语言 poll 内仅执行非阻塞读取。
已接受的 socket 由操作状态拥有，stop 关闭；监听器和对端由测试驱动管理。
完成时经过固定、显式 #repr(C) 请求复制四个字节，验证 0、128、255、42，无文本编码假设。
为验证部分进度，探针累计四个字节才成功，语义类似 read_exact；它不是首版 read 的接口。
该固定布局是探针协议，不是公开可变长字节 ABI；手工投递真实就绪不代表已经有生产 reactor。
每个操作最终关闭其 socket，尚未证明取消后持久连接的复用。

| 探针 | 证据目标 |
| --- | --- |
| 完成与部分读取 | 两次发送、跨 Pending 累积前缀；额外轮询不重复启动，不重复消费字节；字节经同步 Result 解包后，以 Array 跨 yield 转移 |
| EOF / 业务错误 | 已收两个字节后 EOF 或注入 I/O 错误转为 Err，父任务继续处理，保留已消费计数 |
| 失败 | 部分启动后宿主失败、Pending 后宿主失败、兄弟分支运行时失败，均先 stop/关闭再析构任务数据 |
| 超时 / 取消 | 可控单调时钟驱动 race；取消组后无活动 I/O，迟到唤醒不恢复终态任务；未启动分支无宿主回调及 stop |
| 重载 | VM 重载并释放后，旧任务仍可完成或取消，原回调和清理代码保活 |
| 实例关闭 | poll_tasks 在无事件时不推进；close_instance 先停止网络操作再析构 State，任务与实例计数归零 |
| 拒绝用例 | 固定现有 Drop、opaque 聚合/Result、Array/Slice 异步宿主 ABI、拥有型聚合跨挂起、局部借用和非 Unit race 等九个缺口，检查诊断及源码位置 |

NET0 当时没有把 TcpStream 放进 Result 的正例：该阶段的类型系统尚不支持该资源能力。
成功的 Result<Array<UInt8>, IoError> 只在同步 helper 内构造和解包；跨挂起传递的是 Array。
Packet/PendingBytes 的自定义 Drop 也只在同步 helper 内执行，不能替代拥有型记录跨挂起验证。
VM 用例额外检查任务 arena 分配计数归零；AOT 通过宿主事件检查操作恰好清理一次，
未把 VM 的测试分配计数当作 AOT 泄漏证明。

2026-10-09 本机验证通过：

- `cargo test net0_`：4 项 VM 测试覆盖 JIT/解释器，1 项独立链接的 AOT 程序测试；
  共享运行用例覆盖八个入口及显式取消组，九个编译拒绝用例分别检查三后端。
- `cargo fmt --check`、`rustfmt --edition 2021 --check tests/fixtures/net0_host.rs`。
- `cargo clippy --all-targets -- -D warnings`。
- 本地文档链接、锚点与 `git diff --check`。

按独立探针范围验证，未运行无关全量测试、基准或 Release，也未运行 Godot 引擎场景。

## 6. NET1a 分步实施

每步通过相关 Debug 验证后单独提交，依次推进：

1. **拥有型聚合跨挂起**：无引用拥有记录、枚举（含 Option/Result）、元组及容器的
   局部存储、移动参数和返回值。Drop 临时数据使用稳定 CPS 存储；动态分支拥有独立的
   清理快照，父循环复用不覆盖未轮询捕获；返回期间析构失败会回收待返回拥有者。
   不开放局部容器借用跨挂起，不放宽含引用元素、闭包或宿主 ABI 的准入。
2. **通用外部资源拥有者**：独立资源叶、唯一清理责任、聚合与 Result、持久 State。
3. **原子资源交接**：创建到接管之间不存在取消/失败泄漏切点，旧身份不能访问新资源。
4. **有界字节请求**：缓冲区和额度、等待期间的接收者权限，锁定首版 API/ABI。

第 1 步行为用例见 [suspending_owners.jk](../tests/fixtures/suspending_owners.jk)，
覆盖三后端拥有记录、两种 Result 分支、元组、嵌套字段、字节结果、Map、StringBuilder、拥有元素数组、静态 race、动态分支、
运行时失败和返回析构失败；解释器另逐指令恢复后取消，包含显式 yield 后的切点。
NET0 第 5 节保留当时的验收事实，原先两个聚合拒绝探针已由这些成功用例替换。

第 1 步验证通过：`cargo test suspending_owners -- --include-ignored`（含独立 AOT 和逐指令取消），
以及 `./scripts/check.sh` 的完整默认 Debug 检查；未运行 Release。

第 2 步已实现独立 `Resource` 类型叶：由宿主注册名字，语言无法直接构造，非 Copy；
复用拥有值移动、聚合、Result、Array 元素和 State 的清理与存储规则。
`VmEngine::register_resource_type` / `AotCompiler::register_resource_type` 注册类型；
`register_resource_factory` 绑定严格的同步 `(UInt64) -> Resource` 工厂。
`runtime::resource::ResourceFactoryV1` 的非空输出总是交出一个释放责任，即使回调返回失败；
运行时先接管责任再检查状态和分配控制块，失败时调用释放函数。描述符及代码拥有者
随编译模块保留；AOT 链接 `__joky_resource_factory_v1_<prefix><name>` 描述符。
资源控制块不依赖伪造的 String/Array 字段，不公开 Buffer 布局或裸 token 给语言。
普通 opaque 仍保持原有限制；局部借用仍限同步函数，异步资源请求接口留给后续步骤。

行为用例 [resource_owners.jk](../tests/fixtures/resource_owners.jk) 使用真实 TCP 监听句柄，
覆盖移动、聚合、容器、创建部分失败、配额失败、State 重载后关闭、静态/动态分支取消，
并拒绝复制、伪造和未注册的资源 ABI；释放日志记录在实际关闭监听句柄之后。

第 2 步验证通过：`cargo test resource_ -- --include-ignored`（含独立 AOT、逐指令取消、
回调代码存活和配额失败），`./scripts/check.sh` 默认 Debug 检查；追加测试后另跑
`cargo clippy --all-targets -- -D warnings` 和格式检查。未运行 Release 或 Godot 场景。

第 3 步增加 `register_resource_factory_async` 和独立的 `ResourceFactoryCpsV1`：
语言签名固定为 `(UInt64) @suspends -> Resource`，AOT 描述符符号为
`__joky_resource_factory_cps_v1_<prefix><name>`。Pending 必须输出空指针，已取得但未
交出的资源由操作 context 持有；完成输出交入运行时守卫，先同步 stop，再分配拥有者
控制块并交给调用者。错误输出也先 stop 后释放，stop 仍能安全访问最后的宿主存储。
完成后的控制块分配失败也会释放输出。解释器的清理登记不消耗指令预算，因此返回与
登记之间不存在取消切点；JIT/AOT 在同一次 poll 内完成这些动作。

每个资源获得不复用的 `ResourceEvent` 身份；关闭在调用 release 前使事件失效，旧事件
即使仍被宿主持有也不能代表新资源。身份计数耗尽返回错误，不回绕。
[resource_handoff.jk](../tests/fixtures/resource_handoff.jk) 覆盖成功、部分启动、输出后失败、
非法 Pending、空成功、错误通道、未启动/已启动 race，以及交接时 arena 配额失败。

第 3 步验证通过：`cargo test resource_ -- --include-ignored`（三后端及逐指令取消/预算耗尽），
`./scripts/check.sh` 完整默认 Debug 检查；未运行 Release 或 Godot 场景。


### 第 4 步：有界字节请求与接收者权限

`VmEngine::register_resource_io` / `AotCompiler::register_resource_io` 接受资源类型名与
`ByteIoKind::Read` / `Write`。只允许以下精确签名（名字由注册决定）：

| 原语 | 语言签名 | 完成语义 |
| --- | --- | --- |
| Read | `(&mut Resource, UInt64) @suspends -> Array<UInt8>` | 正上限读取最多指定字节，空数组是 EOF；零上限拒绝 |
| Write | `(&mut Resource, Array<UInt8>) @suspends -> UInt64` | 消费输入并先取得宿主快照，返回实际写入数；允许部分写入，非空输入零进度失败；空输入直接返回 0 |

这两个入口是嵌入层原语，不自动注册 `joky/net`。其非 0/1 回调状态表示终止性宿主或运行时
失败，不能用来冒充语言层 IoError、EOF 或成功。TCP 业务错误的 Result 包装、connect/accept
参数、超时组合和 write_all 仍归 NET2；NET1b 负责就绪事件与驱动。

单次请求上限固定为 1 MiB。缓冲区长度与请求管理数据通过 `ExternalCharge` 计入任务 arena
及实例共享峰值预算；接收复制到 Array 时，宿主副本仍计费。创建失败、完成、取消和复制
失败均退还额度。解释器可以移动 arena 对象，退款句柄不保存其裸地址。
同时活动的请求受任务帧/任务树上限及字节预算限制，平台就绪队列与连接准入上限在 NET1b/NET2 设置。

独占借用覆盖首次调用至 stop 结束；同一资源的重叠借用、移动后使用、把资源视图存入容器
或返回本地资源视图会被拒绝。运行时另有独占 lease 防止错误的宿主重入。资源控制块保持
稳定，因此可借用局部资源或拥有记录中的资源字段并跨 Pending，普通局部数据/容器视图
限制保持不变。同一拥有记录的字段借用仍采用保守的整体冲突规则；两个独立资源可并行。
静态分支的权限在 join/取消后归还，动态分支的权限保留至所属 scope 结束。

`ResourceIoV1` 的 poll 取得 opaque `ByteRequest`，可使用自己的字节区域和资源 token，
不能解释帧槽内的语言指针。Pending 不交出语言数据，完成时先同步 stop 再复制结果和恢复
接收者权限。每个请求有独立 `ResourceEvent`，在 stop 之前失效；资源本身的事件在 close
之前失效。事件只能用来识别/丢弃迟到通知，检查 active 并不授权访问已经释放的存储。

首版公开描述符布局与回调约定固定于 [joky_resource.h](../include/joky_resource.h)，
AOT 可用 `resource_header_v1()` 生成带符号前缀的声明；布局变更必须使用新版本。
CPS 帧、Buffer 和 ByteRequest 布局保持不透明。回调代码由 VM 的 owner 参数保活，
独立 AOT 宿主负责维持描述符与代码的完整生命周期。

行为用例 [resource_bytes.jk](../tests/fixtures/resource_bytes.jk) 使用真实回环 TCP，覆盖
任意字节、EOF、部分写入、空输入、非法长度/计数、零进度、取消后复用、字段借用、间接
语言调用、独立资源并行、动态分支 join 后复用及冲突拒绝；解释器扫描每个实际执行的指令切点，验证取消和预算
耗尽。运行时另验证重复 lease 拒绝、接收副本的共享峰值预算与失败后的退款/权限恢复。

资源覆盖赋值和 `mem.replace` 也在三后端及逐指令取消中验证；底层控制块替换明确
释放旧资源并使其事件身份失效，再接管新资源的唯一清理责任。

第 4 步验证通过：`cargo test resource_ -- --include-ignored`（资源、交接、字节请求、
独立 AOT 和相关逐指令取消/预算耗尽），`./scripts/check.sh` 完整默认 Debug 检查
（格式、Clippy、测试与三后端短输入基准正确性）。追加动态分支用例后，另跑
`cargo test resource_bytes -- --include-ignored`、`cargo fmt --check`、
`cargo clippy --all-targets -- -D warnings`；宿主 fixture 格式、本地文档链接/锚点和
`git diff --check` 均通过。未运行 Release、无关长输入穷举或 Godot 引擎场景。

NET1a 四步已完成，下一阶段为 NET1b 事件驱动；公开 TCP 模块与 Godot 网络联合验收
仍分别由 NET2、NET3 交付。

## 7. NET1b 事件驱动

分两步提交：先完成 Reactor、执行器非阻塞接线与取消验证，再接入阻塞驱动、AOT 和
Godot 验收。首版支持范围定为 macOS、Linux、Windows；当前本机只验证 macOS，
其他平台仍需实际运行验收。

第 1 步使用 Mio 1.2.4 的 `os-poll` / `net`：`runtime::reactor::IoSource` 拥有
非阻塞 source，`IoRegistration` 持有一次操作的就绪登记。注册需处于执行器上下文，
使用 `prepare_async` 后交给 `HostExecutor` 或实例任务，确保首次宿主 poll 已有上下文。
停止时先从身份表删除，再同步 deregister；失败的 deregister 使 source 禁止重新登记。
身份不复用、耗尽报错，资源/请求的失效事件和已取消根的唤醒会被丢弃。source 绑定
同一个 Reactor，保活 Poll 至 source 关闭；不跨执行器迁移活跃 source。

每个执行器默认最多 4096 个就绪登记，每次 drive 最多收取 256 个内核事件；余下事件
保留到下一次收取。通知只进入根的去重 FIFO 队列，每轮仍按 max_polls 公平推进，
不在事件收取或后台线程运行脚本。`HostExecutor::poll` 和 `VmEngine::poll_tasks`
自动使用零等待收取，因此可直接用于逐帧驱动。

就绪是提示，宿主回调必须实际执行 Mio I/O；WouldBlock 后 rearm，其他 I/O 错误按
接口协议处理。若有界处理尚未到 WouldBlock，必须保留进度并再次排队唤醒；单次请求
完成时撤销登记，下一次请求先实际尝试 I/O，不能假设内核会再次发送同一就绪边沿。
不能通过裸 fd 绕过 Mio 的跨平台 I/O 状态管理。

用例 [net1b.jk](../tests/fixtures/net1b.jk) 与共享宿主
[net1b_host.rs](../tests/fixtures/net1b_host.rs) 使用真实本地 TCP，覆盖虚假唤醒、EOF、
取消后重新登记、动态分支、定时器竞争、兄弟失败、实例关闭和旧代码保活。解释器另
逐实际执行的指令切点取消。Reactor 单元测试覆盖配额、身份耗尽、失败 deregister、
迟到通知和有界内核批次；独立 AOT 使用同一 fixture 和运行时。

第 1 步验证通过：`cargo test reactor_ -- --include-ignored`（三后端和逐指令取消）、
`cargo test executor_ -- --include-ignored`（执行器生命周期与独立 AOT）、
`./scripts/check.sh` 完整默认 Debug 检查；Godot 绑定层 `cargo check` 通过并更新锁文件。
另检查共享宿主格式、文档链接与 `git diff --check`。未运行 Release 或其他平台验收。

第 2 步将 `HostExecutor::wait` 与 `VmEngine::wait_tasks` 作为宿主等待入口：不运行脚本，
返回后再调用 poll。Rust `block_on` 使用同一执行器；CLI 通过 prepare_async 延迟首次
poll，C/AOT `joky_cps_block_on` 在首次原生 poll 前建立时钟/唤醒/Reactor 上下文。
已开始的普通 continuation 仍可接管；I/O 和首次捕获外部令牌需要未轮询根。

等待入口先附加内核 Waker，再复查去重队列：早于附加的通知留在队列中，晚于复查的
通知打断内核等待。等待覆盖所有根的最近截止时间，队列已有工作或截止时间已到则立即
返回；虚假/失效通知不会完成 I/O。没有定时器或保留唤醒令牌的未知 Pending 仍终止报错。
虚拟/暂停时钟默认不允许阻塞等待；`Clock::allows_blocking_wait` 仅由真实单调时钟声明，
已到期时仍可直接返回。Godot 的现有 SystemClock 和每帧零等待 poll 保持主线程驱动。

新增行为验证覆盖真实延迟回环发送、C/AOT 阻塞入口、等待前及等待期间的跨线程通知、
在最后一次队列检查后同步发布通知的确定性竞态、长定时器被外部通知打断、虚拟时钟
等待拒绝/到期恢复，以及多个就绪根受 max_polls 约束并在 VM 销毁后保活旧代码。
测试发送线程只访问自己拥有的对端 socket；不访问脚本、Node、帧或请求存储。

第 2 步验证通过：`cargo test net1b_ -- --include-ignored`（含独立 AOT 原生阻塞入口与
逐指令取消）、`cargo test runtime::driver -- --include-ignored`（外部通知与确定性竞态）、
`cargo test executor_ -- --include-ignored`、`cargo test resource_bytes -- --include-ignored`
（资源请求取消/预算耗尽）、`./scripts/check.sh` 完整默认 Debug 检查。
`./integrations/godot/check.sh` 在本机 Godot 4.7.2 通过格式、Clippy、8 项单元测试、
Debug 构建及现有编辑器/运行场景；覆盖异步任务、暂停/缩放、节点销毁、脚本替换、
带活任务退出和 borrowed dyn。宿主格式、本地链接/锚点及 `git diff --check` 通过。
未运行 Release、Linux/Windows 实机或公开 TCP/Godot 网络联合场景。

NET1b 两步已完成；NET2 已接入 TCP 公共接口，NET3 再补引擎网络联合验收。


## 8. NET2 TCP 公共模块

`joky/tcp` 已提供 IPv4/IPv6 数值地址（含 scope ID）、绑定并监听、accept/connect、
local_addr/peer_addr、read/write、shutdown 和消费式 close。公共签名、错误类别及调用约束
集中见 [语言模块说明](language.md#内置-tcp-模块-jokytcp)。不隐式执行 DNS。

资源控制块与有界字节请求沿用 NET1a 的拥有、独占和峰值计费模型。
内部适配器按完整签名检查，每次资源/数组输出只有一个拥有通道，错误以独立 UInt64
输出存储交付；由 Joky 包装成 Result。这个内部协议不导出 C 描述符，也不授予普通
extern/C 聚合 ABI 权限。AOT 只链接运行时助手，不将宿主地址写入对象；CLI 与 Godot
使用同一编译器/VM 入口，无需另一个网络执行器或逐脚本 socket 注册。

创建失败的内部空资源和读取失败的空数组只承担原子拥有交接；错误输出决定公共 Err
分支并释放临时拥有者，不能被公共 API 观察成成功或 EOF。运行时预算、协议或驱动故障
直接终止任务，copy-out 预算失败会先同步撤销登记再退还快照与 lease。
正常 Drop 不挂起；显式 close 完成本地关闭，shutdown 的可报告系统错误走业务 Result。

真实并行回环与逐指令取消暴露并修复了一个驱动问题：根的预算切片可能先消耗就绪通知，
尚未轮到对应 Pending 子任务。就绪提示现在保留到该操作 rearm/stop，每轮有界旋转重放，
不会依赖另一个内核边沿；失效事件和已停止登记不重放。公共取消后可在同一执行器复用资源。

验收用例 [tcp.jk](../tests/fixtures/tcp.jk) 在 JIT、解释器和独立 AOT 上覆盖 IPv4/IPv6
客户端/服务器、任意字节、部分读取、EOF、端点查询、空写、读限制、端口冲突、连接拒绝、
半关闭后的写错误、显式关闭及 race 取消后复用；连接拒绝使用单独的
[tcp_refused.jk](../tests/fixtures/tcp_refused.jk) 模板，由测试注入受保留端口。运行时另用真实缩小发送缓冲验证部分
写入前缀与断连 RST，检查 IPv6 scope 位与读复制峰值配额失败/退款。
拥有者和 arena 探针覆盖 Pending 取消、重载后 VM 销毁、每个解释器实际指令边界的交接清理；
签名伪造、资源复制、并发别名和私有字段构造均被拒绝。

验证通过：`cargo test net2_tcp_ -- --include-ignored`（9 项库测试含逐指令取消穷举，
2 项 CLI 三后端验收）、`cargo test reactor_ -- --include-ignored`、
`cargo test resource_ -- --include-ignored`，以及 `./scripts/check.sh` 默认 Debug 检查。
最后收敛了 TCP 内部成员可见性、计费和连接完成检查，补跑对应 TCP、source_modules、
source cache、Reactor 测试及全目标 Clippy/格式；没有重复无关基准或压力模型。
Godot 4.7.2 的既有 `./integrations/godot/check.sh` 场景回归及最终绑定层 Clippy 通过。

连接完成需先观察 writable 就绪，再检查 SO_ERROR 与 peer_addr；早期 peer_addr 错误
不作为最终业务结果。拒绝连接用例在系统临时端口范围外保留同端口的 IPv6 监听器，
连接未监听的 IPv4 端点，避免并行测试端口复用与 TCP 自连接。未监听但已 bind 的 socket
在部分平台会丢弃 SYN 并超时，因此不以它模拟 ConnectionRefused。

实际运行平台为 macOS；Linux/Windows 实机和 Godot 网络联合场景尚未验收，后者仍属 NET3。
未运行 Release。


## 9. NET3 组合与联合验收

write_all 在 Joky 库层实现，保留发送偏移、按实际写入量推进，并在分块之间 yield。
公开签名、输入上限、取消前缀及重试约束见 [TCP 模块说明](language.md#内置-tcp-模块-jokytcp)，
额度转移和峰值计费见 [运行时架构](architecture.md#非阻塞执行器与实例任务)。
NET3 阶段的实现使用同步 helper 按值接收并归还原数组；后续整对象借用能力见下节。

本轮发现并修正两处问题：

- 原有 4096 就绪登记上限不覆盖空闲 socket，容器/State 可以持续保留已创建的连接或监听器。
  现在同一执行器的监听器、连接和正在创建/接受的预留名额共有 4096 上限，系统调用前准入，
  随拥有交接转移，失败、取消、close 和 Drop 同步归还；超额是终止性 ResourceLimit。
- 原 NET2 测试把一次 read 的尾部结果假定为完整的 3 字节。现在按实际长度累计，
  对 EOF 单独断言，避免把 TCP 分段时序误判为运行时错误。

[组合用例](../tests/fixtures/tcp_composition.jk) 在三个后端运行三个并发客户端，验证
非整块长度、字节顺序、EOF、空 write_all 和写错误传播；真实 sleep_ms 与 accept 组成 race，
超时后复用同一监听器。发送与 yield_now 的 race 还验证已发送的是严格前缀、取消后借用释放，
并在同一连接写入终止标记，排除重复前缀、越界重发和永久占用。
这些用例也是当前 Unit 分支约束下的并发/超时示例，不宣称已有带结果的通用 timeout。

运行时测试用真实缩小发送缓冲制造 WouldBlock，等待宿主写请求实际 Pending 后取消，
检查暂存退款、登记归零、前缀不变，以及迟到可写事件不恢复旧根；随后复用连接发送。
准入测试同时覆盖空闲监听器、上限拒绝、close 退款、Pending accept 取消及拥有者发布预算失败。
短 write_all 已加入原 TCP 的解释器逐指令取消扫描，覆盖同步分块 helper 和拥有元组转移。
VM 侧检查所有者/arena 计数归零；独立 AOT 执行同一组合行为，未使用堆泄漏检测器。

[Godot 脚本](../integrations/godot/demo/network.jk) 与
[GDScript 对端](../integrations/godot/demo/tests/network.gd) 在真实 Godot 4.7.2 中验证：
两个实例、分段输入和非 UTF-8 响应、空闲等待期间帧继续推进、暂停及 time_scale=0.01 下
真实单调超时、取消读后复用连接、reload 保留旧代码/State、新实例使用新快照、12 轮
脚本移除或节点销毁后的迟到数据、对端关闭以及任务/实例计数归零。
已接入 Godot check.sh 的既有场景集合；不增加第二套 Godot 网络执行器。

2026-10-09 验证通过：

- `cargo test net3_`：3 项库测试及 1 项 CLI 三后端组合验收。
- `cargo test net2_tcp_ -- --include-ignored`：9 项库测试（含扩展到 write_all 的逐指令取消穷举）
  和 2 项 CLI 三后端回归。
- `./scripts/check.sh`：格式、全目标 Clippy、1078 项库测试、122 项 CLI 测试、19 项模块常量测试、
  23 项源码模块测试及三个后端的短 while/Map 基准正确性；默认忽略的 34 项库测试、5 项 CLI 测试
  未作为全量压力验证执行，上述 TCP 穷举单独运行。
- `./integrations/godot/check.sh`：格式、Clippy、8 项 Rust 测试、Debug 构建、全部既有引擎场景
  与新 NET3 网络场景；实际引擎版本为 4.7.2。
- 修改文档中的 294 个本地链接/锚点及 `git diff --check`。

实际运行平台仍为 macOS；Linux/Windows 实机、Release、TLS/DNS、网络延迟/吞吐基准
及任意用户脚本的毫秒级帧预算不在本轮验证范围。


## 10. 网络缓冲前置：拥有值与挂起组合

分两步实现并分别提交：先开放挂起函数同步片段中的短借用，再开放整个无引用拥有对象的
共享/独占引用跨挂起。具体语义见[语言说明](language.md#挂起函数中的拥有值借用)，
来源推导与 CPS 存储不变量见[架构说明](architecture.md#拥有值引用与挂起)。

无需增加网络后端或改变宿主 ABI，即可在语言层定义含 TcpStream、Array<UInt8> 和偏移的
BufferedStream，并通过异步 `&mut self` 方法读取、更新缓冲和推进偏移。
[真实回环用例](../tests/fixtures/async_owner_tcp.jk) 在 JIT、解释器、独立 AOT 中验证跨多次读取、
不同 TCP 分段与 EOF 后状态一致。用例是下一步缓冲/framing API 的基础，不是已发布的标准库接口。

该轮同时补齐普通 `&Int32` 元素视图的挂起检查，并为 helper 返回视图传播容器内部来源。
[组合用例](../tests/fixtures/async_owner_views.jk) 覆盖异步方法、递归、字段替换、共享分支、
race 取消后复用和动态 scope；[清理用例](../tests/fixtures/async_owner_drop.jk) 验证正常组合、
失败、Pending 取消及未启动分支的析构顺序。元素/Slice 借用、含引用拥有存储和宿主 Array ABI 仍未开放。

2026-10-10 验证记录：

- 新增 12 项库用例（含两组 Debug 逐指令取消穷举）和 4 项 CLI 三后端用例均已通过；
  后续补充的拥有枚举/元组及临时 String 实参跨挂起用例，重跑对应执行、取消和 AOT 入口通过。
- 默认 Debug 检查项已完成：格式、全目标 Clippy、1088 项非忽略库测试、126 项非忽略 CLI 测试、
  19 项模块常量、23 项源码模块及三后端短 while/Map 基准正确性。
  `./scripts/check.sh` 首轮在旧拒绝断言处停止；10 项库测试和 1 项 CLI 测试原本要求
  禁止整个拥有对象的引用跨挂起，更新为仍不支持的边界后定向补跑通过，并执行剩余检查项。
  其间实现未再修改，没有重复全量测试；不将首轮脚本记录为一次性通过。
- `./integrations/godot/check.sh` 通过：8 项 Rust 测试、Debug 构建和全部引擎场景，包括 NET3 网络场景。
- 本轮仅在 macOS Debug 验证；未运行 Release 或全部忽略压力测试。


## 11. NET4 缓冲与消息边界

本阶段沿用 TCP 传输与共享执行器，不扩展 UDP。公共语义集中见
[BufferedTcp 说明](language.md#有界接收缓冲-bufferedtcp)。计划拆成四步，每步单独验收和提交：

| 阶段 | 交付 | 状态 |
| --- | --- | --- |
| NET4a | BufferedTcp、接收上限、消费偏移、EOF/错误状态、独立拥有的部分读取结果 | 已实现 |
| NET4b | read_exact、read_until、read_line；区分正常 EOF、消息截断、上限和 UTF-8 错误 | 已实现 |
| NET4c | 四字节大端长度前缀的消息读取/写入；分配前检查消息长度 | 已实现 |
| NET4d | 分段/合并消息、背压、协议取消、真实程序示例与 Godot 联合验收 | 已验收 |

NET4a 在原 joky/tcp 源码模块中实现，不修改编译器、运行时 ABI 或 Godot 绑定。
只在缓存耗尽时填充；消费偏移避免反复移动尾部，返回值通过独立数组隔离所有权。
读取失败与 EOF 都是持续状态；非法输入先验证，不消耗数据。包装同时转发 write_all 和
消费式 close，使接收缓冲与原连接使用同一个拥有者。未提供绕过缓存取出原 stream 的入口。

取消契约是结束协议连接：应用须 drop/close 包装，不重试未完成消息。
把数组放在 self 中并不能保证逐指令取消安全：底层读返回与缓存发布之间，或偏移推进与
结果交付之间仍可能取消；发送取消也无法撤销已写前缀。后续消息读取不承诺透明续读。

[回环用例](../tests/fixtures/buffered_tcp.jk) 验证不同读取上限、字节顺序、独立返回数组、
重复 EOF、非法上限与构造失败、最大/默认容量、EOF 后写入和显式关闭。
测试不把一次 TCP read 假定为完整的发送块。
[私有探针](../tests/fixtures/buffered_tcp_invariants.jk) 在测试专用标准库快照中预置缓冲，
关闭底层 socket，确定性验证消费剩余缓存，以及 EOF/错误终态不重复读取。
另以[真实 RST 用例](../tests/fixtures/buffered_tcp_reset.jk) 验证读取错误被记住并保留原生错误码。
普通标准库不导出探针或私有状态。VM 用拥有者/arena 计数检查 Pending 取消、源码重载与
VM 销毁后的清理，并对填充、复制和终态的实际解释器指令边界执行取消扫描。
EOF/错误终态探针使用 JIT 与解释器，公开回环路径另覆盖独立 AOT。

2026-10-10 验证记录：

- `cargo test net4a_buffered_tcp -- --include-ignored`：6 项库测试（含 Debug 逐指令取消穷举）
  和 2 项 CLI 三后端用例；真实回环、RST 及取消后关闭都覆盖 JIT、解释器与独立 AOT。
- `./scripts/check.sh` 默认 Debug 检查通过：格式、全目标 Clippy、1093 项非忽略库测试、
  128 项非忽略 CLI 测试、19 项模块常量、23 项源码模块和三后端短 while/Map 基准正确性。
  最后调整填充时的偏移发布顺序，保证取消边界上偏移仍在数组范围内，并增强取消后查询/关闭用例；
  定向重跑 NET4a（含穷举、三后端）、格式与全目标 Clippy，复用其余未受影响的检查结果。
- 修改文档的本地链接/锚点及 `git diff --check` 通过。
- 本轮在 macOS Debug 验证，未运行 Release、全部忽略压力测试或新的 Godot 缓冲联合场景；
  Godot 的 NET4 消息边界与生命周期验收安排在 NET4d。

## 12. NET4b 完整读取与文本行

read_exact、read_until、read_line 在语言库层组合共享的 fill/take_into，实现接收填充与
消费偏移的一处维护；不新增原生接口或执行器。消息结束、上限计数与错误后的消费状态见
[BufferedTcp 公共语义](language.md#有界接收缓冲-bufferedtcp)。Result<Option<T>, ReadError>
把无数据 EOF 与完整的空值分开；不把非空未结束前缀当作完整消息。
目标载荷上限在分配前检查，exact 在收到数据后才 reserve；接收阶段按最多 16 KiB 的块
扫描/复制并在继续前 yield，最终 UTF-8 校验沿用同步的 String.from_utf8。
消息边界处理不需要逐字节 socket read 或每次移动已消费前缀。

[完整读取用例](../tests/fixtures/buffered_tcp_frames.jk) 包含超出接收容量的消息、多个消息
连续读取、单字节/NUL 分隔符、刚好到上限的结束符、超长后缓存尾部、空值与 EOF、
截断字节数、CRLF、分片 UTF-8、两种 UTF-8 错误及错误后的下一行。
并行生产者用延迟写入切开 exact 数据与多字节字符；消费者验证完整边界与重复 EOF。
未结束前缀的等待覆盖三种接口，取消后关闭、重载/VM 销毁检查拥有者与 arena 归零。
短组合用例另做逐指令取消扫描，覆盖累积数组、嵌套 Option/Result、UTF-8 转换失败与清理。

测试专用标准库沿用[私有探针](../tests/fixtures/buffered_tcp_invariants.jk)，通过关闭底层
socket 验证缓存余量、EOF/错误终态和相邻行隔离；标准库普通接口不导出这些探针。
独立缓存探针让 exact 与分隔符读取分别跨过 16 KiB 分块边界，验证 yield 前后的余量和结束符。
真实 RST 模板分别调用三种完整读取，检查 IoError 原码与后续持续错误；公开行为也在
JIT、解释器、独立 AOT 中验证。Godot 消息与生命周期联合验收仍安排在 NET4d。

本轮另观察到现有借用来源分析的保守限制：一起通过拥有元组/Result 返回的两个 socket，
消费拆构后分别进行并行可变借用仍可能被判为冲突。并行生产/消费用例通过分别 connect、accept
获得独立拥有者；NET4b 未修改借用分析，该限制在下文 NET5a 中处理。

NET4b 开发验证：

- `cargo test buffered_tcp -- --include-ignored` 通过：NET4a–NET4b 共 11 项库测试，
  含两组逐指令取消穷举；4 项 CLI 用例覆盖 JIT、解释器与独立 AOT 的回环及真实 RST。
- 补充独立 16 KiB 缓存探针后重跑上述定向测试、`cargo fmt --check` 和
  `cargo clippy --all-targets -- -D warnings`，均通过。
- `./scripts/check.sh` 默认 Debug 检查通过：格式、全目标 Clippy、1097 项非忽略库测试、
  130 项非忽略 CLI 测试、19 项模块常量、23 项源码模块和三后端短 while/Map 基准正确性。
  补充探针只调整测试，复用其余未受影响的检查结果。
- 修改文档的 206 处本地链接/锚点及 `git diff --check` 通过。
- 本轮在 macOS Debug 验证，未运行 Release、全部忽略压力测试或新的 Godot 消息联合场景；
  Godot 的 NET4 验收仍安排在 NET4d。

## 13. NET4c 长度前缀消息

read_message/write_message 继续在 joky/tcp 源码库组合 read_exact/write_all，不改变原生接口、
执行器或 Godot 绑定。线上格式是四字节大端 UInt32 长度加二进制载荷；合法载荷至多 1 MiB，
调用者可以指定更小上限（含 0）。返回值、长度限制、截断计数与取消约束集中见
[BufferedTcp 公共语义](language.md#有界接收缓冲-bufferedtcp)。

接收先验证参数，消费固定大小的头，按 UInt64 解码后比较上限，最后才调用 read_exact
累积载荷。拒绝超限声明后保留缓存尾部，不自动丢弃或把它解释为下一条消息。
发送在写头前验证全部参数；头与载荷分开发送，避免整体副本和「1 MiB 加四字节」超出
原有 write_all 限制。拥有输入在成功、错误或取消时沿用任务存储清理。

[消息用例](../tests/fixtures/buffered_tcp_messages.jk) 验证原始大端头、空消息/EOF、二进制载荷、
头与载荷跨读取/延迟写入、相邻消息、1–3 字节头截断、完整头后缺失/截断载荷、非法参数
不消费或发送、0 上限、UInt32 最大声明长度，以及完整 1 MiB 载荷和超出硬上限的发送拒绝。
写方向已 shutdown 的真实连接验证空消息仍须发送头，并保留原写入错误。
头/载荷等待分别覆盖 race 取消后关闭、任务取消、源码重载与 VM 销毁；短收发组合另做
解释器逐指令取消扫描，检查 socket 和 arena 归零。

[私有探针](../tests/fixtures/buffered_tcp_invariants.jk) 以关闭的底层 socket 和预置缓存验证
超限时仅消费头、消息 EOF 及载荷阶段的持续 I/O 错误。
在 8 KiB 的任务存储预算内拒绝近 4 GiB 的 UInt32 最大声明长度，验证拒绝先于按声明长度分配载荷。
公开回环和真实 RST 另覆盖 JIT、解释器与独立 AOT；NET4d 将继续做真实程序和 Godot 联合验收。

NET4c 开发验证：

- `cargo test net4c_buffered_tcp -- --include-ignored` 通过：6 项库测试（含逐指令取消穷举）
  和 2 项三后端 CLI 用例，覆盖完整 1 MiB 载荷、真实回环、RST 与写方向 shutdown。
- 共享探针变更后，`cargo test buffered_tcp -- --include-ignored` 通过：NET4a–NET4c 共
  17 项库测试（含三组逐指令取消穷举）和 6 项三后端 CLI 用例。
- `./scripts/check.sh` 默认 Debug 检查通过：格式、全目标 Clippy、1102 项非忽略库测试、
  132 项非忽略 CLI 测试、19 项模块常量、23 项源码模块和三后端短 while/Map 基准正确性。
- 修改文档的 209 处本地链接/锚点与 `git diff --check` 通过。
- 本轮仅在 macOS Debug 验证；未运行 Release、全部忽略压力测试或新的 Godot 消息场景。
  真实程序、背压和 Godot 联合验收继续安排在 NET4d。

## 14. NET4d 真实消息程序与联合验收

[回声程序](../examples/tcp_messages.jk) 并行驱动有限 TCP 服务与客户端：顺序交换 ASCII、
UTF-8 与空消息，区分空载荷与 EOF，通过关闭客户端连接结束服务；输出与运行方式见
[公共模块说明](language.md#有界接收缓冲-bufferedtcp)。程序在 JIT、解释器与独立 AOT 中验证。
现有 NET4b/NET4c 分片、合并消息与长度边界用例继续作为基础回归，本轮未修改其实现。

[原生对端](../tests/support/tcp_messages.rs) 限制接收窗口，并在宿主许可前不读取任何数据。
[发送用例](../tests/fixtures/buffered_tcp_pressure.jk) 连续写入 128 条 64 KiB 消息；测试观察
已完成部分消息、任务仍存活且执行器连续没有就绪根，确认实际 socket 背压，而非仅遇到
协作 yield。放开接收后核对所有大端头、序号和载荷；另在相同 Pending 状态重载、销毁 VM
并取消任务，验证 socket/arena 归零、对端收到的完整前缀，以及迟到写就绪不恢复旧任务。
CLI 三后端使用同一发送用例的短输入，另通过 race 留下不完整载荷、显式关闭并核对原始字节；
完整消息为 64 KiB 载荷加四字节头，取消后不向同一连接重发消息或追加恢复标记。

[Godot 脚本](../integrations/godot/demo/network_messages.jk) 与
[GDScript 对端](../integrations/godot/demo/tests/network_messages.gd) 组合消息与文本行。
跨引擎帧拆分头、载荷和 UTF-8 字符，合并后续空消息、CRLF/NUL 行和空行，逐字节核对回复。
两实例独立完成；等待时引擎继续推进；超限头和 EOF 截断通过业务错误结束连接。
暂停/缩放下的消息截止时间仍由真实单调时钟驱动，race 取消后关闭协议连接。
reload 保活旧任务/State 方法，新实例使用新快照；12 轮头/载荷等待中移除脚本、退出树或
销毁节点，迟到数据不触达旧实例，任务与 State 根归零。真实引擎流程纳入 Godot check.sh。

本轮验收未发现需要调整 NET4a–NET4c 实现的回归；没有修改编译器、运行时 ABI 或绑定。
NET4b 记录的拥有元组来源分析限制在 NET4d 时仍待处理。本轮新增示例、独立测试与验收驱动，
按相应范围验证，复用 NET4c 的实现检查结果，不因阶段完成重复主仓库整套检查。

NET4d 开发验证：

- `cargo test net4d_buffered_tcp` 通过：2 项库测试覆盖 JIT/解释器的实际背压、恢复、
  Pending 取消与 VM 重载/销毁；2 项 CLI 用例覆盖 JIT、解释器与独立 AOT 的示例输出、
  原始消息字节和取消后关闭。补充迟到写事件断言后重跑上述用例通过。
- `cargo fmt --check`、原生对端文件的 `rustfmt --check` 和
  `cargo clippy --all-targets -- -D warnings` 通过。
- `./integrations/godot/check.sh` 通过：扩展格式/Clippy、8 项 Rust 测试、Debug 构建和全部
  真实引擎流程，包括 NET3 回归与新增 `NET4 GODOT PASS`；使用 macOS arm64 的
  Godot 4.7.2 标准版，包含原有实际渲染检查。
- 修改文档的 238 处本地链接/锚点与 `git diff --check` 通过。
- 未运行 Release、主仓库整套检查或全部忽略压力测试；本轮没有核心实现变更，复用前阶段
  已通过的相关 Debug 实现检查与取消穷举结果。Linux/Windows 未在本机运行。

## 15. NET5 TCP 读写拆分

NET4 支持完整消息，但原 TcpStream/BufferedTcp 的独占借用覆盖整个读写调用。
持续等待输入时无法通过同一拥有者发送命令或回复。NET5 将一个 TCP 连接消费式拆成
不可复制的接收端与发送端：同一连接允许一个读取和一个写入同时等待，单方向仍串行。
CLI、独立 AOT 与 Godot 继续使用同一 reactor/执行器，不增加后台脚本线程。
DNS、TLS、通用带结果 timeout、重连策略及 UDP 不属于本阶段。

### 15.1 分步交付

每步完成相关 Debug 验证后单独提交。NET5b 已实现原始字节拆分；BufferedTcpReader 与
缓存/消息拆分仍属于 NET5c 的目标接口。NET5a 只处理拥有者拆构的借用身份。

| 阶段 | 交付 | 验收门槛 | 状态 |
| --- | --- | --- | --- |
| NET5a | 拆分契约；消费式拆构后独立拥有者的借用身份 | 普通拥有聚合和 Result/元组返回；独立资源并行借用；原别名、内容引用和逃逸规则不被绕过；三后端及相关取消穷举 | 已完成 |
| NET5b | TcpReader/TcpWriter、TcpStream.split、原始字节 I/O、方向 shutdown/close；共享 socket 与双向就绪 | 一个读加一个写实际 Pending；独立完成、取消和迟到事件；半关闭、额度、失败和发布清理；三后端 | 已完成 |
| NET5c | BufferedTcpReader、TcpWriter 消息发送及 BufferedTcp.split | 已预读缓存/偏移/EOF/错误状态全部转移；原 NET4 行/消息语义不变；分片、背压与取消 | 待实现 |
| NET5d | 单连接双向长连接示例及 Godot 联合验收 | 等待推送时发送、写背压时接收；两实例、暂停截止时间、重载、节点销毁与泄漏/迟到事件 | 待实现 |

### 15.2 能力与拥有权

- `TcpStream.split()` 消费完整 stream，签名为
  `@suspends -> (TcpReader, TcpWriter)`。通常首轮完成，不发起网络 I/O；
  内存/配额失败保持终止性运行时错误，不伪装成业务网络失败。
- 拆分后完整 TcpStream 不再可用；两端不可复制、不可由用户构造，也不提供 clone/reunite。
  接收端只提供 read、地址查询及本方向 shutdown/close；发送端只提供 write/write_all、
  地址查询及本方向 shutdown/close。每端使用独占借用，不能启动两个同方向操作。
- 两端分别拥有稳定的资源控制块和独立操作身份，共同保活一个底层 Mio socket。
  不复制 OS fd，不通过两个 TcpStream 绕过权限。内核 I/O 在所属执行器线程依次调用；
  内部短借用在每次尝试后结束，不把 Rust RefCell 借用保持到 Pending。
- 拆分不增加 socket 准入计数；一个底层 socket 始终只占一个名额。共享状态、两个控制块、
  操作状态及字节快照按实际占用计费，共享部分只计一次。共享存储的计费/清理责任须在
  两端进入不同任务或 State 时仍连续保留，不借语言字段复制底层拥有责任。
- 发布两端期间失败或取消必须回收所有已取得资源。拆分前的拥有者、临时共享状态和
  已登记的两端之间不存在未归属窗口；两端及操作 lease 全部结束后关闭底层 socket、退还名额。

### 15.3 方向结束与取消

- 显式 `reader.shutdown()` / `writer.shutdown()` 只关闭本方向，报告系统错误并保留端拥有者。
  成功后本方向不再发起传输；接收返回 EOF，发送返回 NotConnected，地址仍可查询。
  重复成功的本方向 shutdown 为 Ok，不再次调用 OS；失败不发布成功的本地关闭状态。
- 消费式 `close()` 尝试本方向 shutdown，然后无论成功或错误都释放该端；另一端仍可使用。
  Drop 同步执行同样的方向结束与释放，忽略不可返回的系统错误。发送端结束尝试发送 FIN，
  不等待对端确认、不保证完整消息送达；接收端结束不主动结束剩余发送方向。
- 普通操作取消只同步撤销本方向等待、资源 lease 与字节请求，不关闭仍存活的端，
  不撤销另一方向的登记。所属任务清理若随后 Drop 该端，另按上一条结束其方向。
  同一根任务/任务组整体取消仍清理其中全部分支，不承诺另一分支继续运行。
- EOF 与写方向半关闭分别处理。接收 EOF 不自动释放发送能力，允许 EOF 后发送最终回复。
  socket 错误可以分别通过两端的 IoError 报告，不把共享连接故障当成成功或 EOF。
- raw read/write 沿用当前已发生 I/O 不可撤销的约束。完整行/消息读取或消息发送被取消、
  截断或分帧超限时，仍不承诺恢复协议边界。应用须协调收发任务结束并释放两端；
  只丢弃一端不能声称整条连接已关闭，不能盲目重试不完整消息。

### 15.4 就绪与缓冲

NET5b 已将 IoSource/IoRegistration 从单活动登记扩展为最多两个方向等待者。
一个底层 socket 使用一个内核 token，读、写方向各有独立等待身份、ResourceEvent 与 TaskWaker，
内核 interest 为当前活动方向的并集。收取事件按方向排队唤醒；错误/关闭提示唤醒可能受影响
的方向，由实际 I/O 决定结果。虚假就绪仍执行至 WouldBlock，再重置本方向提示。
新方向加入时主动尝试 I/O；取消、完成或 rearm 一方向不清除另一方向已收集的提示。
只有最后一个等待者退出才撤销内核登记；再次登记使用新的 token，不让旧事件命中新操作。
登记额度按活动逻辑等待者计数；同一 socket 的读写同时等待占两个名额，不能通过共享 token
绕过现有上限。修改 interest 或撤销失败时保持失效身份并按运行时失败处理。

`BufferedTcp.split()` 消费已有包装，目标返回 `(BufferedTcpReader, TcpWriter)`。
已有 bytes、offset、capacity、EOF/失败状态随接收端转移，不能丢弃缓存后重新读取 socket。
接收端沿用 NET4 的 read/read_exact/read_until/read_line/read_message 返回类型、上限与错误；
发送端提供 write_message，沿用四字节大端头、消费输入和发送前缀约束。
不建立隐式发送队列，不自动重发；完整 TcpStream/BufferedTcp 原 API 继续可用。

### 15.5 NET5a 借用前置

消费式拆构由受检 Detach 分离清理组。函数调用返回的多个拥有槽可能带有保守的共同来源，
不能将这一共同来源永久当作拆构后两个唯一拥有者的控制块身份。
Detach 先按原来源拒绝仍活跃/保留的借用，再为已分离的拥有子树建立独立分析根；
后续完整对象独占借用可以并行。新根仍属于函数内拥有存储，不能让视图逃逸或取消词法检查。
容器内容的引用依赖单独迁移到新身份，不重建被引用对象，不缩短已保留借用的期限。
含多个字段的单一拥有对象仍按整体冲突规则处理；此步骤不开放记录字段的任意并行借用。

[普通拥有用例](../tests/fixtures/net5_owner_destructure.jk) 覆盖转发调用、Result、嵌套 Option/元组、
同步记录拆构、循环、parallel、动态 scope、race 取消后复用及容器内容引用。
[真实 TCP 用例](../tests/fixtures/net5_owner_tcp.jk) 经挂起 Result 返回两个独立连接，
拆构后并行收发任意字节、验证 EOF 并分别关闭。两连接是回环的一对端点，
这项前置测试不等于 NET5b 在同一 socket 上的双向 I/O 已实现。

NET5a 开发验证：

- 修复前复现调用返回的元组在 `&mut right` 处报告活跃借用冲突；修复后新增普通拥有与
  真实 TCP 用例在 JIT、解释器及独立 AOT 执行通过。
- `cargo test net5a_`：5 项库测试及 1 项 CLI 三后端用例通过；覆盖独立借用、资源边界取消、
  重载/销毁后保活，7 个拒绝用例检查同一拥有者别名、活跃视图、逃逸、移动后使用、
  容器内容的局部引用及单一记录字段的保守冲突。
- `cargo test --lib -- --ignored exhaustive_net5a_ exhaustive_async_owner_ exhaustive_suspending_owners_`
  通过 4 组 Debug 逐指令取消穷举，含新增 Detach 与既有短借用、完整拥有视图及拥有聚合。
- `./scripts/check.sh` 通过：格式、全目标 Clippy、1109 项库测试、135 项 CLI 测试、
  19 项模块常量、23 项源码模块测试，以及三后端短 while/Map 基准正确性。
  默认忽略的 40 项库测试、5 项 CLI 测试未作为全量压力检查执行，上述 4 组单独运行。
- `./integrations/godot/check.sh` 通过：扩展格式/Clippy、8 项 Rust 测试、Debug 构建和全部
  真实 Godot 4.7.2 引擎流程，包括 NET3/NET4 网络与实际渲染回归。未新增 NET5 双向场景。
- 修改文档的 181 处本地链接/锚点及 `git diff --check` 通过。
- 本轮在 macOS Debug 验证；未运行 Release、全部忽略压力测试或 Linux/Windows 实机验收。

### 15.6 NET5b 原始字节拆分

公共 API 见 [语言文档](language.md#独立读写端-tcpreader--tcpwriter)。原 TcpStream/BufferedTcp 接口继续保留。
运行时共享 Connection 保存 Mio source、唯一 socket 许可、两个方向关闭位与建立连接时的地址，
两个端使用独立资源控制块和 lease。共享内存单独计费，端控制与每个操作分别计费。
地址快照避免系统半关闭后 peer_addr 返回 NotConnected，使端的地址查询保持可用。

拆分使用两个私有受检单资源原语，保留现有 CPS 返回 ABI。第一步发布 reader 并将原拥有者
转成负责未发布 writer 的中间状态；第二步发布 writer 后清空原拥有者。中途取消/配额失败
时已发布 reader 和未发布 writer 分别同步清理，没有无人负责的 socket 窗口。
私有半端资源禁止外部 factory 注册，同方向并行借用和完整 stream 的拆分后使用仍由共享 IR 拒绝。

Reactor 为每个 source 保存一个内核 token 与最多两个逻辑等待身份，interest 为并集；
读/写/关闭/错误事件分别路由，rearm 和 stop 只清除自己的提示。最后等待退出才撤销内核登记，
再次登记使用新 token。失败的 OS 登记/更新/撤销失效路由并使 source 不可复用，
仍活跃的对端等待者获得唤醒并观察运行时失败，不能无限停留在 Pending。

验收包括 [回环脚本](../tests/fixtures/tcp_split.jk) 的两端并行发送与接收、实际字节顺序、EOF 后回复、
方向 shutdown/close/Drop、取消后复用和普通 State 拥有聚合；运行时探针填满真实发送缓冲，
验证同一 socket 上读和写同时 Pending、独立完成/取消与迟到就绪。探针另覆盖两次发布的
预算边界失败、共享 socket 许可与最终退款；reactor 测试注入注册/更新/撤销失败并检查对端唤醒。
这些普通聚合用例不新增 VM 异步任务向持久实例 State 传入拥有值的入口。

NET5b 开发验证：

- `cargo test net5b_`：10 项库测试与 1 项 CLI 三后端用例通过；默认忽略的新拆分穷举另行运行。
- `cargo test --lib -- --ignored exhaustive_net5b_ exhaustive_net1b_ exhaustive_net2_ exhaustive_net4a_ exhaustive_net4b_ exhaustive_net4c_`
  通过 6 组 Debug 逐指令取消穷举，覆盖拆分发布、旧字节资源 reactor、TCP 交接以及全部 NET4 读取路径。
- `./scripts/check.sh` 通过：格式、全目标 Clippy、1119 项库测试、136 项 CLI 测试、
  19 项模块常量、23 项源码模块，以及三后端短 while/Map 基准正确性。
  默认忽略的 41 项库测试与 5 项 CLI 测试没有作为全量压力检查执行，上述 6 组单独运行。
- `./integrations/godot/check.sh` 通过：扩展格式/Clippy、8 项 Rust 测试、Debug 构建和
  真实 Godot 4.7.2 全部流程，含 NET3/NET4 网络及渲染回归。新的双向长连接引擎场景仍在 NET5d。
- 本轮修改文档的 246 处本地链接/锚点与 `git diff --check` 通过。
- 本轮仅在 macOS Debug 验证；未运行 Release、全部忽略压力检查或 Linux/Windows 实机验收。
