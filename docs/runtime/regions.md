# 统一 Region 生命周期与 Cown 不逃逸检查

2026-09-23 首版实现。语法简化为 `region { ... }`，没有区域名字或一等区域句柄。
保留 `Cown.new` 和 `when`；所有 Cown 采用区域生命周期。
其他堆对象仍使用原有所有权规则，通用 arena 分配留待后续扩展。

## 1. 决策与目标

每个 Cown 由创建时的当前区域拥有。编译器检查句柄不能活过所属区域，runtime
在区域退出时排空相关任务，再批量销毁 Cown。不依赖引用计数、查环或全局停顿。
同一区域内允许自环、多对象环，以及经过共享不可变容器的环。

| 创建位置 | Cown 归属 | 回收时机 |
| --- | --- | --- |
| 显式 `region` 内 | 当前最内层区域 | 该区域退出 |
| 区域调用的函数、启动的任务内 | 继承调用/启动时的当前区域 | 同上 |
| 没有显式 `region` 的入口代码 | 隐式根区域 | 入口工作排空、根区域关闭 |

普通块、函数、branch 和循环迭代不自动建立内存区域。task scope 和内存区域是
两种不同边界；显式 `region` 同时建立两者。

所有 Cown 句柄复制/丢弃都不操作原子引用计数。丢弃最后一个句柄也不会提前释放
Cown。因此长期运行的根区域可能持续积累对象，应按请求、会话或循环轮次使用
`region`。没有自动晋升、RC 回退模式、祖先区域分配 API 或跨区域复制。

## 2. 源码形式

```joky
class Counter {
    var value: Int32 = 0
    fn bump() { self.value = self.value + 1 }
}

fn make_counter() -> Cown(Counter) { Cown.new(Counter()) }

fn main() {
    let total = region {
        let counter = make_counter()
        let _ = parallel {
            | when (counter) |state| { state.bump() }
            | when (counter) |state| { state.bump() }
        }
        when (counter) |state| { state.value }
    }
    println(total) // 2；counter 已释放。
}
```

- `region` 是表达式；可以返回标量、普通 String/List 等快照，以及祖先区域的引用。
  结果不能包含本区域或后代区域的 Cown。
- `Cown.new(payload)` 消费现有规则允许的唯一 payload，并登记到当前区域。
  函数返回不关闭继承的区域，所以工厂函数可以返回 Cown。
- 闭包调用中的分配使用调用时的区域；捕获的 Cown 保持原归属。
- 每次执行 `region` 都创建独立实例，包括循环、递归和并发进入同一源码块。
- `when` 内禁止直接或经辅助函数进入 `region`，以免带着 lease 等待任务。
  区域体本身可以挂起；已有 `when` 不挂起、不嵌套的约束不变。
- 嵌入式执行的根区域属于该次受管理执行，不能将 Cown 保存到执行之外。

完整运行示例见 [examples/concurrency/regions.jk](../../examples/concurrency/regions.jk)。

## 3. 区域身份与存储规则

源码类型继续写 `Cown(T)`，不引入用户生命周期参数。typed MIR 上的跨函数分析
跟踪当前区域、局部区域、参数引用和参数存储的依赖，传播至 tuple、Option、Result、
class、容器、闭包、任务结果、别名和控制流合流。类型注解不能擦除区域依赖。

### 3.1 引用方向

```text
inner Cown ──允许──> ancestor Cown
ancestor Cown ──禁止──> inner Cown
同一区域 A <──────> B              允许
```

规则同时约束构造、变量赋值、payload 修改、可变容器和辅助函数调用。向一个
存储位置写入句柄时，该句柄必须至少活到这个位置的使用边界。不能先把空容器
放到外层，再经别名插入内层句柄；也不能靠之后清空来通过检查。

```joky
let escaped = region { Cown.new(Counter()) } // 拒绝：结果逃逸。

var outer = Cown.new(Counter())
region { outer = Cown.new(Counter()) }      // 拒绝：写入外层变量。

let old = Cown.new(Counter())
let alias = region { old }                 // 允许：祖先区域仍有效。
```

Cown payload 可以通过 class 字段或容器包含其他 Cown。不能直接写
`Cown.new(Cown.new(...))`：内层 Cown capability 不是允许的唯一 payload 类型。

### 3.2 函数与模块边界

