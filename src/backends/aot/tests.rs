use super::*;
use crate::{FunctionSignature, Type};
use std::{
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

struct Cleanup(std::path::PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Create a unique scratch directory removed when the returned guard drops.
fn scratch_dir(label: &str) -> (std::path::PathBuf, Cleanup) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "joky-aot-{label}-{}-{unique}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let cleanup = Cleanup(dir.clone());
    (dir, cleanup)
}

#[test]
fn object_emission_requires_a_successful_single_compilation() {
    let invalid = crate::parser::parse("fn f() -> Bool { 1 }").unwrap();
    let valid = crate::parser::parse("fn f() -> Int32 { 42 }").unwrap();
    let mut compiler = AotCompiler::new("failed").unwrap();
    assert!(compiler.compile_module(&invalid).is_err());
    assert!(compiler.compile_module(&valid).is_err());
    // An empty path also fails at the filesystem layer; require the state error.
    for compiler in [compiler, AotCompiler::new("new").unwrap()] {
        assert!(compiler
            .write_object(std::path::Path::new(""))
            .unwrap_err()
            .contains("before successful compilation"));
    }
    let mut compiler = AotCompiler::new("compiled").unwrap();
    compiler.compile_module(&valid).unwrap();
    assert!(compiler.compile_module(&valid).is_err());
}

#[test]
fn scope_aot_lowering_emits_all_branch_frames() {
    let ast = crate::parser::parse(
        r#"
        extern fn wait_value(x: Int32) @suspends -> Int32
        fn left(x: Int32) @suspends { wait_value(x) {} }
        fn right(x: Int32) @suspends { wait_value(x) {} }
        fn all() @suspends -> Int32 {
            scope {
                | left(1)
                | right(2)
            }
            42
        }
    "#,
    )
    .unwrap();
    let mut compiler = AotCompiler::new("scope").unwrap();
    compiler.compile_module(&ast).unwrap();
    assert!(compiler.cps_frame_slots("all").unwrap() >= 2);
}

#[test]
#[cfg(unix)]
fn direct_aot_object_links_and_executes_unit_and_bool_abis() {
    let ast = crate::parser::parse(
        r#"
        extern fn record(x: Int32) -> Int32
        fn identity(value: Unit) { value }
        fn pick(before: Unit, value: Int32, after: Unit) -> Int32 {
            before
            after
            value
        }
        fn bool_identity(value: Bool) -> Bool { let saved: Bool = value saved }
        fn compute(x: Int32) -> Int32 {
            let unit: Unit = {}
            identity(unit)
            let saved: Int32 = { let x = x + 1 x }
            pick({ record(1) unit }, record(saved + x), { record(2) unit })
        }
    "#,
    )
    .unwrap();
    let (dir, _cleanup) = scratch_dir("direct");
    let object = dir.join("direct.o");
    let host = dir.join("host.c");
    let executable = dir.join("direct");
    let mut compiler = AotCompiler::new("direct").unwrap();
    compiler.compile_module(&ast).unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(
        &host,
        r#"
        #include <stdbool.h>
        #include <stdint.h>
        static int32_t events[3];
        static int count;
        int32_t record(int32_t value) {
            if (count < 3) events[count] = value;
            count++;
            return value;
        }
        extern void identity(void);
        extern int32_t pick(int32_t);
        extern bool bool_identity(bool);
        extern int32_t compute(int32_t);
        int main(void) {
            identity();
            if (pick(7) != 7) return 1;
            if (!bool_identity(true) || bool_identity(false)) return 2;
            if (compute(20) != 41) return 3;
            if (count != 3 || events[0] != 1 || events[1] != 41 || events[2] != 2) return 4;
            return 0;
        }
    "#,
    )
    .unwrap();
    let linked = Command::new("cc")
        .args(["-Wall", "-Wextra", "-Werror"])
        .arg(&host)
        .arg(&object)
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        linked.status.success(),
        "{}",
        String::from_utf8_lossy(&linked.stderr)
    );
    assert!(Command::new(&executable).status().unwrap().success());
}

