# 普通函数 Pending ABI 规范

## 概述

本文档定义了 Joky 运行时中普通函数（ordinary functions）处理异步挂起操作的 ABI（Application Binary Interface）。这套 ABI 使得普通函数可以调用 `@suspends` 效应操作并在操作完成前挂起执行，然后在结果就绪时恢复。

从阶段 5.1 起，本文中的 `Continuation*` 是指针宽度的不透明 token，不能
解引用。Native Pending 交接及终态退役责任、迟到回调与 JIT 释放顺序见
[资源生命周期与句柄协议](resource-lifetimes.md)。

## 状态机

每个函数调用在运行时通过 `FunctionCallPhase` 枚举跟踪其生命周期：

```rust
pub(crate) enum FunctionCallPhase {
    Calling,           // 正在调用中，尚未返回
    CompletedInline,   // 内联完成（Ready 路径）
    ReturnedPending,   // 已返回 Pending 状态
    Armed,             // 等待异步完成
    CompletedPending,  // 异步完成，等待发布
    Delivered,         // 结果已传递给调用者
}
```

### 状态转换

1. **Calling → CompletedInline**
   - 函数直接返回 Ready 状态
   - 结果存储在调用者的 suspend result slot
   - 不进行异步操作

2. **Calling → ReturnedPending**
   - 函数返回 Pending 状态
   - 表示需要异步等待

3. **ReturnedPending → Armed**
   - `publish_function_pending` 调用
   - 函数现在等待被完成

4. **Armed → Delivered**
   - 异步完成路径（成功或失败）
   - 调用者被加入调度队列
   - 通过 machine entry 恢复

5. **ReturnedPending → CompletedPending**
   - 在 publish 前完成（竞态条件）
   - 不需要调度

6. **CompletedPending → Delivered**
   - `publish_function_pending` 检测到已完成
   - 立即标记为 Delivered 并调度

## 核心 ABI 函数

### 1. jk_continuation_begin_function_pending

```c
uint8_t jk_continuation_begin_function_pending(Continuation* handle);
```

**调用时机**: 在调用可能挂起的函数之前

**功能**:
- 初始化函数调用状态为 `Calling`
- 分配 scope work 资源
- 返回 1 表示成功，0 表示取消或错误

### 2. jk_continuation_publish_function_pending

```c
uint8_t jk_continuation_publish_function_pending(Continuation* handle);
```

**调用时机**: 当被调用函数返回 Pending 后

**功能**:
- 将状态从 `ReturnedPending` 转换到 `Armed` 或 `Delivered`
- 如果已经完成（`CompletedPending`），立即调度恢复
- 返回当前状态码

### 3. jk_continuation_poll_function_pending

```c
uint8_t jk_continuation_poll_function_pending(Continuation* handle);
```

**调用时机**: 在 native boundary 退出时检查结果

**功能**:
- 检查函数调用是否完成
- 如果是 `CompletedInline`，释放 scope work 并返回 Ready
- 如果仍在 Pending，返回 Pending 状态
- 返回状态码

### 4. jk_continuation_complete_function_pending

```c
bool jk_continuation_complete_function_pending(
    Continuation* handle,
    const uint8_t* payload,
    size_t payload_size
);
```

**调用时机**: 由异步 provider 或被调用的 continuation 调用

**功能**:
- 复制成功结果到调用者的 suspend result slot
- 状态转换:
  - `Calling` → `CompletedInline`
  - `ReturnedPending` → `CompletedPending`
  - `Armed` → `Delivered` (并调度恢复)
- 返回 true 表示成功

### 5. jk_continuation_complete_function_pending_failure

```c
bool jk_continuation_complete_function_pending_failure(
    Continuation* handle,
    const uint8_t* payload,
    size_t payload_size
);
```

**调用时机**: 当被调用函数失败时（未实现的 effect、abort 等）

**功能**:
- 复制失败 payload 到调用者的 suspend result slot
- 设置 failure flag
- 状态转换与成功路径相同
- 在 `Armed` → `Delivered` 时释放 scope work
- 返回 true 表示成功

## 内存布局

### Suspend Result Slot

每个 continuation 有一个 suspend result slot，用于存储：
- 成功的返回值
- 失败的 payload（错误信息等）

大小在编译时确定，基于函数的返回类型。

### Function Frame

对于 suspending 函数调用，编译器分配 frame 存储：
- 局部变量的 spill
- 调用参数（如果需要跨挂起点保留）
- 恢复时需要的元数据

