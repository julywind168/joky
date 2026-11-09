# Chat：控制桥整段复制与 outbox Vec 移交（2026-10-30）

> 原始样本、构建元数据与一次性分析脚本已归档到 Git 历史；明细链接指向固定快照，恢复方法见[归档说明](README.md)。

在 outbox 单次接收基础上，先将控制桥的单字节循环改为整段复制，再将出站队列的 Vec
直接移交给控制回复。三版交替测量显示，本轮 **64 字节广播吞吐中位数提高约 1.5%～6.3%**，
每条广播服务端 CPU 成本下降约 2%～6%；单客户端请求回显延迟基本不变。
补充的 1 KiB 正文组吞吐提高约 4.3%。结果存在交叠与波动，不能宣称稳定或显著提升。

本轮确实消除了两次中间复制和相关分配，但收益有限，尚不足以大幅缩小与 Tokio 的差距。
没有重测 Tokio，不拼接之前的 Tokio 数据计算新比值。后续优先复核共享预算与调用路径热点，
不以本轮结果为由继续扩大零拷贝改造。

## 两步改动

1. **bulk：整段复制。** [控制桥](../../src/runtime/parallel/control.rs) 的 stride=1 路径，
   通过受检 `Buffer::bytes()` 取得连续已初始化字节，用整段复制建立 Vec；反向使用
   `Buffer::copy_bytes()` 一次初始化任务存储。非单字节元素仍使用原路径。
   该变化也覆盖控制桥的其他字节参数／回复，不改变调用接口或元素布局。
2. **moved：在 bulk 上移交 Vec。** [outbox](../../src/vm/state_domains/outbox.rs) 共用所有者校验、
   等待和出队逻辑。Local 接收仍在出队前分配结果 Buffer；Parallel 接收先为回复预留元数据与字节预算，
   成功后移出原 Vec，再归还队列消息的旧收费。回复携带自己的 Lease，取消或丢弃时释放数据及收费。

Parallel 的接收路径由 `队列 Vec → 控制端 Buffer → 回复 Vec → worker Buffer`，变成
`队列 Vec → 移交给回复 → worker Buffer`。最终安装语言 Array 时仍复制一次，不是端到端零拷贝。
不跨线程传递语言指针；消息顺序、空消息／关闭区别和控制回执清理不变。
出队前预算预留失败保留队首；worker 安装失败不重投已经出队的消息，沿用原取消／失败契约。
本轮没有改变公共 ABI、chat 业务、共享预算算法或生成代码优化设置。

新增低层行为测试直接检查 Vec 地址保持不变，并验证预算失败保留队首、替代收费在出队后存活、
丢弃回复后归还预算。现有生成代码测试验证上述路径在 JIT、解释器和独立 AOT 中执行。

## 构建和测量

机器为 Apple M1 Pro（6P + 2E）、16 GiB、macOS 26.6.2 arm64；Rust 1.99.0、Python 3.14.6。
工具／运行时使用 Release，Joky 生成代码仍采用原默认设置；应用为 AOT host + 内嵌 JIT 脚本。

| 版本 | 实现与产物 |
| --- | --- |
| before | `76ab329` 的单次接收实现，复用 `target/benchmarks/chat-outbox-receive/build`；其原构建身份为 `58ad67a` 加前一阶段补丁 |
| bulk | `76ab329` 加整段复制补丁，`target/benchmarks/chat-byte-transfer/bulk` |
| moved | `76ab329` 加本轮完整改动，`target/benchmarks/chat-byte-transfer/moved` |

三份构建记录：[before](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-byte-transfer-2026-10-30/before-build.json)、
[bulk](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-byte-transfer-2026-10-30/bulk-build.json)、[moved](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-byte-transfer-2026-10-30/moved-build.json)。
中间版本的[整段复制补丁](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-byte-transfer-2026-10-30/bulk.patch.gz)解压后可应用于独立的 `76ab329` checkout 复建。
[产物哈希](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-byte-transfer-2026-10-30/artifact-hashes.json)包含代码、补丁、二进制和测量文件身份。

- 第一步完成后先跑 before／bulk 各四配置，共 8 个预备样本。结果不一致，未据此定论。
  这组保留在 `probe-before`／`probe-bulk`，不进入下面的三轮中位数。
- 正式矩阵继续使用同一个 [chat-workers.py](../chat-workers.py)：16 客户端、64 字节正文，
  1 房间各 16 人／4 房间各 4 人，1／4 worker；每连接预热 25 条，每批计时 250 条，完整批次重复到至少 3 秒。
  每连接等待本条广播所有接收者完成后才发送下一条。广播/秒为发言数，送达/秒还需乘房间人数。
- 单客户端使用 1 房间、1 worker、64 字节正文，每批 5000 条，同样至少 3 秒。
- 每轮先测三版矩阵，再测三版单客户端；版本顺序为 before→bulk→moved、bulk→moved→before、
  moved→before→bulk。worker 顺序为 `1,4`、`4,1`、`1,4`，房间顺序同样轮换，共 **45 个主样本**。
- 小消息测完后补充 1 KiB 正文、16 客户端、1 房间、4 worker；每连接预热 25 条，每批 100 条、至少 3 秒。
  三版按相同版本顺序再测三轮，共 **9 个补充样本**，只用于检查正文大小的影响，不替代主矩阵。
- 计时串行执行，期间没有构建、测试或 profiler。启动、join、预热、余额核对和 shutdown 不计时。
  全部 54 个正式样本通过消息内容、接收者、每发送者顺序、扣费及正常退出校验；没有丢弃慢样本。

## 三轮结果