#[test]
#[cfg(unix)]
fn injected_module_symbols_link_without_inheriting_the_export_prefix() {
    use crate::{
        modules::{ModuleRegistry, NativeBinding},
        resolver::BindingSpec,
    };
    use std::rc::Rc;

    let mut registry = ModuleRegistry::default();
    // Link-only declarations contain no callable in-process pointers.
    unsafe {
        registry.register_module(
            "user/math",
            [NativeBinding {
                spec: BindingSpec::with_symbol(
                    "math.next",
                    FunctionSignature::new(vec![Type::Int32], Type::Int32),
                    "native_increment",
                ),
                entry: None,
                owner: None,
            }],
        )
    }
    .unwrap();
    let mut compiler = AotCompiler::with_symbol_prefix_and_registry(
        "custom_registry",
        "export_",
        Rc::new(registry),
    )
    .unwrap();
    let ast = crate::parser::parse(
        "import user/math extern fn legacy(x: Int32) -> Int32 fn answer() -> Int32 { math.next(legacy(40)) }").unwrap();
    compiler.compile_module(&ast).unwrap();
    let (dir, _cleanup) = scratch_dir("custom-registry");
    let object = dir.join("custom.o");
    let host = dir.join("host.c");
    let executable = dir.join("custom");
    compiler.write_object(&object).unwrap();
    std::fs::write(
        &host,
        r#"
        #include <stdint.h>
        int32_t native_increment(int32_t x) { return x + 1; }
        int32_t export_legacy(int32_t x) { return x + 1; }
        extern int32_t export_answer(void);
        int main(void) { return export_answer() == 42 ? 0 : 1; }
    "#,
    )
    .unwrap();
    let linked = Command::new("cc")
        .args(["-Wall", "-Wextra", "-Werror"])
        .arg(&host)
        .arg(&object)
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        linked.status.success(),
        "{}",
        String::from_utf8_lossy(&linked.stderr)
    );
    assert!(Command::new(&executable).status().unwrap().success());
}

#[test]
fn registry_symbols_cannot_alias_source_exports() {
    use crate::{
        modules::{ModuleRegistry, NativeBinding},
        resolver::BindingSpec,
    };
    use std::rc::Rc;

    let mut registry = ModuleRegistry::default();
    unsafe {
        registry.register_module(
            "user/math",
            [NativeBinding {
                spec: BindingSpec::with_symbol(
                    "math.next",
                    FunctionSignature::new(vec![], Type::Int32),
                    "export_answer",
                ),
                entry: None,
                owner: None,
            }],
        )
    }
    .unwrap();
    let mut compiler =
        AotCompiler::with_symbol_prefix_and_registry("collision", "export_", Rc::new(registry))
            .unwrap();
    let source = "import user/math fn answer() -> Int32 { math.next() }";
    let error = compiler
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap_err();
    assert_eq!(
        (error.phase(), error.code()),
        (ErrorPhase::Abi, ErrorCode::AbiError)
    );
    assert_eq!(
        error.span().unwrap().slice(source),
        Some("import user/math")
    );
}

