# 子进程

`import joky/process`。构造命令是纯操作，执行需要 `effects { process }`。
执行通过 `@suspends` operation 调度，等待子进程时让出 Joky 调度线程。

```joky
import joky/process

fn main() effects { process } {
    let command = process.command("git").with_args(List("--version"))
    let output = process.capture(command)!
    println(output.stdout.to_string()!)
    if !process.success(output.status) { panic("git failed") }
}
```

## 配置与结果

`process.Command` 是保存不可变配置的普通 class，不持有操作系统进程。
`process.command(program)` 使用空参数、当前目录和继承环境。
`command.with_args(args)`、`with_cwd(directory)`、`with_env(policy)` 返回新配置。
执行借用 `&Command`，同一配置可以重复执行。

| 类型 / API | 语义 |
| --- | --- |
| `process.EnvPolicy.Inherit(Map(String, Option(String)))` | 从本次 runtime 运行的环境快照继承，`Some` 覆盖，`None` 删除 |
| `process.EnvPolicy.Replace(Map(String, String))` | 完全替换子进程环境 |
| `process.ExitStatus.Exited(code: UInt32)` | 正常退出，包括非零退出码；Windows 退出码保留 32 位 |
| `process.ExitStatus.Signaled(signal: Int32)` | Unix 信号终止 |
| `process.Output` | `status: ExitStatus`、`stdout: Bytes`、`stderr: Bytes` |
| `process.success(status)` | 仅 `Exited(0)` 为 true |
| `process.code(status)` | 正常退出返回 `Some(code)`，信号终止返回 `None` |

参数逐项直接传给程序，不解释 shell 命令、通配符、管道或重定向。需要 shell 时
显式执行 `/bin/sh` 等程序。空参数、Unicode 和空环境值保留原样；程序名为空、
参数或目录包含 NUL、环境变量名为空或包含 `=` / NUL 时返回 `Err`。
父进程的环境和工作目录不会改变。

绝对程序路径直接使用；带路径分隔符的相对程序名相对于子进程 cwd。
裸程序名在**配置完成后的子进程 PATH** 中搜索，PATH 中的相对目录也相对于子进程 cwd。
没有 PATH 时裸程序名返回 `Err`，此时可以传入明确路径。
Windows 搜索支持原生 `.exe` 后缀，不提供 PATHEXT / `.bat` / `.cmd` 的 shell 执行。
操作系统仍可能自行设置特殊环境值；例如 shell 可以重建 PWD。

## 执行

| API | 返回值 | 标准输入输出 |
| --- | --- | --- |
| `process.status(command)` | `Result(ExitStatus, String)` | 继承 stdin / stdout / stderr |
| `process.output(command, input, max_output)` | `Result(Output, String)` | 写入 `Bytes` 后关闭 stdin，并发收集 stdout / stderr |
| `process.capture(command)` | `Result(Output, String)` | 空 stdin，stdout + stderr 总上限为 16 MiB |

`max_output: UInt64` 是两条输出流**合计**的字节上限，允许零。超过上限后终止并回收
直接子进程，返回 `Err`，不返回部分输出。它限制捕获内容大小，不是整个进程的内存预算；
输入复制、缓冲区容量和结果复制仍占用额外内存。

输出保持原始字节，不隐式解码。`Bytes.to_string()` 执行严格 UTF-8 解码，非法编码返回 `None`。
非零退出和信号终止仍是 `Ok`；无法启动或管道 I/O 失败返回 `Err`。
子进程提前关闭 stdin 时，未写完的输入会被舍弃，不把 BrokenPipe 单独视为失败。

## 管道

`Command.pipe(next)` 创建 `Pipeline`，`Pipeline.pipe(next)` 在末尾添加一个命令。
这两个方法借用配置并返回新配置，不会启动进程，也不改变原命令或原管道。
`process.pipeline(command)` 可以构造只有一个阶段的管道。推荐通过这些 builder
创建管道；`Stage` 和 `Pipeline.stages` 是配置的内部表示（按逆序存储）。空管道执行返回 `Err`。

```joky
import joky/process

fn main() effects { process } {
    let files = process.command("git").with_args(List("ls-files"))
    let filter = process.command("grep").with_args(List("\\.jk$"))
    let count = process.command("wc").with_args(List("-l"))
    let pipeline = files.pipe(filter).pipe(count)
    let result = process.capture(pipeline)!
    println(result.stdout.to_string()!)
    println(result.success())
}
```

等价于 `git ls-files | grep '\.jk$' | wc -l` 的标准流连接方式。各阶段并发执行，
前一阶段 stdout 通过操作系统管道直接连接后一阶段 stdin，不在 runtime 中累积
中间输出；只有最终 stdout 和各阶段 stderr 会被捕获。
每一阶段独立使用自己的参数、cwd 和环境配置。

