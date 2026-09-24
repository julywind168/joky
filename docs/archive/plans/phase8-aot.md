# 阶段 8：AOT 与可发布程序

> 历史记录：本文保留原阶段的设计、环境与验收结论，不代表当前能力。归档于 2026-09-24；当前说明见[文档入口](../../README.md)，未完成工作见[计划索引](../../plans/README.md)。

> 状态：实施中（已完成独立 runtime archive 与最小 AOT 纵切）。
> 前置：阶段 6；FFI 集成依赖阶段 7。

## 任务

- [x] 复用 MIR/Cranelift 实现 object emission，不另建语义后端。
- [x] 生成可执行文件，链接独立 runtime、drop glue、字符串和静态注册表。
- [x] 固定泛型实例化、trait 静态派发和 continuation/machine entry 注册策略。
- [ ] 支持 debug info、strip、交叉目标选择和可重复构建记录（跨目标编译暂缓，接续事项见 [待办](../../plans/cross-compilation.md)）。
- [x] 保留 JIT 作为开发模式，保证 JIT/AOT 的行为测试共用。

当前纵切包含 `joky-runtime-abi` ABI crate 和 `joky-runtime` 静态库。CLI
只使用 `libjoky_runtime.a`；编译器 crate 已不再生成 staticlib，CLI 也不再把
历史 `libjoky.a` 当作 runtime。独立
runtime 当前覆盖通用值、任务、continuation、handler、reactor、callback ABI、
独立的 `ProviderScope` 生命周期，以及不依赖 `TypeTable` 的 file/socket provider
实现。file 的签名发现、12-slot operation ID 元数据和 AOT launcher 注册已经接通；
socket 的签名适配、固定 slot 元数据和 launcher 注册也已接通。sqlite runtime、
8-slot 元数据和 launcher 注册也已接通。每个 launcher 都引用版本化 AOT ABI
哨兵，不兼容或过旧的 runtime archive 会在链接时失败；后续重点转为目标平台
配套、产物裁剪和更广泛的 AOT provider 回归。

`joky build --release` 显式选择 release runtime archive，不受 compiler 自身
debug/release 构建方式影响；默认构建使用 Cranelift `opt_level=none`，
`--release` 切换为 `speed` 优化，`--strip` 在最终链接阶段移除符号。
每次成功构建会在可执行文件旁生成 `<output>.build.json`，记录实际编译的
模块内容哈希、编译器身份、目标和优化配置、runtime ABI/文件哈希、链接器
身份与参数，以及最终产物 SHA-256。清单不包含生成时间戳；链接或清单发布
失败会返回错误，重新链接前移除旧清单。此项提供产物追溯，尚不保证逐字节
可重复构建。`--target <TRIPLE>` 已支持显式选择编译器自身目标，与默认构建
共用统一的目标、指针宽度和 native CPU 配置；其他目标在读取源码、改写产物前
报错。跨目标发射、布局、FFI 选择及 runtime/linker 配套暂缓，待实际发布需求
再推进；已验证的 Windows runtime 构建和工具链候选方案记录于
[待办](../../plans/cross-compilation.md)，不视为跨目标可执行文件已验收。

`joky build -g` / `--debug-info` 已支持最小 DWARF 函数名、源文件和行号，
包括跨模块泛型定义与 continuation/machine entry。源码位置经过 HIR/MIR、
模块链接和优化保留；MIR 缓存升级为 `JKMIR016`。`--release -g` 可组合使用，
`-g --strip` 在改写产物前报错。macOS 在清理临时 object 前生成旁置 dSYM，
清单记录其 DWARF 文件哈希；已验证 LLDB 源码断点实际命中挂起恢复入口。
ELF 发射路径仍待 Linux 原生调试器验证；局部变量和异步逻辑调用栈不在首轮范围内。

泛型实例化、trait 静态派发和 entry 注册规则见
[AOT 编译与注册契约](../../compiler/aot.md)。已实现程序内确定性 entry 编号、
稳定入口符号和注册冲突检查；JIT/AOT 共用组合用例覆盖跨模块实例去重、
关联类型、调用方 trait 实现、Ready/Pending、重复恢复、并行调用及取消后的
单次析构。回归同时补齐了对象分配、字符串拼接、闭包环境、Cown 和用户 Drop
的 AOT runtime 导出，以及泛型回调参数的结构化函数类型去重。

## 验收

一个包含 Cown、task、挂起调用和 FFI 的示例可脱离编译器进程运行；启动、取消、退出后的资源计数与 JIT 模式一致。

已在 macOS ARM64 验证 `examples/concurrency/aot_runtime.jk`：Cown 握手确保被取消分支
已恢复执行，取消后 C 文件成功且仅关闭一次，再完成并行挂起任务。共用 harness
覆盖冷/热 JIT、debug/release AOT，删除源码、缓存和包配置后从其他目录运行
独立产物；同时覆盖取消后返回 `Result` 错误的退出路径。启用
`runtime-test-support` 后，14 个共用用例均检查启动和退出清理后的 18 项
资源计数归零，取消过程以单次成功析构及有界完成时间验证。AOT 正常退出
显式 drain 并撤销 entry/callback 注册，测试 hook 只读计数，不代替清理。
用例还覆盖 file/socket/sqlite、跨 C 线程回调；Linux/Windows 尚待原生验证。
运行方式与计数范围见 [AOT 回归说明](../../compiler/aot.md#regression-coverage)。
