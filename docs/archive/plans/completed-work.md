# 已完成工作摘录

> 历史记录：从原待办清单分离，归档于 2026-09-24。条目描述当时的实现与验证，不构成当前 API 规范；后续工作见[待办索引](../../plans/backlog.md)。

- [x] 统一 Region 生命周期与 Cown 不逃逸检查：实现匿名 `region { ... }`，所有 Cown 归属当前/隐式根区域，函数、任务和 continuation 继承区域。typed MIR 静态检查逃逸并保存跨模块函数摘要；排空后两阶段销毁、句柄免原子 RC。覆盖缓存 JIT / legacy / AOT；首版保守限制及后续方向见[区域文档](../../runtime/regions.md)。

- [x] 修复 `when` 内 abort effect 的 lease 清理 lowering：直接 abort 在终止前释放 lease，间接调用的取消 / 失败边同样清理；按控制流目标保留或释放外围 lease，已终止的 `when` 不再追加 release。补齐多 Cown、正常 / 失败分支、嵌套外层 handler、同一 `when` 内直接 handler 及缓存 JIT / legacy / AOT 回归；verifier 继续拒绝遗漏 release 的 MIR。

- [x] 2026-09-21 移除 `joky/sqlite` 的 `query_one` 兼容接口：该操作借用 SQLite 的隐式类型转换把任意第一列变文本，签名 `Result(String, String)` 在类型上撒谎，属于 C 形状残留。替代路径为 `query()?` 游标 / `sqlite.step` 加 `row.text(0)` 类型化读取（NULL 显式为 `Ok(None)`，无效 UTF-8 在推进时报错）。std 声明、runtime 实现（`query_one`、`Operation::QueryOne`、`Output::Text` 及其结果写入分支）、钩子表行、契约掩码（`0x1fff`→`0x1ff7`）与文档同步删除；runtime 与 CLI fixture 测试全部改走 `step()`/游标路径，错误契约测试改为类型化等价断言。

- [x] 2026-09-21 收敛句柄类型：`File`、`SqliteConnection`、`SqliteStatement`、`TcpListener`、`TcpStream`、`UdpSocket`、`UnixListener`、`UnixStream`、`UnixDatagram` 九个 `Type` 变体合并为 `Type::Native(usize)`，索引 `types.rs` 中的 `NATIVE_TYPES` 注册表。此前每加一个句柄类要在 sema/codegen/MIR 约 15 个文件补 match 臂；现在新句柄类是一行注册表数据。`CCallback` 保留独立变体（C-FFI 专属分派逻辑），`Hasher`/`MutBytes` 属核心运行时类型不在范围。索引进入模块缓存序列化，旧缓存按 miss 重建。

- [x] 2026-09-21 effect 名单单一来源:`sema/effects.rs` 新增 `RUNTIME_EFFECTS` 注册表(7 行:effect 名 + provider 归属,`time` 无 provider 由 reactor 直服),`@runtime/{name}` 身份重写与编译器侧 provider 分组都从它派生;`providers.rs` 的 `PROVIDER_EFFECTS` 常量(含 unix 条件编译两份)删除,改为 `provider_effects()` 运行时派生——平台差异交给注册时按名跳过,不再需要编译器 cfg 分叉。契约测试固定分组顺序(AOT 注册失败退出码 = 2 + 组序号)。至此库与编译器/runtime 去耦闭环:新增 effect 库 = `RUNTIME_EFFECTS` 一行 + runtime `dispatch_provider` 一行 + 库本体,编译器代码零改动。

- [x] 2026-09-21 provider 注册 ABI v2：注册单元从「按槽位对齐的 u64 数组 + mask」改为 `(effect, name, operation_id)` 条目。runtime 侧三个 provider（file/socket/sqlite）各自持有名字键控的 hook 表，未知名字跳过、命中即按 ID 注册；`host.rs` 的 `dispatch_provider` 是唯一安装点（provider 名 → 实现的一张表），并导出通用 C 符号 `jk_provider_register`/`jk_provider_unregister`（替代每库一对的 `jk_*_provider_register*`，共删除 8 个符号）。编译器侧 `providers.rs` 从三张 Rust 签名校验表收缩为名单派生，操作条目直接从 TypeTable 的 `eff` 声明产出；`AotProviderMetadata` 收敛为 `Vec<ProviderDescriptor>`；`main.rs` AOT C 模板变为对 provider 列表的循环，不再逐库手写。std ↔ runtime 布局契约（原内建签名校验）迁移到 `providers/contract.rs` 测试模块——生产路径不再持有第二事实源，漂移在 CI 拦截。注意：改 runtime 后需 `cargo build -p joky-runtime [--release]` 重建 staticlib 档案，否则 AOT 链接缺符号（本次回归已两次踩到）。

- [x] 完善 `joky/sqlite`：多行查询、类型化列读取和完整值绑定已接入标准 `Cursor`；provider 正确性、取消和资源排空已补齐。见 [iteration-protocol.md D8](../../plans/iteration-protocol.md#d8-sqlite-游标的错误与资源生命周期已实施阶段-4) 与 [SQLite API](../../stdlib/sqlite.md)。

- [x] 任务本地 lease 批次、单 Cown 快速路径、每 Cown 等待队列及精确唤醒。

- [x] 获取阶段接入 MIR/continuation Pending ABI，等待任务释放 worker 调用栈。
  已处理调用者传播、恢复后 Store/Cown 返回值、多 Cown 重试、
  成功与取消竞争、lease 归属及 region 排空；保持 body 不挂起。
