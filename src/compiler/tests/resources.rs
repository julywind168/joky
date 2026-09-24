use super::*;
use crate::test_metrics;
use std::time::{Duration, Instant};

fn settle_standalone(runtime_scope: &joky_runtime::host::RuntimeScope) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let counts = joky_runtime::host::testing::resource_counts(runtime_scope);
        let managed = joky_runtime::host::testing::managed_objects(runtime_scope);
        if counts == [0; 6] && managed == 0 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "standalone resources did not drain: {counts:?}, managed={managed}"
        );
        thread::sleep(Duration::from_millis(2));
    }
}

fn run_scope(mir: &MirProgram) {
    let runtime_scope = joky_runtime::host::RuntimeScope::new();
    {
        let _guard = runtime_scope.enter();
        let _providers =
            super::super::providers::NativeProviderRegistry::install(mir.types(), &runtime_scope);
        let mut backend = CraneliftBackend::new().unwrap();
        backend
            .compile_and_run_program(mir, &runtime_scope)
            .unwrap();
        runtime_scope.close_and_wait();
    }
    settle_standalone(&runtime_scope);
    assert_eq!(
        joky_runtime::host::testing::scope_strong_count(&runtime_scope),
        1,
        "closed scope is retained by a host handle"
    );
}

#[test]
fn sqlite_suspensions_release_owned_handles_and_preserve_borrows() {
    let source = format!(
        r#"{}
        fn main() effects {{ sqlite }} {{
            let db = sqlite.open(":memory:")!
            db.prepare("select 1")!.finalize()!
            match db.prepare("select ?")!.bind_i64(0, 42) {{
                Err(_) => ()
                Ok(_) => panic("invalid bind succeeded")
            }}
            let statement = db.prepare("select ?")!.bind_text(1, "kept alive")!
            db.close()!
            for result in statement.query()! {{
                let row = result!
                if row.text(0)!! != "kept alive" {{ panic("statement lost its connection") }}
            }}
        }}
        "#,
        include_str!("../../../std/joky/sqlite.jk"),
    );
    let program = syntax::parse_program(&source).unwrap();
    let types = sema::check_program(&program).unwrap();
    let core = CoreProgram::lower(program, types).unwrap();
    let mut mir = MirProgram::lower(&core).unwrap();
    MirPassManager::default_pipeline().run(&mut mir).unwrap();
    run_scope(&mir);
}

#[test]
fn file_and_socket_close_release_owned_handles_after_borrowed_operations() {
    let source = format!(
        r#"{}
        {}
        fn main() effects {{ file, udp }} {{
            let handle = file.open("{}/Cargo.toml", FileMode.Read)!
            let _ = handle.read_chunk(1)!
            let position: UInt64 = 1
            if handle.position()! != position {{ panic("borrowed file was released") }}
            handle.close()!
            let socket = udp.bind("127.0.0.1", 0)!
            match socket.send(Bytes.from_string("unconnected")) {{
                Err(_) => ()
                Ok(_) => panic("unconnected send succeeded")
            }}
            socket.close()!
        }}
        "#,
        include_str!("../../../std/joky/file.jk"),
        include_str!("../../../std/joky/socket/udp.jk"),
        env!("CARGO_MANIFEST_DIR"),
    );
    let program = syntax::parse_program(&source).unwrap();
    let types = sema::check_program(&program).unwrap();
    let core = CoreProgram::lower(program, types).unwrap();
    let mut mir = MirProgram::lower(&core).unwrap();
    MirPassManager::default_pipeline().run(&mut mir).unwrap();
    run_scope(&mir);
}

