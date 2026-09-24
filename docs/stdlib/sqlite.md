# SQLite

`import joky/sqlite` 提供 SQLite effect、连接、语句和查询游标。运行环境需有
SQLite 动态库；修改计数优先使用 `sqlite3_changes64`（SQLite 3.37.0+），旧
版本自动回退 `sqlite3_changes`，计数上限为 32 位。
完整示例见 [examples/io/sqlite.jk](../../examples/io/sqlite.jk)。

```joky
import joky/sqlite

fn main() -> Result(Unit, String) effects { sqlite } {
    let db = sqlite.open(":memory:")?
    for result in db.prepare("select ?, ?")?
        .bind_text(1, "Alice")?.bind_i64(2, 42)?.query()? {
        let row = result?
        println(row.text(0)?!)
        println(row.i64(1)?!)
    }
    db.close()?
    Ok(())
}
```

## 语句与参数

`db.prepare(sql)` 只接受一条 SQL，允许尾部空白和注释；空 SQL、多条语句、
含 NUL 的 SQL 返回 `Err`。参数索引从 **1** 开始，列索引从 **0** 开始。

所有绑定方法消费语句，成功返回 `Result(SqliteStatement, String)`；失败
返回错误并释放语句。链式调用需要在每次绑定后使用 `?` 或显式处理错误。
这是原有 `bind_text` / `bind_i64` 的 API 变更。

| 方法 | 参数值 |
| --- | --- |
| `bind_null(index)` | SQL NULL |
| `bind_i64(index, value)` | Int64 |
| `bind_f64(index, value)` | Float64；拒绝 NaN，避免 SQLite 将其静默变为 NULL |
| `bind_text(index, value)` | String；保留内嵌 NUL |
| `bind_blob(index, value)` | Bytes；空 Bytes 仍为 BLOB，不是 NULL |

`execute()` 消费语句并返回 SQLite 的 `changes64` 计数；SQL 产生行时返回
错误，应使用查询接口。`finalize()` 消费语句并报告 native finalize 错误。
未显式 finalize 的语句也会自动清理。

## 行与游标

`query() -> Result(SqliteRows, String)` 转移语句所有权，不执行 step。
`SqliteRows` 是持有 `Option(SqliteStatement)` 的普通 Owned struct，实现
`Cursor`，其 `Item = Result(SqliteRow, String)`。每次 `advance()` 才执行一次
`sqlite3_step`，有行则复制当前行，无行则返回 `None`。查询失败返回一次
`Err` item，后继游标耗尽。`break` 停止继续拉取，`?` 可向外传播错误。

`SqliteRow` 持有不可变 `List(SqliteValue)`。每列保留 SQLite 的存储类型：
`Null`、`Integer(Int64)`、`Real(Float64)`、`Text(String)`、`Blob(Bytes)`。
行拥有独立的数据快照，可在后续 step、游标释放或连接关闭后继续读取。
无效 UTF-8 的 TEXT 导致该次推进返回 `Err`，原始二进制数据应使用 BLOB。

| 方法 | 返回值 |
| --- | --- |
| `column_count()` | UInt64 |
| `value(index)` | `Result(SqliteValue, String)` |
| `i64(index)` | `Result(Option(Int64), String)` |
| `f64(index)` | `Result(Option(Float64), String)` |
| `text(index)` | `Result(Option(String), String)` |
| `blob(index)` | `Result(Option(Bytes), String)` |

类型化 getter 对 NULL 返回 `Ok(None)`，越界或存储类型不匹配返回 `Err`，
不隐式转换整数、浮点数或文本。单行复制成本为 O(列数 + 文本/二进制字节数)；
`column_count()` 为 O(列数)，getter 为 O(列索引)，因为首版采用 List 存储。
游标不预取整份查询结果。循环体为 `Unit` 时不收集输出；保留行或直接
使用循环尾值收集时，内存仍随保留的数据量增长。

具体 `for` 或 `advance()` 传播 `sqlite` effect。当前无 effect 多态，
`T: type + Cursor` 不接受 `SqliteRows`；`@parallel for` 仍只接受 List。

## 执行与清理

SQLite 操作通过 blocking pool 执行。同一连接的完整操作串行化，包括
step 与列复制、execute 与修改计数读取；CPU worker 不等待 SQLite 调用。
连接与语句由 native 引用保持存活，managed 输入和输出分别负责自己的引用。

`close()` 消费连接引用。已创建的语句仍可使用，其引用会延迟物理关闭。
自动 finalize 和 close 进入 runtime 清理队列；作用域退出等待工作和清理
排空。每条成功创建的语句最终 finalize 一次。

取消会移除尚未执行的请求。已开始的工作仍运行至完成，不调用
`sqlite3_interrupt`，也不回滚已经发生的副作用；worker 保留资源直到退出。
迟到结果不得恢复已取消代码，未交付的行、错误字符串及语句引用全部释放。
因此取消后资源归零以作用域排空为准，不承诺 native 查询立即停止。

事务 helper、语句 reset/reuse、列名读取、busy timeout、WAL 配置及查询
中断接口留待后续按需求扩展。
