# Joky Godot 脚本适配器（G0–G2）

普通 `Node` 直接挂载 `.jk` 资源，运行时使用 Joky 字节码解释器。扩展库本身由
Rust 编译成 GDExtension；Joky 脚本不会转成 AOT 产物。
此适配器定位为嵌入验证，不作为持续扩充引擎功能的目标。

## 构建与打开

当前固定 Godot **4.7.x** 扩展 API、`godot`/`godot-core` **0.5.5**。
实际验收目标为官方标准版 **4.7.2.stable.official.ed1daf0bf**、macOS arm64；
不需要 .NET。其他平台的库路径已预留，但尚未验收。

```sh
./integrations/godot/build.sh
/Applications/Godot.app/Contents/MacOS/Godot --editor --path integrations/godot/demo
```

也可以在 Godot 项目管理器中导入 [demo/project.godot](demo/project.godot)。
打开 `main.tscn`，`CounterA` 与 `CounterB` 都是普通 `Node`，共同引用
[counter.jk](demo/counter.jk)。运行时每个节点各自调用 `create()`、`_ready()`。
G1 示例为 [movers.tscn](demo/movers.tscn) 与 [mover.jk](demo/mover.jk)：打开场景，选中
MoverA/MoverB，在 Inspector 中分别配置 `speed`，用 F6 运行可看到两个三角形独立移动。
场景中的说明文字不依赖 GDScript；`demo/tests/*.gd` 和 `demo/addons/joky_acceptance` 仅为引擎验收驱动；
验收插件只在 `JOKY_G0_EDITOR_TEST=1` 时执行检查并退出编辑器。

Cargo 子工程独立于主仓库 workspace，普通 Joky 构建不下载 Godot 依赖。
`build.sh` 将当前平台的 Debug 动态库复制到 `demo/bin/`。
修改 Rust 后退出 Godot、重建再打开；扩展动态卸载/重载已关闭。
`godot-core` 的直接依赖用于 0.5.5 尚未由 `godot::meta` 转出的 `RawPtr` 类型，
必须与 `godot` 保持同版本。

## 脚本约定

- 根类型名固定为 `State`，必须符合 P2 的无引用记录约束，可含 String、无引用 Array 与 Drop。
- `fn create() -> State` 无参数；构造失败不发布实例。
- 顶层函数的第一个参数为 `&State` 或 `&mut State`，经编译器验证为不逃逸的实例入口，
  才能作为 Godot 实例方法调用；接收者由扩展注入，不出现在 Godot 参数列表中。
- `_ready` 若存在，必须只有接收者、返回 Unit。Godot 调用它时执行解释器。
- 方法其余参数及返回值可为现有数值类型、Bool、Rune、Unit。整数严格检查位宽，
  UInt64 返回值超出 Int64 范围会报错；Float32 入参采用 IEEE 窄化，但拒绝有限值变无穷。
  Rune 对应恰好一个 Unicode 标量的 Godot String，Unit 对应 Nil。
- 加载与编辑器验证只编译，不创建状态。编辑器中的脚本实例为占位实例，不运行 Joky。
- 脚本资源共享编译快照；每个节点持有独立 `InstanceId`。删除节点或移除脚本释放根。
  扩展关闭时先关闭仍存活的根，再注销语言及加载/保存器。
- `Script.reload()` 为后续实例编译新快照，现存实例继续使用旧快照及其状态。
  `keep_state` 不表示迁移状态；源码保存不保存运行时 `State`，场景重开重新构造。
- 解析、类型及运行错误提供 `路径:行:列`。方法运行失败沿用持久实例的 Failed 语义；
  重新创建节点才能重新运行该实例。参数转换失败发生在执行前，不改动状态。

同步解释器调用最多 256 帧、100,000 条指令；异步任务最多 256 个同时存活的帧，
每次推进执行至多 10,000 条指令后让出，无任务生命周期总指令上限。此限额不覆盖编译时间和宿主原生调用，
也不是毫秒级帧预算。脚本加载和执行限主线程，不支持线程加载或从 Godot WorkerThread 调用。
绑定启用 `experimental-threads` 以接受编辑器后台扫描的元数据查询；
持久 VM 快照存于主线程局部注册表，Script 资源只保存键，后台释放资源时排队到主线程清理。

