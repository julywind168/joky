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

第一次构建会拉取 Cranelift（约 100 MB），之后会走缓存。

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

当前有 30 个测试，覆盖解析、基础 runtime、CPS 状态分派、多次挂起、嵌套调用、
临时值与局部变量保存、宿主 ABI 检查、取消，以及模块替换后的 continuation 生命周期。
AOT 测试会生成 `.o`，与 C 宿主链接并执行。另有真实 JIT → AOT → 宿主集成测试，
将生成的 AOT 动态库注册到 VM，使用同一个 Rust runtime 验证多次挂起、错误传播、
取消及重载/VM 销毁后的代码保活。动态库集成测试在 Unix 平台运行。

## 项目结构

```
src/
├── lib.rs               # Rust 嵌入接口与 staticlib runtime
├── main.rs              # 阶段1–5演示驱动
├── parser/
│   ├── mod.rs           # 递归下降解析器，暴露 parse(src) -> Module
│   ├── lexer.rs         # 词法分析器
│   └── ast.rs           # AST 节点定义
├── jit/
│   ├── compiler.rs      # Cranelift JIT 编译器
│   ├── cps.rs           # JIT/AOT 共用的帧规划与 CPS lowering
│   └── module.rs        # 版本化入口表（阶段4）
├── runtime/
│   ├── scheduler.rs     # 任务调度器（阶段6扩展为多worker）
│   ├── suspension.rs    # Continuation 生命周期
│   ├── cps.rs           # CPS 帧 ABI 与子帧管理
│   └── allocator.rs     # 分配器接口（阶段5）
├── aot/mod.rs           # AOT 编译器（阶段3）
└── vm/
    ├── engine.rs        # VM 引擎，load_source + call_int32_*
    └── ffi.rs           # C runtime API 说明
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
`VmEngine::register_aot_suspending_fn(name, entry, slot_count, owner)`。
`owner` 是保持 AOT 动态库及其依赖存活的 `Rc<dyn Any>`，生成的 JIT 模块会保留它。
完整帧大小包括参数、局部变量和临时值，不能仅按参数数量分配。
AOT 模块若调用其他生成模块的 CPS 导出，使用 `compile_with_import_layouts` 传入导入帧大小。
手写的静态宿主回调使用 `register_suspending_fn`，仅获得参数槽位，其他状态放入 `host_data`。

集成测试中的 C 桥接函数只把符号转发到 Rust runtime；帧分配和子帧管理均使用
同一套 runtime。主程序保留较轻量的 JIT → Rust 宿主演示，真实生成 AOT 的链路由测试验证。

当前调度器为单线程协作式调度，事件就绪由调用方通知；演示使用模拟事件，不包含真实
定时器、I/O reactor 或多 worker。`scope`、字符串插值 lowering、完整的 C VM API
仍未实现。模块版本保活已覆盖挂起任务，完整热替换场景留到阶段7。

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
