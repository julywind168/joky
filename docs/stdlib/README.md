# 标准库

本目录面向使用 Joky 的读者，记录 API、错误、取消和资源所有权契约。provider 的线程、注册与清理实现见 [Runtime](../runtime/README.md)。

| 模块 / 主题 | 文档 | 完整示例 |
| --- | --- | --- |
| `joky/env` | [环境变量、参数与目录](env.md) | [env.jk](../../examples/io/env.jk) |
| `joky/path` | [纯路径操作](path.md) | [path.jk](../../examples/io/path.jk) |
| `joky/file` | [文件与分块 I/O](file-io.md) | [file_copy.jk](../../examples/io/file_copy.jk) |
| `joky/process` | [命令、输出与管道](process.md) | [process.jk](../../examples/io/process.jk)、[pipeline.jk](../../examples/io/pipeline.jk) |
| `joky/sqlite` | [语句、绑定与行游标](sqlite.md) | [sqlite.jk](../../examples/io/sqlite.jk) |
| `joky/binary`、`joky/socket/buffered` | [二进制编解码与缓冲读取](binary.md) | [二进制测试](../../tests/fixtures/binary.jk) |
| `joky/encoding/base64` | [严格 Base64 编解码](base64.md) | [crypto.jk](../../examples/basics/crypto.jk) |
| `joky/crypto/sha256` | [SHA-256 与增量摘要](sha256.md) | [crypto.jk](../../examples/basics/crypto.jk) |
| `joky/crypto/hmac_sha256`、`joky/crypto/pbkdf2` | [HMAC 与 PBKDF2](hmac-pbkdf2.md) | [crypto.jk](../../examples/basics/crypto.jk) |
| `joky/crypto/random` | [系统安全随机数](random.md) | [random.jk](../../examples/basics/random.jk) |
| `joky/crypto/scram_sha256` | [SCRAM-SHA-256 核心](scram-sha256.md) | [协议向量](../../tests/fixtures/scram_sha256.jk) |
| `joky/crypto/saslprep`、`joky/unicode/nfkc32`、`joky/pgsql/password` | [SASLprep 与密码准备](saslprep.md) | [边界测试](../../tests/fixtures/saslprep.jk) |
| `joky/pgsql` | [PostgreSQL 协议原型](pgsql.md) | [pgsql.jk](../../examples/networking/pgsql.jk) |
| TCP 端点 | [连接分割与半关闭](tcp.md) | [push_server.jk](../../examples/networking/push_server.jk) |

内建值与集合见[字符串和常用值](../lang/values.md)、[集合](../lang/collections.md)及[游标迭代](../lang/iteration.md)。模块声明位于 [`std/`](../../std/)；这里尚未逐个模块建立完整 API 页面。