G2 已提供所属实例任务、引擎等待和暂停语义。尚无信号载荷映射、工具脚本、脚本继承、
内嵌脚本、用户文件模块解析、自动补全、断点调试和状态热迁移。
此原型尚不具备全面替代 GDScript 的能力；后续阶段见
[持久实例与 Godot 计划](../../docs/persistent-instances-numerics.md#43-godot-适配线)。

## G1 属性与帧回调

导出属性使用三组普通函数，不新增语言语法。例如 [mover.jk](demo/mover.jk)：

```joky
fn default_speed() -> Float64 { 60.0 }
fn get_speed(state: &State) -> Float64 { state.speed }
fn set_speed(state: &mut State, value: Float64) { state.speed = value }
```

`default_` 前缀保留为属性声明；默认函数必须无参数，函数体为单个数值、Bool 或 Rune
字面量（可有负号），不会在编辑器执行。getter 必须直接读取一个 State 标量字段，禁止
引擎回调；setter 必须接收 `&mut State` 和同型标量并返回 Unit，由编译器自动检查
接收者借用不逃逸，无需标注。最多 128 个属性，属性名不能覆盖实际挂载节点的引擎属性。
String、Array、对象和向量的属性导出留待后续，拥有字段仍可保存在非导出的 State 中。

运行时先执行 `create()`，通过 setter 应用声明的默认值，再接受场景保存的属性覆盖；
`_ready` 看到最终配置。读取与写入均调用受检实例方法，不直接访问字段偏移。
编辑器使用原生占位实例和静态默认值；刷新属性保留已有覆盖值，不创建 Joky 根。
PackedScene 保存导出属性的当前值，重开重新构造并应用这些值；其他状态不序列化。
旧实例在脚本 reload 后保留旧布局、属性与代码，新实例采用新快照；不作保状态迁移。

`_process(state: &mut State, delta: Float64)` 与
`_physics_process(state: &mut State, delta: Float64)` 返回 Unit，由引擎正常调度。
每次回调同步结束，不创建隐式异步任务。节点排队删除或实例已经 Failed 时，后续帧回调
跳过，避免重复诊断；普通方法仍拒绝访问失败实例。

## G1 Node API

显式导入 `godot/node as node.{type Node, type Vector2}`。适配器提供固定的内存源模块，
不搜索项目中的同名文件。当前接口为：

| 接口 | 行为 |
| --- | --- |
| `node.none()` / `node.self()` | 空句柄 / 当前调用所属节点的句柄；构造和 Drop 也有所属节点上下文 |
| `node.is_valid(handle)` | 检查当前程序登记、引擎身份及排队删除状态 |
| `node.get_node(handle, &path)` | 查找节点；未找到报运行错误 |
| `node.name(handle)` / `node.set_name(handle, &name)` | 复制 UTF-8 名称 / 设置合法名称；输入文本只借用到本次调用结束 |
| `node.position(handle)` / `node.set_position(handle, vector)` | 读取 / 原子写入 Node2D 的二维位置 |
| `node.translate(handle, vector)` | 在现有 Node2D 位置上增加位移 |
| `node.queue_free(handle)` | 由引擎排队删除；句柄立即视为失效 |

`Node` 是含私有登记 token 的 Copy 记录，可保存在 State；不包含引擎地址、不强持有节点，
Drop 不自动删除节点。token 不复用，并仅能在签发它的 Program 快照中解析；新快照有独立
登记表。每个快照最多 4096 个活登记，签发时清除失效项。该宿主登记表不计入核心实例 arena
额度；它是单独有界的适配器元数据。

`Vector2 { x: Float64, y: Float64 }` 是 Joky 记录。跨边界逐个分量转换到绑定的 Godot `real`
类型，默认标准版为 Float32；允许 IEEE 窄化舍入，拒绝非有限值和有限值变无穷。
两分量及 Node2D 类型均验证成功后才修改位置，不重解释记录内存。

所有访问限主线程。宿主上下文按调用栈恢复，同一实例经引擎信号重入会被核心 Busy 检查拒绝，
另一个实例可正常访问。失效句柄和非法向量触发普通实例执行失败与清理规则；外部引擎副作用
不回滚。删除节点、移除脚本或停止引擎释放状态和拥有字段，旧代码保活至最后一个根关闭。

## G2 异步任务

打开 [async.tscn](demo/async.tscn)，F6 运行。[async_mover.jk](demo/async_mover.jk)
通过 `scope` 的两个 `branch` 分别等待下一帧移动、等待游戏时间改变方向；两节点共享代码，
有独立的 stride 属性、State 和任务树。关闭场景会取消两条分支。

`task_` 前缀声明引擎可启动的任务入口，必须为非泛型 `@suspends` 函数、标量输入和 Unit
结果，不带 State 接收者。Godot 调用 `node.call("task_NAME", ...)` 只登记任务，返回 Nil；
Joky 中用 `godot/task.start(&name)` 启动无参数入口。同步方法（包括 `_ready`）内启动时先保存
有界的启动请求，归还 State 借用后再登记；第一段代码在后续执行器推进时执行。
`_ready` 在同一脚本挂载期间最多执行一次；`request_ready()`、手动重调或移出后再入树不重复启动。
多个显式 start 会创建多个根；一个根内部使用既有 `join`/`race`/`scope`/`branch`。

以下为 [async.jk](demo/async.jk) 的任务访问方式：

```joky
fn task_access() @suspends {
    let getter = "count"
    let setter = "set_count"
    let value = task.read_int32(&getter)
    task.next_frame()
    task.write_int32(&setter, value + 10)
}
```

getter/setter 为当前脚本的经编译器证明借用不逃逸的同步方法，不是字段地址。State 只在每次调用内借用；
等待期间正常 Godot 方法仍可更新它。`task.call` 调用无额外参数、Unit 结果的同步方法；
`read_int32`/`read_float64` 调用无额外参数的同型结果方法，`write_int32`/`write_float64`
调用单个同型参数、Unit 结果的方法。此首版仅提供这三类短访问形式；其他参数组合可放在
无额外参数的同步方法中处理。异步调用错误必须传播，所属实例进入 Failed 并取消其他根。

| API | 唤醒与暂停行为 |
| --- | --- |
| `task.next_frame()` | 订阅 SceneTree process_frame，所属节点 `can_process()` 为真时标记就绪 |
| `task.next_physics()` | 订阅 physics_frame，使用同一节点处理资格检查 |
| `task.sleep_game(seconds)` | 非负有限秒数；游戏暂停停止，遵循 Engine.time_scale |
| `task.sleep_real(seconds)` | 非负有限秒数；继续处理暂停，忽略 Engine.time_scale |
| `node.signal(handle, &name)` | 验证节点及已有信号，返回不保活节点的 Signal 描述符 |
| `node.wait_signal(signal)` | 订阅一次等待；丢弃载荷，发射后标记就绪；信号源退出树时等待失败 |

计时使用单次内部 Timer 子节点，游戏计时显式 PAUSABLE，真实计时显式 ALWAYS 并忽略缩放。
零时长也让出至后续引擎处理，不原地完成；Timer 停止后排队删除，普通 get_children 不显示。
信号描述符每 Program 最多 4096 项，信号名最多 256 UTF-8 字节；与 Node token 一样不复用、
拒绝跨程序快照，登记时清理失效项。这是独立的宿主元数据额度。
每次等待的请求由 CPS 标量槽传入，不在任务 arena 内追加分配；订阅数据在 stop 中释放。
引擎事件回调只更新就绪标记并发送 TaskWaker；Joky 在语言 frame 回调中恢复，
不在信号发射栈内运行，也不保证在引擎的物理回调阶段内恢复。每帧每个 Program 至多推进
64 个就绪根、每根至多一次；没有事件的等待不会被逐帧轮询。
信号和真实计时可以在暂停中恢复；需要游戏暂停语义的流程应使用游戏等待。
核心 `joky/task.sleep` 在适配器中仍使用系统单调时钟；游戏流程用 `godot/task.sleep_game`。

节点退出树后，该脚本挂载永久禁止新任务，并取消现有根及后代；同步 State 保留到脚本移除
或节点销毁。重新挂载脚本/重建节点开始新的任务生命周期。排队删除的节点在下一次驱动前
停止任务；实际移除脚本、节点销毁及扩展退出均先完成 stop，再释放 State 和旧代码。
`race` 输家、失败和取消都同步断开事件连接、停止 Timer；迟到事件不能触达新实例。
任务恢复、任务资源析构及 State 清理都绑定原实例节点上下文；State 析构不能启动新任务。
源码 reload 不迁移等待任务，它们继续访问旧快照的 State 与方法；新节点使用新代码。

调度是协作式的，根数及指令额度不是毫秒帧预算，原生宿主调用必须及时返回。
游戏/真实计时依据
[Timer](https://docs.godotengine.org/en/4.7/classes/class_timer.html) 和
[节点暂停模式](https://docs.godotengine.org/en/4.7/tutorials/scripting/pausing_games.html)；
帧事件依据 [SceneTree](https://docs.godotengine.org/en/4.7/classes/class_scenetree.html)。

## 验证

```sh
./integrations/godot/check.sh
# 其他安装位置：
GODOT_BIN=/path/to/Godot ./integrations/godot/check.sh
```

该命令独立运行扩展格式/Clippy、Rust 行为测试、构建与真实 Godot 进程验收，
末项打开实际渲染窗口并保存 `/tmp/joky-g1.png`，因此需要可用的桌面图形环境。
主仓库的 `scripts/check.sh` 不隐式要求安装 Godot。
引擎测试包含编辑器导入/源码打开/占位实例、未支持的符号查询与补全响应、两个实例的 `_ready` 与独立状态、标量边界、
脚本资源保存、场景保存后另进程重开、反复构造/销毁和错误位置检查。G1 增加属性覆盖/默认值/
编辑器刷新、Float64 两类帧回调、Node2D 移动、失效句柄、向量失败原子性、同步信号重入、
带活根退出进程以及实际渲染画面检查。G2 增加两节点异步上下文、短 State 访问、帧/物理步/
信号等待、暂停/缩放与双计时、race 输家退订、动态 scope、重复 ready、reload 活任务、
切场景、反复销毁、异步失败/信号源退出、State Drop 与带挂起任务退出进程；
异步移动示例另用真实引擎检查位置变化；同步信号中移除脚本的用例检查延迟 State 清理的节点上下文。

[borrowed_dyn.jk](demo/borrowed_dyn.jk) 在普通同步 State 方法内部创建共享/独占 dyn 视图，
通过 helper 和显式重借用调用关联类型接口；导出的方法仍使用原有标量 ABI。
对应真实引擎用例检查两个独立实例、重载前后的方法表快照、32 次构造/销毁与实例计数归零。
视图不能保存到 State，也不能通过 Godot Variant 或宿主接口传递。

实现入口为 [language.rs](src/language.rs)、[script.rs](src/script.rs)、
[instance.rs](src/instance.rs) 和 [resource.rs](src/resource.rs)。ABI 参考
[Godot ScriptLanguageExtension](https://docs.godotengine.org/en/4.7/classes/class_scriptlanguageextension.html)、
[ScriptExtension](https://docs.godotengine.org/en/4.7/classes/class_scriptextension.html)、
[gdext ScriptInstance](https://godot-rust.github.io/docs/gdext/master/godot/obj/script/trait.ScriptInstance.html)。

共享 VM 的 NET1b Reactor 已接到现有每帧 `poll_tasks(64)`：零等待收取最多 256 个
内核事件，与引擎信号及真实单调定时器共用去重就绪队列，事件来源只排队，脚本仍在
主线程恢复。脚本替换或节点关闭走同一取消和同步撤销登记路径。`joky/tcp` 已提供网络公开模块。
[network.jk](demo/network.jk) 与 [network.gd](demo/tests/network.gd) 组成 NET3 联合场景，
检查两个实例的真实二进制收发、部分读取、等待期间帧推进、暂停/缩放下的网络截止时间、
超时后连接复用、reload 保活旧代码、脚本替换/节点销毁及迟到数据，详见
[网络运行时开发计划](../../docs/network-runtime.md)。

[network_messages.jk](demo/network_messages.jk) 与
[network_messages.gd](demo/tests/network_messages.gd) 完成 NET4 缓冲/消息联合验收。
单字节接收容量下，GDScript 对端跨帧拆开长度头、载荷和 UTF-8 字符，并把相邻消息、
CRLF、NUL 与空行合并发送；Joky 返回二进制消息与文本行。另验证超限拒绝、载荷截断、
等待时帧推进、暂停/缩放下的真实网络截止时间、reload 前后的 State 快照，以及脚本移除、
节点退出树/销毁后的任务清理与迟到数据隔离。取消消息读取会关闭连接，不恢复不完整消息。
这些脚本的真实引擎验收已纳入上述 check.sh，未新增网络绑定或另一个执行器。

[network_duplex.jk](demo/network_duplex.jk) 展示 NET5 单连接双向会话：
`task_session(port, deadline_ms)` 消费式拆分 BufferedTcp，并行运行接收推送和发送命令。
调用 `request(count)` 提交累计命令数，发送端按序发送其十进制文本；
`sent()` / `received()` 查询进度。对端发送单字节 42 的消息作为推送，发送空消息或干净 EOF
结束会话；`stop()` 可先结束发送方向。拥有者只存在于任务局部，State 不保持跨挂起借用。
deadline_ms 从连接建立并拆分后开始计时，使用真实单调时间；超时或节点生命周期取消
会释放整个会话，不重试不完整协议。此示例每个实例启动一个会话；连接建立超时与重连策略
尚未包含在示例中。

[network_duplex.gd](demo/tests/network_duplex.gd) 使用真实 TCP 对端验收等待推送时发送、
两实例状态隔离、写背压下接收推送及恢复发送、暂停/时间缩放下的截止时间、
重载前后代码快照、脚本移除/节点退出树/销毁、迟到事件和实例/任务计数归零。
背压探针由 GDScript 注入大字面量，使用 TcpWriter.write 发送有界块并核对实际部分写入字节数；
原生对端暂停读取，进度连续多帧停滞后仍能发送推送。TCP 确认包可能释放少量发送空间，
检查发送保持未完成并重新停滞，不要求每次读取事件都维持原写入计数。
消息发送与取消前缀另由 NET5c 三后端用例覆盖。全部真实引擎场景已纳入 check.sh。

[network_json.jk](demo/network_json.jk) 组合 `joky/json` 与长度前缀 TCP 消息。
`task_session(port, deadline_ms)` 发出启动推送，然后处理 `["request", id, payload]`，
回复 `["reply", id, payload]`；完整帧内的 JSON/UTF-8 或信封错误返回 error，后续请求继续。
`["end", 0, null]` 或干净 EOF 结束会话，传输错误和截止时间则关闭两个方向。
deadline 从连接建立并拆分后开始，以真实单调时间计时。
`completed/replies/errors/broken/timed_out` 返回标量状态；拥有 JSON 值只留在任务局部。

`task_pressure(port)` 是联合验收入口：一次组装固定 ASCII 长度头和编码内容，借用该帧发送 512 个消息，
每条含 8192 字节文本，原始 JSON 树跨整个会话保活；同时解码三个 id 为 99 的 push。
每八次成功发送主动让出，避免同步成功的发送循环延后接收分支。
此探针用 `TcpWriter.write` 核对并累计部分写入；发送方按字节偏移借用原帧的剩余切片。
普通会话使用 `write_message`。该接口当前在解释器中逐字节复制分块，密集大消息开销较高，
探针不把该复制开销当作 socket 背压；这仍是后续可以优化的网络发送路径。
帧和 codec 输入/输出上限都是 64KiB，深度 16、节点 4096、字符串 32KiB；
这些限制控制数据规模，不承诺帧耗时，codec 仍为同步接口。

[network_json.gd](demo/tests/network_json.gd) 用 Godot 自身 JSON 实现独立生成请求、解析输出，
按语义比较嵌套值和 Unicode；Godot 将 JSON 数字解析为浮点数，验收模型也先经独立解析，
不使用其浮点表示证明 Joky 的大整数精度。大数字原始拼写与 NUL 由主仓库原生对端核对字节。
真实引擎场景覆盖两实例、分片/合包、错误恢复、背压下推送及解除后的完整输出、暂停截止时间、
重载快照、三类生命周期取消、旧连接迟到数据隔离与计数归零，已接入 check.sh。


## 嵌入验证范围

Godot 适配器用于验证 Joky 嵌入、脚本实例与任务生命周期，不随标准库增加对应的引擎功能。
不提供文件服务或文件能力开关；`joky/fs` 仍能导入，合法 open 返回 Unsupported，
包括 res://、user:// 和普通路径。需要文件访问的其他宿主可通过 Joky 的通用文件服务接口接入，
见[文件服务架构](../../docs/architecture.md#文件服务能力与后台生命周期)。

适配器自己的 `.jk` 脚本资源加载/保存仍使用 Godot FileAccess，这属于脚本接入基础设施，
不向 Joky 脚本授予文件访问能力。
