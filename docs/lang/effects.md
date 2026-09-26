# Effect 与 Handler

> 状态：语言设计基线；基础 Effect、Handler、typed suspends continuation、provider registry 和 worker-pool 调度已可运行。
>
> Effect 描述一段计算依赖的外部能力、控制流操作或挂起操作；Handler 为这些操作提供当前词法作用域内的实现。Effect 不替代 trait/interface：trait 描述值支持哪些操作，Effect 描述计算可能请求哪些操作。

## 1. 核心模型

Joky 的函数可以声明它使用的 Effect：

```joky
eff Log {
    write(message: String) -> Unit
}

eff Network {
    @suspends
    fn request(request: Request) -> Response
}

fn load_user(id: String) -> User effects { Network, Log } {
    Log.write("loading " + id)
    let response = Network.request(Request(path: "/users/" + id))
    decode_user(response)
}
```

`effects { ... }` 是函数声明允许使用的 Effect 组集合；实际调用的 operation 单独记录。只有实际调用带 `@suspends` 的 operation，函数才会被标记为可能挂起。调用一个函数会传播它实际使用且未被当前作用域处理的 operation；调用者必须继续声明对应 Effect，或者使用 `do ... with` 安装 Handler。重复声明自动消除。

可以用顶层 `effects` 声明给常用的 Effect 组命名，并在函数签名中复用；别名可以组合，也可以引用后面声明的别名。编译器会在本模块内展开别名，模块 ABI 仍记录实际的 Effect 组：

```joky
effects Transport = { tcp, tls }
effects SecureTransport = { Transport, auth }

fn open() -> Result(Stream, String) effects { SecureTransport } {
    tcp.connect("example.com", 443)
}
```

别名中的每个名称必须是已声明或已导入的 Effect 组；空别名和循环别名会被拒绝。

`effects` 描述的是计算依赖，不是权限的唯一来源。需要访问具体资源时仍然应显式传递 trait、值或 capability。`Cown(T)` 和其他 capability 决定代码可以访问什么资源，Effect 决定代码可能向 runtime 请求什么操作。

```text
trait      = 值或类型的接口
Effect     = 计算的接口
Capability = 资源访问权限
```

## 2. Effect operation 模式

Operation 必须通过注解明确它的控制流模式：

### 普通 operation

调用后继续执行当前计算：

```joky
eff Clock {
    now() -> Instant
}
```

### 可恢复 operation

Handler 对 `@resumable` operation 直接返回值即可恢复触发 operation 的计算。每个 continuation 只能恢复一次：

```joky
eff Ask {
    @resumable
    fn question(prompt: String) -> String
}

let name = do {
    Ask.question("What's your name?")
} with {
    Ask.question(prompt) => "Alice"
}
```

Handler 返回值类型必须与 operation 的返回类型一致。需要放弃 continuation 时使用 `abort value`；`abort` 的值必须与外层 `do` 表达式类型一致。

### 挂起 operation

`suspends` 表示 operation 可能把当前计算保存为 continuation，并在稍后恢复：

```joky
eff time {
    @suspends
    sleep(duration: Duration) -> Unit
}

eff Input {
    @suspends
    read(prompt: String) -> Int32
}
```

挂起性沿调用链传播。用户不需要声明 `async fn` 或使用 `await`；编译器根据 Effect operation 和结构化运行上下文进行挂起 lowering。`@suspends` 的参数和返回值不限定为 `Duration` 或 `Unit`，它们由 operation 声明决定。

`import joky/time` 提供标准的 `time.sleep(Duration)` operation。

`time.sleep(Duration)` 使用单调时钟和可取消的 timer。其他 operation 使用统一的 flattened payload ABI：runtime 为参数和结果保留 continuation-owned storage，provider 可以通过参数/结果 pointer 和 size 查询数据，并用 `jk_continuation_complete_suspend_with_payload` 完成请求。String、class、Tuple/Struct、Option/Result/Enum 等包含托管值时，所有权随 payload 转移，完成、取消和失败路径都会执行对应 cleanup。native provider 通过 `jk_continuation_register_suspend_provider` 按稳定 operation ID 注册，并可提供 cancel hook；cancel hook 返回前必须停止该请求后续的 completion。未注册 provider 的请求会被拒绝，不再伪装成 timer 完成。

