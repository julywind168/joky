# Joky VM 互操作验证项目

> **独立验证项目**：用于验证 AOT 与 VM 互操作的核心机制。
> 
> 本项目在独立仓库开发，不依赖 Joky 主仓库的完整编译器实现。验证通过后，核心机制将合并回主仓库的阶段 10（嵌入式执行、脚本热更新与宿主集成）。

## 项目背景

Joky 计划在阶段 10 支持 AOT 宿主加载可重载模块（通过 `joky/vm` 标准库），实现类似 Lua 的嵌入式执行能力。为了降低风险，需要先用一个**最小化项目**验证核心技术可行性：

1. AOT 代码与 JIT 代码如何交换数据（传参）
2. 挂起函数跨边界时的 continuation 处理
3. 共享 runtime 的任务调度
4. 模块热替换的版本管理

本项目使用极简的语法集和 runtime，快速验证这些机制，避免被完整语言特性拖累进度。

## 项目目标

✅ **验证**以下技术可行性：
- AOT ↔ JIT 双向函数调用
- 基础类型（Int32、String、Bool）的参数传递
- 挂起函数跨边界的 continuation 保存与恢复
- 并发任务在共享调度器上执行
- 运行时替换 JIT 代码，新旧版本并存

❌ **不做**：
- 完整的 Joky 语法实现
- Effect 系统、Cown、Region、泛型、闭包
- Pulley 解释器（首轮只验证 JIT）
- 复杂类型传递（class、trait object）
- 状态迁移（布局变化）

## 最小语言定义

为了聚焦核心机制，定义一个**极简的 Joky 子集**（称为 `Joky-Lite`）：

### 支持的语法

```joky
// 1. 函数定义
fn add(x: Int32, y: Int32) -> Int32 {
    x + y
}

// 2. 挂起函数
fn delayed_add(x: Int32, y: Int32) @suspends -> Int32 {
    sleep_ms(100)  // 内置挂起函数，模拟异步操作
    x + y
}

// 3. 并发任务
fn parallel_work() {
    scope {
        | task_a()
        | task_b()
    }
}

// 4. 外部函数声明（宿主提供）
extern fn host_multiply(x: Int32, y: Int32) -> Int32

// 5. 变量与字符串插值
fn greet(name: String) -> String {
    let msg = "Hello, {name}!"
    msg
}
```

### 支持的类型

- `Int32`：32 位有符号整数
- `String`：不可变字符串（胖指针：ptr + len）
- `Bool`：布尔值（`true` / `false`）
- `Unit`：单元类型（函数无返回值时）

### 不支持

- ❌ Effect 系统（`eff` / `do ... with`）
- ❌ Cown / `when` / `region`
- ❌ 类（`class`）、枚举（`enum`）
- ❌ Trait、泛型、闭包
- ❌ `if` / `match` / 循环（验证场景中不需要）
- ❌ 容器（`List` / `Map`）

## 验证场景

### 场景 1：基础数据交换

**目标**：验证 AOT 调用 VM 函数，传递基础类型参数。

```joky
// === 文件：host.jk（AOT 编译） ===
fn main() {
    let vm = vm_create()
    vm_load(vm, "script.jk")
    
    // AOT 调用 VM 函数
    let result: Int32 = vm_call_int32(vm, "add", 10, 20)
    println("结果: {result}")  // 期望输出 30
    
    let msg: String = vm_call_string(vm, "greet", "Joky")
    println(msg)  // 期望输出 "Hello, Joky!"
}

// 内置 VM API（AOT 宿主提供）
extern fn vm_create() -> VmHandle
extern fn vm_load(vm: VmHandle, path: String)
extern fn vm_call_int32(vm: VmHandle, fn_name: String, arg1: Int32, arg2: Int32) -> Int32
extern fn vm_call_string(vm: VmHandle, fn_name: String, arg1: String) -> String
```

```joky
// === 文件：script.jk（JIT 执行） ===
fn add(x: Int32, y: Int32) -> Int32 {
    x + y
}

fn greet(name: String) -> String {
    "Hello, {name}!"
}
```

