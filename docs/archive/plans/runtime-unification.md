# 单一 runtime 实现迁移

> 历史记录：本文保留原阶段的设计、环境与验收结论，不代表当前能力。归档于 2026-09-24；当前说明见[文档入口](../../README.md)，未完成工作见[计划索引](../../plans/README.md)。

## 边界

- 编译器负责语义检查、MIR/对象生成、provider 签名验证和 operation ID 元数据。
- `joky-runtime-abi` 定义生成代码和 runtime 共用的状态、布局与版本。
- `joky-runtime-core` 过渡期间承载可独立链接的 I/O、C 内存和析构区域等
  基础模块；编译器与 AOT runtime 均通过 rlib 使用它。
- `joky-runtime` 负责值、scope、任务、continuation、provider hooks 和资源回收；
  它不依赖编译器类型。JIT 通过 rlib、AOT 通过静态库使用相同实现。

## 切片和验收

1. [x] 启用独立 runtime 单元测试。用固定槽位/operation ID 测试原生 hooks，
   编译器特有的 provider 签名及程序行为留在编译器测试。验收：独立 crate
   运行非零数量测试，既有库/CLI 测试继续通过。
2. [x] 先确定 JIT 共用 runtime 的符号归属和 host API：scope、continuation、
   入口注册以及 generated-code ABI 必须作为一个整体切换。两份 runtime
   当前都导出大量同名 `jk_*` 符号，不能在一个进程中直接链接两套实现。
   过渡期间先把无 scope 依赖的模块放入 `joky-runtime-core`，让 JIT 与
   AOT 正常链接同一份 rlib；stateful runtime 仍须作为整体切换。
   编译器内嵌 runtime 已取消裸 `jk_*` 导出，JIT 继续按函数地址注册；
   正式 C 符号只由 `joky-runtime` 导出，为后续链接其 rlib 清除冲突。
3. [x] 编译器侧 adapter 将 `TypeTable` 转成验证后的 provider 清单。JIT 改为
   使用独立 runtime 的 rlib 后，逐个迁移 file/socket/sqlite，并删除
   `src/runtime` 的重复实现。验收包括挂起、取消、错误传播与资源计数。
   当前 JIT provider 安装协调器已移至 `src/compiler/providers.rs`，runtime 的
   `ProviderScope` 不再依赖语义类型；file/socket/sqlite 的 AOT operation ID
   表已转入编译器。standalone runtime 已提供 opaque host scope API，统一管理
   scope、机器入口和 provider 生命周期；JIT hooks 与 file handle 校验仍在
   JIT 实现中。编译器已链接该 rlib，并通过 scope smoke test 验证生命周期。
   scope/continuation/callback 的 66 个 JIT ABI 地址由 runtime 通过 visitor
   提供；task/handler/value/batch 也已纳入合并 visitor，共覆盖当前 Cranelift
   使用的 201 个唯一 ABI 名称。opaque host scope 也已补齐可克隆的代码生命期、
   Pending 根入口、机器入口注销及运行错误/失败读取。生产 JIT 现已通过该 visitor
   和 host scope 使用 standalone runtime，file/socket/sqlite provider 也由编译器
   metadata adapter 安装到同一 scope。内嵌 runtime 仅保留给尚待迁移的白盒测试；
   `joky-runtime/test-support` 只在编译器测试构建中开放 machine-resumption
   与 scope-owned managed object 观测、raw provider hook 及 opaque continuation
   handle，Pending、task-machine 及 resource codegen 测试已全部切换到 standalone。
   task/continuation/handler 的布局常量、handler ownership tag 和 codegen 使用的
   ABI 符号名已移入 `joky-runtime-abi`。重复的 compiler `src/runtime` 已删除，
   JIT 与 AOT 现在只链接 `joky-runtime`。
4. [ ] 发布时按目标平台配套编译器和 runtime archive，并检查交叉目标。launcher
   已通过 `joky-runtime-abi` 中的版本化链接哨兵拒绝不兼容的 runtime archive。

## JIT 切换契约

- 生成代码的 `jk_*` ABI 由 `joky-runtime` 唯一实现：AOT 用静态库的
  `#[no_mangle]` 导出，JITBuilder 用同一 rlib 的函数地址注册符号。
  移除编译器中的旧 stateful runtime 后再链接该 rlib；只重命名旧导出
  不会合并两份独立的 scope、continuation 和全局注册表。
- 编译器的 host API 需要创建/进入/关闭 scope、等待本次工作完成、
  注册和注销本 scope 的机器入口，并在关闭之前持有 provider 注册。
  语义签名验证及 operation ID 构造留在编译器，runtime 只接收验证后的
  ID、hook 和作用域；JIT 的符号注册与 host API 必须指向同一 runtime。
- 切换时保留 JIT 的挂起、取消、失败传播和资源计数测试，并验证复用
  `Compiler` 实例的连续运行不会遗留入口或作用域状态。

每个切片通过对应测试后提交；不在删掉重复实现前取消现有 JIT 行为测试。
