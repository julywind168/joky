# 文件 I/O

`import joky/file`。所有文件 operation 都是 `@suspends` Effect；无 `await`，
复用 blocking pool 的 FIFO 异步准入、Pending ABI 和协作取消。

## 小文件便利 API

| API | 返回值 | 语义 |
| --- | --- | --- |
| `file.read(path)` | `Result(String, String)` | 整文件读取，拒绝非法 UTF-8 |
| `file.read_bytes(path)` | `Result(Bytes, String)` | 原样读取二进制，含 NUL 和非法 UTF-8 |
| `file.write(path, text)` | `Result(UInt64, String)` | 创建或截断覆盖，写入 String 的 UTF-8 字节 |
| `file.write_bytes(path, bytes)` | `Result(UInt64, String)` | 创建或截断覆盖，写入原始 Bytes |

路径为 String；错误为 String。空文件读取成功，空写入仍创建/截断目标。
写入成功返回完整字节数，内部处理短写及 Interrupted；写入失败报告
`write failed after N bytes: ...`。打开目标失败时还没有写入进度。
这些接口内存按整个文件大小增长；普通覆盖不是原子替换。

## 句柄与分块

`file.open(path, mode) -> Result(File, String)`，打开模式为：

| FileMode | 权限与创建行为 |
| --- | --- |
| `Read` | 只读，文件必须存在 |
| `Write` | 只写，不存在则创建，存在则截断 |
| `Append` | 追加，不存在则创建，每次系统写入由 OS 定位到末尾 |
| `ReadWrite` | 读写，必须存在，保留内容 |
| `CreateNew` | 读写，排他创建，已存在则失败 |

以下方法也可用 `file.operation(handle, ...)` 调用。除 `close(self)` 外，
方法 receiver 都是 `&self`，effect 对应参数是 `handle: &File`；它们
允许同一 owning 句柄连续读写。`close` 消费句柄，无论返回 Ok 还是 Err，
原绑定都不能再使用。

| 方法 | 返回值 | 约定 |
| --- | --- | --- |
| `read_chunk(max_bytes: UInt64)` | `Result(Bytes, String)` | 最多读取 max_bytes，必须为正；空 Bytes 为 EOF，短块不必然为 EOF |
| `write_chunk(data: Bytes)` | `Result(UInt64, String)` | 写完一块或返回带部分进度的错误；成功返回完整字节数 |
| `position()` | `Result(UInt64, String)` | 当前文件偏移，以字节计 |
| `seek(offset: Int64, origin: SeekFrom)` | `Result(UInt64, String)` | origin 为 Start/Current/End；Start 不接受负数；返回新位置 |
| `flush()` | `Result(Unit, String)` | 排空语言层写缓冲；当前无缓冲实现，不等于落盘 |
| `sync()` | `Result(Unit, String)` | 调用 OS 的文件数据及元数据同步；不能同步父目录创建记录 |
| `close()` | `Result(Unit, String)` | 消费 File 并释放 OS 文件资源，不隐含 sync |

读写按实际字节数推进共享文件位置。追加模式的 seek 不改变后续写入的
追加行为。OS close 的延迟错误无法通过 Rust File::drop 获知；需要同步
错误时，应在 close 前显式 sync。不能据此声称完整的原子/掉电安全发布。

`File` 是唯一所有权资源：可以移交，不能复制到多个并发分支；同一句柄
从等待准入到 syscall 完成持有 busy 预约，冲突操作（含 close）返回
`Err("file is busy")`，没有隐式游标竞争。完成前先释放预约，再恢复用户
代码。源码中的关闭后使用和重复关闭在编译期拒绝；底层关闭仍幂等，
原生协议中的关闭后操作仍返回 `Err("file is closed")`。

离开所有权 scope、失败或取消时，未关闭的句柄自动释放。provider 在挂起
期间持有原生资源引用，不复制语言层 owned 句柄；最后一个引用释放后，
自动关闭进入 blocking pool。显式 close 在 blocking worker 上执行。

## 取消与内存边界

- 等待准入和执行队列中的请求可被移除，立即释放参数及 busy 预约。
  不提前复制完整写缓冲区，也不提前分配读取缓冲区。
- 已开始的系统调用无法强制中断，取消可先返回；资源保持到 syscall
  返回。晚到结果不会恢复已取消的代码，结果释放由一方负责。
- 取消可能发生在截断、创建、部分写入或位置更新之后，无回滚保证，
  也没有可交给已取消代码的进度返回值。写循环在调用之间检查取消。
  调用方不得把取消/失败当作整块未写入后自动重试。
- 分块复制额外缓冲随并发度和块大小增长；当前从 OS 读缓冲构造 managed
  Bytes 时会复制一次，可短暂保留两块内存。丢弃处理完的块，才能避免
  用户自行收集全部内容。句柄、等待任务和收集结果仍有各自的内存成本。
- 自动关闭任务不可取消，与其他 blocking 工作共用准入队列。scope 不等待
  不可中断的 syscall；本地资源测试在工作排空后检查最终归零。

运行示例（仓库根目录）：`cargo run -- run examples/io/file_copy.jk`、
`examples/io/file_batch.jk`、`examples/io/file_timeout.jk`。复制/超时示例只写
`target/` 下的示例文件；取消示例的目标可能不存在、部分写入或全部写入。

实现和验收记录：[5.3](../archive/reports/phase5-3.md)。
