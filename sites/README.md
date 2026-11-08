# Joky 首页

`joky-home/public/` 保存直接编辑的静态站点源码，无构建步骤。HTML、CSS、JavaScript 和 SVG 均需提交。
`joky-home/.openai/hosting.json` 保存站点 ID 和静态目录，
不含凭据，也需提交。

本地预览完整站点（从仓库根目录执行）：

```sh
cargo run -- doc -o target/site
python3 -m http.server 4173 --bind 127.0.0.1 --directory target/site
```

在浏览器中访问 `http://127.0.0.1:4173`，或使用 `cargo run -- doc --open`
直接打开默认生成的离线首页。首页导航链接到 `./std/index.html`，
标准库的模块页可返回首页；所有资源与导航均使用相对链接。

## 文档产物

`joky doc` 将这里的首页、CSS、JavaScript、SVG 与生成的标准库文档组合成完整站点。
源码在编译时内嵌；修改首页后需重新构建 Joky 再生成。生成目录不要手工编辑或提交。
`joky doc --std` 保留只生成独立标准库文档的能力。

发布包应使用 `joky doc -o <空目录或已有站点生成目录>` 的完整产物，
包含首页资源及 `std/`（含 `std/json/typed/` 等嵌套模块）。
不要直接对非空的首页源码目录执行生成，以免覆盖手工维护的源码。
本地生成不会自动发布线上站点。命令说明见[语言参考](../docs/language.md#标准库文档)。

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
