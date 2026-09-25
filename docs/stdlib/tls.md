# 验证型客户端 TLS

`import joky/socket/tls` 提供可复用的 `TlsStream`，由 runtime 内的 rustls 驱动。
PostgreSQL 的协议和认证仍由 Joky 实现；TLS 的证书处理、握手、记录加密复用 rustls，
不依赖 libpq、系统 OpenSSL 或同步网络线程。支持 TLS 1.2 和 TLS 1.3。

## API

```joky
import joky/socket/tcp
import joky/socket/tls

fn open(host: String, port: UInt16, ca: Bytes) -> Result(TlsStream, String) effects { tcp, tls } {
    let stream = tcp.connect(host, port)?
    tls.upgrade(stream, host, ca)
}
```

- `tls.upgrade(stream: TcpStream, server_name: String, ca_pem: Bytes)` 消费 TCP，
  返回 `Result(TlsStream, String)`；失败或取消也关闭底层连接。不能继续使用原 TCP。
- `read(&self, max_bytes: UInt64) -> Result(Bytes, String)` 返回最多指定长度；
  每次实际输出不超过 64 KiB，调用方循环读取。零长度立即返回空 Bytes。
- `write(&self, value: Bytes) -> Result(UInt64, String)` 在对应的密文全部写入
  socket 后返回输入字节数；不表示远端已经处理数据。空输入返回 0。
- `close(self) -> Result(Unit, String)` 发送并刷新 close_notify，然后关闭连接；
  不等待对端 close_notify。隐式析构与取消直接关闭 socket，不执行挂起 I/O。

read、write、close 需要 `tls` effect。升级后不再调用 tcp.read/write。
同一 TlsStream 只允许一个在途操作，包括读写互斥；尚无 split 或并发双向 I/O。

## 信任与边界

`server_name` 是 DNS 名称或 IP 地址，不能含端口；验证证书链、有效期和该身份。
DNS 名称用于 SNI；IP 按证书 IP SAN 验证。
`ca_pem` 为空时使用构建时捆绑的 Mozilla 公共根，不读取系统钥匙串；
非空时完全替换为调用方给出的 PEM 根证书集合，不合并公共根。
没有跳过验证的选项、0-RTT 或会话恢复缓存。未实现客户端证书、ALPN、
代理协商、吊销检查配置。TLS 提供已验证叶证书的 `tls-server-end-point` 摘要，供 PostgreSQL SCRAM channel binding 使用。

单次 read 请求和 write 输入上限均为 16 MiB，CA bundle 上限为 1 MiB。
写入按 16 KiB 推进，rustls 应用输出缓冲上限 64 KiB。
握手和 I/O 都接入现有非阻塞 reactor，按实际需要切换可读/可写兴趣。
没有内置超时；可用结构化 `race` 取消。DNS 仍沿用 TCP 的同步解析。

传输/协议错误或取消使连接不可继续使用，后续操作报错；参数超限在 I/O 前拒绝。
对端 close_notify 后读完缓冲内容返回空 Bytes；没有 close_notify 的 EOF 报错，
避免把截断当作正常消息结束。错误文本供诊断，不是稳定的可机器解析错误码。

## PostgreSQL

[驱动](pgsql.md)通过 SSLRequest 获取恰好一个响应字节，只有 `S` 才升级 TLS，
随后才发送 Startup 和 SCRAM；拒绝或验证失败不会回退明文。
这是 PostgreSQL 协议升级，不是直接对数据库端口发送通用 TLS ClientHello。

## 测试

```bash
cargo build --release --manifest-path crates/joky-runtime/Cargo.toml --target-dir target
JOKY_TEST_AOT=full cargo test --test cli tls
python3 scripts/test-pgsql.py --aot full
```

测试生成隔离 CA，覆盖 TLS 1.2/1.3、DNS/IP、自定义根、错误身份/根/过期证书、
分片大数据、空 I/O、close_notify、截断、取消、SSLRequest 和重复查询。
真实 PostgreSQL 测试使用临时启用 SSL 的 cluster，不修改现有数据库。
