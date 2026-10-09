# 网络运行时开发计划与验收记录

状态：NET0 与 NET1a 已完成；NET1b 正在实施；生产 TCP 模块尚未实现。起点为 `f76a407`。
首轮目标是 CLI、独立 AOT 程序与 Godot 中的 Joky 脚本共用异步 TCP 能力，
复用现有任务树、执行器、错误值与清理规则。NET0 交付契约、行为探针和明确的能力缺口；
它不注册 joky/net，不提供可用于业务代码的 socket 类型或 ABI。

## 1. 已有能力与前置缺口

现有 @suspends、parallel/race、单调定时器、HostExecutor、TaskWaker 及
BorrowingHostV1 的同步 stop 协议可以复用。Godot 的 tasks.drive 通过
VmEngine.poll_tasks 推进同一执行器；CLI 当前使用仅支持定时器/主动让出的阻塞驱动。
参见 [现有调度与驱动](architecture.md#continuation-与调度)、
[异步宿主借用](arena-borrowing-ffi.md#54-挂起宿主接口阶段-6已实现)。

| 边界 | 当前事实 | 生产网络所需工作 |
| --- | --- | --- |
| 外部资源拥有者 | Resource 叶、同步/异步工厂、Result/聚合、容器、持久 State 和唯一回收责任已实现 | 基于通用资源定义 TCP 类型与业务错误 |
| 字节数据 | ResourceIoV1 已实现有界宿主请求、发送快照、接收 Array 交接与峰值计费 | 接入平台 I/O、错误值和库层组合操作 |
| 拥有型聚合跨挂起 | 无引用拥有记录、枚举、元组及资源叶已支持 CPS 存储、参数和返回 | 复用现有清理，不扩大普通容器视图准入 |
| 借用 | Resource 的独占引用可跨 Pending；资源 I/O 用单独描述符，普通容器借用仍限同步 | TCP 首版复用每连接一个活动操作的权限规则 |
| race | 分支要求 @suspends 且返回 Unit，胜出会停止其他分支 | 可直接验证超时取消；返回 Result 的超时包装还需设计，不能假定已有带结果的 race |
| 就绪事件 | 执行器已有去重与失效唤醒；网络事件源未实现 | 把 I/O 就绪、定时器和外部唤醒接到统一驱动，覆盖丢失/迟到/虚假事件 |

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
普通 Array/Slice 借用跨挂起仍未开放。TCP 的 Result/IoError 公共包装在 NET2 实现。

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

Reactor 候选是 Mio；它提供跨平台可读/可写就绪接口。选型以可嵌入、取消边界和平台测试为准，
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
| 后续 | DNS、UDP、TLS、HTTP、WebSocket、读写拆分 | 每项另定协议、取消与平台范围；DNS 不允许阻塞执行器 |

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