`import joky/file` 提供三层文件 API。便利函数 `file.read` 保留 UTF-8 校验，`file.read_bytes` 原样读取二进制；`file.write` 和 `file.write_bytes` 默认覆盖目标文件并返回写入字节数。它们都使用有界 blocking pool，不占用 CPU worker；普通写入只表示系统调用成功，不承诺掉电持久化。

显式句柄通过 `file.open(path, FileMode)` 获得 `File`。句柄唯一所有权，`read_chunk`、`write_chunk`、`position`、`seek`、`flush` 和 `sync` 借用句柄，`close` 消费句柄；未关闭句柄由所有权 scope 清理。同一句柄从排队到执行结束只接受一个操作，竞争返回 `Err("file is busy")`。`sync` 才表达持久化请求；取消或失败可能已经写入部分数据，不会自动回滚。完整约定见 [文件 I/O](../stdlib/file-io.md)。

原生句柄的方法接口可以在类级别绑定 effect，无须逐个方法重复声明：

```joky
@intrinsic(effect = file)
pub class File {
    fn read_chunk(&self, max_bytes: UInt64) -> Result(Bytes, String)
    fn close(self) -> Result(Unit, String)
}
```

这里每个方法绑定 `eff file` 中的同名 operation，`self` 对应 operation 的第一个参数。编译器在检查声明时校验接收者、参数、返回类型以及借用／消费方式，并从 operation 派生 effect、控制流模式和 `@suspends` 属性。`handle.read_chunk(n)` 因而与 `file.read_chunk(handle, n)` 请求同一个 operation；调用者仍须声明或处理该 effect。只有类中列出的方法才会暴露为句柄方法，`open` 等操作不受影响。

此绑定目前用于 File 和网络原生句柄；纯容器仍使用无参数的 `@intrinsic`。同一原生类型的不同 effect 接口若冲突会报错，不会按导入顺序选择。绑定在模块产物中保留定义模块身份，支持独立编译和磁盘缓存。

Effect 参数也通过 `value: &Class` 显式借用，通过 `value: Class` 消费。
handler 接收同样的参数模式，不能把借用参数作为 owning 结果返回。borrow
参数不进入请求的 owned cleanup；持有它的调用方负责释放对象。

blocking pool 的执行队列满时，文件操作保持 Pending，等待准入；CPU worker 可以继续运行其他任务。容量释放后按 FIFO 顺序接纳等待请求，调用方无需因为内部队列饱和而重试。等待期间只保留 continuation、共享路径/写入数据引用和需要的原生资源，读取缓冲区在 blocking worker 执行时分配。执行队列和线程数有界，但等待任务及其保留参数的总内存仍随并发调用数增长；处理海量文件时可使用 `@parallel(limit: N) for` 控制生产任务的并发量。

scope 取消后，等待准入或仍在执行队列中的请求会立即移除并释放参数，空出的队列位置可用于其他等待请求。已被 blocking worker 取出的请求会在读取前检查取消；已经开始的系统调用允许返回，但晚到结果会被丢弃，不再恢复用户代码。取消与完成同时发生时，结果由 continuation 接收并清理，或由 provider 释放，责任只归一方。scope 退出不等待不可中断的文件系统调用；这些调用及其保留的路径、读缓冲区可能继续占用 blocking worker 和内存直到系统调用返回。

`@suspends` operation 也可以被 `do ... with` 拦截。此时 Handler arm 直接返回 operation 结果，不启动 provider 或保存挂起 continuation，适合测试替身和同步 fallback：

```joky
eff Input {
    @suspends
    read() -> Int32
}

let value = do { Input.read() } with {
    Input.read() => 42
}
```

Handler arm 自身不能再次调用会挂起的 operation；需要异步工作时，应让当前 operation 正常挂起，而不是在 Handler 处理上下文中嵌套挂起。