#[test]
fn option_none_comparisons_release_temporaries_and_preserve_borrows() {
    let source = r#"
        struct Payload { let text: String }
        impl PartialEq for Payload {
            fn equals(&self, other: &Self) -> Bool { panic("payload compared to None"); false }
        }
        class Owned { let text: String }
        fn is_none(T: type, value: &Option(T)) -> Bool { value == None }
        fn main() {
            let shared = Some(Payload(text: "shared" + " payload"))
            if shared == None { panic("shared == None") }
            if None == shared { panic("None == shared") }
            if !(shared != None) { panic("shared != None") }
            if !(None != shared) { panic("None != shared") }
            if is_none(shared) { panic("borrowed shared value") }
            if Some("temporary" + " right") == None { panic("temporary == None") }
            if None == Some("temporary" + " left") { panic("None == temporary") }
            if !(Some(List#{"nested" + " value"}) != None) { panic("nested temporary") }
            let owned = Some(Owned(text: "owned" + " value"))
            if owned == None { panic("owned == None") }
            if None == owned { panic("None == owned") }
            if is_none(owned) { panic("borrowed owned value") }
            if !(None != Some(Owned(text: "owned" + " temporary"))) { panic("owned temporary") }
            let shared_payload = shared!
            if shared_payload.text != "shared payload" { panic("shared payload released early") }
            let owned_payload = owned!
            if owned_payload.text != "owned value" { panic("owned payload released early") }
            let none: Option(String) = None
            if !is_none(none) { panic("borrowed None tag") }
            if None != none { panic("None tag") }
        }
    "#;
    let program = syntax::parse_program(source).unwrap();
    let types = sema::check_program(&program).unwrap();
    let core = CoreProgram::lower(program, types).unwrap();
    let mut mir = MirProgram::lower(&core).unwrap();
    MirPassManager::default_pipeline().run(&mut mir).unwrap();
    run_scope(&mir);
}

fn rss_bytes() -> usize {
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .expect("ps for resident memory measurement");
    assert!(output.status.success());
    std::str::from_utf8(&output.stdout)
        .unwrap()
        .trim()
        .parse::<usize>()
        .unwrap()
        * 1024
}

#[test]
#[ignore = "process-wide resource and allocator measurements; use scripts/check-runtime.py"]
fn bounded_runtime_metadata_across_batches_and_scopes() {
    let directory = std::env::temp_dir().join(format!("joky-resources-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(directory.clone());
    std::fs::write(directory.join("input"), "hello").unwrap();
    let source = format!(
        r#"
        eff file {{ @suspends fn read(path: String) -> Result(String, String) }}
        eff time {{ @suspends fn sleep(duration: Duration) -> Unit }}
        eff Marker {{ fn mark() -> Int32 }}
        fn main() effects {{ file, time }} {{
            let results = parallel {{
                | match file.read(path: "{}/input") {{ Ok(_) => 1, Err(_) => 0 }}
                | match file.read(path: "{}/missing") {{ Ok(_) => 0, Err(_) => 1 }}
                | {{ do {{ time.sleep(0ms); Marker.mark() }} with {{ Marker.mark() => 2 }} }}
                | race {{
                    | {{ time.sleep(1ms); 3 }}
                    | {{ time.sleep(60000ms); 4 }}
                }}
            }}
            if results.0 + results.1 + results.2 + results.3 != 7 {{ panic("bad result") }} else {{ }}
        }}
    "#,
        directory.display(),
        directory.display()
    );
    let program = syntax::parse_program(&source).unwrap();
    let types = sema::check_program(&program).unwrap();
    let core = CoreProgram::lower(program, types).unwrap();
    let mut mir = MirProgram::lower(&core).unwrap();
    MirPassManager::default_pipeline().run(&mut mir).unwrap();
    // 4 warmup + 20 measured batches. Each batch: 1024 public handles and
    // hour-long timer cancellations at width 32, plus four JIT runs with
    // concurrent branches and one reused-Compiler run.
    let runtime_scope = joky_runtime::host::RuntimeScope::new();
    let mut reusable_compiler = Compiler::new().unwrap();
    let mut warm_live = 0;
    let mut warm_capacity = 0;
    for batch in 0..24 {
        joky_runtime::host::testing::churn_runtime_metadata(&runtime_scope);
        for _ in 0..4 {
            run_scope(&mir);
        }
        // Embedding users may keep one Compiler for many independent runs.
        reusable_compiler
            .run_program("fn main() { let x = 42 }")
            .unwrap();
        let capacity = joky_runtime::host::testing::runtime_metadata_capacity();
        let (live, allocated) = test_metrics::allocations();
        if batch < 4 {
            warm_live = warm_live.max(live);
            warm_capacity = warm_capacity.max(capacity);
        }
        if batch >= 4 {
            assert!(
                capacity <= warm_capacity * 2,
                "metadata capacity grows with history: {capacity} > {warm_capacity}"
            );
            assert!(
                live <= warm_live + 512 * 1024,
                "live Rust allocation grows: {live} > warm baseline {warm_live}"
            );
        }
        eprintln!("resource batch={batch} operations={} scopes={} active={:?} capacity={capacity} live_bytes={live} allocated_bytes={allocated} rss_bytes={}",
            (batch + 1) * 1024, (batch + 1) * 5,
            joky_runtime::host::testing::resource_counts(&runtime_scope), rss_bytes());
    }
    runtime_scope.close_and_wait();
    settle_standalone(&runtime_scope);
}

#[test]
fn failed_provider_start_releases_native_reservations() {
    let source = r#"
        eff Missing { @suspends fn load() -> Int32 }
        fn child() -> Int32 effects { Missing } { Missing.load() }
        fn main() effects { Missing } { println(child()) }
    "#;
    let program = syntax::parse_program(source).unwrap();
    let types = sema::check_program(&program).unwrap();
    let core = CoreProgram::lower(program, types).unwrap();
    let mut mir = MirProgram::lower(&core).unwrap();
    MirPassManager::default_pipeline().run(&mut mir).unwrap();
    let runtime_scope = joky_runtime::host::RuntimeScope::new();
    {
        let _guard = runtime_scope.enter();
        let mut backend = CraneliftBackend::new().unwrap();
        assert!(backend
            .compile_and_run_program(&mir, &runtime_scope)
            .is_err());
    }
    runtime_scope.close_and_wait();
    settle_standalone(&runtime_scope);
}

#[test]
fn backend_drops_unclaimed_failure_before_releasing_code() {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicUsize, Ordering};

    unsafe extern "C" fn drop_payload(payload: *const c_void) {
        let address = unsafe { payload.cast::<usize>().read_unaligned() };
        let dropped = unsafe { Arc::from_raw(address as *const AtomicUsize) };
        dropped.fetch_add(1, Ordering::SeqCst);
    }

    let program = syntax::parse_program("fn main() { }").unwrap();
    let types = sema::check_program(&program).unwrap();
    let core = CoreProgram::lower(program, types).unwrap();
    let mut mir = MirProgram::lower(&core).unwrap();
    MirPassManager::default_pipeline().run(&mut mir).unwrap();
    let runtime_scope = joky_runtime::host::RuntimeScope::new();
    let _guard = runtime_scope.enter();
    let dropped = Arc::new(AtomicUsize::new(0));
    {
        let mut backend = CraneliftBackend::new().unwrap();
        backend
            .compile_and_run_program(&mir, &runtime_scope)
            .unwrap();
        // Model an early error that leaves a root payload unclaimed. The
        // real drop callback may live in this backend's executable mapping.
        let address = Arc::into_raw(Arc::clone(&dropped)) as usize;
        unsafe {
            joky_runtime::host::testing::record_root_failure(
                &runtime_scope,
                42,
                (&address as *const usize).cast(),
                std::mem::size_of::<usize>(),
                drop_payload as *const c_void,
            );
        }
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
    }
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert!(!joky_runtime::host::testing::has_root_failure(
        &runtime_scope
    ));
    settle_standalone(&runtime_scope);
}

#[test]
#[ignore = "process-wide standalone task metrics; use scripts/check-runtime.py"]
fn bounded_for_task_storage_and_cancellation_cleanup() {
    // Runtime construction keeps the source small and avoids measuring the
    // parser's unrelated large-literal recursion limit.
    let prelude = r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        fn build(count: Int32) -> List(String) {
            if count == 0 { List#{} } else { build(count - 1).push_front("payload") }
        }
        fn inputs() -> List(String) { build(1024) }
    "#;
    for limit in [1, 2, 5] {
        joky_runtime::host::testing::reset_heap_task_metrics();
        let source = format!(
            r#"{prelude}
            fn main() effects {{ time }} {{
                let input = inputs()
                let expected: UInt64 = 1024
                if input.length() != expected {{ panic("wrong input count") }} else {{ }}
                let output = @parallel(limit: {limit}) for (index, item) in input {{
                    time.sleep(0ms)
                    continue
                }}
                if !output.is_empty() {{ panic("skip retained output") }} else {{ }}
            }}
        "#
        );
        run_program(&source);
        let metrics = joky_runtime::host::testing::heap_task_metrics();
        eprintln!("batch limit={limit}, input=1024, (spawned, peak storage, retained)={metrics:?}");
        assert_eq!(metrics, (limit, limit, 0));
    }
    for _ in 0..8 {
        let source = format!(
            r#"{prelude}
            fn main() effects {{ time }} {{
                let input = inputs()
                let winner = race {{
                    | {{ time.sleep(1ms); List("winner") }}
                    | @parallel(limit: 4) for item in input {{ time.sleep(60000ms); item }}
                }}
                if winner.head()! != "winner" {{ panic("cancelled batch won") }} else {{ }}
                let expected: UInt64 = 1024
                if input.length() != expected {{ panic("cancellation damaged the caller input") }} else {{ }}
            }}
        "#
        );
        run_program(&source);
    }
    // Cancellation during input preparation must also drop a call result that
    // arrives just before the post-call cancellation checkpoint.
    for _ in 0..4 {
        run_program(&format!(
            r#"{prelude}
            fn main() effects {{ time }} {{
                let winner = race {{
                    | {{ time.sleep(1ms); List("winner") }}
                    | @parallel(limit: 4) for item in inputs() {{ time.sleep(60000ms); item }}
                }}
                if winner.head()! != "winner" {{ panic("input preparation cancellation") }} else {{ }}
            }}
        "#
        ));
    }
    let source = r#"
        fn main() {
            let limit: UInt64 = 0
            let output = @parallel(limit: limit) for item in List("a", "b") { item }
            panic("continued after invalid limit")
        }
    "#;
    assert!(try_run_program(source).is_err());
}
