# PostgreSQL 驱动原型

`import joky/pgsql`。协议、消息解析、查询和结果处理全部由 Joky 实现，
通过现有 TCP provider 访问网络，不使用 libpq 或 PostgreSQL 专用 native provider。

当前支持协议 **3.0**、trust / SCRAM-SHA-256 认证、Simple Query 和 UTF-8 文本结果。
`connect(config, password)` 要求 SCRAM；`connect_trust(config)` 保留显式的无密码入口。
尚未实现 TLS、channel binding、参数绑定、COPY、二进制结果、流式游标、
CancelRequest 或连接池。不要向 `query` 拼接不可信的 SQL 参数。

认证复用纯 Joky [SCRAM-SHA-256 核心](scram-sha256.md)和
[SASLprep 与 PostgreSQL 密码准备](saslprep.md)，不新增 runtime ABI。
传输仍是明文 TCP；SCRAM 不会加密后续查询或结果，也不替代 TLS 证书验证。

## 使用

```joky
import joky/pgsql
import joky/socket/tcp

fn main() -> Result(Unit, String) effects { tcp } {
    let config = pgsql.PgConfig(user: "postgres", database: "postgres")
    let connection = pgsql.connect_trust(config)?
    let response = connection.query("SELECT 1 AS answer")?
    let results = response.result?
    println(results.head()!.rows.head()!.text(0)?!)
    response.connection.close()
    Ok(())
}
```

完整[示例](../../examples/networking/pgsql.jk)还验证中文、NULL、空字符串，
以及 SQL 错误后继续查询。通过 `JOKY_PG_PORT`、`JOKY_PG_USER`、
`JOKY_PG_DATABASE` 配置示例，默认分别为 5432、postgres、postgres；主机固定为 loopback。

## 连接与所有权

`pgsql.connect(config, password: Bytes)` 和 `pgsql.connect_trust(config)`
都返回 `Result(PgConnection, String)`，只有收到了 `AuthenticationOk`
和 `ReadyForQuery` 才返回成功。trust 入口仍只需要 `tcp` effect；
密码入口需要 `tcp, random`，在打开网络前完成密码准备并生成 24 字节随机 nonce。
`PgConfig` 的主机默认 `127.0.0.1`、端口默认 5432，`user` 和 `database` 必填。
启动参数显式选择 UTF8；不支持的认证方法立即报错并释放连接。

密码连接示例（password 由调用方从凭证来源取得）：

```joky
import joky/pgsql
import joky/socket/tcp
import joky/crypto/random

fn open(password: Bytes) -> Result(pgsql.PgConnection, String) effects { tcp, random } {
    pgsql.connect(pgsql.PgConfig(user: "app", database: "app"), password)
}
```

`connect` 按 `AuthenticationSASL → SASLContinue → SASLFinal → AuthenticationOk`
推进，必须验证 server-final 签名，不接受跳步、重复认证或提前 ReadyForQuery。
机制列表中精确选择 `SCRAM-SHA-256`；只有 PLUS 或未知机制时报错。
不会降级到 trust、明文密码或 MD5。服务端只要求 trust 时应显式使用 `connect_trust`。
SCRAM 用户名为空，PostgreSQL 使用启动消息中的用户名。

密码先按 PostgreSQL 的 Unicode 3.2 SASLprep 规则准备；非 UTF-8 或禁止字符
按照 PostgreSQL 兼容语义保留原始字节，NUL 与资源超限直接报错，详见密码准备文档。
密码单独传入，不保存到返回连接或 PgConfig；不保证释放时内存清零。
`joky/pgsql/auth` 与连接的 startup/receive 等方法为内部实现接口，不作为应用 API。

`connection.query(self, sql)` 消费连接，返回 `Result(PgQuery, String)`：

| 返回路径 | 含义 |
| --- | --- |
| 外层 `Err(String)` | 本地校验、传输或协议错误；连接已被消费并释放 |
| `Ok(response)` 且 `response.result` 为 `Ok(List(PgResult))` | 查询完成，可继续使用 `response.connection` |
| `Ok(response)` 且 `response.result` 为 `Err(String)` | 服务器 SQL 错误，已排空到 ReadyForQuery，可继续使用 `response.connection` |

普通 class 的 `&self` 当前不能跨 I/O 挂起，驱动采用上述所有权传递方式。
同一连接不会同时执行两个查询。`close(self) -> Unit` 消费连接并通过资源析构
关闭 TCP；不发送 Terminate。未显式关闭的连接在 owner 离开作用域时释放。

