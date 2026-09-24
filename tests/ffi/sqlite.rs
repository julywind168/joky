use super::*;

#[test]
fn sqlite_update_hook_calls_retained_callback_from_worker_thread() {
    let fixture = Fixture::new();
    fixture.write(
        "api.jk",
        r#"
@extern(c, "@LIB@", "jk_sqlite_async_start") pub fn start(cb: CPtr(Unit), ctx: CMutPtr(Unit)) -> CMutPtr(Unit);
@extern(c, "@LIB@", "jk_sqlite_async_join") pub fn join(job: CMutPtr(Unit)) -> Int32;
"#,
    );
    fixture.write(
        "main.jk",
        r#"
import api
eff time { @suspends fn sleep(duration: Duration) -> Unit }
fn main() effects { time } {
    let label = "sqlite worker"
    let callback = CCallback.new(fn (operation: Int32, database: CStr, table: CStr, rowid: Int64) -> Int32 {
        time.sleep(1ms)
        if label == "sqlite worker" {
            if database.to_string().is_some() {
                if table.to_string().is_some() {
                    {}
                }
            }
        }
        0
    }, 0)
    let job = api.start(callback.function(), callback.context())
    if job.is_null() { panic("sqlite worker start") }
    if api.join(job) != 0 { panic("sqlite worker query") }
    if callback.failed() { panic("sqlite callback failed") }
    callback.close()
    println("sqlite async callback ok")
}
"#,
    );
    assert_eq!(success(&fixture.run(&[])), "sqlite async callback ok\n");
}
