# Cown 条件等待协议

用户语法与限制见[`when until`](../lang/when-until.md)。本文描述登记、释放、恢复和取消的实现契约。

## Runtime 协议

- 假条件仍持有全部 lease 时，创建一个共享 waiter 并挂入每个 Cown 的条件队列。
- 登记完成后 quiet release 全部 lease；quiet release 只唤醒争用 lease 的任务，
  不通知条件观察者，避免几个假条件互相唤醒形成忙循环。
- 普通 lease release 取走当前条件观察者，发布可获取状态，再逐个通知。第一条通知
  就能令等待任务就绪；同一 waiter 的后续通知无效。
- resume 先注销整组条件登记，再通过现有 Cown acquisition 协议重新获取全部 lease。
  若获取发生争用，重新挂入实际阻塞 Cown 的获取队列；拿齐后才重新检查条件。
- Pending 的 native 交接协议处理提前通知和提前取消；登记先于释放，因此无需 epoch
  或补偿性定时器来防止丢唤醒。

MIR 的 `CownAcquire.wait_for_change` 表示“释放、等变化、重新获取”的内部等待。
其输入和 resume 边在逻辑上持有同一组 lease，等待期间 runtime 不持有 lease。
校验器要求其参数恰好覆盖全部活动 lease，resume 后重新投影 payload。

每个 Cown 保存条件队列，状态字中的汇总位使没有条件观察者的普通 release 无需
锁条件队列。普通 `when` 完成即视为可能发生变化，不追踪字段写入；可能有多余唤醒，
所以每次 resume 都必须重新检查条件。分批获取失败后的回滚也使用 quiet release。

新增 AOT 入口 `jk_continuation_start_cown_wait`，runtime ABI 为 v26。升级编译器后
应重建实际使用的 runtime archive，构建方式见 README。