`import joky/socket/tcp` 提供非阻塞 TCP 客户端和服务端操作：`tcp.connect` 建立出站连接，`tcp.listen` 创建监听资源，`tcp.accept` 接收连接，`tcp.read`/`tcp.write` 传输 `Bytes`。连接和监听器分别使用 `TcpStream` 与 `TcpListener` native resource，资源离开所有权图时自动关闭。

`import joky/socket/udp` 提供非阻塞 UDP 操作。`udp.bind` 创建 `UdpSocket`；可使用 `socket.connect` 设置默认 peer，使用 `socket.send_to`/`socket.recv_from` 进行带地址收发，或在连接后使用 `socket.send`/`socket.recv`。UDP 没有 `accept` 或 listener 语义，`recv_from` 返回 `(Bytes, String, UInt16)`（数据、来源主机和端口）。

socket operation 的取消语义与文件 provider 一致，由 reactor 驱动：scope 取消（例如 `race` 中输家被取消）时，尚未完成的请求会通过 provider 的 cancel hook 从 reactor 注销；已经就绪的完成事件与取消竞争时，continuation 的 generation/状态检查保证结果只被接收或丢弃一次，不会被重复清理。provider 在挂起开始时复制全部所需数据（如 `Bytes` 参数），因此完成、失败或取消路径释放的都是 continuation 自有的拷贝。socket 是非阻塞 I/O，没有不可中断的系统调用阶段；scope 退出时 provider 拥有的每个资源都会被关闭并注销。

### 不可恢复 operation

不可恢复 operation 不会回到触发点，不能调用 `resume`：

```joky
eff Failure {
    @aborts
    fn abort(message: String) -> Never
}
```

`aborts`、取消和进程终止等操作必须经过 runtime 的清理路径，不能被普通 Handler 随意吞掉。具体内置取消语义由结构化并发部分定义。

## 3. `do ... with`

`do ... with` 在一个词法作用域内安装 Handler：

```joky
eff Log {
    write(message: String) -> Unit
}

fn load() -> String effects { Log } {
    Log.write("loading")
    "value"
}

let value = do {
    load()
} with {
    Log.write(message) => println("[test] " + message)
}
```

`do` 的 body 是一个完整代码块，`with` 的 Handler block 绑定它左侧完整的计算。Handler arm 的参数、返回值和 `resume` 类型由对应 operation 的声明检查。

未匹配的 Effect 继续向外层传播；最终入口必须处理所有需要处理的用户 Effect。没有 Handler 的纯计算可以直接调用，不需要额外语法。

普通 operation 的 handler 可以跨普通函数调用链返回到最近的 `do`。runtime 为每个 `do` 安装带父指针的 `HandlerFrame`；子 task 捕获当前 frame，stackless resume 也会重新安装它，因此动态词法上下文不会因为普通调用或 `time.sleep` 丢失。Normal operation 通过统一的 runtime continuation request 携带 typed payload 回到最近 handler，handler 返回值写入 request 的结果 payload 后，body 从 continuation 点继续执行；不会为普通 handler 额外创建私有 task。只有 body 同时包含 Aborts、需要任务边界的挂起调用，或显式创建 `parallel`/`race` 时，才会保留结构化 task；其他可挂起调用仍由 continuation 处理。Handler 捕获的可变状态必须通过 `Cown(T)` 或其他明确的共享 capability 访问。

`@resumable` 支持直接词法调用、命名普通函数调用链，以及 struct/class 方法调用链：handler 直接返回值，值会在请求点作为 operation 的结果继续执行；`abort value` 则结束当前 `do`。为了保持当前栈式 ABI，编译器会在存在 resumable handler 的调用链中内联携带 resumable operation 的命名函数或方法；递归调用会被明确拒绝。该调用链可跨 `time.sleep` 的 stackless machine entry 继续运行；函数值/闭包间接调用、多次或逃逸 continuation 目前不受支持。

## 4. Effect 与结构化并发

