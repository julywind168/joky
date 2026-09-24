# 标准库

本目录面向使用 Joky 的读者，记录 API、错误、取消和资源所有权契约。provider 的线程、注册与清理实现见 [Runtime](../runtime/README.md)。

| 模块 / 主题 | 文档 | 完整示例 |
| --- | --- | --- |
| `joky/env` | [环境变量、参数与目录](env.md) | [env.jk](../../examples/io/env.jk) |
| `joky/path` | [纯路径操作](path.md) | [path.jk](../../examples/io/path.jk) |
| `joky/file` | [文件与分块 I/O](file-io.md) | [file_copy.jk](../../examples/io/file_copy.jk) |
| `joky/process` | [命令、输出与管道](process.md) | [process.jk](../../examples/io/process.jk)、[pipeline.jk](../../examples/io/pipeline.jk) |
| `joky/sqlite` | [语句、绑定与行游标](sqlite.md) | [sqlite.jk](../../examples/io/sqlite.jk) |
| TCP 端点 | [连接分割与半关闭](tcp.md) | [push_server.jk](../../examples/networking/push_server.jk) |

内建值与集合见[字符串和常用值](../lang/values.md)、[集合](../lang/collections.md)及[游标迭代](../lang/iteration.md)。模块声明位于 [`std/`](../../std/)；这里尚未逐个模块建立完整 API 页面。
