# 测试与测量

这里维护当前的运行方法和测量口径。带日期、机器配置、commit 与测试数量的历史结论位于[验收与测量归档](../archive/README.md#验收与测量)，不能当作当前版本的测试结果。

## 常用入口

| 目的 | 入口 |
| --- | --- |
| 编译器 / CLI 测试及 AOT 准备 | [根 README](../../README.zh.md#测试与基准) |
| 选择 JIT / AOT 矩阵、缓存覆盖和任务并发度 | [测试范围与耗时](suite.md) |
| 独立 runtime 与隔离资源测试 | [Runtime 开发约定](../runtime/development.md#验证命令) |
| 库测试、隔离检查与可选 TSan | [`scripts/check-runtime.py`](../../scripts/check-runtime.py) |
| 模块编译分配量与缓存测量 | [模块编译基准](module-compilation.md) |
| 文档文件、锚点与导航检查 | [文档维护约定](../maintenance.md#检查) |

## 记录原则

- 命令从仓库根目录执行，区分编译器、独立 runtime 与 CLI 测试范围。
- ignored 测试不计为已经运行；资源计数与压力测试按其隔离要求执行。
- 性能比较记录 commit、工具链、平台、构建模式、样本数与测量边界。
- 新结果另建带日期的报告；旧报告保留原结论与适用版本，通过链接说明后续变化。
