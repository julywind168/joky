# 待办索引

这里只保留尚未完成或尚未决定实施的工作，不承担变更日志职责。原清单的已完成条目见[历史摘录](../archive/plans/completed-work.md)。

## 有独立计划的工作

- [ ] 完成[资源控制 5.4](resource-control.md#54-用真实程序验收)的独立工作流总验收和 Linux 长跑；保留资源与故障注入证据。
- [ ] 按需扩展[动态 trait](dynamic-traits.md)：借用向上转型、向下转型、类型查询及其余装箱边界。
- [ ] 按需推进[迭代协议](iteration-protocol.md)的并发泛化、Dyn 变体和字符迭代。
- [ ] 验证[嵌入式执行与热更新](embedded-execution.md)的后端可行性，再细化 API 和状态兼容契约。
- [ ] [Child 与交互式子进程](process-child.md)：暂缓，现有 Command / Pipeline 不受此提案影响。
- [ ] [AOT 交叉编译与发布配套](cross-compilation.md)：暂缓，不能以 runtime 静态库编译成功代替目标程序实跑。

## Region 与资源模型

- [ ] 放宽区域检查的保守边界：更精确的字段 / 别名及高阶调用约束、模块泛型实例摘要、携带 Cown 的用户析构顺序证明；补充大图与分配 / 退出延迟基准。当前规则见[Region](../runtime/regions.md)。
- [ ] 通用 arena：先定义结果外带 / 复制、共享节点、资源析构与兼容性规则，再扩展到其他堆对象。首版只改变 Cown 生命周期，见[后续范围](../runtime/regions.md#6-后续通用-arena-扩展)。

## 调度与 Pending ABI

- [ ] 降低 Cown Pending 保存 / 恢复、等待节点分配和重复入队成本；极短 body 高竞争场景仍需评估吞吐、尾延迟与公平性，不承诺严格 FIFO 或无饥饿。已实现协议见[Cown 调度](../runtime/cown-scheduling.md)。
- [ ] timer capability：历史阶段决定暂留 operation identity fast path；若重启改造，先定义 capability 与注册 / 查询 API，再移除 `is_time_sleep_operation` 特判并保留 payload 回归。见[阶段 4 的 D3 决策](../archive/plans/phase4-robustness-release.md#42-挂起语义边界决策)。
- [ ] 长期语义与资源优化：挂起借用、machine-entry 类型边界、未完成 parent 链压缩需重新对照当前实现评估，不能直接沿用旧阶段的拒绝清单。来源见[阶段 4 的 D1–D4](../archive/plans/phase4-robustness-release.md#42-挂起语义边界决策)。
- [ ] 历史压力 SIGSEGV 的根因仍缺确认记录；接续[资源控制验收](resource-control.md)时单列证据，不能以有限次数通过宣称已证明并发正确性。

## FFI 与发布

- [ ] 按真实调用需求扩展按值 aggregate ABI、拥有型 C buffer、有界指针加长度读取及其他宿主实跑。这些不属于[阶段 7](../archive/plans/phase7-c-ffi.md)已完成范围，当前边界见[C FFI](../lang/c-ffi.md)。
- [ ] 目标平台编译器 / runtime 配套与 Linux 原生调试器验证由[交叉编译与发布计划](cross-compilation.md#发布配套)承接。

完成条目时更新主要契约文档和测试，再从本索引移除或链接到历史记录；不要在此持续堆积已完成实现细节。

## PostgreSQL 驱动

[纯 Joky 协议原型](../stdlib/pgsql.md)已实现协议 3.0、trust / SCRAM 认证和简单文本查询。
[Base64](../stdlib/base64.md)、[SHA-256](../stdlib/sha256.md) 和
[HMAC-SHA-256 / PBKDF2](../stdlib/hmac-pbkdf2.md) 与 [OS 安全随机数](../stdlib/random.md) 基础库已可用。
[SCRAM-SHA-256 核心](../stdlib/scram-sha256.md)已实现随机 nonce、proof 计算、
严格挑战解析与服务端签名校验；[SASLprep / PostgreSQL 密码准备](../stdlib/saslprep.md)
及固定 Unicode 3.2 NFKC 已接入数据库认证，并通过真实数据库密码认证验收。下一步按以下依赖顺序推进：

- 通用 TLS provider，支持升级已有 TCP 连接、证书链和主机名验证；之后支持 channel binding。
- Parse/Bind/Execute/Sync 参数绑定，结构化错误，以及更多 PostgreSQL 类型转换。
- Deadline、CancelRequest、流式结果与连接池，明确提前结束结果和取消后的同步规则。
- 评估普通 class 借用跨挂起的语言设计；当前 API 显式消费并返回连接，无须放宽 verifier。