局部 `var` 的捕获限制按源语法统一检查：`do` body、每个 handler arm、
`branch` / `parallel` / `race` 和并发 `for` 都不能直接读取、赋值或移出
外层 `var`。是否生成私有任务、handler 是否普通或可恢复，都不改变这一规则。
各边界内部可以声明自己的 `var`；handler 参数按内部绑定处理。当前任务中的
普通块、顺序循环和 `when` 可以修改外层 `var`，Cown lease 的限制继续适用。
需要跨边界传值时先创建 `let` 快照，共享修改使用 Cown。

Joky 不提供 Actor、mailbox、`receive`、`send`、`stop` 或 behavior。并发采用多 worker 线程上的结构化任务：

```joky
fn main() -> Unit effects { Network } {
    let (user, posts) = parallel {
        | load_user("alice")
        | load_posts("alice")
    }

    render(user, posts)
}
```

- `parallel` 建立父子任务边界（一个 task scope）；
- 每个 `|` arm 创建可以在不同 worker 上并行执行的子任务；
- `parallel` 在返回前等待所有 arm；
- 一个 arm 中未处理的 `@aborts` operation 逃出任务时，scope 记录首个
  failure，其他 arm 收到取消并完成清理；随后该 failure 连同其 typed
  payload 向父 scope 的 Effect frame 传播；
- arm 的任务值不能逃出创建它的 task scope；
- `parallel` 不自动处理 Effect，未处理的 Effect 仍然向外传播。

因此，`scope` 管理任务生命周期，Effect 描述任务中的外部操作。一个纯 CPU branch 可以不使用任何挂起 Effect；使用 `Network.request` 或 `time.sleep` 的 branch 则可能保存 continuation。

### `race`

`race` 竞争多个 branch，第一个成功结果获胜，同时取消其他 branch。arm 使用 `|` 分隔，所有 arm 必须产生相同类型：

```joky
let response = race {
    | request_primary()
    | request_backup()
}
```

`race` 必须等待被取消 branch 完成资源清理；winner 的结果被保留，loser 的 owned value 被释放。任何 branch 的未处理 `@aborts` failure 会优先取消其余 branch，并向父 scope 传播首个 failure；它不是一个普通的 race 值，也不做错误聚合。

结构化并发的 `scope`、`branch`、`race`、取消和 join 是语言级任务原语，不应通过普通用户 Handler 替换。它们有额外的任务逃逸、ownership 和清理不变量。

任务取消、Cown acquire/release、scope 退出等内部操作不是 Effect operation：它们不进入 operation registry，也不经过 Handler frame 分发，因此用户 Handler 无法声明、拦截或观测它们。当任务在 `do ... with` frame 内挂起后被取消时，运行时直接取消 continuation 并走 cleanup 路径，Handler arm 不会被调用。

## 5. Cown 与 `when`

`Cown(T)` 是可以跨任务复制的隔离 capability。它持有一个由 runtime 管理的 `T`，但 capability 本身不能直接读取或修改 payload：

```joky
class Counter {
    var value: Int32 = 0
}

let counter = Cown.new(Counter(value: 0))
```

`Cown.new(value)` 消费 `value` 的唯一所有权，并创建一个 runtime-managed control block，登记到创建时的当前区域。`Cown(T)` 本身是可复制的共享句柄；复制只复制 capability，不复制 payload，也不操作原子引用计数，丢弃最后一个句柄不会提前释放 Cown。payload 可以包含同一区域或祖先区域的 Cown capability，因此同一区域内的环不需要用户手动打断：区域退出时先排空相关任务，再批量析构 payload 并释放 control block，不做查环或全局停顿。编译器拒绝句柄逃出所属区域；长期运行的代码应按请求、会话或循环轮次使用 `region { ... }` 限定对象寿命。规则与限制见 [统一 Region 生命周期](../runtime/regions.md)。

Cown payload 只能保存内存中的业务状态：普通值、不可变容器、可变字段和其他 Cown capability 都可以。file、socket、native handle、task、continuation 和 lease 等外部资源不得由 payload 直接拥有；它们必须由唯一所有者持有，需要时可显式关闭、取消或释放，离开所有权图时由 runtime 自动释放。

