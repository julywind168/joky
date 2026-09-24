# 结构化并发与 Cown

本文描述当前语言行为。章节导航见[语言索引](README.md)。

- [Effect 与结构化并发](#effect-与结构化并发)
- [Cown 与 `when`](#cown-与-when)

## Effect 与结构化并发

Effect 描述一段计算可能请求的外部能力，trait 描述值或类型支持的操作。Effect 不替代 trait/interface。

Effect 声明中每个 operation 都是一个 `fn`；挂起语义用标注声明在 `fn` 之前：`@suspends` 表示可挂起（provider 完成或被取消）、`@resumable` 表示可挂起且 Handler 必须恢复、不加标注表示普通（同步）operation：

```joky
eff Network {
    @suspends fn request(request: Request) -> Response
}

eff Log {
    fn write(message: String) -> Unit
}

fn load_user(id: String) -> User effects { Network, Log } {
    Log.write("loading " + id)
    let response = Network.request(Request(path: id))
    decode_user(response)
}

let user = do {
    load_user("alice")
} with {
    Log.write(message) => println("[test] " + message)
}
```

`do { ... } with { ... }` 在词法作用域内安装 Effect Handler。未处理的 Effect 继续向外层传播；用户层不使用 `async/await`，`suspends` operation 的挂起由编译器自动降低为 continuation。

`@suspends` operation 可以由 Handler 直接返回值同步拦截；`@resumable` operation 的 Handler 也使用直接返回值隐式恢复 continuation。需要结束当前 `do` 时使用 `abort value`；Handler block 可以先执行普通表达式再以 `abort value` 结束。

`parallel` 建立结构化任务边界；每个以 `|` 开始的 arm 都是可在多 worker 线程上并行执行的子任务。它在返回前等待全部 arm，并按源码顺序返回 arm 结果组成的元组。失败会取消同级任务，任务不能逃出当前结构化作用域：

```joky
let (left, right) = parallel {
    | heavy_compute_left()
    | heavy_compute_right()
}
```

`race` 同样使用 `|` 分隔 arm；所有 arm 必须产生相同类型，第一个成功结果获胜，并等待其他 arm 完成取消清理：

```joky
let response = race {
    | request_primary()
    | request_backup()
}
```

`branch { ... }` 表示一个单独的子任务表达式；`main` 自带隐式 root scope。`parallel`、`race` 和 `branch` 会降低为 task MIR，并由 runtime 的结构化 task group 执行；scope 退出前会等待其中所有子任务完成。唯一所有权捕获会 move 到任务；`race` 自动清理输家结果。

```joky
fn main() {
    branch { println("background work") }
    println("main continues after the branch joins")
}
```

scope 不是用户关键字，而是 runtime 的结构化边界：每个 `parallel`/`race`/`branch`（以及 `main` 的隐式 root scope）都建立一个新的 task scope。scope 的语义约束由编译器和 runtime 共同保证——任务值不跨 scope 逃逸（join 结果必须先由 scope 认领）、owned 捕获 move 进任务、scope 取消会传播到其中所有子任务并等待清理完成。

## Cown 与 `when`

`Cown(T)` 是可跨任务复制的隔离 capability；它不能直接访问内部 payload。Cown 使用显式工厂构造：

```joky
let counter = Cown.new(Counter(value: 0))
```

`Cown.new(value)` 消费 `value` 的唯一所有权，并返回可跨任务复制的 `Cown(T)`。
所有 Cown 自动归属当前区域；函数和任务继承区域，没有显式区域时归属入口的隐式根区域。
Cown 句柄不使用引用计数，区域退出排空任务后批量销毁 payload 和控制块，内部循环也能回收。

```joky
let snapshot = region {
    let counter = Cown.new(Counter(value: 0))
    when (counter) |state| { state.value }
}
```

语法只接受 `region { ... }`，不接受名字。区域结果和写入外层存储的值不得携带
本区域的 Cown，包装进容器或闭包也一样；可以返回祖先句柄和普通数据快照。
`when` 内不能直接或间接进入区域。普通循环不会自动建立内存区域，长期分配应按
每轮/每请求显式划分。其他堆对象的通用 arena 分配留待后续实现。
完整规则及保守限制见[区域文档](../runtime/regions.md)。

Cown payload 只适合内存中的业务状态：可以包含普通值、不可变容器、可变字段和其他 Cown capability，但不能直接拥有 file、socket、native handle、task、continuation 或 lease。此类资源必须由唯一所有者持有；需要时可显式关闭或取消，离开所有权图时由 runtime 自动释放。

`when` 获取一个或多个 Cown 的临时独占访问权，payload 用尾随闭包参数命名：

```joky
when (counter) |state| {
    state.value = state.value + 1
}
```

多个 Cown 必须显式列出。runtime 为每个 Cown 分配稳定 ID，并按 ID 原子获取整组 lease；竞争失败会回滚本次已获取的 lease，等待时 worker 会让出到任务调度器，lease 释放会唤醒等待者，避免源码顺序造成死锁：

```joky
when (source, target) |source_state, target_state| {
    transfer(source_state, target_state)
}
```

省略 `|...|` 参数列表时，全部 Cown 都必须是简单名称，payload 隐式绑定为同名局部变量。参数数量必须与 Cown 数量一致，也可以使用 `_` 丢弃某个 payload 绑定。

`when` body 不能挂起、启动捕获 lease 的 branch、返回 lease 或把 lease 存入堆对象。需要同时访问多个 Cown 时禁止依赖嵌套 `when`，应一次列出完整集合。

条件等待的用法与限制见[`when until`](when-until.md)，实现登记与唤醒协议见[Runtime 条件等待](../runtime/when-until.md)。