**验收**：
- ✅ AOT 成功调用 VM 函数
- ✅ 参数和返回值正确传递
- ✅ 无内存泄漏

---

### 场景 2：VM 回调 AOT

**目标**：验证 VM 代码调用 AOT 注册的宿主函数。

```joky
// === host.jk（AOT） ===
fn host_multiply(x: Int32, y: Int32) -> Int32 {
    println("[AOT] multiply: {x} * {y}")
    x * y
}

fn main() {
    let vm = vm_create()
    vm_register_fn(vm, "host_multiply", host_multiply)  // 注册宿主函数
    vm_load(vm, "script.jk")
    
    let result: Int32 = vm_call_int32(vm, "compute", 5)
    println("最终结果: {result}")  // 期望输出 50
}

extern fn vm_register_fn(vm: VmHandle, name: String, fn_ptr: FnPtr)
```

```joky
// === script.jk（JIT） ===
extern fn host_multiply(x: Int32, y: Int32) -> Int32

fn compute(x: Int32) -> Int32 {
    let a = x + 5          // 10
    host_multiply(a, 5)    // 调用 AOT：10 * 5 = 50
}
```

**验收**：
- ✅ VM 成功调用 AOT 宿主函数
- ✅ 调用栈正确（AOT → VM → AOT）
- ✅ 返回值正确传递回 VM

---

### 场景 3：挂起函数跨边界

**目标**：验证挂起函数跨边界时的 continuation 处理。

```joky
// === host.jk（AOT） ===
fn host_sleep(ms: Int32) @suspends {
    builtin_sleep(ms)  // 内置挂起函数
    println("[AOT] sleep 完成")
}

fn main() {
    let vm = vm_create()
    vm_register_suspending_fn(vm, "host_sleep", host_sleep)
    vm_load(vm, "script.jk")
    
    // 调用可挂起的 VM 函数
    vm_call_async(vm, "delayed_compute")
    println("[AOT] 任务已启动，继续执行")
}

extern fn builtin_sleep(ms: Int32) @suspends
extern fn vm_register_suspending_fn(vm: VmHandle, name: String, fn_ptr: FnPtr)
extern fn vm_call_async(vm: VmHandle, fn_name: String)
```

```joky
// === script.jk（JIT） ===
extern fn host_sleep(ms: Int32) @suspends

fn delayed_compute() @suspends {
    println("[VM] 开始计算")
    host_sleep(1000)       // 挂起点：调用 AOT 挂起函数
    println("[VM] 计算完成")
}
```

**验收**：
- ✅ VM 挂起函数成功调用 AOT 挂起函数
- ✅ 挂起时 continuation 正确保存（包含 VM 和 AOT 栈帧）
- ✅ 恢复后从正确位置继续执行
- ✅ 主线程不阻塞

---

### 场景 4：并发任务跨边界

**目标**：验证 `scope` 任务调度在共享 runtime 上工作。

```joky
// === host.jk（AOT） ===
fn host_work(id: Int32) -> Int32 {
    println("[AOT] 任务 {id} 执行")
    id * 10
}

fn main() {
    let vm = vm_create()
    vm_register_fn(vm, "host_work", host_work)
    vm_load(vm, "script.jk")
    
    vm_call(vm, "parallel_test")
}
```

```joky
// === script.jk（JIT） ===
extern fn host_work(id: Int32) -> Int32

fn parallel_test() {
    scope {
        | {
            let r1 = host_work(1)
            println("[VM] 任务 1 结果: {r1}")
        }
        | {
            let r2 = host_work(2)
            println("[VM] 任务 2 结果: {r2}")
        }
    }
    println("[VM] 所有任务完成")
}
```

**验收**：
- ✅ VM 并发任务能调用 AOT 函数
- ✅ 任务在共享调度器上正确调度
- ✅ 所有任务完成后才继续

---

### 场景 5：代码热替换

**目标**：验证运行时替换 VM 模块，新旧版本并存。

