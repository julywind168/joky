# Joky

[![CLI smoke](https://github.com/julywind168/joky/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/julywind168/joky/actions/workflows/ci.yml)

Joky 是一门用 Rust 实现的静态类型语言，适合语言实现、脚本嵌入和运行时实验。
同一份代码可以通过 JIT 或字节码解释器运行，也可以编译成原生程序；
支持所有权与借用、可挂起任务，以及 Rust / C 宿主嵌入。
目前处于早期开发阶段，语法和 API 仍可能变化，支持范围与限制见下文。

```joky
import joky/task

fn main() @suspends {
    let name = "Joky"
    println("Hello, {name}!")
    task.sleep(10ms)
    println("Ready.")
}
```

[官网](https://julywind168.github.io/joky/) · [在线标准库](https://julywind168.github.io/joky/std/) ·
[入门教程](docs/getting-started.md) · [语言参考](docs/language.md) ·
[命令行指南](docs/cli.md) · [文档索引](docs/README.md) ·
[源码仓库](https://github.com/julywind168/joky) · [问题反馈](SUPPORT.md) ·
[更新记录](CHANGELOG.md) · [兼容与升级](docs/compatibility.md)

## 快速开始

已安装 Joky 时，将上面的代码保存为 `hello.jk`，执行 `joky run hello.jk`。
预期依次输出 `Hello, Joky!` 和 `Ready.`。

首次从源码试用，需要 Rust 1.99.0+ 和系统 C 工具链（用于 Rust 依赖的构建）：

```sh
git clone https://github.com/julywind168/joky.git
cd joky
cargo run -- run examples/hello.jk
```

预期输出 `42`。首次编译需要下载并构建依赖；后续运行复用缓存。
该命令使用 Debug 构建，无需先进行 Release 编译。
已有 CLI 的 `run` / `check` 不调用系统 `cc`，`build` 原生链接需要它。

## 安装与产物

当前推荐从源码安装完整工具链；在上面的仓库目录执行：

```sh
./scripts/install.sh
export PATH="$HOME/.local/bin:$PATH"
joky --version
joky run examples/hello.jk
joky build examples/hello.jk -o /tmp/joky-hello
/tmp/joky-hello
```

最后两次程序运行均输出 `42`。将 PATH 设置加入你的 shell 配置后，新终端也可直接使用 `joky`。
安装脚本默认构建 Release，首次耗时会比快速试用更长；本地开发可用
`./scripts/install.sh --debug /tmp/joky-dev` 验证相同安装布局。
默认前缀是 `~/.local`，可传入其他目录，如 `./scripts/install.sh /path/to/prefix`。

完整安装包含 `bin/joky`、`lib/joky/libjoky.a`、`include/joky/` 和许可证。
`cargo install --path .` 只安装 CLI，不是完整安装；AOT 程序通常还需要匹配的 runtime archive。
构建时按 `JOKY_RUNTIME`、CLI 旁的 `libjoky.a`、安装目录 `lib/joky/libjoky.a` 的顺序查找。
生成的程序运行时不再需要源码、CLI 或 archive。

| 平台 | 可核实的验证入口与范围 |
| --- | --- |
| macOS arm64 | `macos-15`：CLI smoke CI 的 Debug 安装与基本流程；具体提交是否通过以对应 job 为准 |
| Linux x86_64 | `ubuntu-24.04`：相同的 Debug CLI smoke 范围；具体提交是否通过以对应 job 为准 |
| macOS x86_64 / Linux arm64 / Windows | 尚未配置对应 CLI smoke job，也未作为首版已验证发行平台 |

[查看平台 CI](https://github.com/julywind168/joky/actions/workflows/ci.yml)的 job summary 与日志，
可核对提交、Rust 版本、实际架构、C 编译器，以及初始化／检查／格式化／JIT／解释器／AOT 的结果。
badge 显示 `main` 最近状态；工作流尚未运行、失败或跳过时，不代表该提交已通过。
这项检查不等同于全量测试、压力检查或 Release 发行验收，发行包另看对应 Release 的证据。

macOS 可用 Xcode Command Line Tools 提供 `cc`；Linux 需要发行版的 C 编译器和链接工具。
最低构建工具链为 Rust 1.99.0，与当前实际开发版本一致；CLI smoke 和发行打包 CI 均固定使用 1.99.0。

预编译包发布后可从 [Releases](https://github.com/julywind168/joky/releases) 获取；
没有适合平台的附件时使用上述源码路径，不要只拷贝 `joky` 二进制。
完整包的安装、校验与维护者打包步骤见[发行包说明](docs/distribution.md)。
VS Code 用户可安装 [Joky 扩展](editors/vscode/README.md)：下载 VSIX 即可获得高亮，
配好编译器后可直接运行、检查和格式化，无需自行打包扩展。

## 下一步

| 想做什么 | 从哪里开始 |
| --- | --- |
| 写第一个项目 | [入门教程](docs/getting-started.md)：变量、容器、借用、错误处理、测试与任务 |
| 创建项目 | `joky init hello` 后执行 `joky run hello`；[项目操作与帮助](docs/cli.md#创建项目与查看帮助) |
| 使用命令行 | [命令行指南](docs/cli.md)：输入输出、检查、格式化、测试和项目 |
| 查语法与标准库 | [语言参考](docs/language.md)；`joky doc --open` 生成并打开离线标准库文档 |
| 嵌入 Rust / C | [架构与 ABI](docs/architecture.md)、[Rust 示例](examples/embedding.rs)、[C VM 示例](examples/vm_runtime.c) |
| 尝试有状态服务 | [聊天室](examples/chat/README.md)、[HTTP 服务](examples/http_server.jk) |
| 参与开发 | [贡献规范](CONTRIBUTING.md)与[文档索引](docs/README.md) |
| 报告错误或准备升级 | [反馈与最小复现](SUPPORT.md)、[兼容政策](docs/compatibility.md)和[更新记录](CHANGELOG.md) |

常用命令：

```sh
joky check hello.jk
joky fmt hello.jk
joky run hello.jk
joky run --interpreter hello.jk
joky doc --open
```

`check` 成功时静默退出，错误写入 stderr。`run` 的终端进度提示可用 `-q` 关闭，程序 stdout 不受影响。
`build` 单文件时默认输出到源码旁；`-o` 的父目录需要已存在。项目构建生成可搬动的目录包。

## 当前能力

| 组成 | 已实现 |
| --- | --- |
| 语言基础 | 八种定宽整数、Float32 / Float64、Rune、Bool、Unit、表达式与控制流、名义记录和枚举、元组、模式匹配、用户文件模块 |
| 类型系统 | 泛型与常量参数、透明别名、固有方法、静态 trait、默认方法、关联类型与常量、拥有型 dyn（含挂起方法）与同步动态借用、同步函数值与闭包、拥有闭包的记录存储与返回、挂起函数值与 move FnOnce 闭包 |
| 所有权与存储 | String、Array、List、Set、Map、FixedArray、拥有型聚合与 Drop、受检共享／独占借用、任务 arena 与可跨挂起、按作用域回收的显式 Arena |
| 执行与任务 | 共享语义分析、Cranelift JIT / AOT、字节码解释器；`@suspends`、`join` / `race`、`scope` / `branch`、`isolate` 失败隔离、取消与资源清理、局部值和容器视图跨挂起借用；可选多线程分支执行 |
| 内置模块 | Option / Result、Iterator / IntoIterator、Array 稳定排序与消费迭代、Array/Slice/Map 借用遍历、List 只读顺序遍历、for 自动转换与可选编号、Map 键值解构、泛型 range 与 take/zip/map/filter 惰性组合器、数值、文本与格式化、容器、JSON、任务定时器、文件、TCP、DNS、状态域与有界准入 |
| 嵌入与热更新 | Rust `VmEngine`、C VM / CPS API、宿主注册与 FFI、持久实例、状态域、Script 调用与兼容发布、分步迁移 |
| Godot 原型 | Node 挂载 `.jk`、解释器状态与属性、受检 Node API、帧回调、所属任务和引擎等待；[构建和示例](integrations/godot/README.md) |

具体类型准入、API 与边界以[语言参考](docs/language.md)为准。任务执行可选固定 worker 池；支持本地 `path` 库包依赖，尚无 Git/registry 下载与锁文件。
解释器执行限额不覆盖编译阶段或原生回调，其他执行与嵌入限制见[已知边界](docs/architecture.md#已知边界)。
文件服务支持读写、重命名、删除文件和列出目录。能力由宿主提供，CLI 启用本机实现，嵌入缺省为 Unsupported；Godot 验证适配器不提供文件服务。

## 验证

开发者使用 `./scripts/check.sh` 运行格式、Clippy、Debug 测试和三后端短输入基准正确性检查。
按改动范围选择检查，完整规则见 [AGENTS.md](AGENTS.md#按改动范围验证)；
性能测量见[基准方法](benches/README.md)。普通安装和试用不需要执行整套测试。

## 许可

Joky 采用 [MIT](LICENSE-MIT) 或 [Apache-2.0](LICENSE-APACHE) 双许可，可任选其一。
第三方依赖保留各自的许可证。
