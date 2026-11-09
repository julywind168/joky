# 示例

先从仓库根目录运行最小程序：

```sh
cargo run -- run examples/hello.jk
```

输出 `Hello, world!`。安装工具链后可直接使用 `joky run`；
追加 `--interpreter` 使用解释器，`joky build <文件> -o <程序>` 生成原生程序。
完整用法见[命令行指南](../docs/cli.md)。

| 想了解什么 | 示例 |
| --- | --- |
| 变量、分支和循环 | [conditionals.jk](conditionals.jk)、[loops.jk](loops.jk)、[for.jk](for.jk) |
| 数组及元素管理 | [arrays.jk](arrays.jk)、[array_management.jk](array_management.jk)（顺序删除、交换删除与容量复用） |
| Map、List、Set | [maps.jk](maps.jk)、[lists.jk](lists.jk)、[sets.jk](sets.jk) |
| 借用与迭代流水线 | [range_slices.jk](range_slices.jk)、[iterator_adapters.jk](iterator_adapters.jk)、[borrowed_pipelines.jk](borrowed_pipelines.jk) |
| 字符串与 JSON | [string_queries.jk](string_queries.jk)、[string_collections.jk](string_collections.jk)、[json_derive.jk](json_derive.jk) |
| 文件模块与本地包依赖 | [packages/app](packages/app/joky.toml)（同包 `math` 模块与依赖包 `common/greeting`） |
| 有限的 TCP 消息交互 | [tcp_messages.jk](tcp_messages.jk)、[tcp_duplex.jk](tcp_duplex.jk) |
| HTTP 与有状态服务 | [http_server.jk](http_server.jk)、[Chat](chat/README.md) |

项目示例使用项目目录运行，让编译器读取依赖清单：

```sh
joky run examples/packages/app
joky test examples/packages/common
```

默认 prelude 已提供打印函数、`Array` / `Map` / `Set`、`Option` / `Result` 及其构造器、断言和基础方法。
`--no-prelude` 关闭这些隐式导入，需要自行补齐；模块函数、额外类型和 trait 仍按需显式导入。
迭代协议、arena、FFI 与宿主嵌入示例面向实现和集成场景，保留相应的底层接口用法。
