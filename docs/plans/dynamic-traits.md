# 阶段 9：动态 trait/interface

当前用户语法与边界见[Trait 与动态分派](../lang/traits.md)，模块表示见[模块 ABI](../compiler/module-abi.md)。

> 状态：动态分派、多 trait 组合和显式所有权向上转型已实现；借用转型、向下转型和类型查询待实现。
> 前置：阶段 6；建议在阶段 7、8 后实施。

## 任务

- [x] 实现 `Dyn(Trait)`、显式装箱和借用/消费 receiver。
- [x] 关联类型显式绑定，拒绝参数/结果中未绑定的 Self。
- [x] 动态调用接入 Pending ABI，覆盖恢复、取消和恰好一次析构。
- [x] 模块间传递动态对象并重定位方法签名，更新模块 ABI 元数据版本为 19。
- [x] 添加 examples、fixtures 和 JIT/缓存/debug AOT/release AOT 验证。
- [x] 实现 `Dyn(A + B)` 多 trait 组合、顺序无关的类型身份与名称冲突诊断。
- [x] 实现 `Dyn(A)(value)` 显式所有权向上转型，支持接口子集与连续转换。
- [ ] 实现借用向上转型、向下转型和运行时类型查询。
- [ ] 扩展 primitive 装箱、泛型装箱和容器支持。

## 验收

不同模块提供的实现通过 `Dyn(Trait)` 调用；对象释放、跨模块调用和取消均有
端到端测试。首版边界与示例见 [语言说明](../lang/traits.md#动态-trait)。

## 表示与生命周期

动态值的 ABI 是两个指针：方法表地址和 managed allocation 地址。allocation
包含存活标记、按方法名排序的入口地址，以及具体值的 ABI payload。方法入口
采用统一 Pending ABI，drop callback 知道具体 payload 类型。当前方法表随
对象分配，后续可在保持接口语义的前提下改为共享只读表。

| 路径 | payload 所有者 |
| --- | --- |
| 构造后未调用消费方法 | 动态对象；释放时由具体 drop glue 清理 |
| 借用方法运行或挂起 | 调用方保留动态对象，实现借用或复制共享值 |
| 消费方法进入 | 入口清空存活标记，将 payload 转交具体函数 |
| 消费方法挂起或取消 | 具体函数的 continuation 负责 payload；外层只释放空包装 |

禁止擦除带 effect 的析构契约，避免取消清理时缺少必要的 handler。无 effect
的用户析构通过具体类型的 drop glue 执行。C ABI 不接受动态值。模块缓存版本
19 包含排序后的 trait 身份集合、关联绑定、receiver/参数借用模式、方法签名和
`DynamicUpcast` MIR 指令，
链接时核对表结构。组合接口复用同一个 payload 和 drop glue，方法表仍按方法名排序。

所有权向上转型原地压缩对象私有方法表，保留原始 payload 偏移、方法入口和 drop
callback，不分配新对象。连续转换按当前接口映射槽位。此策略依赖动态对象的
唯一所有权以及每个对象独有的方法表；未来共享只读表时需调整转换表示。
