# 服务端并发模型重构计划

状态：2026-10-26，设计提案，尚未实施。以 `examples/chat.jk`、`chat_logic.jk` 为验收对象，
综合 chat 可用性评审、多线程执行器适配分析与并发技术选型讨论。

本文规划下一轮收敛方向，不改变现行规则。当前接口以 [language.md](language.md) 为准，
不变量见 [architecture.md](architecture.md)。本文承接
[有状态服务的编程模型](stateful-service-ergonomics.md)（S0–S4 已完成）、
[服务端状态域设计](server-concurrency.md)、[状态域语法](state-domain-syntax.md)、
[热更新计划](aot-script-hot-reload.md)与[多线程任务执行器](task-parallel-executor.md)，
不复制它们的现行契约；各阶段实施后更新对应参考文档。

## 1. 现状评估

### 1.1 已经成立的部分

- **宿主与脚本分工**：宿主只负责 I/O、生命周期与并发骨架；业务全部在 Script。
  `main` 用 `race`／`scope`／`branch` 写出的服务骨架清晰。
- **契约单源与发布前预检**：`chat_contract.jk` 与 `require_contract` 让热更新边界明确。
- **多资源 `when`**：`when "say" (mut account, room)` 让收费规则在一次业务操作内可读；
  换房的三方写集合一眼可见。这是本组合最有辨识度的部分，重构保留它作为核心。
- **所有权归属**：账户归连接，房间归服务；重试、淘汰均有上限。
- **多线程底座**：TP0–TP10 已交付转移资格推导、受检任务包、冻结代码共享与保活、
  任务状态机、私有分片与共享预算、取消检查点和关闭协议，可直接复用于服务端。

### 1.2 协议负担

`chat_logic.jk` 约 490 行，真正的业务规则（扣费、加入／离开、消息格式）约 150 行，
其余为协议与样板。业务作者实际接触的概念包括 Owner、目录、请求根、就绪邮箱、停止邮箱、
发布屏障、重试、排除列表、`Attempt` 双层 Result 等十余项。主要来源：

| 负担 | 表现 | 根因 |
| --- | --- | --- |
| 慢客户端淘汰 | `excluded` 参数穿过 say／join／leave／reply／enter／depart／deliver／broadcast；`run_command`、`retry_command`、`slow_client`、`target_member` 约 100 行 | 收件人队列容量参与业务提交，一个慢连接可让他人的操作失败 |
| 取得或创建房间 | Catalog 四态、等待者邮箱、请求邮箱、`next_room`／`init_room`／`open_room`／`fail_room`、`refresh` 屏障，约 120 行；宿主 `create_room`／`manage_rooms` 固化创建步骤 | 域不能在 `when` 内创建；宿主承担了业务流程，修改创建流程需重编宿主 |
| 手写依赖校验 | `expected` 参数、`Retry.Changed`、64 次重试循环 | 访问集合依赖状态（账户 → 所在房间），需在 `when` 外解析 |
| 捕获仪式 | 每个操作先 `text.clone()`、`copy_ids(excluded)` | `when` 捕获须为拥有值，缺少派生 Clone |

### 1.3 已核实的语言限制

以下由 2026-10-26 的 Debug 探针确认：

- 挂起函数内不能消费式解构拥有记录（`Owned record destructuring requires a synchronous function`），
  chat 因此需要 `split`、`split_request`、`split_work`、`split_room` 四个辅助函数。
- 无派生 Clone（`Command.clone` 手写），Array 无 clone／contains（`copy_ids`、`contains` 手写）。
- 枚举不支持 `==`，`match status { Pending => true, _ => false }` 重复出现。
- `for x in &array`／`&mut array` 与 `if let` 已可用；chat 中约十处手写下标循环属于示例债务。
- `--task-workers N` 下 chat 整体被拒：子任务包含引用；仅含 `join` 与 `println` 的程序也被拒，
  原因是 `println` 未声明 ParallelSafe。

## 2. 目标组合

