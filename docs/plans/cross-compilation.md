# AOT 交叉编译与发布配套

> 状态：暂缓，尚未支持跨目标可执行文件；本机行为见[AOT 契约](../compiler/aot.md)。

2026-09-17 决定暂缓跨目标编译，不继续安装目标工具链或接通跨目标链接。
本机 AOT、debug info、strip 和构建记录等已完成能力保留。

- 已验证：在 ARM64 macOS 上用 `cargo build --manifest-path crates/joky-runtime/Cargo.toml --target x86_64-pc-windows-msvc --locked` 成功生成 Windows runtime 静态库 `joky_runtime.lib`；尚未链接或运行 Windows Joky 可执行文件。
- 编译器待办：启用目标架构后端，接通目标 ABI、类型布局、FFI 平台选择和对象格式；按目标选择 runtime archive、launcher 编译参数和链接器，不能只放开 `--target` 校验。
- Windows 首选候选方案：Rust MSVC runtime + `clang-cl` + `lld-link`，用 [xwin](https://github.com/Jake-Shadle/xwin) 准备 Windows SDK/CRT。本机已有两个 LLVM 工具；xwin 官方说明已可访问，但工具未安装，SDK/CRT 尚未下载，最终链接未验证。恢复时需确认工具版本、固定 SDK/CRT 版本并统一 CRT 链接方式。
- 平台边界：xwin 只提供 Windows 目标配套。Linux 可评估 Zig 或独立 sysroot；macOS 需要 Apple SDK 及相应工具链，并确认许可条件，不将 xwin 当作通用跨平台方案。
- 验收安排：用户可在 Windows 云机器手动验证。恢复后先生成最小 `.exe`，再验证 Cown、task、挂起、取消、FFI 和资源清理组合用例；跨平台运行验证不以静态库编译成功替代。

## 发布配套

- [ ] 按目标平台配套编译器、runtime archive 和链接工具链，并核对 ABI 兼容性；接续[单一 runtime 迁移](../archive/plans/runtime-unification.md)的发布事项。
- [ ] 完成 Linux 原生调试器的 ELF/DWARF 验证；其他调试扩展见[阶段 8 原始记录](../archive/plans/phase8-aot.md)。
