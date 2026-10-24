# 文档索引

当前可用语法与内置 API 以语言参考为准，架构文档说明实现边界。
设计文档中的待办不表示已经实现；开发记录保留当时的阶段目标、限制与验收证据。
同一能力发生变化时更新参考文档，历史记录明确其适用阶段，不复制一份新的现行规范。

## 使用参考

- [语言与内置模块](language.md)
- [架构与嵌入约束](architecture.md)
- [Joky Interpreter](interpreter.md)
- [入门、安装与能力概览](../README.md)
- [Godot 适配器](../integrations/godot/README.md)
- [示例源码](../examples)

## 实现设计与后续目标

- [Joky 语法与类型系统设计（第一版草案）](syntax.md)
- [Arena、隐式生命周期与 FFI 设计](arena-borrowing-ffi.md)
- [服务端状态域与任务并发设计草案](server-concurrency.md)
- [状态域语法糖草案](state-domain-syntax.md)
- [有状态服务的编程模型与实施计划](stateful-service-ergonomics.md)
- [文件运行时设计与开发计划](file-runtime.md)
- [for 与迭代器分阶段开发计划](for-iterators.md)

## 开发记录与专项验收

- [可选多线程任务执行器：TP0–TP5 交付与验证](task-parallel-executor.md)

以下文档按功能演进保留阶段设计和验收记录；其中历史限制不覆盖语言参考中的当前规则。

- [匿名函数与闭包开发计划](anonymous-functions-closures.md)
- [Joky AOT 与 Script 热更新开发计划](aot-script-hot-reload.md)
- [动态数组与缓冲区所有权](array-storage-ownership.md)
- [借用型 dyn Trait 开发计划](borrowed-dyn-trait.md)
- [FixedArray 设计与分阶段开发计划](fixed-array-storage-ownership.md)
- [JSON 编解码设计与分阶段验收](json-codec.md)
- [List / Set 设计与分阶段开发计划](list-set-storage-ownership.md)
- [Map 设计与分阶段开发计划](map-storage-ownership.md)
- [模块常量与记录字段语法开发计划](module-constants-record-syntax.md)
- [网络运行时开发计划与验收记录](network-runtime.md)
- [持久实例、数值类型与 Godot 脚本接入开发计划](persistent-instances-numerics.md)
- [记录解构开发计划](record-destructuring.md)
- [String 设计与分阶段开发计划](string-storage-ownership.md)
- [join、scope 与 branch 开发计划](structured-concurrency-scopes.md)
- [trait 默认方法与关联类型开发计划](trait-default-methods-associated-types.md)

## 开发与测量

- [贡献规范](../CONTRIBUTING.md)与[代理验证规则](../AGENTS.md)
- [基准方法](../benches/README.md)
- [解释器历史性能数据](interpreter-performance.md)

修改文档标题时同步更新仓库中的章节链接。文件组织以主题和职责命名；阶段编号保留在开发记录中。
