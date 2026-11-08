# Joky for VS Code

为 `.jk` 文件提供语法高亮、运行与检查命令、文档格式化，以及注释快捷键、括号配对和基础缩进。
高亮无需编译器；运行、检查和格式化使用你安装的 Joky。

## 安装与第一次使用

1. 从 [Releases](https://github.com/julywind168/joky/releases) 下载 `joky-<version>.vsix`。
   尚无附件时，可在 [VS Code extension 工作流](https://github.com/julywind168/joky/actions/workflows/editor.yml)
   的成功运行中下载 `joky-vscode` artifact，解压后取出 VSIX（下载 Actions artifact 需要登录 GitHub）。
   维护者首次推送并运行工作流前，这两个入口都可能没有产物。
2. 在 VS Code 扩展视图的 `…` 菜单中选择 **Install from VSIX…**，打开下载的文件。
   用户安装无需 Node.js、npm 或本地打包；也可使用 `code --install-extension <下载的文件.vsix>`。
3. 打开 `.jk` 文件，右下角语言模式应为 **Joky**；之前手动关联过其他语言时，用 **Change Language Mode** 改回 Joky。
4. 按[工具链安装说明](../../README.md#安装与产物)安装 Joky，并将其加入 PATH。
   打开 `hello.jk` 后，点击编辑器右上角的运行按钮，或在命令面板执行 **Joky: Run File or Project**。

随包提供的 `joky-vscode-SHA256SUMS` 可用 `shasum -a 256 -c joky-vscode-SHA256SUMS` 校验。
当前标识为 `joky-local.joky`，尚未发布到 Marketplace；正式发布 Marketplace 前需配置 publisher。
扩展采用 MIT / Apache 2.0 双许可，许可证随 VSIX 分发。

## 编译器与命令

扩展默认从 VS Code 扩展宿主的 PATH 自动寻找 `joky`。安装后仍找不到时，重启 VS Code，
或执行 **Joky: Select Compiler...** 选择编译器文件；也可以在设置中填写绝对路径：

```json
{
  "joky.executablePath": "/Users/you/.local/bin/joky"
}
```

路径不加引号，不附带参数，也不展开 `~` 或 `${workspaceFolder}`。Remote SSH / 容器窗口使用远端工具链路径。
找不到编译器时会提供安装与选择文件入口，高亮仍然可用；选择完成后重新执行命令。

| 操作 | 入口与行为 |
| --- | --- |
| 运行 | **Joky: Run File or Project**，或编辑器右上角运行按钮；在集成终端运行，支持交互输入 |
| 检查 | **Joky: Check File or Project**；调用 `joky check`，结果显示在任务终端 |
| 格式化 | **Format Document** 或 **Joky: Format Document**；调用 `joky fmt --stdin`，支持未保存内容与撤销 |
| 选择编译器 | **Joky: Select Compiler...**；更新当前生效的编译器路径设置 |

运行与检查从所选文件向上寻找最近的 `joky.toml`，不越过所属工作区文件夹；找到后操作整个项目，
否则操作当前 `.jk` 文件。没有活动 Joky 文件时使用工作区文件夹，多根工作区会提示选择。
执行前保存目标目录内已打开且修改过的 `.jk` 与 `joky.toml`；取消保存则不执行。
未命名文件运行前需要另存，格式化可以直接处理未命名的 Joky 文档。
项目外的 path 依赖请先自行保存。工作区内可用 **Tasks: Terminate Task** 终止运行。
只打开单个文件时使用独立的 Joky 终端，支持行输入、Ctrl+C 终止与 Ctrl+D 结束输入；
进程退出后保留输出，关闭终端也会终止仍在运行的进程。

保存时自动格式化可在用户设置中开启：

```json
{
  "[joky]": {
    "editor.defaultFormatter": "joky-local.joky",
    "editor.formatOnSave": true
  }
}
```

格式化成功后由编辑器应用修改，不直接写磁盘；保存、格式化失败或格式化期间继续输入时不会用旧结果覆盖文档。
详细错误可在 **Output → Joky** 查看。Restricted Mode 与虚拟工作区保留高亮，执行编译器需可信的文件系统工作区。
扩展不自动运行程序、不自动下载编译器，也尚未提供保存时检查、Problems 面板诊断、补全或定义跳转。

## 支持范围

- 声明、控制流、内置类型、函数名、效果与声明属性。
- 十进制、十六进制、二进制、浮点、Duration（如 `1s`、`250ms`、`1.5s`）、Rune、转义与 Unicode 标识符。
- 行注释与嵌套块注释。
- 普通字符串及嵌套插值，插值内的块、注释、字符串与跨行表达式。
- `sql"..."` 与 SQL 三引号模板，分别识别 `${...}` 参数孔和 `{...}` 标识符孔，孔内使用 Joky 高亮。
- `r` 原始字符串、任意数量的 `#` 定界符，以及普通和原始三引号多行字符串。
- 注释切换、括号与引号补全、选中包围、基础括号缩进。

高亮以仓库的 `src/core/parser/lexer.rs` 和 `docs/language.md` 为依据。
高亮采用 TextMate 词法规则，不提供语义类型识别；检查和格式化由编译器负责。
SQL 正文按模板字符串着色，不嵌入 SQL 语法检查；孔是否处于 SQL 引号／注释内以及类型是否合法由编译器判断。
大写名称按类型名称着色；上下文关键字按局部文本判断，复杂跨行的 `when` 访问列表
可能按普通调用着色。缩进是基础规则，不解析整个程序。
未结束的单行字符串和 Rune 在行尾恢复高亮；未闭合的多行字符串、块注释和插值
会继续影响后文，直到补齐结束符。

## 开发与验证

使用 `code --extensionDevelopmentPath="$PWD"` 启动 Extension Development Host，
然后在新窗口打开仓库中的 `.jk` 示例。可用 **Developer: Inspect Editor Tokens and Scopes**
查看 token scope，并在浅色与深色主题中检查显示效果。

```sh
npm ci
npm test
npm run package
```

开发和打包需要 Node.js 22+ 与 npm。生成 `joky-0.2.0.vsix` 后可本地安装验证；
CI 在扩展相关 push / PR 与手动触发时执行测试并保存 VSIX，GitHub Release 发布后自动附加该版本源码生成的 VSIX 与校验文件。
工作流不创建 Release，也不发布 Marketplace。

本机有 `code` 命令及 Debug Joky 时，运行真实 VS Code 扩展宿主测试：

```sh
npm run test:host
JOKY_TEST_NO_FOLDER=1 npm run test:host
```

测试使用隔离的 VS Code 配置与临时工作区，不修改日常编辑器设置。默认使用仓库的 `target/debug/joky`，
可用 `JOKY_TEST_COMPILER` 指定绝对路径，`VSCODE_CLI` 指定 `code` 命令路径。
宿主测试实际调用编译器，覆盖未保存／未命名文档格式化、带空格和 shell 字符的文件名、单文件及项目任务。
宿主测试单独运行，CI 打包不会自动构建编译器或执行这些测试。

测试使用 VS Code 同款 `vscode-textmate` 与 `vscode-oniguruma`，覆盖嵌套注释、
插值、原始定界符、多行状态、Unicode、数字和编辑器配置，并扫描现有 examples 与 stdlib
检查高亮状态恢复。关键字测试与 lexer 对照，新增保留字或内置类型时会提醒同步 grammar。
另有工具链测试覆盖 PATH 查找、项目边界、取消、超时与 Unicode 格式化编辑范围。
测试需在完整仓库中运行；仅打包后的 VSIX 不包含测试、开发依赖或编译器。

这些检查只验证编辑器扩展；编译器检查仍使用仓库的 `scripts/check.sh`。
