use super::parity::{assert_package_parity, Package};
use super::*;

#[test]
fn jit_and_aot_drain_cown_tasks_suspensions_and_native_owners() {
    assert_package_parity(
        "runtime-acceptance",
        &[(
            "main.jk",
            include_str!("../../examples/concurrency/aot_runtime.jk"),
        )],
        "closed\nwinner\n42\n",
        &[],
    );
}

#[test]
fn jit_and_aot_drain_cancelled_work_before_a_result_error() {
    Package::new(
        "runtime-acceptance-error",
        &[
            (
                "work.jk",
                include_str!("../../examples/concurrency/aot_runtime.jk"),
            ),
            (
                "main.jk",
                r#"
                import work
                fn main() -> Result(Unit, String) effects { work.time } {
                    work.exercise()
                    Err("after cancellation")
                }
            "#,
            ),
        ],
    )
    .check("closed\nwinner\n42\n", Some("after cancellation"), &[]);
}

#[cfg(all(unix, target_pointer_width = "64"))]
#[test]
fn jit_and_aot_resume_callbacks_on_foreign_threads() {
    assert_package_parity(
        "ffi-callback",
        &[(
            "main.jk",
            include_str!("../../examples/ffi/async_callback.jk"),
        )],
        "42\n",
        &[],
    );
}

#[test]
fn jit_and_aot_share_c_heap_cell_behavior() {
    assert_package_parity(
        "c-heap",
        &[(
            "main.jk",
            "fn main() { let cell = CMutPtr.alloc(42); println(cell.read()); cell.free() }\n",
        )],
        "42\n",
        &[],
    );
}

#[test]
fn jit_and_aot_report_main_result_errors() {
    Package::new(
        "result-error",
        &[(
            "main.jk",
            "fn main() -> Result(Unit, String) { Err(\"entry failed\") }\n",
        )],
    )
    .check("", Some("entry failed"), &[]);
}

#[test]
fn jit_and_aot_run_suspending_sqlite_main() {
    Package::new(
        "sqlite",
        &[("main.jk", include_str!("../../examples/io/sqlite.jk"))],
    )
    .check_cached("Alice: 42\nBob: unknown\n", None, &[]);
}

#[test]
fn jit_and_aot_report_pending_sqlite_errors() {
    Package::new("sqlite-error", &[("main.jk", "import joky/sqlite\nfn main() -> Result(Unit, String) effects { sqlite } {\n    let db = sqlite.open(\":memory:\")?\n    let _ = db.prepare(\"invalid sql\")?\n    Ok(())\n}\n")])
        .check("", Some("syntax error"), &[]);
}

