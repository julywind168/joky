# Joky 首页

`joky-home/public/` 保存直接编辑的静态站点源码，无构建步骤。HTML、CSS、JavaScript 和 SVG 均需提交。
`joky-home/.openai/hosting.json` 保存站点 ID 和静态目录，
不含凭据，也需提交。

本地预览：

```sh
python3 -m http.server 4173 --bind 127.0.0.1 --directory sites/joky-home/public
```

在浏览器中访问 `http://127.0.0.1:4173`。验证范围为 JavaScript 语法、页面布局、
示例切换、复制按钮和页内导航；只改静态首页无需运行编译器的 Cargo 检查。

## 标准库文档产物

`joky doc --std` 生成独立、可离线浏览的标准库文档。需要预览与首页同站的 `/std/` 时，
从仓库根目录执行：

```sh
cargo run -- doc --std -o sites/joky-home/public/std
```

`public/std/` 是生成产物，不手工编辑、不提交；发布包必须在构建后包含它，
嵌套模块（如 `/std/json/typed/`）的链接也可直接访问。
首页导航接入时将目标设为 `/std/`，并把文档生成加入发布步骤；目前首页导航尚未改动，
本地生成不会自动发布线上站点。生成命令和当前限制见[语言参考](../docs/language.md#标准库文档)。

## 发布工作区

Sites 发布工具使用独立 Git 历史。其本地 checkout 放在被忽略的
`sites/.local/joky-home/`，避免主仓库把首页记录成嵌套仓库或 gitlink。
该目录只用于发布，日常编辑以 `sites/joky-home/` 为准。

后续发布时，通过 Sites 技能打开同一个站点的发布 checkout，再将受版本控制的
首页文件同步到该 checkout（包括删除已移除的文件，但保留它的 `.git/`）。
在发布 checkout 中验证、打包和推送。若本地 checkout 不存在，通过站点 ID
恢复远端源码；不要为同一个首页重新创建站点。

以下内容不提交：本地发布 checkout、`*-deploy.tar.gz` 部署归档、
`*-preview.jpg` 预览截图、依赖目录、工具本地状态和 `.env` 凭据文件。
凭据只交给 Sites 工具使用，不写入源码或配置。