```joky
// === host.jk（AOT） ===
fn main() {
    let vm = vm_create()
    vm_load(vm, "script_v1.jk")
    
    // 第一次调用
    let r1: Int32 = vm_call_int32(vm, "compute", 10)
    println("v1 结果: {r1}")  // 期望输出 20
    
    // 热替换为新版本
    vm_reload(vm, "script_v2.jk")
    
    // 第二次调用使用新版本
    let r2: Int32 = vm_call_int32(vm, "compute", 10)
    println("v2 结果: {r2}")  // 期望输出 100
}

extern fn vm_reload(vm: VmHandle, path: String)
```

```joky
// === script_v1.jk ===
fn compute(x: Int32) -> Int32 {
    x + 10  // 旧逻辑
}
```

```joky
// === script_v2.jk ===
fn compute(x: Int32) -> Int32 {
    x * 10  // 新逻辑
}
```

**验收**：
- ✅ 新版本加载后，新调用使用新代码
- ✅ 旧版本模块能正常关闭
- ✅ 无内存泄漏

## 技术架构

### 1. 项目结构

```
joky-vm-interop/
├── README.md
├── Cargo.toml
├── src/
│   ├── main.rs              # 测试驱动程序
│   ├── runtime/             # 极简 runtime
│   │   ├── mod.rs
│   │   ├── scheduler.rs     # 任务调度器
│   │   ├── suspension.rs    # 挂起与 continuation
│   │   └── allocator.rs     # 简单分配器
│   ├── jit/                 # JIT 编译器（基于 Cranelift）
│   │   ├── mod.rs
│   │   ├── compiler.rs      # Joky-Lite → Cranelift IR
│   │   └── module.rs        # 模块管理
│   ├── aot/                 # AOT 编译器（复用 JIT 前端）
│   │   └── mod.rs
│   ├── parser/              # Joky-Lite 语法解析器
│   │   ├── mod.rs
│   │   ├── lexer.rs
│   │   └── ast.rs
│   └── vm/                  # VM API 实现
│       ├── mod.rs
│       ├── engine.rs
│       └── ffi.rs           # C API
├── tests/
│   ├── scenario_1_data_exchange.rs
│   ├── scenario_2_callback.rs
│   ├── scenario_3_suspension.rs
│   ├── scenario_4_concurrency.rs
│   └── scenario_5_reload.rs
└── examples/
    ├── host.jk              # AOT 宿主示例
    ├── script.jk            # VM 脚本示例
    ├── script_v1.jk
    └── script_v2.jk
```

### 2. Runtime 设计

#### 2.1 调度器

```rust
// runtime/scheduler.rs
pub struct Scheduler {
    workers: Vec<Worker>,
    global_queue: WorkQueue,
}

pub struct Task {
    entry: fn(),
    stack: Option<Continuation>,
}

impl Scheduler {
    pub fn spawn(&self, task: Task) { /* ... */ }
    pub fn suspend(&self, continuation: Continuation) { /* ... */ }
    pub fn resume(&self, continuation: Continuation) { /* ... */ }
}
```

#### 2.2 Continuation 表示

```rust
// runtime/suspension.rs
pub struct Continuation {
    pub resume_ptr: ResumePtr,    // 恢复入口
    pub frame_data: Vec<u8>,       // 保存的栈帧
    pub module_version: ModuleId,  // 归属模块版本
}

pub enum ResumePtr {
    Native(fn()),              // AOT 函数指针
    Jit(*const u8),            // JIT 代码入口
}
```

### 3. JIT 编译器（Cranelift）

```rust
// jit/compiler.rs
use cranelift::prelude::*;

pub struct JitCompiler {
    builder_context: FunctionBuilderContext,
    ctx: codegen::Context,
    module: JITModule,
}

impl JitCompiler {
    pub fn compile_function(&mut self, ast: &FunctionDef) -> *const u8 {
        // 1. 将 AST 转换为 Cranelift IR
        let mut builder = FunctionBuilder::new(&mut self.ctx.func, &mut self.builder_context);
        
        // 2. 生成函数体 IR
        self.lower_function_body(&mut builder, &ast.body);
        
        // 3. 编译为本机代码
        let id = self.module.declare_function(&ast.name, Linkage::Export, &self.ctx.func.signature)?;
        self.module.define_function(id, &mut self.ctx)?;
        
        // 4. 返回函数指针
        self.module.get_finalized_function(id)
    }
}
```