### 2.1 前沿方向的收敛

| 语言／系统 | 状态单位 | 数据竞争安全 | 可借鉴点 |
| --- | --- | --- | --- |
| Verona | cown，`when` 多资源获取 | 区域所有权 | 多资源原子行为；`when` 即消息，无 channel |
| Pony | actor | 引用能力 | 编译期竞争自由 |
| Erlang／Elixir | 进程 | 不可变数据、独立堆 | 监督树、热更新与 `code_change` |
| Swift 6 | actor | Sendable、区域隔离推导、`sending` | 推导减少标注；actor 重入是反例 |
| Haskell STM／Clojure | TVar／ref | 不可变 + 事务 | Clojure 在 `dosync` 内发送 agent 消息，提交后才投递 |
| OCaml 5／Koka／Unison | — | — | 代数效应统一“代码能做什么” |
| Roc | — | 纯函数 | 平台／应用分离，与 AOT 宿主 + Script 同构 |
| FoundationDB／TigerBeetle | — | 单线程 | 确定性仿真与重放 |

收敛为五层：结构化并发；编译期推导的数据竞争自由；共享状态单位（actor 为主流，
多对象原子操作由 BoC 或 STM 补足）；效果约束；热更新、监督与确定性重放等运维能力。

### 2.2 Joky 的选择

1. **结构化并发**：保留 `scope`／`branch`／`join`／`race`，补监督策略。
2. **`when` 是唯一共享状态原语**：BoC 调度 + 事务语义；等待结果与不等待结果两种形式；
   访问集合可依赖状态。
3. **提交后生效的效果**：发送消息、创建域、登记定时器统一暂存，提交成功才发布。
   outbox 退为运行时内部机制。
4. **转移资格由编译器推导**：沿用 Transfer／Share，暂不开放用户可写 trait。
5. **效果标注逐步统一**：`@suspends`、事务内允许的效果、ParallelSafe 收敛到一套内部分类。
6. **按调用切换代码的热更新**：保持现状，后续补状态布局迁移。
7. **确定性模式**：Local 执行器 + 可配置种子的调度顺序，用于重放。

差异化定位是 **BoC 多资源原子操作 + 提交后生效的效果 + 按调用热更新** 的组合：
Verona 有 BoC 而无热更新与事务效果，Erlang 有热更新而无多 actor 原子操作。

目标是业务作者只需理解五个概念：任务作用域、状态（`#state`）、`when`、会话、提交后效果。

## 3. 设计决策

各节的“示意”代码块使用 `text`，不是当前可执行语法，名称未冻结。

### D1 收件人容量与提交解耦

**现状**：`outbox.emit` 在提交时统一检查全部目标容量，`OutboxFull(target)`／`OutboxClosed(target)`
使整个操作失败；chat 通过排除列表、逐个淘汰与重放善后。

**方案**：

- 提交只检查本批消息自身的条数与字节预算（现有 DomainLimits 批次上限），超限仍为 ResourceLimit。
- 收件队列满或已关闭不再拒绝提交。消息按队列的溢出策略处理，由会话层执行：
  事件流溢出即断开该连接；状态流可选择只保留最新或合并。
- 会话输出端持有队列与策略，业务只调用发送；溢出连接标记后由会话任务关闭，
  并按普通断线路径清理成员关系。
- 语义上“入队不等于送达”原本成立，因此不削弱已承诺的保证。

**收益**：业务签名移除 `excluded`，删除淘汰工作表与重放；多线程下避免对 N 个跨线程队列
做两阶段预留（见 D7）。

**迁移**：`OutboxFull`／`OutboxClosed` 从提交结果中移除或降级为诊断事件；
既有测试中“满队列整体回滚”的断言改为“提交成功、溢出连接被断开、其他成员正常收到”。
需要逐目标强一致的场景（如可靠控制消息）另行评估，不默认保留旧语义。

### D2 统一提交后效果

