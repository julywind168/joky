# 模块编译基准

基准位于 [`src/compiler/module_bench.rs`](../../src/compiler/module_bench.rs)，用于观测模块图扩大时的冷 / 热编译耗时、累计分配与缓存体积。

## 运行

在仓库根目录单独运行，避免其他测试的分配进入进程级计数：

```bash
cargo test --locked --lib compiler::module_bench::module_compilation_scaling -- \
  --ignored --exact --nocapture --test-threads=1
```

## 测量口径

- 使用独立缓存目录比较冷编译与热命中；冷缓存不等于清空操作系统文件缓存。
- 累计分配字节数不等于峰值内存或 RSS。
- 模块阶段测量不能直接外推为完整 `joky run` / `build` 的性能。
- 比较前后版本时固定平台、工具链、features、构建模式和样本设置；以基准代码确定本次输出字段。

2026-09-21 的依赖快照及前端数据拆分结果保存在[历史报告](../archive/reports/module-compilation.md)，原始环境、数值和验证边界均保留。当前前端架构见[编译器架构](../compiler/architecture.md)，测试职责见[测试范围](suite.md)。