### 4. 版本化入口表

```rust
// jit/module.rs
pub struct ModuleEntryTable {
    entries: HashMap<String, Entry>,
}

pub struct Entry {
    pub fn_ptr: *const u8,      // 当前版本的函数指针
    pub version: ModuleVersion,
}

impl ModuleEntryTable {
    pub fn call(&self, fn_name: &str, args: &[Value]) -> Value {
        let entry = self.entries.get(fn_name).unwrap();
        // 通过函数指针调用（可能是旧版本或新版本）
        unsafe { call_fn_ptr(entry.fn_ptr, args) }
    }
    
    pub fn reload(&mut self, fn_name: &str, new_ptr: *const u8, new_version: ModuleVersion) {
        self.entries.insert(fn_name.to_string(), Entry {
            fn_ptr: new_ptr,
            version: new_version,
        });
    }
}
```

### 5. C API

```c
// vm/ffi.rs 生成的 C 头文件
typedef struct JkVmHandle JkVmHandle;

JkVmHandle* jk_vm_create();
void jk_vm_destroy(JkVmHandle* vm);
void jk_vm_load(JkVmHandle* vm, const char* source_path);
void jk_vm_reload(JkVmHandle* vm, const char* source_path);

int32_t jk_vm_call_int32(JkVmHandle* vm, const char* fn_name, int32_t arg1, int32_t arg2);
const char* jk_vm_call_string(JkVmHandle* vm, const char* fn_name, const char* arg1);

void jk_vm_register_fn(JkVmHandle* vm, const char* name, void* fn_ptr);
```

## 实施步骤

### 阶段 1：基础设施（2 周）

- [ ] 搭建项目结构
- [ ] 实现 Joky-Lite 词法分析器和解析器
- [ ] 实现简单的 AST
- [ ] 实现极简 runtime（无调度，单线程执行）

**产出**：能解析 `fn add(x: Int32, y: Int32) -> Int32 { x + y }` 并输出 AST

---

### 阶段 2：JIT 编译器（2 周）

- [ ] 集成 Cranelift（使用 crates.io 最新稳定版）
- [ ] 实现 AST → Cranelift IR 的 lowering
- [ ] 支持函数定义、调用、基础类型
- [ ] 生成本机代码并执行

**产出**：能 JIT 编译并执行 `add(10, 20)`，返回 `30`

---

### 阶段 3：AOT 编译器（1 周）

- [ ] 复用 JIT 前端，输出目标文件（.o）
- [ ] 实现链接步骤，生成可执行文件
- [ ] 验证 AOT 和 JIT 的调用约定一致

**产出**：能 AOT 编译 `host.jk` 为可执行文件

---

### 阶段 4：跨边界调用（2 周）

- [ ] 实现版本化入口表
- [ ] AOT 调用 VM 函数（通过入口表）
- [ ] VM 调用 AOT 函数（通过注册的函数指针）
- [ ] 验证场景 1 和场景 2

**产出**：场景 1 和 2 通过

---

### 阶段 5：挂起与 Continuation（2 周）

- [x] 实现 `@suspends` 函数的 lowering
- [x] 实现 Continuation 的保存与恢复
- [x] 集成 runtime 调度器
- [x] 验证场景 3

**产出**：场景 3 通过

---

### 阶段 6：并发任务（1 周）

- [ ] 实现 `scope` 的 lowering
- [ ] 实现多 worker 调度器
- [ ] 验证场景 4

**产出**：场景 4 通过

---

### 阶段 7：模块热替换（2 周）

- [ ] 实现模块版本管理
- [ ] 实现入口表切换
- [ ] 实现旧模块回收
- [ ] 验证场景 5

**产出**：场景 5 通过

---

### 阶段 8：测试与文档（1 周）

- [ ] 性能测试（跨边界调用开销）
- [ ] 稳定性测试（大量调用）
- [ ] 内存泄漏检测（Valgrind）
- [ ] 编写最终报告

**产出**：测试报告 + 技术文档

---

**总计**：13 周（约 3 个月）

## 关键设计决策

### 1. 为什么选择 Cranelift？

