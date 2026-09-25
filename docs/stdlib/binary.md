# 二进制编解码与 TCP 缓冲读取

`joky/binary` 是纯 Joky 实现，基于 `Bytes` 和 `MutBytes`。

```joky
import joky/binary

fn main() -> Result(Unit, String) {
    let output = binary.writer()
    output.write_i32_be(-1)
    output.write_cstring("中文")?
    let input = binary.reader(output.to_bytes())
    println(input.read_i32_be()?)
    println(input.read_cstring()?)
    input.finish()?
    Ok(())
}
```

## Reader

`binary.reader(data: Bytes)` 创建 `BinaryReader`。方法借用接收者并推进内部游标：

| 方法 | 返回值 | 语义 |
| --- | --- | --- |
| `remaining()` | `UInt64` | 未消费的字节数 |
| `read_u8()` | `Result(UInt8, String)` | 一个字节 |
| `read_u16_be()` / `read_u32_be()` / `read_u64_be()` | 对应无符号整数的 `Result` | 网络大端序 |
| `read_i16_be()` / `read_i32_be()` / `read_i64_be()` | 对应有符号整数的 `Result` | 大端序，按补码解释 |
| `read_bytes(length)` | `Result(Bytes, String)` | 读取指定长度，允许零长度 |
| `read_cstring()` | `Result(String, String)` | 读取 NUL 结尾的严格 UTF-8 字符串，并消费终止符 |
| `finish()` | `Result(Unit, String)` | 拒绝未消费的尾部字节 |

截断、缺少终止符或非法 UTF-8 返回 `Err`，读失败不改变游标。
长度检查使用剩余长度比较，避免 `position + length` 溢出。
通过 `reader` 工厂创建对象；直接构造类型时必须保持 `position <= data.length()`。

## Writer

`binary.writer()` 创建 `BinaryWriter`。`write_u8`、`write_u16_be`、
`write_u32_be`、`write_u64_be` 和对应的有符号版本返回 `Unit`。
`write_bytes(Bytes)` 追加原始字节；`write_cstring(String)` 返回
`Result(Unit, String)`，拒绝内嵌 NUL，失败不改变缓冲。

`length()` 返回当前长度。`to_bytes()` 返回独立快照，后续追加不改变旧值。
当前快照、字节切片均会复制数据；Reader/Writer 不提供零拷贝承诺。

## TCP 缓冲

`import joky/socket/buffered`，并在调用模块中导入 `joky/socket/tcp`。
`buffered.empty()` 创建 `TcpReadBuffer`。该状态是普通值，拥有未消费的
字节和偏移，不拥有 socket。

- `buffered.read_exact(stream: &TcpStream, state: TcpReadBuffer, length: UInt64, max_read: UInt64)`
  返回 `Result((Bytes, TcpReadBuffer), String)`，声明 `effects { tcp }`。
  成功后必须将返回的新状态用于同一连接的下一次读取。
- `buffered.write_all(stream: &TcpStream, data: Bytes)` 返回
  `Result(Unit, String)`，成功表示完整发送；空写入直接成功。

读取以 8192 字节块预读，保留多读的字节。零长度读取不执行 I/O；
`length > max_read` 或非法状态偏移在 I/O 前返回错误。
正常返回的数据长度始终等于请求长度。开始时 EOF 报 `end of stream`；
消费了部分请求后 EOF 报 `truncated input`。

每次读取的输出内存随请求长度增长，预读块最多 8192 字节；快照产生额外复制。
不要复制旧状态用于重放，也不要在该状态之外读取同一个 socket。
网络错误、截断或取消可能已经消费／发送部分字节，调用者应关闭连接，
不能将其当作未发生 I/O 而重试。参数校验失败则不消费输入。

缓冲状态显式传递是为了遵守普通 class 借用不能跨挂起点的规则，
原生 `TcpStream` 借用仍由其 owner 保活。参见[所有权与借用](../lang/types.md#class)。
