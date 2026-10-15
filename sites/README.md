# Joky 首页

`joky-home/dist/` 是无构建步骤的静态站点源码，HTML、CSS、JavaScript 和 SVG 均需
提交，不能按常见构建输出目录忽略。`.openai/hosting.json` 保存站点 ID 和静态目录，
不含凭据，也需提交。

本地预览：

```sh
python3 -m http.server 4173 --bind 127.0.0.1 --directory sites/joky-home/dist
```

在浏览器中访问 `http://127.0.0.1:4173`。验证范围为 JavaScript 语法、页面布局、
示例切换、复制按钮和页内导航；只改静态首页无需运行编译器的 Cargo 检查。

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
