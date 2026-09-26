# Joky VM 互操作验证项目

用最小化的方式验证 Joky 语言 AOT ↔ JIT 互操作的核心技术可行性。
不依赖 Joky 主仓库，单独成立，验证通过后合并回阶段 10。

## 环境要求

| 依赖 | 版本 |
|------|------|
| Rust | 1.96 及以上（当前 Cargo.lock 中 Cranelift 的要求） |
| cargo | 随 Rust 附带 |
| cc | 系统 C 编译器，用于 AOT 链接验证 |

macOS 上用 rustup 安装：

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

## 构建

```bash
cargo build
```

Cranelift 依赖统一约束为 `0.136` 系列，具体版本由 `Cargo.lock` 固定。
第一次构建会拉取依赖，之后会走缓存。

## 运行主程序

主程序依次演示解析、JIT、AOT、跨边界调用，以及阶段5的嵌套挂起与恢复：

```bash
cargo run
```

期望输出末尾：

```
=== Phase 5 demonstration completed ===
```

## 运行测试

```bash
cargo test
```

测试覆盖带行号的解析错误、类型注解、普通/CPS 函数的 Unit 语义、CPS 状态分派、
多次挂起、临时值与局部变量保存、完整宿主签名检查、取消、任务表回收，
以及加载失败和模块替换后的 continuation 生命周期。
AOT 测试会生成 `.o`，与 C 宿主链接并执行。另有真实 JIT → AOT → 宿主集成测试，
将生成的 AOT 动态库注册到 VM，使用同一个 Rust runtime 验证多次挂起、错误传播、
取消及重载/VM 销毁后的代码保活。动态库集成测试在 Unix 平台运行。

## 项目结构

```
src/
├── lib.rs               # Public embedding API and staticlib runtime
├── main.rs              # Phase 1–5 demonstrations
├── signature.rs         # Public function signatures
├── parser/
│   ├── mod.rs           # parse(source) -> Result<Module, Diagnostic>
│   ├── lexer.rs         # Fallible tokenization with line numbers
│   └── ast.rs           # Syntax and type annotations
├── compiler/
│   ├── mod.rs           # Shared compilation pipeline
│   ├── ir.rs            # Typed linear IR and frame planning
│   └── lower.rs         # Direct SSA and CPS state-machine emission
├── jit/mod.rs           # Immutable compiled exports and code ownership
├── aot/mod.rs           # Object emission through the shared compiler
├── runtime/
│   ├── scheduler.rs     # Cooperative tasks with monotonic IDs
│   ├── suspension.rs    # Continuation lifecycle and module identity
│   └── cps.rs           # Shared frame ABI and C runtime exports
└── vm/
    ├── mod.rs           # Public VmEngine export
    └── engine.rs        # Host registration, checked calls and module loading
```

## 验证场景进度

| 场景 | 描述 | 状态 |
|------|------|------|
| 1 | AOT 调用 VM 函数，传递基础类型 | 阶段4 |
| 2 | VM 回调 AOT 宿主函数 | 阶段4 |
| 3 | 挂起函数跨边界 continuation | 阶段5 ✅ |
| 4 | 并发任务跨边界（scope） | 阶段6 |
| 5 | 代码热替换 | 阶段7 |

## 阶段完成情况

- **阶段1** ✅ 解析器：词法分析 + AST + 递归下降解析
- **阶段2** ✅ JIT 编译器：Cranelift 0.136，支持 Int32/Bool 函数、四则运算、函数互调、let 绑定
- **阶段3** ✅ AOT 编译器：生成目标文件并链接执行
- **阶段4** ✅ 跨边界调用（AOT ↔ JIT，Rust 宿主接口）
- **阶段5** ✅ JIT/AOT CPS lowering、嵌套帧保存、单线程挂起/恢复与场景3验证
- **阶段6** 🔲 并发任务（scope）
- **阶段7** 🔲 模块热替换
- **阶段8** 🔲 测试与文档

## 编译流程与嵌入接口

JIT/AOT 共用 `AST → typed IR → Cranelift` 流程，语义检查只执行一套：
函数签名、调用参数、返回值及 `let` 类型注解都在生成机器码前验证。
普通函数将 IR 值映射到 SSA；可挂起函数将值保存到 CPS 帧。
Unit 参数在直接调用 ABI 中省略，在 CPS 参数槽位中保留位置。

VM 使用不可变编译产物保存导出入口、签名、帧大小及代码所有者，编译上下文在编译完成后释放。
`load_source` 成功后才替换当前模块；失败保留旧代码、签名和版本。
`function_signature` 返回独立副本，不能修改已编译函数的 ABI。

```rust
use joky_vm_interop::{vm::VmEngine, FunctionSignature, Type};

extern "C" fn double(value: i32) -> i32 { value * 2 }

let mut vm = VmEngine::new();
unsafe {
    vm.register_fn(
        "double",
        FunctionSignature::new(vec![Type::Int32], Type::Int32),
        double as *const u8,
    );
}
vm.load_source("extern fn double(x: Int32) -> Int32 fn f() -> Int32 { double(21) }")?;
assert_eq!(vm.call_int32_0("f")?, 42);
```

三个宿主注册接口都要求显式 `FunctionSignature`，并标记为 `unsafe`：
调用方保证入口地址、真实 ABI、代码生命周期和不跨边界展开栈；加载时验证声明与注册信息一致。
同名注册会替换后续编译使用的绑定，已编译模块继续使用原绑定。
JIT 编译器内部结构不再是公共 API，嵌入方使用 `vm::VmEngine`。