错误字符串保留服务器 SQLSTATE 和主消息，例如 `pgsql [22012]: division by zero`。
Notice 和 Notification 消息会被解析并丢弃，尚无订阅接口。

`transaction_status` 是 ReadyForQuery 的字节值：73 (`I`) 表示 idle，84 (`T`)
表示事务内，69 (`E`) 表示失败事务。SQL 错误后的连接可用不代表事务恢复；
失败事务通常需要 `ROLLBACK`。`parameters` 保存 ParameterStatus，
`backend_pid` 和 `backend_key` 保存协议 3.0 的取消信息，但尚无取消 API。

## 结果与边界

一个 SQL 字符串可以返回多个 `PgResult`，顺序与命令一致。
`PgResult` 含 `columns: List(PgColumn)`、`rows: List(PgRow)` 和命令标签 `command`。
空查询返回空列表。若整个查询周期发生 SQL 错误，本次已收集的结果被丢弃。

列保留名称、table OID、attribute、type OID、type size 和 type modifier。
`PgRow.values` 是 `List(Option(String))`；`row.text(index)` 返回
`Result(Option(String), String)`。NULL 为 None，空文本为 Some("")，数字也先按文本返回。
结果严格检查 UTF-8、列数量、长度和消息尾部；收到不支持的 COPY 或二进制结果后关闭连接。

`PgConfig` 提供以下限制，均为 `UInt64`：

| 配置 | 默认 | 计量方式 |
| --- | --- | --- |
| `max_message` | 16777216 | 单个消息 body 字节数，不包含 tag 和四字节长度；也限制查询 body |
| `max_result` | 67108864 | 一次启动或查询周期接收的消息总字节数，包含消息头 |
| `max_rows` | 100000 | 一次查询周期累计行数，跨多个结果集 |

消息长度先校验再分配 body；结果总量在收到每条消息后检查，因此可额外接收一条
不超过 `max_message` 的消息。这些是协议字节／行数限额，不是进程内存的精确上限；
字符串、列表和列元数据还有对象开销。当前一次收集全部结果。

`PgConfig.scram_limits` 使用 `scram_sha256.Limits()`，默认接受 4096–100000 次
迭代，每条 SCRAM 消息和准备后密码上限 16384 字节。机制列表使用同一消息限制；
认证消息在读取 body 前校验长度（额外允许四字节认证类型），也受 `max_message`
和启动总量 `max_result` 约束。发送的 SASL body 同样受 `max_message` 限制。
`password_limits` 使用 `nfkc32.Limits()`，约束准备阶段输入、输出、scalar 数量和重排工作。
如需调整，请显式导入对应模块并构造 Limits；资源超限不会触发密码回退。

查询可以放入 `race`，输家取消会释放整个连接。关闭 TCP 不保证服务器上的语句立即停止，
也不保证已执行写操作被撤销。需要服务器主动取消的行为仍待 CancelRequest 实现。
主机名解析沿用现有 TCP 的同步 DNS；暂未提供 driver deadline 或地址回退。

## 验证

无需 PostgreSQL 的协议和二进制测试会随 CLI 套件运行：

```bash
cargo build --release --manifest-path crates/joky-runtime/Cargo.toml --target-dir target
JOKY_TEST_AOT=full cargo test --test cli pgsql
```

安装 PostgreSQL 并将 `initdb`、`pg_ctl`、`psql` 放在 PATH 后，可以运行真实数据库验证：

```bash
cargo build
python3 scripts/test-pgsql.py --aot full
```

脚本创建临时 cluster，仅监听 loopback 随机端口；trust 管理用户创建三个
仅允许 SCRAM 认证的测试角色，不连接现有数据库，结束时停止实例并清理目录。
覆盖正确/错误密码、Unicode 规范化、禁止字符原字节回退、认证后查询，
以及原有示例、多结果集、较大消息、事务失败与回滚恢复。分别执行 JIT、
debug AOT、release AOT；使用 `--aot off` 可只运行 JIT。

模拟服务端测试覆盖机制选择、独立参考 proof、错误签名、消息乱序/重复、
异常长度与迭代限制，以及在各认证阶段取消后释放连接；运行冷/热 JIT 和 AOT。

基础库契约见[二进制编解码与 TCP 缓冲](binary.md)。后续所需能力见
[PostgreSQL 后续工作](../plans/backlog.md#postgresql-驱动)。
