# Joky Godot 脚本原型（G0）

普通 `Node` 直接挂载 `.jk` 资源，运行时使用 Joky 字节码解释器。扩展库本身由
Rust 编译成 GDExtension；Joky 脚本不会转成 AOT 产物。

## 构建与打开

当前固定 Godot **4.7.x** 扩展 API、`godot`/`godot-core` **0.5.5**。
实际验收目标为官方标准版 **4.7.2.stable.official.ed1daf0bf**、macOS arm64；
不需要 .NET。其他平台的库路径已预留，但尚未验收。

```sh
./integrations/godot/build.sh
/Applications/Godot.app/Contents/MacOS/Godot --editor --path integrations/godot/demo
```

也可以在 Godot 项目管理器中导入 [demo/project.godot](demo/project.godot)。
打开 `main.tscn`，`CounterA` 与 `CounterB` 都是普通 `Node`，共同引用
[counter.jk](demo/counter.jk)。运行时每个节点各自调用 `create()`、`_ready()`。
场景中的说明文字不依赖 GDScript；`demo/tests/*.gd` 和 `demo/addons/joky_acceptance` 仅为引擎验收驱动；
验收插件只在 `JOKY_G0_EDITOR_TEST=1` 时执行检查并退出编辑器。

Cargo 子工程独立于主仓库 workspace，普通 Joky 构建不下载 Godot 依赖。
`build.sh` 将当前平台的 Debug 动态库复制到 `demo/bin/`。
修改 Rust 后退出 Godot、重建再打开；扩展动态卸载/重载已关闭。
`godot-core` 的直接依赖用于 0.5.5 尚未由 `godot::meta` 转出的 `RawPtr` 类型，
必须与 `godot` 保持同版本。

## G0 脚本约定

- 根类型名固定为 `State`，必须符合 P1 的无拥有字段、无引用记录约束。
- `fn create() -> State` 无参数；构造失败不发布实例。
- 顶层函数的第一个参数为 `&State` 或 `&mut State`，经编译器验证为不逃逸的实例入口，
  才能作为 Godot 实例方法调用；接收者由扩展注入，不出现在 Godot 参数列表中。
- `_ready` 若存在，必须只有接收者、返回 Unit。Godot 调用它时执行解释器。
- 方法其余参数及返回值可为现有数值类型、Bool、Rune、Unit。整数严格检查位宽，
  UInt64 返回值超出 Int64 范围会报错；Float32 入参采用 IEEE 窄化，但拒绝有限值变无穷。
  Rune 对应恰好一个 Unicode 标量的 Godot String，Unit 对应 Nil。
- 加载与编辑器验证只编译，不创建状态。编辑器中的脚本实例为占位实例，不运行 Joky。
- 脚本资源共享编译快照；每个节点持有独立 `InstanceId`。删除节点或移除脚本释放根。
  扩展关闭时先关闭仍存活的根，再注销语言及加载/保存器。
- `Script.reload()` 为后续实例编译新快照，现存实例继续使用旧快照及其状态。
  `keep_state` 不表示迁移状态；源码保存不保存运行时 `State`，场景重开重新构造。
- 解析、类型及运行错误提供 `路径:行:列`。方法运行失败沿用 P1 的失败实例语义；
  重新创建节点才能重新运行该实例。参数转换失败发生在执行前，不改动状态。

每次解释器调用最多 256 帧、100,000 条指令。此限额不覆盖编译时间和宿主原生调用，
也不是毫秒级帧预算。脚本加载和执行限主线程，不支持线程加载或从 Godot WorkerThread 调用。
绑定启用 `experimental-threads` 以接受编辑器后台扫描的元数据查询；
持久 VM 快照存于主线程局部注册表，Script 资源只保存键，后台释放资源时排队到主线程清理。

G0 只接通脚本语言与同步纯值状态。尚无属性导出、引擎对象/Node API、信号、
帧回调的专门适配、拥有型 String/Array 状态、异步、工具脚本、脚本继承、
内嵌脚本、用户文件模块解析、自动补全、断点调试和状态热迁移。
此原型尚不具备全面替代 GDScript 的能力；后续阶段见
[持久实例与 Godot 计划](../../docs/persistent-instances-numerics.md#43-godot-适配线)。

## 验证

```sh
./integrations/godot/check.sh
# 其他安装位置：
GODOT_BIN=/path/to/Godot ./integrations/godot/check.sh
```

该命令独立运行扩展格式/Clippy、Rust 行为测试、构建与真实 Godot 进程验收。
主仓库的 `scripts/check.sh` 不隐式要求安装 Godot。
引擎测试包含编辑器导入/源码打开/占位实例、未支持的符号查询与补全响应、两个实例的 `_ready` 与独立状态、标量边界、
脚本资源保存、场景保存后另进程重开、反复构造/销毁和错误位置检查。

实现入口为 [language.rs](src/language.rs)、[script.rs](src/script.rs)、
[instance.rs](src/instance.rs) 和 [resource.rs](src/resource.rs)。ABI 参考
[Godot ScriptLanguageExtension](https://docs.godotengine.org/en/4.7/classes/class_scriptlanguageextension.html)、
[ScriptExtension](https://docs.godotengine.org/en/4.7/classes/class_scriptextension.html)、
[gdext ScriptInstance](https://godot-rust.github.io/docs/gdext/master/godot/obj/script/trait.ScriptInstance.html)。