Frame 大小在编译时计算，运行时通过 `jk_continuation_alloc_frame` 分配。

## 竞态条件处理

### 完成 vs 发布竞态

**场景**: 被调用函数可能在 `publish_function_pending` 调用前就完成。

**处理**:
1. `complete_function_pending` 检查当前状态
2. 如果是 `Calling`，转到 `CompletedInline`（不调度）
3. 如果是 `ReturnedPending`，转到 `CompletedPending`（不调度）
4. 如果是 `Armed`，转到 `Delivered`（调度恢复）

### 取消 vs 完成竞态

**场景**: Continuation 可能在完成前被取消。

**处理**:
1. `complete_function_pending[_failure]` 检查 `cancellation_requested` flag
2. 如果已取消，拒绝完成并返回 false
3. 调用者负责清理资源

## Scope Work 管理

### 分配
- `begin_function_pending` 调用 `begin_scope_work()`
- 增加 scope 的活动工作计数器

### 释放
- **内联完成**: `poll_function_pending` 检测到 `CompletedInline` 时释放
- **失败的异步完成**: `complete_function_pending_failure` 在 `Armed` → `Delivered` 时释放
- **成功的异步完成**: 由后续机制处理（通过 machine entry 恢复后的清理）

### 重要性
- 确保 scope 在所有工作完成前不会被销毁
- 防止测试或程序过早退出
- 必须在所有路径正确释放，否则导致资源泄漏

## 恢复机制

### Native Boundary

函数调用在 native boundary 内发起：
1. 调用 `begin_function_pending`
2. 执行函数调用
3. 如果返回 Pending，调用 `publish_function_pending`
4. 在退出 boundary 时调用 `poll_function_pending`

如果 `poll` 返回 Pending，native boundary 返回 Pending 给其调用者。

### Machine Entry 恢复

当异步操作完成时：
1. `complete_function_pending[_failure]` 将状态转为 `Delivered`
2. 调用 `enqueue_resume(handle)`
3. Continuation 通过其 machine entry 恢复
4. Machine entry 继续执行 resume block（编译时生成）
5. Resume block 访问 suspend result slot 中的结果

## 调用链传播

普通函数可以调用其他 suspending 函数，形成调用链：

```
main() -> middle() -> leaf() -> time.sleep()
```

每个调用点都有自己的：
- Function call phase 状态
- Suspend result slot
- Frame（如果需要）

当 `leaf` 挂起时：
1. `leaf` 返回 Pending
2. `middle` 检测到 Pending，保存 frame，发布并返回 Pending
3. `main` 检测到 Pending，保存 frame，发布并返回 Pending

当 `time.sleep()` 完成时：
1. 完成 `leaf` 的 pending 调用，恢复 `leaf`
2. `leaf` 继续执行并返回结果
3. 完成 `middle` 的 pending 调用，恢复 `middle`
4. `middle` 继续执行并返回结果
5. 完成 `main` 的 pending 调用，恢复 `main`

## 错误传播

失败通过 `complete_function_pending_failure` 传播：

1. 被调用函数遇到未实现的 effect 或 abort
2. 调用 `complete_function_pending_failure` 并传递 failure payload
3. 调用者在恢复时检测到 failure flag
4. 调用者可以：
   - 处理错误并继续
   - 传播错误给其调用者
   - 触发 abort

## 测试覆盖

以下场景在 `src/codegen/cranelift/tests/pending_operations.rs` 中测试：

1. ✅ 调用链从 machine entry 恢复
2. ✅ Ready 分支保持在 native 路径
3. ✅ 跨多次操作保留父级
4. ✅ 恢复后调用第二个 suspending 函数
5. ✅ 返回 managed values
6. ✅ 方法调用中的 receiver 转移
7. ✅ Failure 释放等待链
8. ✅ 拒绝操作传播失败到根
9. ✅ Ready 结果保持 native managed ownership
10. ✅ Inline provider 完成后重新挂起
11. ✅ 恢复操作被拒绝后完成链
12. ✅ JIT pending 操作导入传递结果
13. ✅ Abort 在挂起期间发生
14. ✅ 多次挂起后 abort

## 版本历史

- **v1.0** (2026-09-08): 初始规范
  - 定义状态机和核心 ABI 函数
  - 文档化竞态条件处理
  - 明确 scope work 管理规则
  - 修复 failure path 的 scope work 释放
