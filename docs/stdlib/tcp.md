# TCP 连接分割

```joky
let (reader, writer) = connection.split()!
branch { receive(reader) }
branch { send(writer) }
```

`TcpStream.split(self)` 消耗连接，返回 `Result((TcpReadHalf, TcpWriteHalf), String)`。
读端提供 `read(&self, max_bytes)`、`close(self)`；写端提供
`write(&self, bytes)`、`close(self)`。两个端点都是唯一资源，可以分别移动到任务。
读取等待数据时，另一个任务可以立即写出主动推送消息。

关闭或丢弃读端关闭接收方向；关闭或丢弃写端发送 EOF，读端仍可接收对端的回复。
底层 socket 由两个独立描述符管理，使用各自的 reactor 注册。复制描述符失败时
`split` 返回 `Err`。分割本身没有阻塞 I/O，沿用 socket provider 的 Pending ABI。

标准库的 intrinsic 方法可用 `as operation_name` 显式映射到所属 effect 的操作，
例如读端的 `read` 映射到 `tcp.read_half`，从而保留静态的端点类型限制。