#[test]
#[cfg(unix)]
fn jit_calls_generated_aot_with_shared_runtime_and_keeps_code_alive() {
    use crate::runtime::{
        cps,
        scheduler::{Scheduler, SchedulerError},
        suspension::{ContinuationStatus, ResumeError},
    };
    use crate::vm::VmEngine;
    use libloading::Library;
    use std::{cell::Cell, rc::Rc};

    thread_local! {
        static READY: Cell<bool> = const { Cell::new(false) };
        static STARTS: Cell<u32> = const { Cell::new(0) };
        static FAIL: Cell<bool> = const { Cell::new(false) };
    }
    unsafe extern "C" fn wait_value(frame: *mut cps::CpsFrame) -> i32 {
        let frame = &mut *frame;
        if frame.state == 0 {
            STARTS.with(|v| v.set(v.get() + 1));
            frame.state = 1;
            return 1;
        }
        if !READY.with(Cell::get) {
            return 1;
        }
        if FAIL.with(Cell::get) {
            return 7;
        }
        frame.result = (frame.slots[0] as i32 + 1) as u32 as u64;
        0
    }
    let ast = crate::parser::parse(
        r#"
        extern fn wait_value(x: Int32) @suspends -> Int32
        fn inner(x: Int32) @suspends -> Int32 {
            let saved = x + 10
            saved + wait_value(x)
        }
        fn aot_work(x: Int32) @suspends -> Int32 {
            let saved = inner(x)
            saved + wait_value(20)
        }
    "#,
    )
    .unwrap();
    let (dir, _cleanup) = scratch_dir("jit");
    let object = dir.join("aot.o");
    let bridge = dir.join("bridge.c");
    let library_path = dir.join(if cfg!(target_os = "macos") {
        "aot.dylib"
    } else {
        "aot.so"
    });
    let mut compiler = AotCompiler::new("aot_callbacks").unwrap();
    compiler.compile_module(&ast).unwrap();
    let slots = compiler.cps_frame_slots("aot_work").unwrap();
    assert!(slots > 1); // Parameters alone cannot hold AOT temporaries.
    compiler.write_object(&object).unwrap();
    // Forward helper calls to the SAME Rust runtime that owns the JIT frames.
    // This avoids a second runtime or a dependency on executable symbol export.
    std::fs::write(
        &bridge,
        r#"
        #include "joky_cps.h"
        static uint64_t *(*slots_fn)(JokyCpsFrame *);
        static JokyCpsFrame *(*child_fn)(JokyCpsFrame *, size_t);
        static void (*release_fn)(JokyCpsFrame *);
        static JokyCpsEntry wait_fn;
        void bind_runtime(uint64_t *(*s)(JokyCpsFrame *),
                          JokyCpsFrame *(*c)(JokyCpsFrame *, size_t),
                          void (*r)(JokyCpsFrame *), JokyCpsEntry w) {
            slots_fn = s; child_fn = c; release_fn = r; wait_fn = w;
        }
        uint64_t *__joky_cps_slots(JokyCpsFrame *f) { return slots_fn(f); }
        JokyCpsFrame *__joky_cps_child(JokyCpsFrame *f, size_t n) { return child_fn(f, n); }
        void __joky_cps_release_child(JokyCpsFrame *f) { release_fn(f); }
        int32_t wait_value(JokyCpsFrame *f) { return wait_fn(f); }
    "#,
    )
    .unwrap();
    let link = Command::new("cc")
        .arg(if cfg!(target_os = "macos") {
            "-dynamiclib"
        } else {
            "-shared"
        })
        .args(["-fPIC", "-Wall", "-Wextra", "-Werror", "-I"])
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("include"))
        .arg(&bridge)
        .arg(&object)
        .arg("-o")
        .arg(&library_path)
        .output()
        .unwrap();
    assert!(
        link.status.success(),
        "{}",
        String::from_utf8_lossy(&link.stderr)
    );
    type Bind = unsafe extern "C" fn(
        unsafe extern "C" fn(*mut cps::CpsFrame) -> *mut u64,
        unsafe extern "C" fn(*mut cps::CpsFrame, usize) -> *mut cps::CpsFrame,
        unsafe extern "C" fn(*mut cps::CpsFrame),
        cps::CpsEntry,
    );
    let library = Rc::new(unsafe { Library::new(&library_path).unwrap() });
    unsafe {
        library.get::<Bind>(b"bind_runtime").unwrap()(
            cps::slots,
            cps::child,
            cps::release_child,
            wait_value,
        );
    }
    let entry = unsafe { *library.get::<cps::CpsEntry>(b"aot_work").unwrap() };
    let weak = Rc::downgrade(&library);
    let mut vm = VmEngine::new();
    unsafe {
        vm.register_aot_suspending_fn(
            "aot_work",
            FunctionSignature::new(vec![Type::Int32], Type::Int32),
            entry,
            slots,
            library.clone(),
        );
    }
    vm.load_source(
        r#"
        extern fn aot_work(x: Int32) @suspends -> Int32
        fn compute(x: Int32) @suspends -> Int32 { 100 + aot_work(x) }
    "#,
    )
    .unwrap();
    let mut scheduler = Scheduler::new();
    let id = scheduler.suspend(vm.call_async("compute", &[2]).unwrap());
    scheduler.resume(id).unwrap(); // Still waiting: no repeated side effects.
    assert_eq!(STARTS.with(Cell::get), 1);
    READY.with(|v| v.set(true));
    scheduler.resume(id).unwrap();
    assert_eq!(scheduler.suspended_count(), 1);
    scheduler.resume(id).unwrap();
    let task = scheduler.take_completed(id).unwrap();
    assert_eq!(task.cps_result(), Some(136));
    assert_eq!(STARTS.with(Cell::get), 2);
    drop(task);

    let failed_id = scheduler.suspend(vm.call_async("compute", &[1]).unwrap());
    FAIL.with(|v| v.set(true));
    assert_eq!(
        scheduler.resume(failed_id),
        Err(SchedulerError::Resume(ResumeError::InvalidCpsStatus(7)))
    );
    assert_eq!(
        scheduler.take_completed(failed_id).unwrap_err(),
        SchedulerError::Resume(ResumeError::InvalidCpsStatus(7))
    );
    let failed = scheduler.take_failed(failed_id).unwrap();
    assert_eq!(
        failed.status(),
        ContinuationStatus::Failed(ResumeError::InvalidCpsStatus(7))
    );
    assert_eq!(failed.cps_result(), None);
    drop(failed);
    FAIL.with(|v| v.set(false));
    let cancelled_id = scheduler.suspend(vm.call_async("compute", &[3]).unwrap());
    let survivor = scheduler.suspend(vm.call_async("compute", &[4]).unwrap());
    vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
    drop(vm);
    drop(library);
    assert!(weak.upgrade().is_some());
    scheduler.cancel(cancelled_id).unwrap();
    scheduler.resume(survivor).unwrap();
    scheduler.resume(survivor).unwrap();
    let task = scheduler.take_completed(survivor).unwrap();
    assert_eq!(task.cps_result(), Some(140));
    drop(task);
    assert!(
        weak.upgrade().is_none(),
        "AOT library must unload after its last task"
    );
}