`when` 等待并原子获取一个或多个 Cown 的临时独占访问权，payload 用尾随闭包参数命名：

```joky
when (counter) |state| {
    state.value = state.value + 1
}
```

多个 Cown 必须显式列出：

```joky
when (source, target) |source_state, target_state| {
    let amount = source_state.value
    source_state.value = 0
    target_state.value = target_state.value + amount
}
```

省略 `|...|` 参数列表时，全部 Cown 都是简单名称才会隐式绑定同名 payload；复杂 Cown 表达式必须显式写出参数列表。参数数量必须与 Cown 数量一致，`_` 可用于丢弃 payload。

`when` 的语义是：

1. 等待所有列出的 Cown 可用；
2. 按稳定顺序取得它们的独占访问权；
3. 执行 body；
4. 在所有正常、错误、取消和 panic 路径释放访问权；
5. 返回 body 的普通结果。

Cown lease 不能跨挂起点或任务边界：

```joky
when (counter) |state| {
    time.sleep(1s) // 错误：when body 不能产生 suspends effect
}
```

body 也不能启动捕获 lease 的 branch，不能把 lease 返回、写入堆对象或捕获到闭包中。需要异步处理时，先在 `when` 中复制出不可变快照，再在外部调用可能挂起的函数。

禁止依赖嵌套 `when` 获取 Cown；需要同时访问多个 Cown 时必须一次列出完整集合。编译器或 runtime 应拒绝重复 Cown，并使用稳定 ID 排序获取顺序，避免不同源码顺序产生死锁。

`CownAcquire` 可以作为内部 scheduler operation，但 `when` 保留为语言级结构化语法，以便静态检查 lease 的生命周期。普通用户 Handler 不能替换 `CownRelease` 或取消清理。

## 6. Handler、挂起和取消

带 `suspends` operation 的计算在 lowering 阶段变成显式 continuation：

```text
operation_start
  -> 保存 live values、Handler frame 和 cleanup 状态
  -> 当前 worker 返回调度器
  -> operation 完成
  -> scheduler 恢复 continuation
```

Handler frame 是 continuation 的一部分。恢复时必须继续原来的 Effect、任务和 ownership 状态。挂起 operation 的结果独立于函数最终结果保存，避免 operation 返回类型和外层函数返回类型不一致。取消直接进入编译器生成的 cleanup 路径，释放挂起 operation、Handler frame、Cown lease 和其他 owned value。

以下规则必须由语义分析和 MIR verifier 共同保证：

- `suspends` operation 只能出现在结构化运行上下文；
- `when` body 不得包含 `suspends`；
- `@suspends` Handler 不能让 continuation、lease 或 task 句柄逃出其结构化作用域；
- 取消不能被普通用户代码吞掉而导致父 scope 提前返回；
- 所有正常返回、Effect 失败、取消和 panic 路径都必须完成 cleanup。

## 7. 与 trait/interface 的边界

Effect 可以替代一部分显式环境参数，但不能替代 trait：

```joky
trait Storage {
    fn get(&self, key: String) -> Option(String)
}
```

`Storage` 描述一个值支持的操作，适合泛型约束、静态派发和类型相关行为。Effect 描述计算请求的环境能力：

```joky
eff StorageAccess {
    @suspends
    fn get(key: String) -> Option(String)
}

fn load_user(id: String) -> User effects { StorageAccess } {
    let value = StorageAccess.get(id)
    decode_user(value)
}
```

需要明确控制依赖对象、选择实现或表达泛型约束时使用 trait；需要跨多层传播 I/O、时钟、日志、随机数、取消或挂起时使用 Effect。两者可以在同一函数中共存。

## 8. 当前限制

- Actor、mailbox 和消息行为；
- 用户层 `async/await`；
- detached task；
- 同一 resumable continuation 多次恢复；
- 可挂起的 `when` body；
- 隐式借用捕获或跨任务 borrow；
- 通过普通 Handler 接管 task cancel、Cown acquire/release 等内部不变量。