## 阶段5：使用方式与边界

源码中的 `@suspends` 函数采用统一入口：
`unsafe extern "C" fn(*mut CpsFrame) -> i32`。
返回 `0` 表示完成，`1` 表示仍在等待；其他值表示执行失败。
普通函数仍采用原来的直接传参 ABI，普通函数调用可挂起函数会报编译错误。

Rust 宿主通过 `register_suspending_fn` 注册非阻塞回调。
`call_async(name, &[i32])` 执行到第一个挂起点或完成；它当前支持 Int32 参数和
Int32/Unit 返回值。回调的参数保存在 `frame.slots`，返回值写入 `frame.result`，
自身恢复进度可保存在 `state` 和 `host_data`。Int32 使用槽位低 32 位，Bool 使用低 8 位。
回调返回 Pending 后，下一次恢复会再次调用同一回调，因此必须根据保存的状态继续执行。

```rust
let task = vm.call_async("compute", &[20])?;
let id = scheduler.suspend(task);
// Resume when ready; execution may suspend again at the next call.
scheduler.resume(id)?;
if scheduler.continuation(id)?.status() == ContinuationStatus::Completed {
    let task = scheduler.take_completed(id)?;
    let result = task.cps_result();
}
```

编译器将参数、局部值、尚未完成的表达式临时值保存到帧中；每个挂起调用拥有独立子帧。
恢复不会重新计算此前的参数或重复执行此前的副作用。`ContinuationStatus` 明确区分
`Pending`、`Completed`、`Failed(error)`；完成或失败后拒绝再次恢复。
`cancel` 丢弃帧，`take_completed` 只取走成功结果，失败任务通过 `take_failed` 取走。
任务表使用递增 `TaskId` 和有序映射，按 ID 顺序轮询；取走或取消会删除记录，旧 ID 不会复用。
`task_count()` 包括等待收取的完成/失败任务，`suspended_count()` 仅统计 Pending 任务。
普通任务通过 `spawn(closure)` 入队，`run_all()` 按入队顺序执行。
恢复错误会保留在任务状态中；即使使用 `resume_all`，也可按 ID 查询原始错误。任务持有对应编译模块，VM 被销毁或重新加载
后，旧任务仍可恢复；释放最后一个所有者时回收该版本的 JIT 内存。

AOT `.o` 使用相同协议，公开 C 头文件为 [`include/jk_cps.h`](include/jk_cps.h)。
`cargo build --lib` 生成 `target/debug/libjoky_vm_interop.a`，导出帧分配、释放、
状态/结果访问接口以及编译器使用的三个 `__jk_cps_*` 符号。
C 宿主使用不透明的 `JkCpsFrame`，不能自行定义包含 Rust `Vec` 的帧布局。
链接时还需平台所要求的 Rust native 系统库，可通过
`cargo rustc --lib -- --print native-static-libs` 查看。
[`examples/cps_runtime.c`](examples/cps_runtime.c) 提供可直接链接该静态库的 C 示例。

注册生成的 AOT 挂起函数时，先在 `write_object` 前保存
`AotCompiler::cps_frame_slots(name)` 返回的槽位数，再调用
`VmEngine::register_aot_suspending_fn(name, signature, entry, slot_count, owner)`。
`owner` 是保持 AOT 动态库及其依赖存活的 `Rc<dyn Any>`，生成的 JIT 模块会保留它。
完整帧大小包括参数、局部变量和临时值，不能仅按参数数量分配。
AOT 模块若调用其他生成模块的 CPS 导出，使用 `compile_with_import_layouts` 传入导入帧大小。
手写的静态宿主回调使用 `register_suspending_fn`，仅获得参数槽位，其他状态放入 `host_data`。

集成测试中的 C 桥接函数只把符号转发到 Rust runtime；帧分配和子帧管理均使用
同一套 runtime。主程序保留较轻量的 JIT → Rust 宿主演示，真实生成 AOT 的链路由测试验证。

每个 `AotCompiler` 只接受一次编译尝试，成功后才允许 `write_object`；失败后应创建新实例。

当前调度器为单线程协作式调度，事件就绪由调用方通知；演示使用模拟事件，不包含真实
定时器、I/O reactor 或多 worker。`scope` 和完整的 C VM API 仍未实现。
String 仅保留解析支持，字面量和函数签名在 JIT/AOT 编译时明确拒绝；
字符串内存所有权及跨进程 ABI 尚未定义。词法/语法错误返回带行号的 `Diagnostic`。
四则运算仍采用同优先级、左结合的最小语法规则，复杂表达式应显式加括号。模块版本保活已覆盖挂起任务，完整热替换场景留到阶段7。

## 代码规范

源码注释、标识符、诊断信息和测试名称统一使用英文；项目说明文档可使用中文。
详细规范见 [CONTRIBUTING.md](CONTRIBUTING.md)。提交前运行：

```bash
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --release
```

## 参考资料

- [Cranelift 文档](https://docs.rs/cranelift/)
- [Cranelift GitHub](https://github.com/bytecodealliance/wasmtime/tree/main/cranelift)
- [System V ABI](https://refspecs.linuxbase.org/elf/x86_64-abi-0.99.pdf)

## 许可证

MIT / Apache 2.0 双许可（与 Joky 主项目保持一致）
