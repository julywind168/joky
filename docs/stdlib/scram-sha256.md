# SCRAM-SHA-256 核心

`import joky/crypto/scram_sha256`。消息解析、proof 计算和服务端签名验证均为
纯 Joky，复用 [Base64](base64.md)、[SHA-256](sha256.md) 和
[HMAC / PBKDF2](hmac-pbkdf2.md)。随机入口通过已有 [random effect](random.md)
取得 OS 熵，没有新增 native 密码学函数或 runtime ABI。

这是独立协议核心。PostgreSQL 应用使用 [pgsql.connect](pgsql.md)，
该入口已经组合本核心、[SASLprep 与 PostgreSQL 密码准备](saslprep.md)和认证状态机。

## API 与状态

| 入口 | 返回 | 行为 |
| --- | --- | --- |
| `start(prepared_user: String, limits: Limits)` | `Result(ClientFirst, String)` | 需要 random effect；读取恰好 24 字节熵并编码为 32 字符 nonce |
| `start_with_nonce(prepared_user: String, nonce: String, limits: Limits)` | `Result(ClientFirst, String)` | 纯函数入口；调用方负责 nonce 的随机性和每次握手唯一性 |
| `first.message(&self)` | `String` | 返回 client-first 消息 |
| `first.respond(self, server_first: String, prepared_password: Bytes)` | `Result(ClientFinal, String)` | 校验挑战、派生密钥并生成 client-final proof |
| `final.message(&self)` | `String` | 返回 client-final 消息 |
| `final.verify(self, server_final: String)` | `Result(Unit, String)` | 只有完整服务端签名匹配才成功 |

ClientFirst 与 ClientFinal 是单次使用的 class。respond / verify 消费前一状态，
即使返回 Err 也不能再次调用；message 只借用，可以在交接前读取多次。
请使用 start 入口创建状态，构造参数和内部字段不属于稳定接口，也不应记录到日志。

目前固定使用无 channel binding 的 GS2 header `n,,`，不支持 authorization identity、
SCRAM-SHA-256-PLUS 或机制协商。必须收到并验证 server-final 才能认定该交换成功。

## 凭证与限制

prepared_user 与 prepared_password 表示调用方已经按上层协议准备好的凭证。
核心不进行 SASLprep、Unicode 规范化或 PostgreSQL 的密码回退处理；Unicode /
二进制向量只验证输入字节的计算。`pgsql.connect` 自动调用
`joky/pgsql/password.prepare`；自行组合协议时应先准备密码，再交给本核心。
独立的 [SASLprep 测试](saslprep.md#数据与复现)验证密码准备。

用户名自动将等号转为 =3D、逗号转为 =2C，并拒绝 NUL。允许空用户名，
PostgreSQL 连接使用此方式，由启动消息提供用户名。密码按原始 Bytes 参与计算，允许
空输入、NUL 与非 UTF-8。输入 nonce 必须是非空、无逗号的可打印 ASCII；
start_with_nonce 不检查熵，固定测试 nonce 不可复用于真实认证。

Limits 的默认值与含义：

| 字段 | 类型 / 默认值 | 含义 |
| --- | --- | --- |
| min_iterations | UInt32 / 4096 | 拒绝低于本地策略的服务端迭代次数 |
| max_iterations | UInt32 / 100000 | 派生前拒绝超出计算预算的挑战 |
| max_message | UInt64 / 16384 | 每条 SCRAM 消息的 UTF-8 字节数上限，也限制密码字节数和未转义用户名 / nonce 输入 |

最小迭代数必须大于零，最大值不得小于最小值，消息限制必须大于零。
迭代数字符串必须是无符号、无前导零的十进制正整数，溢出及超限均在 PBKDF2 前失败。
上述最大值是本库可调资源策略，不是协议强制值或密码存储参数建议。
测试向量可显式将最小值降低到 1；部署时应自行选择可接受的计算预算。

长度校验覆盖输入消息与客户端实际发送的消息，用户名转义和 Base64 proof 的增长
计入上限。盐受到 server-first 的总长度约束；密码受同一限制。
限制以字节计量，不代表进程内存精确上限；返回消息之前会保留若干中间值。
PBKDF2 同步计算，不支持中途取消或 deadline；输入字符串的分配发生在调用之前，
网络层仍需在接收前限制帧长。

## 消息校验

- server-first 要求 r、s、i 顺序及唯一性，拒绝 NUL、空段、尾部逗号、重复属性、
  不支持的 mandatory extension m、错误位置的已知属性。
- 服务端 nonce 必须以客户端 nonce 为前缀，且附加非空的服务端部分。
- 盐和签名使用严格规范 Base64：拒绝空白、非法字母、错误 padding 及非零 pad bits。
  语法允许空盐；签名必须恰好 32 字节。
- 未知可选扩展允许存在，值按语法校验；整个 server-first 原文（包含扩展）参与
  AuthMessage，不能重新排序或丢弃扩展后再计算 proof。
- server-final 必须以 verifier v 或错误 e 开头，不能同时包含两者。所有服务端
  错误统一返回本地错误文本，不直接回显不可信内容。

签名比较扫描全部 32 字节，不按首个差异提前退出。但 Joky 编译后端尚无恒定时间
执行契约，因此本模块不声称具有经验证的恒定时间实现；也不保证密码和派生密钥
内存清零。SCRAM 核心不提供连接加密、TLS 证书验证或 channel binding。

## 验证

[标准向量与非法消息测试](../../tests/fixtures/scram_sha256.jk)包含 RFC 7677 的完整
交换、nonce / Base64 / 迭代数 / 长度边界和畸形属性。
[独立向量](../../tests/fixtures/scram_sha256_vectors.jk)的固定期望值来自 Python
hashlib / hmac，涵盖转义、二进制密码、UTF-8、空输入及带扩展的原始 transcript；
还逐个篡改 32 个签名字节，检查错误密码和消息扩展绑定。
[随机入口测试](../../tests/fixtures/scram_random.jk)覆盖受控 entropy handler、
错误传播、短 / 长返回值和实际 OS nonce 格式。运行回归不依赖 Python。

[CLI 测试](../../tests/cli/scram.rs)额外确认状态不可复用和 random effect 必须声明，
运行时测试检查资源排空，并通过以下命令覆盖冷 / 缓存 JIT、debug / release AOT：

```sh
cargo build --release --manifest-path crates/joky-runtime/Cargo.toml --target-dir target
JOKY_TEST_AOT=full cargo test --test cli crypto_scram
```

规范依据：[RFC 5802](https://www.rfc-editor.org/rfc/rfc5802.html) 第 3、5、7 节、
[RFC 7677](https://www.rfc-editor.org/rfc/rfc7677.html) 第 3 节；
后续接入约定见 [PostgreSQL 18 SASL 认证](https://www.postgresql.org/docs/18/sasl-authentication.html)。
