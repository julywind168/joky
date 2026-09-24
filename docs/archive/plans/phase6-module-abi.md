# 阶段 6：模块、稳定 ABI 与独立编译

> 历史记录：本文保留原阶段的设计、环境与验收结论，不代表当前能力。归档于 2026-09-24；当前说明见[文档入口](../../README.md)，未完成工作见[计划索引](../../plans/README.md)。

> 状态：已完成当前阶段范围。常量、泛型、标准库与 CLI 默认入口已接通，扩展边界回归及完整测试通过。
> 目标：让 Joky 程序可以按模块编译、缓存和链接，为 FFI、AOT 与脚本模块加载提供稳定边界。

## 任务

- [x] 固定模块图的第一版稳定 ID 与版本化 ABI 指纹（源码顺序无关）。
- [x] 定义模块编译产物（ModuleArtifact），包含 MIR functions 和类型表。
- [x] 实现 `Compiler::compile_module()` 生成可缓存的 artifact。
- [x] 序列化和缓存 MIR 编译产物（`.jabi` 元数据与 `.jmir` artifact）。
- [x] 实现基础 linker，合并多个 artifact 为单一 MirProgram。
- [x] 固定导出名和类型/trait 的稳定 ID（跨模块符号解析）。
- [x] 跨模块泛型模板、实例的稳定身份、独立产物与磁盘复用协议。
- [x] 跨模块常量的类型、值导入及内容变化后的传递缓存失效。
- [x] 核对标准库与现有 CLI 用例在模块化路径上的兼容性。
- [x] CLI 默认模块化编译，提供磁盘缓存诊断、`--no-cache` 和显式 `--legacy`。
- [x] 定义 public ABI：标量、String、Tuple、class、managed handle、函数和 effect。
- [x] 区分源码身份、ABI 身份和 JIT 地址，禁止用地址作为长期标识。
- [x] 为跨模块 drop glue、runtime 初始化和诊断保留元数据。

## 验收

相同源码在不同编译批次得到可复用的模块 artifact；依赖未变化时不重新编译；ABI 变化能在链接前给出诊断；独立编译的模块可以正确链接并运行。

## 当前状态（2026-09-11）

模块化入口 `run_modular_program()` 已完成源码单元编译、磁盘 artifact 缓存、稳定符号与类型链接、ABI 校验以及 JIT 执行。CLI 默认使用该路径，提供 `--verbose`、`--no-cache` 和显式 `--legacy`；嵌入 API `run_program()` 保留。具体契约见 [模块 ABI](../../compiler/module-abi.md)。

- 跨模块测试覆盖嵌套 Tuple、同名不同类型、Class 工厂/借用/消费、依赖转发类型、effect 身份、Pending 返回和取消清理。
- 缓存测试覆盖独立编译批次命中、源码变化、传递依赖 ABI 变化、同源码不同模块身份和损坏恢复。
- 常量支持标量、String、嵌套 Tuple、常量 Struct、一元/二元表达式及传递导入；私有常量不能从外部访问。
- 泛型覆盖隐式/显式类型参数、标签参数、静态 trait 及关联类型约束、结构化类型、私有辅助函数、嵌套跨模块调用及 Pending 返回；实例缓存覆盖多调用方复用和模板私有依赖变化后的失效。
- 导出类型函数在定义模块中求值，支持生成类型、嵌套常量默认值和稳定实例身份；方法通过保留接收者所有权的 ABI wrapper 链接，支持调用方的自定义 Show。
- 71 项 CLI 用例通过，默认入口使用模块化路径，覆盖标准库、独立进程缓存命中、常量修改、损坏恢复、禁用缓存与依赖源码诊断；泛型边界回归已完成，effectful closure 字段和动态 trait 对象另行扩展。
- 最终验证（含原生句柄 effect 绑定）：967 项 library 测试通过（9 项原有忽略）、71 项 CLI 集成测试通过，另有 1 项文档测试忽略；`cargo fmt`、`cargo test --quiet` 和 `cargo clippy -- -D warnings` 通过。
- 磁盘缓存保存可重定位 MIR artifact，不保存 JIT 机器码。运行 `cargo run -- run examples/basics/hello.jk --verbose` 可查看缓存目录与命中状态；未变化时再次运行显示 `[cache] hit`。