- ✅ 支持 JIT 编译，无需手写代码生成器
- ✅ 使用 System V ABI（Linux/macOS）和 Windows x64 ABI，与 AOT 一致
- ✅ 成熟度高，被 Wasmtime 使用
- ⚠️ 首轮不用 Pulley（字节码解释器），降低复杂度

### 2. 为什么不实现完整的 Joky 语法？

完整语法（Effect、Cown、Region、泛型）会大幅增加实现复杂度，而验证核心机制只需要最小语法集。验证通过后，完整语法的支持可以逐步扩展。

### 3. 如何保证调用约定一致？

Cranelift 默认使用平台标准 ABI（System V / Windows x64），AOT 编译器也使用相同 ABI。跨边界调用时：
- 参数通过寄存器/栈传递（遵循 ABI）
- 返回值通过 `rax` 寄存器（x86_64）
- 调用者保存寄存器在跨边界前保存

### 4. 挂起点如何实现？

当前实现将可挂起函数编译为 `unsafe extern "C" fn(*mut CpsFrame) -> i32`：

- `0` 表示完成，`1` 表示 Pending，其他值为错误。
- 帧保存参数、局部值与临时值；状态号定位尚未完成的子调用。
- AOT/JIT 共用 CPS lowering 和 runtime；生成的入口在完成时写入 `DONE`。
- JIT 调用外部生成的 AOT 函数时，由注册接口提供完整帧大小及代码所有者。
- `ContinuationStatus` 保存 Pending、Completed 或 Failed；调度器分别取走成功或失败任务。
- C 接口使用 `include/jk_cps.h` 中的不透明帧，由 Rust runtime 分配和释放。

`src/aot/tests.rs` 的真实动态库集成测试覆盖 JIT → AOT → 宿主的嵌套挂起，
以及恢复、取消、错误传播和 VM 重载/销毁后的代码保活。
当前仍采用调用方显式通知就绪的单线程调度器，不包含真实 I/O reactor。

## 验收标准

所有 5 个验证场景通过，且满足：

1. **正确性**：输出符合预期，无崩溃
2. **内存安全**：Valgrind 无错误，无泄漏
3. **资源清理**：模块关闭后，引用计数归零
4. **性能基线**：跨边界调用开销 < 100ns（x86_64，release 构建）
5. **并发安全**：多 worker 模式下，100 次运行无竞态

## 依赖

- Rust 最新稳定版（建议 1.75+）
- Cranelift 最新稳定版（通过 `cargo add cranelift cranelift-module cranelift-jit` 自动获取）
- Valgrind（内存检测）

**Cargo.toml 依赖示例**：
```toml
[dependencies]
cranelift = "*"              # 核心 IR 和代码生成
cranelift-module = "*"       # 模块管理
cranelift-jit = "*"          # JIT 执行引擎
cranelift-native = "*"       # 本机目标检测
```

> 💡 建议在项目初期不锁定版本，使用 Cranelift 最新特性和 bug 修复。稳定后再锁定具体版本。

## 后续整合

验证通过后，核心机制将合并回 Joky 主仓库：

1. JIT 后端集成到 `compiler/mir_backend/`
2. VM API 实现为标准库模块 `joky/vm`
3. Runtime 调度器扩展支持 Cown、Region 等完整特性
4. 支持 Pulley 解释器（用于 iOS）
5. 支持复杂类型跨边界传递
6. 支持 Effect 系统跨边界

## 参考资料

- [Cranelift 官网](https://cranelift.dev/)
- [Cranelift 文档](https://docs.rs/cranelift/)
- [Cranelift GitHub](https://github.com/bytecodealliance/wasmtime/tree/main/cranelift)
- [System V ABI](https://refspecs.linuxbase.org/elf/x86_64-abi-0.99.pdf)
- [Joky 阶段 10 计划](https://github.com/jokylang/joky/blob/main/docs/plans/embedded-execution.md)（需替换为实际链接）
- [Wasmtime Portable Interpretation](https://docs.wasmtime.dev/examples-pulley.html)

## 许可证

与 Joky 主项目保持一致（MIT / Apache 2.0 双许可）