分析按固定点推导函数的返回依赖、存储别名、参数写入、寿命约束和是否进入区域。
直接调用和本地泛型实例使用这些摘要；跨模块函数和方法的摘要保存在模块 ABI 中，
并参与依赖缓存指纹。链接后继续对实际 MIR 验证，不要求内联。

区域约束修改即使未改变源码函数签名，也会使依赖缓存失效。旧缓存和旧 runtime
不兼容：当前 module metadata/cache ABI 为 **43**，managed ABI 为 **8**，AOT
runtime ABI 为 **v25**。需按 [README 构建说明](../../README.md#tests-and-benchmarks) 重建 runtime archive。

### 3.3 首版保守限制

当前实现优先拒绝不能证明安全的用法；以下限制属于已实现的检查行为：

- 聚合字段和控制流依赖采用合并分析。字段覆盖、清空、某些别名或高阶用法可能
  安全但仍被拒绝；诊断给出所在函数、保存/返回/使用原因及可用源码位置，暂不展开
  完整的字段传播链。
- 间接调用按可能写入所有参数及捕获状态、可能进入区域处理。因此 `when` 内的
  间接调用被拒绝；首版也不允许在显式局部区域内调用未知函数体，避免隐藏的
  用户析构对象绕过区域清理约束。
- 尚无具体摘要的导入桩（包括尚待实例化的模块泛型）采用保守约束，可能拒绝
  本来安全的调用。已验证的普通跨模块工厂和方法可以使用。
- Dyn 包装保留引用依赖，但动态调用仍受间接调用限制。携带 Cown 的 effect、
  abort 载荷、FFI 或未知原生持有者没有获准的区域契约，拒绝跨越这些边界。
- 带用户 `Drop` 的对象只能携带隐式根区域的 Cown 引用，包括经辅助函数创建
  的对象；不能携带显式局部区域的引用。
  异常清理可能先关闭区域、再丢弃剩余普通存储；必须阻止此时的用户析构代码
  重新观察已经释放的 Cown。后续可以在更精确的析构顺序证明下放宽。

没有动态寿命检查或隐式 RC 回退来放行上述情况。

## 4. Runtime 所有权与批量清理

### 4.1 区域与继承

每个区域有稳定的 `Arc` 描述符、父区域和短锁保护的 Cown 登记表。Cown 暂时仍
逐对象分配，不是 bump allocator。任务创建时捕获当前区域；continuation 在
挂起边界保存、在恢复入口安装该区域，Pending 展开会恢复调用者的上下文。
线程本地变量只保存当前执行上下文，不是区域唯一的所有者。

显式区域由对应 task group 关闭，普通 task group 不关闭继承的区域。根
`RuntimeScope` 在全部工作排空后关闭根区域，并兜底关闭尚未释放的后代区域。
区域状态为 `Open → Closing → Closed`，重复关闭不会重复析构；并发关闭者等待
第一次关闭完成，保证 JIT drop glue 不会在仍有析构时卸载。

### 4.2 退出顺序

1. 保留合法结果；正常出口等待后代任务，异常出口取消并排空相关工作。
2. 释放活动 lease、任务访问和 continuation 使用权。正常作用域边执行局部清理。
3. 封存区域表；逐个执行编译器生成的 payload 析构，递归释放普通字段/容器拥有引用。
4. 全部 payload 清理后，释放整批 Cown 控制块。

Cown 句柄用对齐指针的低位标记。通用 `dup/drop` 在读取对象头前识别这个标记，
句柄复制/丢弃只是传递或丢弃 capability；acquire/payload/release 会先去掉标记。
异常路径中稍后才清理的容器或局部存储，可以安全丢弃已失效的惰性句柄，不能再
读取其 payload。这也是当前限制用户析构对象只能携带根区域 Cown 的原因。

控制块内部保留通用 managed 分配头，只用于最终释放；没有给语言句柄维持一套
隐藏的原子 RC。普通共享 String/List/Map 的引用计数、lease 同步和区域登记锁仍存在。

活动 lease 已改为任务本地批次；竞争等待使用每 Cown 队列及精确通知，取消和
回滚也参与唤醒，不再依赖全局 lease HashMap 和 1ms 重试。获取阶段已接入 Pending，
等待时交还 worker；协议与性能数据见 [Cown 调度](cown-scheduling.md)。

沿用 Cown payload 限制：不能直接拥有 file、socket、native handle、task、
continuation、lease 或用户 `Drop`。区域析构不能运行用户 `when`。

### 4.3 控制流与取消

挂起不关闭区域，恢复不重新创建区域。正常落出、`?`、跨区域 break/continue、
abort、任务失败和取消都清理实际离开的区域。区域内 handler 或内层循环的跳转
不关闭仍有效的外围区域。取消信号发出后仍需等访问者退出，不能立即释放。

进程被强制终止或 runtime 无法展开的致命故障不提供语言级析构保证。

## 5. 成本与验证

区域关闭只等待相关结构化工作，不 stop-the-world，也不扫描祖先 payload。
清理成本为 **O(区域 Cown 数量 + payload 析构工作)**，不是 O(1)；等待任务和
析构仍可能产生退出延迟。暂不承诺相对 RC 的整体吞吐收益，仍需单独基准测量。

回归覆盖：工厂/任务/跨模块继承、同区域环和共享 List 中间对象、祖先存活、直接与
间接逃逸、外层容器写入、挂起后分配、多轮区域退出、break/continue、abort、取消、
任务失败和错误传播。runtime 测试检查只析构一次；编译器测试还在根区域关闭前
检查多轮显式区域的 managed 对象计数归零。

CLI 矩阵验证 legacy、冷/热缓存 JIT、debug/release AOT，并检查执行后资源计数归零。
相关测试见 `src/compiler/tests/cown.rs`、`crates/joky-runtime/src/runtime/region.rs`
与 `tests/cli/parity.rs`。

本次验证（macOS ARM64）：

| 检查 | 结果 |
| --- | --- |
| `cargo test --lib` | 1130 通过，10 忽略 |
| runtime 独立 `cargo test --lib` | 217 通过，4 忽略 |
| `JOKY_TEST_AOT=off cargo test --test cli` | 182 通过 |
| Cown CLI，`runtime-test-support` + `JOKY_TEST_AOT=full` | 11 通过，覆盖缓存与 debug/release AOT |
| Cursor CLI，`runtime-test-support` + `JOKY_TEST_AOT=off` | 12 通过 |

后续已修复 SQLite/native Debug 的句柄包装对象泄漏：挂起操作的值参数统一由
continuation 清理，借用参数仍由调用者持有，详见
[资源所有权交接](resource-lifetimes.md#native-与-runtime-的所有权交接)。
该泄漏在旧提交 `23fb084` 也能复现，与区域所有权改动无关。

修复验证：compiler lib 1132 通过、10 忽略，runtime lib 217 通过、4 忽略，
FFI 21 通过。启用资源计数的 provider/native Debug CLI 15 项通过冷缓存 JIT
和 debug/release AOT；SQLite 示例另验证热缓存 JIT、legacy 和 no-cache，全部归零。

后续已修复 `equality_crosses_modules_cache_and_aot` 残留的 1 个 managed 对象：
`Option` 与字面量 `None` 比较只读标签，其求值持有的共享引用现在由比较作用域
释放。module metadata/cache ABI 升至 41，使旧 MIR 缓存失效；runtime ABI 不变。
compiler lib 1133 通过、10 忽略；新增回归覆盖共享/唯一所有权、临时值、泛型借用
及 `None` 在比较两侧的情况。equality CLI 的 no-cache、冷/热缓存 JIT 和
debug/release AOT 均归零；单文件回归的 legacy 路径也归零，两个 ordering CLI
矩阵通过。

process/Pipeline 测试的资源报告问题也已修复：三处自行启动 CLI 的路径补齐
`JOKY_TEST_RESOURCE_REPORT`，报告匹配允许子进程 stderr 未换行而产生的前缀，
仍要求每个阶段恰好一份完整的全零报告。5 项测试在启用资源统计的完整矩阵下
通过，覆盖 legacy、冷/热缓存 JIT、debug/release AOT 和取消后的子进程回收。

## 6. 后续通用 Arena 扩展

`region` 保留为通用生命周期边界，Cown 是第一批受区域管理的对象。未来可把块内
新建的其他堆对象纳入区域，再以 bump allocator 优化分配。标量和栈值不必进入 arena，
进入区域前存在的对象也不改变归属。

后续需要先解决：普通 String/List 结果外带的兼容性与复制规则、共享节点及别名、
资源析构、并发分配同步。不能把整块释放等同于“不需要 Drop”，也不能悄悄禁止
首版允许返回的普通快照。

相关文档：[语言基础](../plans/design-baseline.md#9-cown-与共享状态)、
[语法说明](../lang/concurrency.md#cown-与-when)、
[资源生命周期](resource-lifetimes.md)、[待办](../plans/backlog.md)。