## 入口与兼容性收尾记录

1. 常量：导入带类型的常量树，并将常量内容纳入依赖失效协议。
2. 泛型实例：在定义模块环境中实例化，使用稳定类型身份生成独立 artifact，支持跨调用方及跨进程复用。
3. CLI 与标准库：默认模块化编译，提供可观察磁盘缓存、`--no-cache` 和显式 `--legacy`；完成标准 effect、intrinsic 方法和文件相关类型兼容。
4. 泛型边界：补齐导出类型函数、生成类型身份、嵌套常量默认值、静态 trait/关联类型和方法所有权；统一泛型 Pending ABI，隔离依赖模块的 `main`，协议升级为 ABI v6。

后续接口完善：File 和网络句柄使用 `@intrinsic(effect = ...)` 在类级别绑定同名 operation；声明时校验签名和所有权，方法调用继承 operation 的 effect 与挂起属性。绑定及方法子集进入 ABI 指纹，序列化协议升级为 ABI v7。

## 实施前诊断（历史）

以下诊断和渐进式方案保留作为历史背景；实际实现以当前代码和上面的状态为准。

### 已完成
- ✅ `StableId` 和 `module_abi_hash`：基于包相对路径和 public 导出的确定性指纹
- ✅ `ModuleMetadata`：导出、依赖、签名、runtime 初始化和 drop glue
- ✅ `ModuleCacheKey`：包含源码、依赖 ABI、编译器版本、target、指针宽度和 features
- ✅ `ModuleCache`：元数据的持久化存储（`.jabi` 文件）
- ✅ `compilation_order()`：依赖优先的拓扑排序
- ✅ `ModuleSourceUnit`：前端验证单元，包含源码、元数据和 cache key

### 核心问题
**编译边界仍然是整体**：
- `Compiler::run_program()` 的路径是：完整源码 → `CoreProgram` → `MirProgram` → Cranelift → JIT
- `ModuleSourceUnit` 只做前端检查（parse + sema），不生成编译产物
- 缓存基础设施已实现但从未被调用
- 跨模块引用仍通过 `expand_module` 的字符串替换实现
- `MirFunctionId` 和类型索引是 `usize`，只在单个 `MirProgram` 内有效

**缺少关键组件**：
1. `ModuleArtifact`：模块编译产物结构
2. `compile_module()`：单模块编译入口
3. MIR 序列化：让 artifact 可以缓存
4. Linker：合并 artifacts 为 `MirProgram`

## 实施计划（渐进式）

### 阶段 1：定义编译产物和单模块编译（1-2 天）🔥

**目标**：让每个模块可以独立编译到 MIR，生成 `ModuleArtifact`。

1. **定义 `ModuleArtifact`**（`src/module.rs`）：
   ```rust
   pub struct ModuleArtifact {
       pub metadata: ModuleMetadata,
       pub mir_functions: Vec<MirFunction>,
       pub mir_types: Vec<Type>,  // 模块内部类型表
   }
   ```

2. **添加 `Compiler::compile_module()`**（`src/compiler.rs`）：
   ```rust
   pub fn compile_module(&self, unit: &ModuleSourceUnit) 
       -> Result<ModuleArtifact, Diagnostic>
   {
       // parse → sema → hir → mir（仅本模块）
       // 暂时不处理跨模块引用，保留占位符
   }
   ```

3. **实现 `mir::lower_module()`**（`src/mir/mod.rs`）：
   - 类似 `lower_program()`，但只降级单个模块的函数
   - 对导入符号生成占位符 `MirFunction`，标记为 `external`

4. **测试**：
   - 独立模块可以编译到 `ModuleArtifact`
   - 验证生成的 MIR functions 数量和签名正确

### 阶段 2：缓存编译产物（1-2 天）🔥

**目标**：让缓存真正工作，避免重复编译。

