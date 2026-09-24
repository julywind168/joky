use super::*;

const API: &str = r#"
@extern(c, "@LIB@", "jk_callback_call") pub fn call(cb: CPtr(Unit), ctx: CMutPtr(Unit), value: Int32) -> Int32;
@extern(c, "@LIB@", "jk_callback_start") pub fn start(cb: CPtr(Unit), ctx: CMutPtr(Unit), value: Int32) -> CMutPtr(Unit);
@extern(c, "@LIB@", "jk_callback_join") pub fn join(job: CMutPtr(Unit)) -> Int32;
@extern(c, "@LIB@", "jk_callback_release") pub fn release(job: CMutPtr(Unit)) -> Unit;
@extern(c, "@LIB@", "jk_callback_mark") pub fn mark() -> Unit;
@extern(c, "@LIB@", "jk_callback_wait") pub fn wait(count: Int32) -> Unit;
@extern(c, "@LIB@", "jk_callback_float") pub fn floating(cb: CPtr(Unit), ctx: CMutPtr(Unit)) -> Float64;
@extern(c, "@LIB@", "jk_callback_void") pub fn void_call(cb: CPtr(Unit), ctx: CMutPtr(Unit)) -> Int32;
@extern(c, "@LIB@", "jk_callback_narrow") pub fn narrow(cb: CPtr(Unit), ctx: CMutPtr(Unit)) -> Int8;
"#;

fn run_callback(fixture: &Fixture, flags: &[&str], workers: &str) -> Output {
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let mut child = Command::new(env!("CARGO_BIN_EXE_joky"))
        .arg("run")
        .arg(fixture.0.join("src/main.jk"))
        .args(flags)
        .env("JOKY_WORKER_COUNT", workers)
        .current_dir(&fixture.0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "callback deadlock: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn retained_callbacks_capture_and_run_after_registration_returns() {
    let fixture = Fixture::new();
    fixture.write("api.jk", API);
    fixture.write(
        "main.jk",
        r#"
import api
fn make(base: Int32) -> CCallback {
    let label = "captured environment"
    CCallback.new(fn (value: Int32) -> Int32 {
        if label == "captured environment" { base + value } else { -1 }
    }, -99)
}
fn main() {
    let callback = make(40)
    let entry = callback.function()
    let context = callback.context()
    if api.call(entry, context, 2) != 42 { panic("sync capture") }
    let first = api.start(entry, context, 3)
    let second = api.start(entry, context, 4)
    if api.join(first) != 43 { panic("foreign thread") }
    if api.join(second) != 44 { panic("repeated capture") }
    if callback.failed() { panic("callback failure") }
    callback.close()
    if api.call(entry, context, 10) != -99 { panic("late callback") }
    println("retained callback ok")
}
"#,
    );
    for flags in [
        &[][..],
        &["--verbose"][..],
        &["--no-cache"][..],
        &["--legacy"][..],
    ] {
        assert_eq!(
            success(&run_callback(&fixture, flags, "1")),
            "retained callback ok\n"
        );
    }
}

#[test]
fn retained_callbacks_suspend_on_c_threads_and_nested_native_boundaries() {
    let fixture = Fixture::new();
    fixture.write("api.jk", API);
    fixture.write(
        "main.jk",
        r#"
import api
eff time { @suspends fn sleep(duration: Duration) -> Unit }
fn make(base: Int32) -> CCallback {
    let label = "shared across suspension"
    CCallback.new(fn (value: Int32) -> Int32 {
        time.sleep(1ms)
        time.sleep(1ms)
        if label == "shared across suspension" { base + value } else { -1 }
    }, -99)
}
fn main() effects { time } {
    let callback = make(40)
    let entry = callback.function()
    let context = callback.context()
    time.sleep(1ms)
    if api.call(entry, context, 2) != 42 { panic("pending nested boundary") }
    let job = api.start(entry, context, 3)
    if api.join(job) != 43 { panic("pending C thread") }
    if callback.failed() { panic("pending failure") }
    callback.close()
    println("pending callback ok")
}
"#,
    );
    for flags in [
        &[][..],
        &["--verbose"][..],
        &["--no-cache"][..],
        &["--legacy"][..],
    ] {
        assert_eq!(
            success(&run_callback(&fixture, flags, "1")),
            "pending callback ok\n"
        );
    }
}

#[test]
fn retained_callbacks_concurrently_suspend_and_drop_automatically() {
    let fixture = Fixture::new();
    fixture.write("api.jk", API);
    fixture.write(
        "main.jk",
        r#"
import api
eff time { @suspends fn sleep(duration: Duration) -> Unit }
fn main() {
    {
        let label = "alive"
        let cb = CCallback.new(fn (v: Int32) -> Int32 {
            api.mark()
            api.wait(2)
            time.sleep(1ms)
            if label == "alive" { v + 40 } else { -1 }
        }, -99)
        let first = api.start(cb.function(), cb.context(), 2)
        let second = api.start(cb.function(), cb.context(), 3)
        api.release(first)
        api.release(second)
        if api.join(first) != 42 { panic("first") }
        if api.join(second) != 43 { panic("second") }
        if cb.failed() { panic("concurrent failure") }
    }
    println("concurrent callback ok")
}
"#,
    );
    for workers in ["1", "4"] {
        assert_eq!(
            success(&run_callback(&fixture, &[], workers)),
            "concurrent callback ok\n"
        );
    }
}

#[test]
fn retained_callbacks_close_cancels_and_drains_foreign_invocations() {
    let fixture = Fixture::new();
    fixture.write("api.jk", API);
    fixture.write(
        "main.jk",
        r#"
import api
eff time { @suspends fn sleep(duration: Duration) -> Unit }
fn main() {
    let label = "must remain alive until drained"
    let cb = CCallback.new(fn (v: Int32) -> Int32 {
        api.mark()
        time.sleep(1h)
        if label == "must remain alive until drained" { v } else { -1 }
    }, -99)
    let job = api.start(cb.function(), cb.context(), 1)
    api.release(job)
    api.wait(1)
    cb.close()
    if api.join(job) != -99 { panic("cancel fallback") }
    println("cancel callback ok")
}
"#,
    );
    for _ in 0..8 {
        assert_eq!(
            success(&run_callback(&fixture, &[], "1")),
            "cancel callback ok\n"
        );
    }
}

#[test]
fn retained_callbacks_preserve_float_narrow_and_void_abis_after_pending() {
    let fixture = Fixture::new();
    fixture.write("api.jk", API);
    fixture.write(
        "main.jk",
        r#"
import api
eff time { @suspends fn sleep(duration: Duration) -> Unit }
fn main() {
    let fp = CCallback.new(fn (a: Float32, b: Float64) -> Float64 {
        time.sleep(1ms)
        let expected: Float32 = 1.25
        b + 1.0
    }, -99.0)
    if api.floating(fp.function(), fp.context()) != 3.5 { panic("float") }
    let narrow = CCallback.new(fn (a: UInt8, b: Int16) -> Int8 {
        time.sleep(1ms)
        -117
    }, -99)
    let narrow_expected: Int8 = -117
    if api.narrow(narrow.function(), narrow.context()) != narrow_expected { panic("narrow") }
    println("callback ABI ok")
}
"#,
    );
    assert_eq!(
        success(&run_callback(&fixture, &[], "1")),
        "callback ABI ok\n"
    );
}