`when` 内允许的外部效果统一为暂存效果：发送消息、创建域（D3）、登记定时器。
操作成功才发布；业务 Err、异常、资源不足与取消一律丢弃。
用户不再创建 outbox、传递 Handle 或比较 `identity()`；会话提供 `conn.send`，
房间提供 `room.broadcast` 一类方法，底层仍是现有暂存批次。

协调用邮箱（房间就绪等待者、`requests` 请求邮箱、停止信号）由 D3、D6 与会话生命周期取代。
通用 channel 保留为库层流式工具，用于任务间生产者／消费者流水线，不用于共享状态通信。

### D3 事务内创建域

**现状**：域只能在 `when` 外创建并经宿主发布屏障后才对客户端可见，
导致 Catalog 状态机、等待者与宿主侧创建流程。

**方案**：创建域成为暂存效果，与目录条目插入一起在提交时发布；
创建使用本次尝试固定的代码版本，代码发布使尝试失效时整体重算。

```text
when "join" (mut account = accounts[id], mut catalog) {
    let room = catalog.rooms.get_or_create(name, || Room.new(name))
    ...
}
```

**不变量**：失败或取消不留下已发布的域或目录条目；暂存域计入预算；
条件删除仍按域身份比较；新域代码与当前模块一致，不再需要 `refresh` 屏障。

**收益**：删除 `next_room`／`init_room`／`open_room`／`fail_room` 四个入口、Catalog 四态、
宿主 `create_room`／`manage_rooms`；宿主不再含业务流程，房间创建可热更新。

### D4 依赖状态的访问集合

**现状**：chat 手写“先读位置、再获取、再校验”的循环（`expected`、`Retry.Changed`）。
这一模式在确定性数据库中称为 OLLP（Calvin）。

**方案**：

```text
when "say" (mut account = accounts[id], room = rooms[account.room?]) { ... }
```

运行时先以只读方式解析路径，再一次性获取完整集合，然后校验解析结果未变；
变化则自动重算，次数受配置上限约束，超限返回明确错误。路径为空（`?`）时走无房间分支。
解析阶段不执行用户副作用；路径表达式限于目录查找与字段投影，不开放任意代码。

### D5 撤销日志替代深复制

**现状**：候选对完整访问集合深复制；在 512 人房间发言会复制整个成员数组。

**方案**：`when` 块内的状态写入由编译器插入首写记录，成功丢弃日志，失败按日志恢复。
成本与写入量成正比，不随状态规模增长。D7 多线程化后仍适用。

**前提**：先按 [基准方法](../benches/README.md) 测量深复制在 chat 规模下的实际成本，
确认收益后实施；不承诺零复制。

### D6 不等待结果的 `when`

现有 `when` 只有“发出并等待结果”。增加不等待形式：块体排入目标域队列后立即返回，
生命周期挂在目标域上（域关闭时取消），仍属结构化并发而非游离任务。
通知房间、后台推进等原本需要 mailbox 的场景改用此形式。
等待形式可视为“不等待形式 + 等待结果”。

### D7 多线程：BoC 运行于共享 worker 池

**修正**：早期评审建议“每核一个单线程执行器、`when` 不跨执行器”，前提是执行器为单线程。
TP 已提供共享 worker 池与受检包，改为 Verona 式：`when` 是调度单位，
目标域全部可用时派往任一 worker 执行。理由：

- 按分片切分会使跨分片换房失去原子性；BoC 无需域与线程绑定，房间间自动并行。
- 状态域协议的“全部取得或一个都不持有”“单调序号”“不插队”正是 BoC 跨线程调度规则。
- `when` 已要求块体同步、捕获为拥有值、状态为无资源拥有树，正好构成可跨线程的受检包。

**`Local` 是实现状态，不是语义限制**。资格分析把未声明 ParallelSafe 的宿主绑定和
状态域操作视为 Local，并沿调用图传播至根入口。逐项情况：