1. **序列化 MIR**（`src/mir/serde.rs` 新文件）：
   ```rust
   impl MirFunction {
       pub fn serialize(&self) -> Vec<u8>;
       pub fn deserialize(bytes: &[u8]) -> Result<Self, String>;
   }
   ```
   - 可以先用简单的 bincode 或 JSON
   - 后续优化为紧凑二进制格式

2. **扩展 `ModuleCache`**（`src/module.rs`）：
   ```rust
   impl ModuleCache {
       pub fn load_artifact(&self, key: &ModuleCacheKey) 
           -> Result<Option<ModuleArtifact>, String>;
       
       pub fn store_artifact(&self, key: &ModuleCacheKey, artifact: &ModuleArtifact) 
           -> Result<(), String>;
   }
   ```
   - 文件格式：`.jabi`（元数据）+ `.jmir`（MIR）或统一为一个文件

3. **添加 `Compiler::compile_with_cache()`**（`src/compiler.rs`）：
   ```rust
   pub fn compile_with_cache(&self, units: &[ModuleSourceUnit]) 
       -> Result<Vec<ModuleArtifact>, Diagnostic>
   {
       let cache = ModuleCache::new(".joky/cache");
       units.iter().map(|unit| {
           if let Some(cached) = cache.load_artifact(&unit.cache_key)? {
               return Ok(cached);
           }
           let artifact = self.compile_module(unit)?;
           cache.store_artifact(&unit.cache_key, &artifact)?;
           Ok(artifact)
       }).collect()
   }
   ```

4. **测试**：
   - 首次编译生成缓存文件
   - 再次编译命中缓存，不重新解析和降级
   - 修改源码后缓存失效
   - 修改依赖 ABI 后缓存失效

### 阶段 3：实现基础 linker（2-3 天）📋

**目标**：合并多个 `ModuleArtifact` 为单一 `MirProgram`，可以传给 Cranelift。

1. **定义 `Linker`**（`src/linker.rs` 新文件）：
   ```rust
   pub struct Linker {
       artifacts: Vec<ModuleArtifact>,
   }
   
   impl Linker {
       pub fn link(artifacts: Vec<ModuleArtifact>) 
           -> Result<MirProgram, String>
       {
           // 1. 验证 ABI 兼容性
           // 2. 收集所有 MirFunction，重新分配连续 ID
           // 3. 暂时保持字符串导入（复用 expand_module）
           // 4. 构建统一的 MirProgram
       }
   }
   ```

2. **处理跨模块引用**（第一版：简单策略）：
   - 保留当前的字符串替换导入逻辑
   - linker 合并时，将所有模块的 MIR functions 放入同一个 `Vec`
   - 函数调用仍用 `MirFunctionId(usize)`，但在合并后的统一空间

3. **更新编译流程**（`src/compiler.rs`）：
   ```rust
   pub fn run_modular_program(&self, graph: &ModuleGraph) 
       -> Result<Value, Diagnostic>
   {
       let units = graph.source_units("runtime", "target", 64, "jit")?;
       let artifacts = self.compile_with_cache(&units)?;
       let mir_program = Linker::link(artifacts)?;
       // 后续流程不变：MIR passes → Cranelift → JIT
   }
   ```

4. **测试**：
   - 多模块程序可以独立编译并链接
   - 跨模块函数调用正常工作
   - 端到端运行测试

### 阶段 4：符号解析和稳定引用（3-5 天）📋

**目标**：移除字符串导入，用稳定 `SymbolId` 解析跨模块引用。

1. **定义符号标识**（`src/module.rs`）：
   ```rust
   #[derive(Debug, Clone, PartialEq, Eq, Hash)]
   pub struct SymbolId {
       pub module: StableId,
       pub name: String,
   }
   ```

2. **扩展 `ModuleArtifact`**：
   ```rust
   pub struct ModuleArtifact {
       pub metadata: ModuleMetadata,
       pub mir_functions: Vec<MirFunction>,
       pub mir_types: Vec<Type>,
       pub imports: Vec<SymbolId>,        // 新增
       pub exports: HashMap<String, ExportedSymbol>,  // 新增
   }
   
   pub enum ExportedSymbol {
       Function { mir_index: usize, signature: String },
       Constant { mir_index: usize, type_: Type },
   }
   ```

