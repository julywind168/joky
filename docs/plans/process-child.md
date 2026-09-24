# Child 与交互式子进程

> 状态：暂缓，API 为设计草案；当前支持范围见[子进程 API](../stdlib/process.md)。

2026-09-23 决定先记录待办。本节是设计草案，API 尚未实现；当前能力见 [子进程文档](../stdlib/process.md)。

## 范围与候选 API

先支持单个 `Command` 启动后的交互与生命周期管理。`Command` / `Pipeline` 继续是纯配置，
`Child`、`PipeReader`、`PipeWriter` 为唯一拥有的 native 资源。首版不包含 PTY、后台脱离、
进程组 / 进程树管理；`Pipeline.spawn` / `ChildGroup` 留待后续需求。

以下为候选签名；执行和 I/O 需要 `process` effect，等待与管道 I/O 必须可挂起、可取消。

| API | 候选返回值 / 语义 |
| --- | --- |
| `Stdio.Inherit` / `Null` / `Pipe` | 继承、连接空设备、创建管道 |
| `command.with_stdin(mode)` / `with_stdout(mode)` / `with_stderr(mode)` | 返回新配置，供 `spawn` 使用；默认全部继承 |
| `process.spawn(command: &Command)` | `Result(Child, String)`，启动成功即返回 |
| `child.id()` | `UInt32`，原始 PID，不作为跨生命周期的进程身份凭证 |
| `child.try_wait()` | `Result(Option(ExitStatus), String)`，运行中为 `None` |
| `child.wait()` | `Result(ExitStatus, String)`，等待并回收，缓存退出状态，重复等待返回同一状态 |
| `child.kill()` | `Result(Unit, String)`，请求强制终止；仍需等待回收，已确认退出时成功返回 |
| `child.take_stdin()` | `Option(PipeWriter)`，转移端点；未配置 `Pipe` 或已取走时为 `None` |
| `child.take_stdout()` / `take_stderr()` | `Option(PipeReader)`，转移语义同上 |
| `reader.read(max_bytes: UInt64)` | `Result(Bytes, String)`，允许短读；正数请求返回空 Bytes 表示 EOF，零请求返回 `Err` |
| `writer.write_all(data: Bytes)` | `Result(Unit, String)`，全部写入或报错；失败 / 取消前可能已部分写入，不能直接重试整段数据 |
| `reader.close()` / `writer.close()` | `Result(Unit, String)`，幂等关闭；关闭后 I/O 返回 `Err` |
| `child.communicate(input: Bytes, max_output: UInt64)` | `Result(Output, String)`，写入并关闭 stdin，同时排空 stdout / stderr，最后回收进程 |

实现前需定稿：stdio 放在 `Command` builder 中，还是独立 `SpawnOptions` 中。
现有 `status` / `output` / `capture` 的标准流行为必须保持；若采用 builder，需要明确冲突配置的处理方式，
避免静默忽略用户设置。端点及 Child 方法的借用 / 消耗签名也需结合语言所有权规则验证。
建议 `communicate` 消耗 Child，要求三个流均为 `Pipe` 且端点尚未取走；配置不满足时返回明确错误并清理所拥有资源。

## 交互示例（草案）

下面仅演示小消息的一次读写，假设目标平台提供 `/bin/cat`；stderr 继承，避免创建无人读取的 stderr 管道。
`read` 不保证读满或读到整行，真实协议需要自行分帧。批量数据优先使用 `communicate`。

```text
let command = process.command("/bin/cat")
    .with_stdin(process.Stdio.Pipe)
    .with_stdout(process.Stdio.Pipe)
    .with_stderr(process.Stdio.Inherit)
let child = process.spawn(command)!
let input = child.take_stdin()!
let output = child.take_stdout()!
input.write_all(Bytes.from_string("hello\n"))!
let reply = output.read(4096)!
println(reply.to_string()!)
input.close()!
// 继续读取直到 EOF，处理可能的短读。
loop {
    let rest = output.read(4096)!
    if rest.is_empty() { break }
    println(rest.to_string()!)
}
let status = child.wait()!
output.close()!
```

## 生命周期与取消契约

- `wait` / `try_wait` 统一回收与缓存状态，非零退出或信号终止仍返回 `Ok`。`wait` 不负责读取输出，也不自动关闭 stdin；调用方必须关闭输入并消费已取走的输出，防止死锁。
- 管道端点只能取走一次，可以分别移动到结构化任务中并发读写；同一端点的重叠操作应由所有权规则拒绝或明确串行化。子进程退出后，reader 仍可读取剩余缓冲数据。
- `communicate` 复用现有 `output` 的字节输出、stdout + stderr 合计上限和错误清理语义；提前关闭 stdin 的 BrokenPipe 可按现有规则容忍。独立 `write_all` 则报告 BrokenPipe，避免误报写入成功。
- 建议取消一次 `wait` 只取消该次等待，保留仍被拥有的 Child；丢弃 Child、取消消耗 Child 的 `communicate` 或退出 RuntimeScope 时，终止并回收直接子进程。需验证取消路径中所有权释放顺序，不得遗留僵尸进程。
- 已转移端点由各自 owner 关闭；Child 的析构不能通过过期 fd / handle 关闭其他资源。RuntimeScope 退出仍须取消并排空所属端点的 I/O，避免持有端点的后代进程阻止清理。
- native 清理不调用已卸载的生成代码。监督状态、正在进行的 I/O 和作用域清理 lease 必须协调；完成结果被拒收时同样释放 Child / 端点及托管结果。

## 实施顺序与验收

- [ ] 定稿 stdio 配置、方法所有权、取消与端点转移契约；验证端点移动到结构化任务的可表达性。
- [ ] 接入 native 类型注册与析构、provider operation / 布局契约及必要的 ABI / 模块缓存版本更新；从当前每次执行独占子进程的监督线程演进到持续存活的资源状态。
- [ ] 实现单命令 `spawn`、PID、状态缓存、等待、终止与 drop 清理，再接端点 I/O 和 `communicate`。
- [ ] 覆盖启动失败、非零 / 信号退出、重复 wait / kill / close、一次性端点转移、stdin EOF、短读与 BrokenPipe；验证 wait 不隐式消费管道。
- [ ] 用超过管道容量的 stdin / stdout / stderr 验证并发排空；测试输出上限、启动前后取消、挂起读写取消、Child / 端点丢弃及完成结果拒收，无僵尸进程、句柄泄漏或 fd 重用竞态。
- [ ] 覆盖本地 handler、冷 / 热缓存 JIT、legacy、debug / release AOT；macOS / Linux / Windows 实跑资源清理用例，更新文档和交互示例。保留现有单命令和 Pipeline 的回归覆盖。
