# 语言指南

本目录介绍当前语法、语义与限制；设计方向见[计划](../plans/README.md)。先按根 README 的[快速开始](../../README.zh.md#快速开始)运行一个程序。

## 推荐阅读顺序

| 主题 | 内容 |
| --- | --- |
| [绑定与基本表达式](basics.md) | let / var、解构、算术与逻辑 |
| [字符串、常用值与调试](values.md) | 字符串、Option / Result、Duration、Debug / echo |
| [函数与调用](functions.md) | 函数签名、函数值、参数和管道 |
| [类型与泛型](types.md) | struct、class、enum 与编译期类型值 |
| [Trait 与动态分派](traits.md) | trait、关联类型、Dyn 与接口组合 |
| [集合](collections.md) | List、Map、Set 及可变容器 |
| [循环与游标](iteration.md) | Range、Cursor、for、while / loop、有界并发 |
| [匿名函数与尾随闭包](closures.md) | 闭包字面量、捕获和尾随调用 |
| [包与模块](modules.md) | 项目布局、导入和可见性 |

## 并发与外部能力

- [结构化并发与 Cown](concurrency.md)：parallel、race、branch、region 与 when。
- [条件等待](when-until.md)：等待 Cown 状态满足条件。
- [Effect 与 Handler](effects.md)：操作声明、局部处理和挂起。
- [C FFI](c-ffi.md)：C 调用、指针、资源与回调的语言边界。
- [标准库](../stdlib/README.md)：文件、网络、进程等 API。

按语法查询可使用[语法导航](syntax.md)。代码块中有些是依赖上下文的片段或错误示例，完整可执行程序在 [examples](../../examples/) 中。