3. **更新 Linker 的符号解析**：
   - 构建全局符号表：`HashMap<SymbolId, ResolvedSymbol>`
   - 将 `MirFunctionId` 从模块内索引转换为链接后的全局索引
   - 为 Cranelift 生成 `FuncRef` 映射

4. **移除字符串替换导入**：
   - 在 sema 阶段解析导入为 `SymbolId`
   - HIR 和 MIR 使用符号引用而非字符串

### 阶段 5：扩展类型和 ABI 协议（该阶段收尾）📋

**目标**：支持复杂类型跨模块传递，定义 retain/release 协议。

1. **扩展 ABI 支持**：
   - Tuple：展开为多个标量参数
   - Class：作为 opaque managed handle 传递
   - Effect/Pending：定义调用约定

2. **类型稳定化**：
   - 为 class 和 trait 生成 `TypeId`
   - 跨模块类型引用使用稳定 ID

3. **所有权协议**：
   - 明确函数边界的 retain/release 语义
   - 为跨模块类型生成 drop glue 元数据

## 设计约束

- **内部索引可重定位**：artifact 中保存模块局部索引，链接时完整重映射；Cranelift `FuncId` 和 JIT 地址不持久化，也不作为稳定身份。
- **渐进式实现**：先让简单类型（标量、String、函数）可以独立编译链接，再扩展复杂类型。
- **缓存键完整性**：必须包含源码 hash、依赖 ABI hash、编译器 ABI 版本、runtime 版本、target triple、指针宽度和 features。
- **兼容性优先**：CLI 默认模块化，保留 `run_program()` 嵌入 API 和显式 `--legacy`，错误不自动回退。
- **ABI 分层**：native ABI（Cranelift calling convention）与语言 ABI（跨模块符号协议）分离。

## 优先级说明

- 🔥 **立即行动**（1-2 天）：阻塞后续工作的基础组件
- 📋 **计划中**（3-7 天）：核心功能，但可以渐进实现
- 🔮 **未来扩展**（后续阶段）：复杂特性，等基础稳定后再做

## 阶段 5 实施记录

- 第一步：结构化解析跨模块函数 ABI，保留嵌套类型、参数名、借用和 effect 声明；MIR stub 使用已解析签名。为 class、struct、enum、trait 和 effect 定义稳定身份，定义变化进入 ABI 指纹，元数据版本升级为 3。导入上下文仅暴露 public 导出。
- 验证：编译器测试 289 项通过（8 项原有忽略）；模块相关测试 30 项通过。类型表合并、跨模块 drop glue、effect 身份重映射与 artifact 缓存仍在后续步骤完成。
- 第二步：链接器合并模块类型表，递归规范化结构类型并隔离不同模块的命名类型；重映射 MIR 类型、runtime intrinsic、continuation 和 handler 引用。符号先统一分配后解析，并校验导入签名、重复模块与链接后 MIR。新增同名类和嵌套 Tuple 的跨模块运行测试。
- 第三步：从依赖 artifact 导入 Class handle 类型及递归布局，支持工厂返回、`module.Type` 注解、借用参数、消费参数和嵌套 Class 的 drop 元数据。修复通过借用 Class 读取共享字段时遗漏 retain 的问题；增加重复借用、所有权转移、重叠借用拒绝和资源归零测试。语言 ABI 约定见 `docs/compiler/module-abi.md`。
- 第四步：按定义模块和 operation 名称链接 effect，支持限定名调用和 handler；传播导出函数的真实 Pending 属性。实现完整 `.jmir` 缓存、编译器 build/target 指纹、传递 ABI 失效和损坏恢复。补齐依赖转发的名义类型、取消调用后的结果保护和 null Class drop glue。
- 最终验证：945 项 library 测试通过（9 项原有忽略），69 项 CLI 集成测试通过；格式检查及 `cargo clippy -- -D warnings` 通过。后续 Clippy 清理修复 31 项写法及类型标注问题，并为 13 个参数较多的底层函数添加带原因的局部 `expect`，保留现有接口。