| 能力 | 现状原因 | 处理 | 工作量 |
| --- | --- | --- | --- |
| `println` 等输出 | 实现使用 `stdout().lock()`，错误记录经 TP 已保存／恢复的线程局部状态；仅缺 ParallelSafe 声明 | 审计后标注 | 很小 |
| TCP | `Reactor` 为 `Rc<Core>`，内部 `RefCell` 等待表与就绪队列；`Connection` 为 `Rc` + `Cell` | 每 worker 一个 reactor 并按所属任务注册，或改为 `Arc` + 同步与线程安全唤醒路由 | 中 |
| VM 句柄 | 引擎与模块保活使用 `Rc`，加载／发布修改状态 | 本地控制线程负责编译与加载；执行时经原子指针读取当前代际的冻结代码，发布即换代 | 中 |
| Domains／`when` | 注册表为 `Weak<RefCell<…>>`；原子性证明依赖单线程同步轮次 | 每域等待队列、按全局序号排队的 BoC 调度；`when` 块体编译为 ParallelSafe 入口，发起方 Pending，执行后交回结果包 | 大 |

**准入粒度**：由“根调用树任一 Local 即拒绝整个入口”改为按能力分级。
资源固定规则：资源只能由创建它的任务使用；任务固定在 worker 且不迁移，资源随之固定。
未来引入工作窃取时，持有固定资源的任务不可窃取，只窃取 `when` 执行单元与纯计算分支；
该规则现在写入契约。

**多线程下 D1 更必要**：提交只向 MPSC 队列非阻塞推送，溢出由消费端处理，
不在提交路径做跨线程多队列预留。

### D8 收缩跨挂起提案

“快照 + 读取凭据 + 提案提交”的跨挂起乐观路线不再扩展。读状态、等外部 I/O、再写回的场景
（数据库、支付）改用预留模式：一个 `when` 冻结额度或标记进行中，在 `when` 外执行 I/O，
再用一个 `when` 确认或取消。现有能力保留，预留模式在示例中验证后再决定是否收缩公开接口。

### D9 作用域监督策略

`scope` 增加子任务失败策略（隔离并记录、重启、传播），取代 chat 中手写的局部失败隔离；
默认行为保持现有 fail-fast，不静默改变既有执行器组规则。

### D10 确定性模式

Local 执行器增加可配置种子的就绪顺序与外部事件注入，用于重放并发缺陷。
不承诺跨后端或跨版本得到相同调度，仅保证同一构建、同一种子与同一输入可重现。

## 4. 语言与示例的易用性收敛

| 编号 | 内容 | 效果 |
| --- | --- | --- |
| L1 | 挂起函数内的消费式记录解构 | 删除四个 `split_*` |
| L2 | 派生 Clone；Array clone／contains | 删除 `Command.clone`、`copy_ids`、`contains` |
| L3 | 无载荷枚举相等比较或模式测试表达式 | 删除 `match … { X => true, _ => false }` |
| L4 | `when` 块向内传递期望结果类型 | 删除 `Result.Ok::<Route, Retry>` 显式类型参数 |
| L5 | `Vm.require` 返回类型化入口句柄 | 调用处不再重复入口字符串与类型参数 |
| L6 | `LoadError` 提供统一消息 | `chat_admin.report` 不再逐分支打印 |

示例层清理（不依赖新能力）：手写下标循环改为 `for`；`room == 0`、`id == 0`、`skip: 0`、
目录键 1、`key - 1` 换算改为 `Option` 或具名常量；`retry_command` 内重复的 `match &command`
合并；统一 `ctx`／`host` 命名；拆分超长行与单行多语句；失败结果携带原因而非 `Bool`／`Err({})`。

## 5. 目标写法示意

```text
fn say(id: UInt64, text: String) @suspends -> Result<Unit, ChatError> {
    when "say" (mut account = accounts[id], room = rooms[account.room?]) {
        if account.balance_cents < 100 { return Err(ChatError.InsufficientBalance) }
        account.balance_cents -= 100
        room.broadcast(event("message", room.name, id, text))
        Ok({})
    }
}
```

