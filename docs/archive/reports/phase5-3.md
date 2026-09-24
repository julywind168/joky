# 阶段 5.3 文件 I/O 验收

> 历史记录：本文保留原阶段的设计、环境与验收结论，不代表当前能力。归档于 2026-09-24；当前说明见[文档入口](../../README.md)，未完成工作见[计划索引](../../plans/README.md)。

日期：2026-09-10。基于 `326c0f7` 加本次修改；本地 macOS 验收，无远端 CI。
普通与 TSan 环境沿用 [5.2](phase5-2.md)。API 约定见 [文件 I/O](../../stdlib/file-io.md)。

## 功能证据

- `reads_non_utf8_file_as_bytes`、`writes_text_and_binary_files`：文本与二进制
  provider 的端到端 ABI。Result(Bytes, String) 是四个槽，文本 Result 是
  五个槽，错误分支始终是 String。
- `file_empty_binary_errors_and_overwrite_are_typed_results`：NUL/255 的内容
  校验、文本 UTF-8 拒绝、空文件、空写入、覆盖截断、缺失路径及目录错误。
- `file_handles_copy_binary_chunks_seek_append_and_close`：100,007 字节二进制
  文件用 4096 字节块复制，宿主逐字节比较；位置、seek、追加、flush、sync、
  重复 close 及关闭后使用。
- `file_modes_and_invalid_operations`：排他创建、ReadWrite 必须存在、只读
  文件写入失败、零长度读取和负绝对偏移拒绝、隐式关闭。
- `writes_retry_short_and_interrupted_calls_and_report_partial_failures`：可控
  Write 后端注入短写、Interrupted、零写入和 PermissionDenied；中途失败
  报告已写字节数。未依赖 root/平台权限差异制造权限错误。
- `file_handle_cannot_be_captured_by_multiple_parallel_arms`：重复 owned 捕获
  被拒绝；当前诊断来自 MIR，源码位置改进仍属 5.4。
- 文件池饱和隔离测试扩展到 read/read_bytes/write/write_bytes/open，分别
  在 ready 与等待准入阶段取消，核对共享参数计数与最终释放。
- `cancellation_keeps_inflight_file_alive_and_close_rejects_busy_handle`：阻塞
  执行路径的可释放测试门，close 竞争返回 busy；取消及语言句柄释放后
  原生资源仍存活，放行晚到操作后最终归零；跨 scope 句柄拒绝。
- `file_workflows_release_handles_buffers_and_cancelled_outputs`：五轮 scope
  创建/关闭，每轮 limit=2、三份 1,048,589 字节二进制复制，块大小 8192。
  内容逐字节比较，并覆盖 race 取消及未注册 provider 导致的失败；排空后
  六类资源计数、blocking pending 和 managed object 均为零。
- CLI 通过标准库 import 运行复制示例，覆盖 1/2/4 CPU worker。

## 工作流发现的修复

1. Bytes.get 以及 MutBytes.get/pop 的初始化 stack_store 使用了元素 I8
   作为地址类型，导致 stack_addr.i8 被 Cranelift 拒绝。改为目标指针类型，
   文件内容逐字节断言覆盖 Bytes 路径。
2. continuation 复用时局部帧按每次挂起压紧布局，cleanup 却只注册首次
   挂起的局部变量。首次 file.open 后才获得的 File，在后续取消/失败时泄漏。
   改为函数内所有挂起点共享稳定的局部槽位及完整清理表；恢复清零、保存、
   TaskWait、间接调用和结果转移均使用同一偏移。其开销随函数中跨挂起局部
   变量数增长，不随循环次数增长。五轮资源隔离测试锁定回归，全套 Pending
   与任务测试覆盖共享行为。
3. 首次 TSan 套件出现内容截断：复制仅写出 40,960 字节，程序却返回成功，
   没有 TSan 竞争报告。provider 将状态改为 Ready 后、下一恢复回调入队前，
   上一回调可能误认为 scope 空闲而释放最后一个工作计数。Ready 不再触发
   此清理；只在终态释放。`ready_before_next_callback_reservation_keeps_scope_work`
   确定性重现该交接窗口，文件复制继续以内容校验验证完整执行。

调试阶段曾用语言 `panic` 注入运行失败，运行时按现有 trap 语义终止测试
进程（SIGILL）。验收改用未注册 provider 返回 Failed，使 scope 清理能
被观察；本实现不承诺进程 trap/强制退出时执行资源析构。

## 复现

```bash
cargo fmt --check
cargo check --locked
python3 scripts/check-runtime.py
cargo test --locked --test cli
python3 scripts/check-runtime.py --tsan
cargo run --locked -- run examples/io/file_copy.jk
cargo run --locked -- run examples/io/file_batch.jk
cargo run --locked -- run examples/io/file_timeout.jk
```

脚本逐个执行隔离测试，检查实际测试数量和超时；日志位于
`target/runtime-checks/{normal,tsan}/`。TSan 与标准库一起插桩，不加抑制，
JIT 内存访问覆盖边界沿用此前说明。Linux/跨平台长跑留在 5.4，不宣称远端通过。

## 结果

- 普通库套件：884 passed，9 intentionally ignored。
- 普通隔离入口：全部通过；最终 60 秒取消压力 804 轮，门槛 300。
- CLI：69 项通过，含新增复制测试的 1/2/4 worker 配置。
- TSan：884 passed，9 intentionally ignored，无竞争报告。

分块内存界限来自按 max_bytes 分配与每块的所有权释放；测试同时检验循环
和并发组合、内容一致及最终归零。没有把进程 RSS 或任意单次巨大 max_bytes
请求宣称为受硬上限保护。文件创建/关闭、取消均不承诺写入原子性或回滚。
