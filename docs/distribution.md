# 发行包安装与打包

## 用户：安装完整发行包

在 [Releases](https://github.com/julywind168/joky/releases) 选择与操作系统和 CPU 架构匹配的
`joky-<version>-<target>.tar.gz` 与对应 `.sha256` 文件。没有附件时，按
[README](../README.md#安装与产物) 从源码安装。首次发布前不假设二进制包已经可用。

将这两个文件放在同一个目录，先运行 `shasum -a 256 -c <下载文件名>.sha256`，
确认结果为 `OK` 后解压。macOS arm64 的 target 为 `aarch64-apple-darwin`，
Linux x86_64 为 `x86_64-unknown-linux-gnu`；Linux 包不是静态 musl 包，
发行说明应注明构建系统与最低运行环境。

将解压后的整个目录放到你选择的固定位置，并把其中的 `bin` 加入 PATH。
例如目录放在 `~/.local/opt/joky` 后：

```sh
export PATH="$HOME/.local/opt/joky/bin:$PATH"
joky --version
```

不要只移动 `bin/joky`：AOT 链接需要旁边安装布局中的 `lib/joky/libjoky.a`。
包内还有 `include/joky/`、`share/licenses/joky/` 和
`share/doc/joky/`（示例、离线 HTML）。在终端保存一个 `hello.jk`：

```joky
fn main() {
    println(42)
}
```

执行安装自检：

```sh
joky run hello.jk
joky run --interpreter hello.jk
joky build hello.jk -o hello
./hello
```

三次运行均应输出 `42`。`build` 需要系统 `cc`；macOS 可安装 Xcode Command Line Tools，
Linux 可安装发行版的 C 编译器和链接工具。缺少 runtime 时请恢复完整目录，或用
`JOKY_RUNTIME` 指向同版本 `libjoky.a`。`joky: command not found` 时检查 PATH 并重开终端。

升级时解压到新目录，完成自检后更新 PATH；保留旧目录即可回退。
卸载时删除专用安装目录并移除对应 PATH 设置，不要删除整个 `~/.local`。

## 维护者：生成与验证发行包

VS Code 扩展单独分发为 `joky-<version>.vsix`，安装步骤见[扩展说明](../editors/vscode/README.md)。
**VS Code extension** 工作流在相关 push / PR 和手动运行时打包并保存 `joky-vscode` artifact；
已发布 GitHub Release 会触发同一工作流，把对应源码版本的 VSIX 与 `joky-vscode-SHA256SUMS` 附加到该 Release。
首次运行前下载入口可能没有产物。该工作流不创建 Release，也不发布 Marketplace。

从仓库任意位置调用 `scripts/package.sh`（仓库外使用脚本的完整路径），
它使用当前 host target、锁定的 Cargo 依赖和 Release 构建，输出到 `target/dist/`。
可传入输出目录；已有同名包时拒绝覆盖。

```sh
./scripts/package.sh
```

开发中验证安装与打包逻辑，使用 Debug，包名带 `-debug`：

```sh
./scripts/package.sh --debug /tmp/joky-packages
```

包内 CLI 和 runtime 来自同一次构建，包含 SHA-256 校验文件。脚本不上传、不创建 Release。
GitHub 的 **Build distributions** 手动工作流在 macOS arm64 和 Linux x86_64
生成 Release 包，校验解包后的三种运行路径，然后保存为 Actions artifacts。
Runner 标签与架构按 [GitHub 官方列表](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)选择。
这些 artifacts 需下载并经过发布验收后，才能作为 GitHub Release 附件发布。
工作流尚未执行时，不把目标平台列为本次发行已验证。

打包与 smoke 检查不替代发布验收。按 [AGENTS.md](../AGENTS.md#按改动范围验证)
执行明确要求的 Release 验收，并在发布说明记录版本、commit、工具链、平台和验证范围。
第三方依赖保留各自许可证；发布前还需核对随二进制分发的依赖许可与 notices。
