# Joky 代码生成

> 本文档描述 Joky 如何使用 Cranelift 生成机器码。

## 目录

- [概述](#概述)
- [Cranelift 简介](#cranelift-简介)
- [代码生成流程](#代码生成流程)
- [类型映射](#类型映射)
- [表达式生成](#表达式生成)
- [运行时支持](#运行时支持)

## 概述

Joky 使用 [Cranelift](https://cranelift.dev/) 作为 JIT 编译后端。Cranelift 是一个快速、安全的代码生成器，广泛用于 WebAssembly 和其他编译器项目。

### 为什么选择 Cranelift？

- **快速编译**: JIT 场景下编译速度比 LLVM 快得多
- **简单 API**: 易于集成和使用
- **安全性**: 用 Rust 编写，内存安全
- **优化**: 提供足够的优化能力
- **JIT 支持**: 原生支持 JIT 编译

## Cranelift 简介

### 核心概念

**IR (Intermediate Representation)**:
Cranelift 使用自己的 IR，类似于 LLVM IR 但更简单。

**基本块 (Basic Block)**:
顺序执行的指令序列，以终止指令结束。

**值 (Value)**:
指令的结果，可以被其他指令使用。

**函数构建器 (FunctionBuilder)**:
构建 Cranelift 函数的 API。

### Cranelift 类型

```rust
// Cranelift 基本类型
types::I8      // 8-bit 整数
types::I16     // 16-bit 整数
types::I32     // 32-bit 整数
types::I64     // 64-bit 整数
types::F32     // 32-bit 浮点
types::F64     // 64-bit 浮点
```

## 代码生成流程

```
类型化 AST + TypeTable
    ↓
为每个函数创建 Cranelift Function
    ↓
使用 FunctionBuilder 生成指令
    ↓
Cranelift 优化
    ↓
JIT 编译为机器码
    ↓
链接运行时函数
    ↓
执行
```

### 步骤详解

#### 1. 初始化后端

```rust
pub struct CraneliftBackend {
    module: JITModule,                      // JIT 模块
    function_context: FunctionBuilderContext, // 函数构建上下文
    next_function: usize,                    // 函数计数器
    string_literals: Vec<Box<[u8]>>,        // 字符串字面量
}

impl CraneliftBackend {
    pub fn new() -> Result<Self, CodegenError> {
        // 1. 创建 ISA builder
        let mut flag_builder = settings::builder();
        flag_builder.set("opt_level", "speed")?;
        
        // 2. 创建 JIT builder
        let isa_builder = cranelift_native::builder()
            .map_err(|_| CodegenError::UnsupportedPlatform)?;
        let isa = isa_builder.finish(settings::Flags::new(flag_builder))?;
        
        // 3. 创建 JIT module
        let mut builder = JITBuilder::with_isa(isa, default_libcall_names());
        builder.symbol(PRINTLN_SYMBOL, jk_println as *const u8);
        let module = JITModule::new(builder);
        
        Ok(Self {
            module,
            function_context: FunctionBuilderContext::new(),
            next_function: 0,
            string_literals: Vec::new(),
        })
    }
}
```

#### 2. 编译函数

```rust
pub fn compile(&mut self, program: &Program, types: &TypeTable) -> Result<*const u8, Diagnostic> {
    // 1. 声明 runtime 函数
    let println_id = self.declare_println()?;
    
    // 2. 为每个函数生成代码
    for function in &program.functions {
        self.compile_function(function, types, println_id)?;
    }
    
    // 3. 完成编译
    self.module.finalize_definitions()?;
    
    // 4. 获取函数指针
    let main_id = /* ... */;
    let code = self.module.get_finalized_function(main_id);
    
    Ok(code)
}
```

#### 3. 生成函数体

```rust
fn compile_function(
    &mut self,
    function: &Function,
    types: &TypeTable,
    println_id: FuncId,
) -> Result<(), Diagnostic> {
    // 1. 创建 Cranelift 函数
    let mut context = self.module.make_context();
    context.func.signature = Signature { /* ... */ };
    
    // 2. 创建 FunctionBuilder
    let mut builder = FunctionBuilder::new(&mut context.func, &mut self.function_context);
    
    // 3. 创建入口块
    let entry_block = builder.create_block();
    builder.append_block_params_for_function_params(entry_block);
    builder.switch_to_block(entry_block);
    builder.seal_block(entry_block);
    
    // 4. 生成函数体
    self.generate_expression(&function.body, types, &mut builder, println_id)?;
    
    // 5. 添加返回指令
    builder.ins().return_(&[]);
    
    // 6. 完成函数
    builder.finalize();
    
    // 7. 定义函数
    let function_id = self.module.declare_function(/* ... */)?;
    self.module.define_function(function_id, &mut context)?;
    
    Ok(())
}
```

## 类型映射

### Joky 类型 → Cranelift 类型

```rust
fn to_cranelift_type(ty: Type) -> cranelift_codegen::ir::Type {
    match ty {
        Type::I8 => types::I8,
        Type::I16 => types::I16,
        Type::I32 => types::I32,
        Type::I64 => types::I64,
        Type::U8 => types::I8,   // 无符号用有符号表示
        Type::U16 => types::I16,
        Type::U32 => types::I32,
        Type::U64 => types::I64,
        Type::F32 => types::F32,
        Type::F64 => types::F64,
        Type::Bool => types::I8,  // Bool 用 i8 表示
        Type::Unit => types::I32, // Unit 返回 dummy 值
        Type::String => /* 指针类型 */,
    }
}
```

## 表达式生成

### 字面量

```rust
fn generate_expression(
    &mut self,
    expr: &Expr,
    types: &TypeTable,
    builder: &mut FunctionBuilder,
    println_id: FuncRef,
) -> Result<Value, Diagnostic> {
    match &expr.kind {
        ExprKind::Integer(value) => {
            let ty = to_cranelift_type(types.get(expr));
            let value = match ty {
                types::I8 => builder.ins().iconst(ty, *value as i8 as i64),
                types::I16 => builder.ins().iconst(ty, *value as i16 as i64),
                types::I32 => builder.ins().iconst(ty, *value as i32 as i64),
                types::I64 => builder.ins().iconst(ty, *value as i64),
                // ...
            };
            Ok(value)
        }
        
        ExprKind::Float(value) => {
            let ty = to_cranelift_type(types.get(expr));
            let value = match ty {
                types::F32 => builder.ins().f32const(*value as f32),
                types::F64 => builder.ins().f64const(*value),
                // ...
            };
            Ok(value)
        }
        
        // ... 其他表达式
    }
}
```

### 二元运算

```rust
ExprKind::Binary { op, left, right } => {
    let left_value = self.generate_expression(left, types, builder, println_id)?;
    let right_value = self.generate_expression(right, types, builder, println_id)?;
    
    let ty = types.get(expr);
    let result = match op {
        BinaryOp::Add => {
            if ty.is_integer() {
                builder.ins().iadd(left_value, right_value)
            } else {
                builder.ins().fadd(left_value, right_value)
            }
        }
        BinaryOp::Subtract => {
            if ty.is_integer() {
                builder.ins().isub(left_value, right_value)
            } else {
                builder.ins().fsub(left_value, right_value)
            }
        }
        BinaryOp::Multiply => {
            if ty.is_integer() {
                builder.ins().imul(left_value, right_value)
            } else {
                builder.ins().fmul(left_value, right_value)
            }
        }
        BinaryOp::Divide => {
            if ty.is_signed_integer() {
                builder.ins().sdiv(left_value, right_value)
            } else if ty.is_integer() {
                builder.ins().udiv(left_value, right_value)
            } else {
                builder.ins().fdiv(left_value, right_value)
            }
        }
        // 比较运算
        BinaryOp::Equal => {
            if ty.is_integer() {
                builder.ins().icmp(IntCC::Equal, left_value, right_value)
            } else {
                builder.ins().fcmp(FloatCC::Equal, left_value, right_value)
            }
        }
        // ...
    };
    
    Ok(result)
}
```

### 控制流

#### if 表达式

```rust
ExprKind::If { condition, then_branch, else_branch } => {
    // 1. 生成条件
    let condition_value = self.generate_expression(condition, types, builder, println_id)?;
    
    // 2. 创建分支块
    let then_block = builder.create_block();
    let else_block = builder.create_block();
    let merge_block = builder.create_block();
    
    // 3. 添加块参数（用于 phi 节点）
    let result_type = to_cranelift_type(types.get(expr));
    builder.append_block_param(merge_block, result_type);
    
    // 4. 条件跳转
    builder.ins().brif(condition_value, then_block, &[], else_block, &[]);
    
    // 5. 生成 then 分支
    builder.switch_to_block(then_block);
    builder.seal_block(then_block);
    let then_value = self.generate_expression(then_branch, types, builder, println_id)?;
    builder.ins().jump(merge_block, &[then_value]);
    
    // 6. 生成 else 分支
    builder.switch_to_block(else_block);
    builder.seal_block(else_block);
    let else_value = self.generate_expression(else_branch, types, builder, println_id)?;
    builder.ins().jump(merge_block, &[else_value]);
    
    // 7. 合并块
    builder.switch_to_block(merge_block);
    builder.seal_block(merge_block);
    let result = builder.block_params(merge_block)[0];
    
    Ok(result)
}
```

#### while 循环

```rust
ExprKind::While { condition, body } => {
    // 1. 创建循环块
    let loop_header = builder.create_block();
    let loop_body = builder.create_block();
    let loop_exit = builder.create_block();
    
    // 2. 跳转到循环头
    builder.ins().jump(loop_header, &[]);
    
    // 3. 循环头：检查条件
    builder.switch_to_block(loop_header);
    builder.seal_block(loop_header);
    let condition_value = self.generate_expression(condition, types, builder, println_id)?;
    builder.ins().brif(condition_value, loop_body, &[], loop_exit, &[]);
    
    // 4. 循环体
    builder.switch_to_block(loop_body);
    self.generate_expression(body, types, builder, println_id)?;
    builder.ins().jump(loop_header, &[]);
    builder.seal_block(loop_body);
    
    // 5. 循环退出
    builder.switch_to_block(loop_exit);
    builder.seal_block(loop_exit);
    
    // while 返回 Unit
    let unit_value = builder.ins().iconst(types::I32, 0);
    Ok(unit_value)
}
```

### 变量绑定

```rust
// 使用 Cranelift 的变量机制
let var = Variable::new(self.next_variable);
self.next_variable += 1;

// let x = 42
let value = self.generate_expression(value_expr, types, builder, println_id)?;
builder.declare_var(var, to_cranelift_type(value_type));
builder.def_var(var, value);

// 使用变量
let value = builder.use_var(var);
```

## 运行时支持

### println 函数

```rust
// Rust 端实现
#[no_mangle]
pub extern "C" fn jk_println(data: *const u8, len: *const u8) {
    let len = len as usize;
    let data = unsafe { std::slice::from_raw_parts(data, len) };
    let string = std::str::from_utf8(data).unwrap();
    println!("{}", string);
}

// Joky 端调用
fn main() {
    println("Hello, World!")
}

// 生成的代码
ExprKind::Call { callee, arguments } => {
    // 1. 获取字符串数据
    let string_expr = &arguments[0];
    let ExprKind::String(string) = &string_expr.kind else { /* ... */ };
    
    // 2. 存储字符串字面量
    let string_bytes = string.as_bytes().to_vec().into_boxed_slice();
    let string_ptr = string_bytes.as_ptr();
    let string_len = string_bytes.len();
    self.string_literals.push(string_bytes);
    
    // 3. 创建指针常量
    let ptr = builder.ins().iconst(pointer_type, string_ptr as i64);
    let len = builder.ins().iconst(pointer_type, string_len as i64);
    
    // 4. 调用 println
    let call = builder.ins().call(println_ref, &[ptr, len]);
    let result = builder.inst_results(call)[0];
    
    Ok(result)
}
```

## 优化

### Cranelift 自动优化

Cranelift 会自动进行一些优化：
- 常量折叠
- 死代码消除
- 公共子表达式消除
- 寄存器分配

### 优化级别

```rust
// 设置优化级别
flag_builder.set("opt_level", "speed")?;  // speed, speed_and_size, none
```

## 调试

### 查看生成的 IR

```rust
// 在生成代码时打印
println!("{}", context.func.display());
```

### 示例输出

```
function main() {
block0:
    v0 = iconst.i32 42
    v1 = iconst.i32 100
    v2 = iadd v0, v1
    return v2
}
```

## 性能考虑

### JIT 编译时间

- 小型函数: < 1ms
- 中型函数: 1-10ms
- 大型函数: 10-100ms

### 运行时性能

生成的机器码性能接近手写汇编：
- 简单算术: 与 C 相当
- 控制流: 与 C 相当
- 函数调用: 轻微开销

## 限制和未来改进

### 当前限制

- 没有垃圾回收
- 没有通用的用户可控堆分配
- 闭包捕获环境使用托管堆对象，并通过统一函数值 ABI 传递
- 简单的字符串处理

### 未来改进

- 添加内联优化
- 支持 SIMD
- 添加调试信息
- 支持多个优化 pass

## 相关文档

- [架构文档](architecture.md)
- [类型检查](type-checking.md)
- [Cranelift 官方文档](https://cranelift.dev/)

## 参考资料

- [Cranelift Book](https://cranelift.readthedocs.io/)
- [Cranelift API Docs](https://docs.rs/cranelift-codegen/)
- [JIT Compilation](https://en.wikipedia.org/wiki/Just-in-time_compilation)
