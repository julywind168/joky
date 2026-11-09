# 历史基准结论

本目录保存实验结论、必要的汇总表、测量边界与复现方法。
原始样本、完整机器元数据、调用树、临时补丁与一次性分析脚本放在忽略的 `target/`，不再入库。
基准运行器和可复用工具保留在 `benches/`，使用方法见[基准说明](../README.md)。

早期入库的 Chat 实验细项和 While 初始基线已从当前目录树移除，仍可查阅
[清理前的固定 Git 快照](https://github.com/julywind168/joky/tree/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches)。
各报告的明细链接指向该快照；历史测量不能作为当前版本的性能承诺。

需要本地查阅旧数据或恢复当次分析脚本时，从仓库根目录导出到忽略的目录：

```sh
mkdir -p target/archives/benchmark-details-f8fc33a8
git archive --format=tar.gz --output=target/archives/benchmark-details-f8fc33a8.tar.gz \
  f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6 \
  benches/reports benches/baselines/2026-09-27-apple-m1-pro
tar -xzf target/archives/benchmark-details-f8fc33a8.tar.gz \
  -C target/archives/benchmark-details-f8fc33a8
```

该命令需要本地包含此提交的 Git 历史；浅克隆或源码压缩包可直接使用上面的在线快照。
原始二进制、逐消息日志等从未入库的本地产物不包含在 Git 归档中。