#[test]
fn jit_and_aot_sqlite_returns_errors_and_preserves_statement_ownership() {
    Package::new("sqlite-contracts", &[("main.jk", r#"
import joky/sqlite

fn empty_and_null(db: &SqliteConnection) -> Result(Unit, String) effects { sqlite } {
    match sqlite.step(db.prepare("select 1 where 0")?) {
        Ok(None) => println("no rows")
        Ok(Some(pair)) => panic("empty query succeeded")
        Err(message) => println(message)
    }
    match sqlite.step(db.prepare("select NULL")?) {
        Ok(None) => panic("NULL query had no rows")
        Ok(Some(pair)) => match pair.0.text(0) {
            Ok(None) => println("NULL first column")
            Ok(Some(value)) => panic("NULL query returned text")
            Err(message) => println(message)
        }
        Err(message) => println(message)
    }
    Ok(())
}

fn invalid_text(db: &SqliteConnection) -> Result(Unit, String) effects { sqlite } {
    match sqlite.step(db.prepare("select cast(x'80' as text)")?) {
        Ok(rows) => panic("invalid UTF-8 succeeded")
        Err(_) => println("invalid UTF-8")
    }
    Ok(())
}

fn bind_errors(db: &SqliteConnection) -> Result(Unit, String) effects { sqlite } {
    match db.prepare("select ?")?.bind_i64(0, 42) {
        Err(_) => println("bad index")
        Ok(_) => panic("zero index succeeded")
    }
    let wrapped: UInt64 = 4294967297
    match db.prepare("select ?")?.bind_i64(wrapped, 42) {
        Err(_) => println("large index")
        Ok(_) => panic("index wrapped")
    }
    match db.prepare("select ?")?.bind_text(2, "missing") {
        Err(_) => println("missing parameter")
        Ok(_) => panic("missing parameter succeeded")
    }
    Ok(())
}

fn sql_errors(db: &SqliteConnection) -> Result(Unit, String) effects { sqlite } {
    match db.prepare(" -- no statement") {
        Err(_) => println("empty SQL")
        Ok(_) => panic("empty SQL succeeded")
    }
    match db.prepare("select 1; select 2") {
        Err(_) => println("multiple statements")
        Ok(_) => panic("multiple statements succeeded")
    }
    Ok(())
}

fn constraint_and_nul(db: &SqliteConnection) -> Result(Unit, String) effects { sqlite } {
    let _ = db.prepare("create table t (n integer unique)")?.execute()?
    let _ = db.prepare("insert into t values (?)")?.bind_i64(1, 42)?.execute()?
    match db.prepare("insert into t values (42)")?.execute() {
        Err(_) => println("constraint error")
        Ok(_) => panic("constraint succeeded")
    }
    for result in db.prepare("select cast(x'610062' as text)")?.query()? {
        let row = result?
        let text = row.text(0)?!
        for echoed_result in db.prepare("select ?; -- trailing comment")?.bind_text(1, text)?.query()? {
            let echoed_row = echoed_result?
            let echoed = echoed_row.text(0)?!
            if echoed != text { panic("embedded NUL lost") }
        }
    }
    Ok(())
}

fn close_keeps_statement(db: SqliteConnection) -> Result(Unit, String) effects { sqlite } {
    db.prepare("select 1")?.finalize()?
    let pending = db.prepare("select 'after close'")?
    db.close()
    for result in pending.query()? {
        let row = result?
        println(row.text(0)?!)
    }
    Ok(())
}

fn main() -> Result(Unit, String) effects { sqlite } {
    let db = sqlite.open(":memory:")?
    empty_and_null(db)?
    invalid_text(db)?
    bind_errors(db)?
    sql_errors(db)?
    constraint_and_nul(db)?
    close_keeps_statement(db)?
    Ok(())
}
"#)]).check("no rows\nNULL first column\ninvalid UTF-8\nbad index\nlarge index\nmissing parameter\nempty SQL\nmultiple statements\nconstraint error\nafter close\n", None, &[]);
}

#[test]
fn jit_and_aot_sqlite_rows_are_typed_lazy_and_independent() {
    Package::new("sqlite-rows", &[("main.jk", r#"
import joky/sqlite

fn early_error(db: &SqliteConnection) -> Result(Unit, String) effects { sqlite } {
    for result in db.prepare("select 1 union all select 2")?.query()? {
        let row = result?
        let _ = row.text(0)?
        continue
    }
    Ok(())
}

fn typed_rows() -> Result(Unit, String) effects { sqlite } {
    let db = sqlite.open(":memory:")?
    let rows = for result in db.prepare("select 1, 'first', x'000102', 1.25, NULL union all select 2, 'second', x'', 2.5, NULL")?.query()? {
        result?
    }
    let first = rows.head()!
    let second = rows.tail()!.head()!
    println(first.i64(0)?!)
    println(second.text(1)?!)
    let five: UInt64 = 5
    let three: UInt64 = 3
    let zero: UInt64 = 0
    if first.column_count() != five { panic("column count") }
    if first.f64(3)?! != 1.25 { panic("real") }
    if !first.i64(4)?.is_none() { panic("NULL") }
    if !first.text(0).is_err() { panic("coercion") }
    if !first.value(5).is_err() { panic("column bounds") }
    if first.blob(2)?!.length() != three { panic("blob") }
    if second.blob(2)?!.length() != zero { panic("empty blob") }
    db.close()
    println(first.text(1)?!)
    println(second.i64(0)?!)
    Ok(())
}

fn bindings(db: &SqliteConnection) -> Result(Unit, String) effects { sqlite } {
    let bound = db.prepare("select ?, ?, ?, ?, ?")?
        .bind_null(1)?.bind_i64(2, -9223372036854775807)?
        .bind_f64(3, 2.5)?.bind_text(4, "hello")?
        .bind_blob(5, Bytes.from_string("blob"))?.query()?
    for result in bound {
        let row = result?
        if !row.text(0)?.is_none() { panic("bind NULL") }
        println(row.i64(1)?!)
        if row.f64(2)?! != 2.5 { panic("bind real") }
        println(row.text(3)?!)
        if row.blob(4)?! != Bytes.from_string("blob") { panic("bind blob") }
        continue
    }
    Ok(())
}

fn lazy_errors(db: &SqliteConnection) -> Result(Unit, String) effects { sqlite } {
    var seen = 0
    for result in db.prepare("select 'ok' union all select cast(x'80' as text)")?.query()? {
        let row = result?
        seen = seen + 1
        break
    }
    if seen != 1 { panic("lazy break") }
    var errors = 0
    for result in db.prepare("select abs(-9223372036854775808)")?.query()? {
        match result {
            Ok(_) => panic("query error missing")
            Err(_) => { errors = errors + 1 }
        }
        if errors > 1 { panic("repeated error") }
        continue
    }
    if errors != 1 { panic("error not fused") }
    for result in db.prepare("select 1 where 0")?.query()? {
        panic("empty cursor")
    }
    if !early_error(db).is_err() { panic("early return") }
    Ok(())
}

fn main() -> Result(Unit, String) effects { sqlite } {
    typed_rows()?
    let db = sqlite.open(":memory:")?
    bindings(db)?
    lazy_errors(db)?
    db.close()
    Ok(())
}
"#)]).check("1\nsecond\nfirst\n2\n-9223372036854775807\nhello\n", None, &[]);
}

#[test]
fn jit_and_aot_cancel_sqlite_cursor_in_suspending_body() {
    assert_package_parity(
        "sqlite-cancel-body",
        &[(
            "main.jk",
            r#"
import joky/sqlite
eff time { @suspends fn sleep(duration: Duration) -> Unit }
class Gate { var started: Bool = false; fn mark() { self.started = true } }
fn consume(notify: fn() -> Unit) effects { sqlite, time } {
    let db = sqlite.open(":memory:")!
    for result in db.prepare("select 'owned row' union all select 'unread row'")!.query()! {
        let row = result!
        notify()
        time.sleep(60s)
        println(row.text(0)!!)
        continue
    }
    ()
}
fn main() effects { sqlite, time } {
    let gate = Cown.new(Gate())
    let started = fn() -> Bool { when (gate) |state| { state.started } }
    race {
        | consume(fn() -> Unit { when (gate) |state| { state.mark() } })
        | { while !started() { time.sleep(1ms) }; () }
    }
    println("cancelled")
}
"#,
        )],
        "cancelled\n",
        &[],
    );
}
#[test]
fn jit_and_aot_run_suspending_file_provider() {
    let package = Package::new("file", &[]);
    let file = package.root.join("data.txt");
    let source = format!("import joky/file\nfn main() effects {{ file }} {{\n    let _ = file.write(\"{}\", \"aot file\")!\n    println(file.read(\"{}\")!)\n}}\n", file.display(), file.display());
    fs::write(package.root.join("src/main.jk"), source).unwrap();
    package.check_with("aot file\n", None, &[], |mode, _| {
        assert_eq!(fs::read(&file).unwrap(), b"aot file", "{mode}");
        fs::remove_file(&file).unwrap();
    });
}

#[test]
fn jit_and_aot_run_suspending_socket_provider() {
    assert_package_parity("socket", &[("main.jk", "import joky/socket/tcp\nfn main() effects { tcp } {\n    match tcp.listen(host: \"127.0.0.1\", port: 0) {\n        Ok(_) => println(\"socket ok\")\n        Err(error) => panic(error)\n    }\n}\n")], "socket ok\n", &[]);
}

#[test]
fn jit_and_aot_resume_recursive_timer_calls() {
    assert_package_parity(
        "timer",
        &[(
            "main.jk",
            include_str!("../../examples/concurrency/suspending_recursion.jk"),
        )],
        "3\n",
        &[],
    );
}

#[cfg(unix)]
#[test]
fn jit_and_aot_share_native_ffi_resources() {
    assert_package_parity(
        "ffi-file",
        &[("main.jk", include_str!("../../examples/ffi/tmpfile.jk"))],
        "temporary file closed\n",
        &[],
    );
}