下表各项为三个独立服务进程样本的中位数。分位数先逐样本计算再取中位数，不是合并分布的分位数。
百分比按未舍入数据计算；只有三个样本，不给显著性或跨机器保证。完整范围与每次数据保留在 CSV。

| 房间 | worker | 原版广播/秒 | 仅整段复制 | 完整优化 | 完整版较原版 | Vec 移交较 bulk |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | 1 | 738.9 | 728.0 | 750.2 | +1.5% | +3.0% |
| 1 | 4 | 1058.4 | 1082.6 | 1088.6 | +2.9% | +0.6% |
| 4 | 1 | 1878.9 | 1920.2 | 1912.2 | +1.8% | −0.4% |
| 4 | 4 | 2221.6 | 2248.7 | 2361.6 | +6.3% | +5.0% |

整段复制单独没有一致的吞吐收益；Vec 移交也不是所有配置都更快。完整优化的单房间 4 worker
吞吐范围为 1079.1～1199.7，原版为 1054.8～1092.6；四房间 4 worker 范围分别为
2273.3～2552.7 与 2121.8～2406.7，均存在交叠，不能将中位数变化解读为每次都会提速。

| 房间 | worker | 原版 CPU ms/广播 | bulk | moved | 原版 P99 ms | moved P99 ms |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | 1 | 2.0200 | 2.0225 | 1.9700 | 24.69 | 24.18 |
| 1 | 4 | 2.9450 | 2.8950 | 2.7800 | 18.44 | 18.33 |
| 4 | 1 | 0.9025 | 0.8825 | 0.8813 | 12.69 | 12.50 |
| 4 | 4 | 1.4325 | 1.4113 | 1.3863 | 10.72 | 10.07 |

CPU ms/广播逐样本以服务端 CPU 秒数／实际广播数计算后取中位数。
各配置服务端 CPU 核数中位数约 1.48～3.27，客户端约 0.26～0.50，未出现此前 Tokio 对照的一核客户端饱和。
采样 RSS 最大值中位数，原版约 74.6～77.2 MiB、完整版约 74.7～77.0 MiB；没有明显内存改善。
RSS 每 250 ms 采样，包含内嵌编译器和 allocator 缓存，不等同于精确峰值或预算用量。

| 单客户端指标 | 原版 | bulk | moved |
| --- | --- | --- | --- |
| 请求/秒 | 1622.2 | 1638.1 | 1627.2 |
| P50 ms | 0.5555 | 0.5505 | 0.5531 |
| P99 ms | 0.9593 | 0.9820 | 0.9976 |
| 服务端 CPU ms/请求 | 0.522 | 0.520 | 0.522 |

单客户端基本无改善，P99 中位数略高。这是完整业务请求到回显路径，不是 TCP 单程时间。

| 1 KiB 补充组（1 房间／4 worker） | 原版 | bulk | moved |
| --- | --- | --- | --- |
| 广播/秒 | 289.1 | 290.2 | 301.4 |
| CPU ms/广播 | 9.5750 | 9.4563 | 9.1938 |
| P99 ms | 71.99 | 72.76 | 58.74 |

补充组完整优化较原版吞吐增加 4.3%、CPU 成本下降 4.0%。即使正文增加到 1 KiB，
这条复制链的优化仍只带来有限收益；不能由此推导大消息最大容量或过载表现。

## 验证与复现

未运行全量 check.sh、全量测试或压力穷举。bulk 阶段跑跨 worker outbox 定向测试；最终版本实际通过：

- `cargo test --lib outbox`：12 通过、1 个穷举忽略，包含新增的出队预留／Vec 地址／退款测试及生成代码回归。
- 部分回复分配失败与预算归还、独占邮箱借用归还：各 1 个定向测试通过。
- `cargo test --lib runtime::parallel::control::tests`：3 通过，包含原有迁移回复预留契约。
- CLI 的 outbox 原子发布三后端测试，以及独立 AOT 控制桥 1／3 worker 测试：2 通过。
- `cargo clippy --lib -- -D warnings`、`cargo fmt --check`、`git diff --check` 通过。

三个版本先构建／快照，再计时。最终版本的复现命令：

```sh
cargo build --release --bin joky --lib
python3 benches/chat-workers.py prepare \
  --joky target/release/joky --runtime target/release/libjoky.a --profile release \
  --output target/benchmarks/chat-byte-transfer/moved
python3 benches/chat-workers.py run \
  --build target/benchmarks/chat-byte-transfer/moved \
  --output target/benchmarks/chat-byte-transfer/matrix-1-moved \
  --workers 1,4 --rooms 1,4 --clients 16 --messages 250 --warmup 25 --samples 1 --min-seconds 3
```

按上述版本／配置顺序轮换；单客户端改为 `--workers 1 --rooms 1 --clients 1 --messages 5000`。
补充组改为 `--workers 4 --rooms 1 --clients 16 --payload-bytes 1024 --messages 100`。输出目录须尚不存在。
新阶段的原版、bulk 和 moved 必须来自相应实现，不能只切换运行参数。

[54 个正式样本](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-byte-transfer-2026-10-30/samples.csv)、
[环境与 argv](https://github.com/julywind168/joky/blob/f8fc33a8ed6bb4ac9defe93ac1f30528c03ebaa6/benches/reports/chat-byte-transfer-2026-10-30/environments.json)保存紧凑记录。
完整源码快照、补丁、逐消息延迟、日志和二进制在 `target/benchmarks/chat-byte-transfer/`，
原版产物仍在 `target/benchmarks/chat-outbox-receive/build`。大体积原始延迟和二进制不提交。
