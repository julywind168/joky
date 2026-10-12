# Joky AOT 与 Script 热更新开发计划

状态：2026-10-12，设计草案，基线 `f84e6ed`。HR0 代码发布原型已实现（Rust 宿主 API，
JIT 与解释器）；HR1–HR7 尚未开始。

目标是用 **Joky AOT 主程序承载服务，用动态加载的 Joky Script 定义可热更新的业务逻辑**。
服务端开发者不需要编写 Rust 宿主。Script 表示可动态加载的代码单元，可以由 JIT 或解释器
执行，将来也可以是预先编译的原生补丁；热更新语义不依赖具体后端。

本计划负责热更新的语义、能力缺口、实施顺序和验收。并发与状态域的基础语义以
[服务端并发计划](server-concurrency.md)为准，已实现接口以 [语言文档](language.md)为准，
代码与存储不变量以 [架构文档](architecture.md#状态域核心)为准。
本文 `text` 块不可执行，未实现部分的类型名和入口名是概念名称，不提前冻结公开 API。

## 1 核心原则

**状态是数据，操作在每次调用时绑定当前代码，流程按启动时的版本跑完。**

1. 长期可变状态只放在状态域中。域操作是同步、不挂起、不调用宿主的 `fn(&S) -> R` 或
   `fn(&mut S, A) -> R`，操作之间没有业务代码持有状态引用，因此每两次操作之间都是
   天然的静止点，状态本身不需要排空。
2. 域操作按名字和签名晚绑定：发布新代码后，下一次调用就执行新实现。
   房间 tick、结算、道具使用等大部分游戏逻辑属于这一层，热更新时不暂停准入，不排空请求。
3. 挂起流程（等待网络、数据库、定时器的 `@suspends` 代码）只持有句柄，不持有状态引用；
   流程在启动时固定自己的代码，跑完后旧代码自然退役。不迁移挂起栈。
4. 流程的生命周期必须有界。长时间等待改用"状态中的到期时间 + 按名字触发的域操作"，
   不能用一个常驻循环或长时间 sleep 把旧代码钉住。
5. AOT 主程序只放稳定内核：运行时、网络、调度、准入、域目录和性能敏感的库。
   AOT 与 Script 之间只有少量通用入口，业务协议在 Script 侧分发，新增协议不需要重新部署 AOT。

## 2 分层与更新方式

| 层 | 内容 | 更新方式 | 停顿 |
| --- | --- | --- | --- |
| 配置 | 数值表、活动配置 | 不可变快照，请求开始时绑定 | 无 |
| 域操作 | tick、结算、道具、技能逻辑 | 发布代码，下一次调用起生效 | 一次调用的间隔 |
| 状态 schema | 玩家、房间、公会等记录的字段结构 | 迁移函数构造候选，修订校验后发布 | 单个域的迁移耗时 |
| 流程 | 等待外部结果的多步请求 | 按启动时的版本跑完，不迁移 | 无 |
| AOT 内核 | 网络、调度、目录、稳定库 | 重新部署进程 | 进程重启 |

只改逻辑、不改状态结构的热修复只需第二层：不复制状态，回滚就是重新发布旧代码。
状态结构变化才进入迁移，见第 4 节。

## 3 代码发布（HR0，已实现原型）

当前由 Rust 宿主 API 提供，JIT 与解释器均已覆盖；语言侧入口和 AOT 主程序接入在 HR2。

```text
vm.load_source(v2)          // 只影响之后新建的域
vm.publish_domain_code()    // 把结构兼容的现有域切到当前模块，返回切换数量
```

### 3.1 结构兼容

记录类型每次加载都会分配新的名义 id，因此跨版本的状态类型身份按结构判断：同名，
字段名、可见性、标量槽偏移逐一相同，字段类型递归兼容；非记录类型要求相等。
兼容只证明同一份根存储可以被新代码按相同布局读写，不推断业务语义是否一致。

### 3.2 发布协议

1. 收集新模块中合格的状态类型（受检构造函数的返回类型和受检操作的接收者类型）。
2. 遍历存活且健康的域；同名记录结构变化时整次发布失败，返回 `TypeMismatch`，所有域保持原样。
   新模块未声明的类型对应的域留在原代码上。
3. 校验完成后只做移动和标量更新：替换域的模块与类型，并递增该域的代码版本号；
   根存储、修订号、身份和句柄不变，修订号也不因发布推进。
4. 释放注册表借用之后再丢弃旧模块引用；旧代码在最后一个持有者释放后退役。

### 3.3 调用侧规则

- 调用方用自己模块中的签名编码输入、解码结果。源码 `domain.read/write/request` 在准入时
  检查调用方签名与域当前代码的签名兼容，然后执行域当前的代码；不兼容时直接调用返回
  `TypeMismatch`，`request` 返回 `InvalidAccess`。
- 请求任务记录自己运行的模块。旧流程在发布后调用 `domain.write(op, x)`，会用旧签名
  对新实现做兼容检查，兼容则执行新实现。
- 发布前已经排队的调用，执行时若发现代码版本变化，会重新解析操作并核对签名；
  签名不兼容只让这一次调用失败，域保持健康。旧编码的输入不会进入新代码。
- 已加载但未发布时，新代码调用结构兼容的旧域会执行旧域当前的代码。
- 当前限制：pair 操作、候选、`commit_set` 仍要求调用方与域是同一模块，发布后旧流程
  使用这些操作会被拒绝，新流程不受影响（HR6 补齐）。

## 4 状态迁移（HR5）

状态结构变化时，新版本声明显式迁移函数，例如 `migrate(old: &v1.Room) -> Room`。
旧类型由编译器根据归档的旧 schema 生成只读视图，迁移按旧 schema 和新类型共同编译，
不把旧内存重新解释为新类型。迁移只允许受检读取、纯计算和候选分配，不允许挂起、宿主调用、
修改旧状态或启动任务。

迁移复用现有快照、候选和修订校验，不需要停服：

1. 旧代码继续服务。后台按预算逐域快照、运行迁移、在独立 arena 中构造新类型候选，记录读取修订。
2. 全部候选就绪后进入发布区间：修订未变的域直接替换根、类型和代码；
   修订已变的域就地重新迁移，只有这部分产生停顿。
3. 任一域迁移失败、超时或超出预算，整轮作废并丢弃全部候选，旧版本不受影响。
   需要跨域原子性的集合按 `commit_set` 的规则一起发布。

候选与旧根同时存在，峰值内存按两者之和计费；大规模状态可分批，但分批意味着短时间内
新旧版本并存，必须在发布报告中可见。发布后的回退是另一轮兼容迁移，不直接恢复旧指针；
仅改代码的发布可以直接重新发布旧代码。

## 5 流程、定时与 tick（HR4）

- 房间 tick 由运行时调度为一次域写操作，例如每帧调用 `tick(&mut Room, dt)`；
  因为每次都是独立调用，新代码在下一帧生效，不需要业务手写常驻循环。
- buff、冷却、建造计时等到期事件记录在状态中，由运行时到点按名字触发域操作，
  触发时执行当前代码。持久化定时器与进程重启后的恢复另行设计。
- 普通 `@suspends` 流程仍可使用 `task.sleep_ms`，但应有明确上限；
  诊断需要列出长时间持有旧模块的流程。

## 6 AOT 与 Script 边界（HR2）

- AOT 主程序拥有执行器、网络资源、准入池和域目录，通过标准库加载 Script 并触发发布；
  Script 的任务、域操作和定时器进入同一执行器，不启动第二个事件循环。
- AOT 与 Script 之间只暴露少量稳定的通用入口，如连接建立、消息到达、tick 和管理指令；
  业务消息的类型注册与分发在 Script 侧完成。入口签名变化才需要重新部署 AOT。
- 入口参数与结果先限定为标量和平面记录，拥有型载荷随域根类型扩展逐类开放。
  不接受裸指针、私有根引用、闭包或跨模块挂起的借用。
- 现有 `joky/vm` 只提供受限同步调用；HR2 需要把加载、发布和异步调用接入同一执行器，
  并让独立链接的 AOT 可执行文件完成验收。

## 7 通信与组织方式

| 能力 | 结论 |
| --- | --- |
| 出站信箱（HR3） | 必需。域操作在执行中向每个连接的有界队列非阻塞投递纯数据消息；本轮成功后消息才对外可见，失败则一起丢弃。满时按登记的策略合并、丢弃或断开。连接写出任务只搬运字节 |
| 域目录（HR3） | 必需。按玩家 ID、房间 ID 查到域句柄，用于连接路由与换房；目录不保活状态 |
| 准入 | 复用 SC4-A 的有界许可 |
| 通用 Channel | 延后。有具体场景时再按 [服务端并发计划](server-concurrency.md#8-有界准入与通信)的契约实现 |
| 单次应答 | 不需要。请求即函数调用，结果由 `domain.write` 或 `domain.request` 直接返回 |
| Actor | 不作为运行时原语。常驻的 `loop { recv; handle }` 会永久钉住旧代码；如需消息驱动写法，由库把每条消息作为一次独立的域调用交给 `handle(&mut S, msg)` |
| 服务世代与成组排空 | 不需要。代码按调用切换，状态迁移按域进行 |

消息载荷不能携带闭包或代码引用，否则旧代码会随消息被保留。外部效果不在内存原子发布
范围内；可能重复执行的业务使用幂等键或去重，运行时不承诺网络 exactly-once。

## 8 业务写法方向（未冻结）

HR1–HR5 落地后，服务端业务围绕状态域组织：长期状态是 `#state` 记录，域操作写在其 `impl` 中，
规则写成普通函数，流程是 `@suspends` 函数。下例是语法方向的示意，名称与拼写均未冻结，
`text` 块不可执行。声明属性沿用 `#`，`@` 仍只用于签名中的效果标记。

除新提议的结构外，示例沿用现有语法：`while` 循环、`Result.Ok` / `Result.Err` 构造器、
`match` 分支；当前没有 `?` 传播运算符，错误分支用 `match` 提前返回。

```text
import joky/domain.{type Outbox, type Directory}
import joky/net.{type Conn}

// 声明处即检查域根资格，诊断定位到类型而不是首次使用处
#state
struct Room { id: Int64, frame: Int32, members: Array<Member>, cap: UInt64 }
struct Member { player: Int64, conn: Conn, x: Int32, y: Int32 }

#state
struct Player { id: Int64, level: Int32, room: Option<Int64>, conn: Conn }

enum RoomMsg { Frame(Int32), Joined(Int64), Left(Int64) }
enum JoinError { NoPlayer, NoRoom, Full, AlreadyInRoom, Database }

// #state 类型的 impl 方法即域操作：编译器证明同步、不逃逸、不调用宿主
impl Room {
    fn open(id: Int64, cap: UInt64) -> Room {
        Room { id: id, frame: 0, members: Array.new::<Member>(), cap: cap }
    }

    // 运行时按周期调度；每次都是独立调用，发布后下一帧使用新代码
    #every(50)
    fn tick(&mut self, out: &mut Outbox<RoomMsg>) {
        self.frame = self.frame + 1
        step_physics(self)                                  // 普通函数
        var i: UInt64 = 0
        while i < self.members.len() {
            match self.members.get(i) {
            | Some(m) => out.send(m.conn, RoomMsg.Frame(self.frame))   // 调用成功后才投递
            | None => {}
            }
            i = i + 1
        }
    }

    fn full(&self) -> Bool { self.members.len() >= self.cap }
    fn frame(&self) -> Int32 { self.frame }
}

fn step_physics(room: &mut Room) { ... }

// 目录按 ID 查到句柄，不保活状态；模块级声明方式未定
let rooms: Directory<Int64, Room> = Directory.new()
let players: Directory<Int64, Player> = Directory.new()

// 流程只持有句柄；挂起等待期间不持有任何状态
fn join(pid: Int64, rid: Int64) @suspends -> Result<UInt64, JoinError> {
    let player = match players.get(pid) { | Some(h) => h | None => return Result.Err(JoinError.NoPlayer) }
    let room = match rooms.get(rid) { | Some(h) => h | None => return Result.Err(JoinError.NoRoom) }
    let level = match db.load_level(pid) { | Ok(n) => n | Err(_) => return Result.Err(JoinError.Database) }

    // mut 表示写权限，省略为只读；块内同名变量变为对状态的引用
    when (mut player, mut room) {
        if room.full() { return Result.Err(JoinError.Full) }   // 先检查，此时尚未修改
        if player.room.is_some() { return Result.Err(JoinError.AlreadyInRoom) }
        room.members.push(Member { player: pid, conn: player.conn, x: 0, y: 0 })
        player.room = Option.Some(room.id)
        player.level = level                                    // 外层值复制或移动进块
        Result.Ok(room.members.len())
    }
}

// 单域访问可直接在句柄上调用方法；调用会挂起一轮，不需要 await
fn frame_of(rid: Int64) @suspends -> Option<Int32> {
    match rooms.get(rid) { | Some(room) => Option.Some(room.frame()) | None => Option.None }
}

// 状态结构变化：显式迁移，prev.Room 是编译器按归档旧 schema 生成的只读视图
migrate Room from prev.Room {
    Room { id: old.id, frame: old.frame, members: old.members, cap: 8 }
}
```

AOT 主程序只认识少量通用入口，业务消息由 Script 解码和分发。AOT 代码不参与热更新，
其中的常驻管理循环不会钉住 Script 代码：

```text
import joky/script.{type Script}
import joky/net

fn main() @suspends {
    let game = match script.load("game/") { | Ok(s) => s | Err(e) => return fail(e) }
    let server = match net.listen("0.0.0.0:7000") { | Ok(l) => l | Err(e) => return fail(e) }
    parallel {
    | net.serve(server, game.entry("on_message"))   // 连接消息转给 Script 的通用入口
    | admin_loop(&game)
    }
}

fn admin_loop(game: &Script) @suspends {
    while true {
        match admin.next_command() {
        | Reload => reload(game)
        | Stop => return
        }
    }
}

fn reload(game: &Script) @suspends {
    match script.load("game/") {
    | Ok(next) => log(game.publish(next))   // 兼容域切换代码；结构变化则执行 migrate
    | Err(error) => log(error)              // 编译或检查失败时保留旧版本
    }
}
```

每项写法都还原为已有或计划中的原语，不引入新的语义保证：

| 写法 | 底层 | 现状 |
| --- | --- | --- |
| `#state` | 现有 `domain_entry` 根资格检查，提前到声明处 | 资格检查已有 |
| `#state` 类型的 `impl` 方法 | 域操作 `fn(&S, ...)` / `fn(&mut S, ...)` | 已有，限单个标量输入 |
| `handle.method(args)` | `domain.read_from` / `write_to`，多参数由编译器打包 | 单输入已有 |
| `when (mut a, b) { }` | 块体提取为同步函数，走多域短操作 | 原语已有，块提取待做 |
| `Outbox.send` | 出站信箱，调用成功后投递（HR3） | 待做 |
| `#every(n)` | 运行时按名字调度的周期调用（HR4） | 待做 |
| `Directory` | ID 到句柄的目录（HR3） | 待做 |
| `migrate ... from ...` | 类型可变的候选与修订校验（HR5） | 待做 |
| `script.load` / `publish` | 语言侧入口，底层为 `publish_domain_code`（HR2） | 发布原型已有 |

`when` 的取向：

- 名称取"拿到这些域之后执行"的含义，与 Behaviour-Oriented Concurrency 的 `when` 一致；
  区别是本语言的 `when` 只出现在 `@suspends` 函数中，挂起直到块执行完并返回其值。
  首选上下文关键字：仅在 `@suspends` 函数中以 `when (` 开头时识别，不进入保留字表。
- `when` 提供独占访问，不提供回滚。块内返回前已写入的修改保留；块体运行时失败按现有规则
  隔离被写的域。业务应先检查再修改；需要失败整体回滚时使用候选与提交，另设入口。
- 块体同步、不调用宿主、不嵌套 `when`，`break` / `continue` 不跨出块。外层局部值按值复制
  或移动进块，不能借用。块内 `return` 从外层函数返回，由编译器把提前返回编码为结果标签带回。
- 访问列表写作 `mut player`，不写 `&mut player`：`player` 本身是句柄，借用记号会误读为借用句柄。

其余未定项：对句柄的方法调用是否需要显式标记（倾向只靠句柄类型区分）、出站信箱采用显式参数
还是隐式上下文（倾向显式参数，签名中可见副作用）。这些在 HR1 后用真实房间示例验证再定。

## 9 预算与诊断

- 发布返回切换数量；结构不兼容时指出具体类型。迁移报告逐域区分成功、重做、失败与超时。
- 诊断列出仍存活的旧模块及持有原因：未结束的流程、未切换的域、未领取的结果。
- 迁移的候选字节、模块驻留数和待清理资源分别设限；逻辑字节不等同进程 RSS。
- 协作式执行只能在运行时重新取得控制时观察超时，不能抢占不让出的原生循环。

## 10 分阶段实施

| 阶段 | 交付范围 | 关键验收 |
| --- | --- | --- |
| HR0 代码发布（已实现原型） | Rust 宿主 `publish_domain_code`、结构兼容、调用侧签名检查、排队调用重新解析 | 下一次调用执行新代码；挂起流程保留旧代码；结构变化整体拒绝；签名变化只拒绝旧调用；旧模块退役 |
| HR1 域根类型扩展 | 嵌套记录、`Array<Record>`、`Map<K, Record>` 等拥有状态 | 构造、快照、候选、失败与关闭的唯一清理责任；三后端一致 |
| HR2 AOT 加载 Script 与发布入口 | AOT 主程序加载 Script、共享执行器、语言侧加载与发布入口、通用入口契约 | 独立链接的 AOT 程序运行期间加载 v2 并发布；其他任务持续推进；加载失败保留旧代码 |
| HR3 出站信箱与域目录 | 域操作投递的有界连接队列、ID 到句柄的目录 | tick 广播随调用成功才可见；满队列策略生效；路由在换房后正确 |
| HR4 定时调用与 tick | 运行时按名字调度的周期调用和到期调用 | 发布后下一次 tick 使用新代码；取消、关闭与发布排序明确 |
| HR5 状态迁移 | 显式迁移函数、类型可变的候选、预构建与修订校验发布 | 字段变化保留数据；修订变化的域重做；任一失败整轮放弃且旧状态完好；峰值计费回收 |
| HR6 晚绑定补齐与诊断 | pair、候选、`commit_set` 的兼容检查；旧模块持有诊断 | 发布前后的联合访问规则一致；诊断指出旧代码保留原因 |
| HR7 服务器联合验收 | 纯 Joky AOT 接入连接，Script 处理房间 tick 与玩家请求 | 连续热修复与一次 schema 迁移期间连接保持、tick 不中断、资源回到基线 |

HR1 是后续阶段的前提：真实的玩家与房间状态无法放进当前的平面记录，迁移函数与信箱载荷的
表达力也受它限制。HR3 可以先用标量载荷打通"tick 推送到连接"的链路，再随 HR1 扩展载荷。

## 11 行为测试矩阵

| 场景 | 必须观察到的行为 |
| --- | --- |
| 发布时有挂起流程 | 流程按原代码恢复；它此后发出的域操作执行新代码 |
| 发布前已排队的调用 | 签名兼容时执行新代码；不兼容时只失败该调用，域保持健康 |
| 状态结构变化但没有迁移 | 发布被拒绝，所有域保持原代码与原数据 |
| 迁移期间仍有写入 | 修订变化的域重新迁移，最终不丢失写入 |
| 迁移中任一分配或校验失败 | 候选按初始化进度清理，旧根、代码和连接保持有效 |
| 关闭、取消与发布竞争 | 终态唯一；已关闭或失败的域不被发布重新开放 |
| 旧代码的最后持有者结束 | 旧模块退役，资源回到基线 |
| tick 与出站信箱 | 失败的调用不产生可见消息；满队列按策略处理且有界 |

编译器或运行时改动的测试执行生成代码，JIT、解释器与 AOT 三边一致；AOT 当前没有重新加载
入口，相关验收在 HR2 补齐。验证范围按 [AGENTS.md](../AGENTS.md#按改动范围验证)选择，
不因阶段结束自动运行 Release 或无关压力测试。

## 12 不做与延后

首版不提供挂起栈迁移、运行时二进制自身热替换、跨线程或跨进程原子升级、崩溃恢复、
任意外部副作用回滚、透明请求重放和不可信脚本隔离。长期状态表示跨请求存活的内存，
不代表已经落盘；持久化与失败域的恢复与热更新正交，另行设计。
