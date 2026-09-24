# `when until` 条件等待

```joky
let batch = when (outbox) |state| until !state.pending.is_empty() {
    state.take_all()
}
```

`when` 首先获取全部 Cown 的 lease。在同一组 lease 内检查 `until`：满足时执行
body 并返回其值；不满足时登记等待、释放全部 lease，将任务交给 Pending 调度。
相关 Cown 的一次普通 `when` 完成后，等待者重新获取全部 lease 并重新检查条件。
条件检查与 body 之间没有释放窗口，多个消费者不会同时取走同一批数据。

支持多个 Cown 和隐式 payload 名称：

```joky
when (queue, connection) |q, c| until (!q.pending.is_empty()) || c.closed {
    q.take_all()
}
when (state) until state.ready { state.value }
```

Cown 表达式只在进入 `when` 时求值一次。条件为假不会执行 body，也不会定时轮询
或占住 worker；单 worker 下，生产者、计时器与其他任务仍然可以推进。
取消挂起的任务会移除所有条件等待登记，然后执行现有的 continuation / region 清理。

## 条件限制

条件必须返回 `Bool`，只能读取本次绑定的 payload。支持字面量、字段读取、基本
类型的运算和比较，以及内置集合、String / Bytes 的 `length()`、`is_empty()` 查询
（对应类型须已有该方法）。暂不支持任意用户函数或方法调用，也不允许赋值、effect、
创建任务、嵌套 `when` 或其他副作用。body 延续现有 `when` 的限制：不能挂起，
离开 lease 后再做网络读写等 I/O。

条件等待表示“状态满足”，不会累计消息或通知。若需要处理连接关闭，条件应包含
关闭标记；若需要超时，可与 `time.sleep` 放在 `race` 中。没有生产者改变状态时，
条件可无限等待；调度不承诺消费者之间的 FIFO 顺序。

实现协议见[条件等待 runtime](../runtime/when-until.md)。
