# Joky for VS Code

为 `.jk` 文件提供语法高亮、注释快捷键、括号配对和基础缩进。
这是纯声明式扩展，不需要安装 Joky 编译器，也不会运行项目代码。

## 本地安装

在本目录执行（开发和打包需要 Node.js 22+ 与 npm）：

```sh
npm ci
npm run package
code --install-extension joky-0.1.2.vsix
```

也可以在 VS Code 扩展视图的 `…` 菜单中选择 **Install from VSIX…**，
打开生成的 `joky-0.1.2.vsix`。安装后打开 `.jk` 文件，右下角语言模式应为 **Joky**。
如文件之前被手动关联为其他语言，通过 **Change Language Mode** 改回 Joky。

当前扩展使用本地标识 `joky-local.joky`，尚未发布到 Marketplace。
打包脚本允许缺少远程仓库与许可证文件；发布前应确定实际 publisher、仓库地址和许可。

## 支持范围

- 声明、控制流、内置类型、函数名、效果与声明属性。
- 十进制、十六进制、二进制、浮点、Duration（如 `1s`、`250ms`、`1.5s`）、Rune、转义与 Unicode 标识符。
- 行注释与嵌套块注释。
- 普通字符串及嵌套插值，插值内的块、注释、字符串与跨行表达式。
- `sql"..."` 与 SQL 三引号模板，分别识别 `${...}` 参数孔和 `{...}` 标识符孔，孔内使用 Joky 高亮。
- `r` 原始字符串、任意数量的 `#` 定界符，以及普通和原始三引号多行字符串。
- 注释切换、括号与引号补全、选中包围、基础括号缩进。

高亮以仓库的 `src/core/parser/lexer.rs` 和 `docs/language.md` 为依据。
采用 TextMate 词法规则，不提供补全、跳转、格式化、语义类型识别或编译诊断。
SQL 正文按模板字符串着色，不嵌入 SQL 语法检查；孔是否处于 SQL 引号／注释内以及类型是否合法由编译器判断。
大写名称按类型名称着色；上下文关键字按局部文本判断，复杂跨行的 `when` 访问列表
可能按普通调用着色。缩进是基础规则，不解析整个程序。
未结束的单行字符串和 Rune 在行尾恢复高亮；未闭合的多行字符串、块注释和插值
会继续影响后文，直到补齐结束符。

## 开发与验证

用 VS Code 打开本目录，按 F5 启动 Extension Development Host，
然后在新窗口打开仓库中的 `.jk` 示例。可用 **Developer: Inspect Editor Tokens and Scopes**
查看 token scope，并在浅色与深色主题中检查显示效果。

```sh
npm test
npm run package
```

测试使用 VS Code 同款 `vscode-textmate` 与 `vscode-oniguruma`，覆盖嵌套注释、
插值、原始定界符、多行状态、Unicode、数字和编辑器配置，并扫描现有 examples 与 stdlib
检查高亮状态恢复。关键字测试与 lexer 对照，新增保留字或内置类型时会提醒同步 grammar。
测试需在完整仓库中运行；仅打包后的 VSIX 不包含测试、开发依赖或编译器。

这些检查只验证编辑器扩展；编译器检查仍使用仓库的 `scripts/check.sh`。