| API | 管道返回值 | 标准流行为 |
| --- | --- | --- |
| `process.status(pipeline)` | `Result(List(ExitStatus), String)` | 首阶段继承 stdin，末阶段继承 stdout，各阶段继承 stderr |
| `process.output(pipeline, input, max_output)` | `Result(PipelineOutput, String)` | 向首阶段写入 Bytes 后关闭 stdin，收集最终 stdout 和全部 stderr |
| `process.capture(pipeline)` | `Result(PipelineOutput, String)` | 空 stdin，总输出上限 16 MiB |

```text
PipelineOutput {
    statuses: List(ExitStatus)
    stdout: Bytes
    stderrs: List(Bytes)
}
```

`statuses` 和 `stderrs` 按命令执行顺序排列，长度等于阶段数。
`result.success()` 仅在所有阶段都正常退出且退出码为 0 时返回 true；
`process.success(status)` 继续用于单个 `ExitStatus`。

非零退出仍返回 `Ok`，且不会自动取消其他阶段。例如 `grep` 的退出码 1 表示无匹配，
可检查对应 `statuses` 元素决定是否接受；下游提前退出可能让上游收到 SIGPIPE，
同样保留在各阶段状态中。

`max_output` 只计算最终 stdout 与所有 stderr 的合计字节数，不计算中间管道流量。
超限会终止并回收所有已启动阶段，返回 `Err`，不返回部分结果。
配置验证会尽量在启动前完成；中途某阶段无法启动时，会终止并回收此前已启动阶段，
错误消息包含阶段序号（从 1 开始）。已经发生的外部副作用不会回滚。

`status` / `output` / `capture` 是基于 `Executable` 关联类型的泛型辅助函数，
同一调用方式接受 `Command` 或 `Pipeline`。原单命令返回类型不变。
本地 handler 中单命令仍拦截 `process.status` / `process.output`，管道则拦截
`process.pipeline_status` / `process.pipeline_output`：

```joky
import joky/process

fn fixture() -> Result(process.PipelineOutput, String) {
    Ok(process.PipelineOutput(statuses: List(process.ExitStatus.Exited(0)), stdout: Bytes.from_string("mock"), stderrs: List(Bytes())))
}
fn invoke(pipeline: &process.Pipeline) -> Result(process.PipelineOutput, String) effects { process } {
    process.capture(pipeline)
}
fn main() effects { process } {
    let pipeline = process.pipeline(process.command("unused"))
    let result = do { invoke(pipeline) } with {
        process.pipeline_output(_, _, _) => fixture()
    }
    println(result!.stdout.to_string()!)
}
```

## 取消和资源边界

`race` 失败分支被取消时，请求终止并回收所有直接子进程、关闭管道、等待 I/O 线程结束。
运行作用域退出会等待这些清理完成。取消不回滚子进程已经产生的外部副作用。

本版不管理进程树：子进程派生的其他进程不保证被终止。后代若一直持有输出管道，
正常捕获会继续等待 EOF；可通过 `race` 与 `time.sleep` 实现超时取消。
每个执行请求使用一个监督线程。单命令捕获另有最多三个管道线程；
N 阶段管道捕获另有 N + 2 个线程，分别处理各阶段 stderr、最终 stdout 和首阶段 stdin。
批量执行应使用 `@parallel(limit: N)` 控制并发数量。

暂不提供 `spawn` / `Child`、流式交互、后台脱离、手动 kill 或进程组接口。
本次端到端测试覆盖 macOS；Linux / Windows 的平台分支仍需相应系统的 CI 验证。

## 本地 handler

本地 handler 优先于 native provider。可直接构造 Output 来测试，不创建真实进程：

```joky
import joky/process

fn fixture(command: &process.Command) -> Result(process.Output, String) {
    Ok(process.Output(status: process.ExitStatus.Exited(0), stdout: Bytes.from_string(command.program), stderr: Bytes()))
}
fn invoke(command: &process.Command) -> Result(process.Output, String) effects { process } {
    process.capture(command)
}
fn main() effects { process } {
    let result = do { invoke(process.command("mock")) } with {
        process.output(command, _, _) => fixture(command)
    }
    println(result!.stdout.to_string()!)
}
```

匹配导入的枚举时，先绑定类型值，例如 `let Status = process.ExitStatus`，再写
`match status { Status.Exited(code) => ..., Status.Signaled(signal) => ... }`。

AOT runtime ABI 为 v25；升级后请按 [README](../../README.md#tests-and-benchmarks) 重新构建 runtime archive。