相对现状移除 `ctx`、`excluded`、`expected`、`Retry`、`Attempt` 与捕获复制。
发言仍须在完整读取集合中验证账户属于该房间，由 D4 的解析校验承担。

## 6. 实施阶段

| 阶段 | 内容 | 依赖 | 退出条件 |
| --- | --- | --- | --- |
| R0 | 示例层清理 | 无 | 语义不变，chat 六组合实网矩阵通过 |
| R1 | L1–L4 语言收敛，L5–L6 库接口 | 无 | 三后端行为测试；chat 删除对应样板 |
| R2 | D1 容量解耦 + D2 会话输出端与溢出策略 | 无 | 慢连接不影响他人提交；溢出连接按策略断开并清理；chat 删除排除列表与淘汰工作表 |
| R3 | D3 事务内创建域 + D6 不等待 `when` | R2 | 宿主不含房间流程；创建失败／取消不泄漏；房间创建可热更新 |
| R4 | D4 依赖访问路径 | R3 | 并发换房下解析校验与有界重算正确；chat 删除 `expected`／`Retry` |
| R5 | D7 混合模式：输出绑定标注、按能力准入、TCP 与 VM 代际 | 可与 R2–R4 并行 | chat 在 `--task-workers N` 下运行，业务仍串行；资源固定规则有拒绝测试 |
| R6 | D7 Domains 的 BoC 调度 | R2、R5 | 多 worker 下多域原子性、无死锁、取消与关闭清理；房间间实际并行有证据 |
| R7 | D5 撤销日志 | 测量结论 | 回滚正确性与预算；性能数据另行记录 |
| R8 | D8 预留模式示例、D9 监督策略、D10 确定性模式 | R2 | 各自行为测试与文档 |

优先级按收益与成本：R0、R1 成本低先行；语义层 R2 > R3 > R4；R5 中的输出绑定标注可最先独立完成；
R6 必须在 R2 定案后进行，避免固化跨线程多队列预留协议；R7 由测量驱动。

## 7. 验证要求

按 [AGENTS.md](../AGENTS.md) 的范围规则执行。编译器与运行时改动附带执行生成代码的行为测试，
JIT、解释器、独立 AOT 一致；热更新另覆盖内嵌 JIT／解释器脚本。
R2、R3、R6 涉及所有权、取消或任务生命周期，补跑相关 Debug 穷举；
chat 改动运行 `cargo test --test chat -- --ignored` 六组合实网矩阵。
多线程阶段沿用 TP 的受控交错与单 worker 推进验证，不以多个 VM 根替代同树分支并行。
不默认构建 Release；性能结论按基准方法独立测量。

## 8. 待定问题

- D1 是否需要为少数控制消息保留逐目标强一致投递；若保留，如何避免重新引入业务侧淘汰协议。
- D4 路径表达式的允许范围，以及与目录弱句柄、条件删除的交互。
- D6 不等待 `when` 的结果与失败如何观测，域关闭时已排队块体的清理顺序。
- D7 每 worker reactor 与共享 reactor 的取舍；BoC 队列与现有单调序号的合并方式。
- D5 撤销日志对 Map／List 等容器内部写入的记录粒度。
- 效果分类统一（2.2 第 5 项）的内部表示与是否最终开放源级语法。

## 9. 参考

- [When Concurrency Matters: Behaviour-Oriented Concurrency](https://doi.org/10.1145/3622852)：
  Verona 多资源行为调度，D6、D7 的主要参考。
- Calvin 确定性数据库的 OLLP（乐观锁位置预测）：D4 的解析—获取—校验模式。
- Clojure STM 中 agent 发送推迟到事务提交：D2 提交后效果的先例。
- Erlang/OTP 监督树与 `code_change`：D9 与后续状态布局迁移。
- Swift 区域隔离与 `sending`、Pony 引用能力：转移资格推导与未来区域概念。
- FoundationDB 确定性仿真测试：D10。
