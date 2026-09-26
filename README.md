# Joky VM 互操作验证项目

用最小化的方式验证 Joky 语言 AOT ↔ JIT 互操作的核心技术可行性。
不依赖 Joky 主仓库，单独成立，验证通过后合并回阶段 10。

## 环境要求

| 依赖 | 版本 |
|------|------|
| Rust | 1.75 及以上（稳定版） |
| cargo | 随 Rust 附带 |

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

主程序依次演示阶段1（AST 输出）和阶段2（JIT 编译并执行）：

```bash
cargo run
```

期望输出末尾：

```
=== 阶段2验证完成 ✓ ===
```

## 运行测试

```bash
cargo test
```

当前有 8 个单元测试，覆盖词法分析器、解析器和调度器：

```
test parser::lexer::tests::词法化函数定义 ... ok
test parser::lexer::tests::词法化字符串字面量 ... ok
test parser::lexer::tests::词法化extern声明 ... ok
test parser::tests::解析简单加法函数 ... ok
test parser::tests::解析extern函数 ... ok
test parser::tests::解析可挂起函数 ... ok
test parser::tests::解析多个顶层定义 ... ok
test runtime::scheduler::tests::串行执行任务 ... ok
```

## 项目结构

```
src/
├── main.rs              # 演示驱动：解析 + JIT 执行验证
├── parser/
│   ├── mod.rs           # 递归下降解析器，暴露 parse(src) -> Module
│   ├── lexer.rs         # 词法分析器
│   └── ast.rs           # AST 节点定义
├── jit/
│   ├── compiler.rs      # Cranelift JIT 编译器（阶段2）
│   └── module.rs        # 版本化入口表（阶段4）
├── runtime/
│   ├── scheduler.rs     # 任务调度器（阶段6扩展为多worker）
│   ├── suspension.rs    # Continuation 数据结构（阶段5）
│   └── allocator.rs     # 分配器接口（阶段5）
├── aot/mod.rs           # AOT 编译器（阶段3）
└── vm/
    ├── engine.rs        # VM 引擎，load_source + call_int32_*
    └── ffi.rs           # C API（阶段4）
```

## 验证场景进度

| 场景 | 描述 | 状态 |
|------|------|------|
| 1 | AOT 调用 VM 函数，传递基础类型 | 阶段4 |
| 2 | VM 回调 AOT 宿主函数 | 阶段4 |
| 3 | 挂起函数跨边界 continuation | 阶段5 |
| 4 | 并发任务跨边界（scope） | 阶段6 |
| 5 | 代码热替换 | 阶段7 |

## 阶段完成情况

- **阶段1** ✅ 解析器：词法分析 + AST + 递归下降解析
- **阶段2** ✅ JIT 编译器：Cranelift 0.136，支持 Int32/Bool 函数、四则运算、函数互调、let 绑定
- **阶段3** 🔲 AOT 编译器
- **阶段4** 🔲 跨边界调用（AOT ↔ JIT）
- **阶段5** 🔲 挂起与 Continuation
- **阶段6** 🔲 并发任务（scope）
- **阶段7** 🔲 模块热替换
- **阶段8** 🔲 测试与文档

## 参考资料

- [Cranelift 文档](https://docs.rs/cranelift/)
- [Cranelift GitHub](https://github.com/bytecodealliance/wasmtime/tree/main/cranelift)
- [System V ABI](https://refspecs.linuxbase.org/elf/x86_64-abi-0.99.pdf)

## 许可证

MIT / Apache 2.0 双许可（与 Joky 主项目保持一致）
