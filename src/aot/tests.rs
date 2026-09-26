use super::*;
use crate::jit::cps::ModulePlan;
use std::process::Command;

#[test]
fn aot_cps_object_links_and_resumes_nested_native_calls() {
    let source = r#"
        extern fn wait_value(x: Int32) @suspends -> Int32
        fn inner(x: Int32) @suspends -> Int32 { 10 + wait_value(x) }
        fn outer(x: Int32) @suspends -> Int32 { inner(x) + wait_value(20) }
    "#;
    let ast = crate::parser::parse(source).unwrap();
    let plan = ModulePlan::new(&ast).unwrap();
    let dir = std::env::temp_dir().join(format!("jk-aot-cps-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
    let object = dir.join("cps.o");
    let driver = dir.join("driver.c");
    let executable = dir.join("driver");
    let mut compiler = AotCompiler::new("cps-test").unwrap();
    compiler.compile_module(&ast).unwrap();
    compiler.write_object(&object).unwrap();
    // Only the state/result prefix is accessed by generated code. Frame
    // allocation and slot/child storage are owned by the linking runtime.
    let c_source = r#"
        #include <assert.h>
        #include <stdint.h>
        #include <stdlib.h>
        typedef struct Frame {
            uint32_t state;
            uint64_t result;
            uint64_t *slots;
            struct Frame *child;
        } Frame;
        uint64_t *__jk_cps_slots(Frame *f) { return f->slots; }
        Frame *__jk_cps_child(Frame *f, uintptr_t count) {
            if (!f->child) {
                f->child = calloc(1, sizeof(Frame));
                f->child->slots = calloc(count ? count : 1, sizeof(uint64_t));
            }
            return f->child;
        }
        void __jk_cps_release_child(Frame *f) {
            if (f->child) {
                __jk_cps_release_child(f->child);
                free(f->child->slots);
                free(f->child);
                f->child = 0;
            }
        }
        static int ready = 0, starts = 0;
        int32_t wait_value(Frame *f) {
            if (f->state == 0) { f->state = 1; starts++; return 1; }
            if (!ready) return 1;
            f->result = f->slots[0] + 1;
            return 0;
        }
        extern int32_t outer(Frame *);
        int main(void) {
            Frame f = {0};
            f.slots = calloc(SLOT_COUNT, sizeof(uint64_t));
            f.slots[0] = 2;
            assert(outer(&f) == 1 && starts == 1);
            assert(outer(&f) == 1 && starts == 1);
            ready = 1;
            assert(outer(&f) == 1 && starts == 2);
            assert(outer(&f) == 0 && f.result == 34);
            assert(f.state == UINT32_MAX && f.child == 0);
            assert(outer(&f) == 2 && starts == 2);
            free(f.slots);
            return 0;
        }
    "#
    .replace(
        "SLOT_COUNT",
        &plan.functions["outer"].frame_slots.to_string(),
    );
    std::fs::write(&driver, c_source).unwrap();
    let link = Command::new("cc")
        .arg(&driver)
        .arg(&object)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("AOT tests require the system C compiler cc");
    assert!(
        link.status.success(),
        "{}",
        String::from_utf8_lossy(&link.stderr)
    );
    let output = Command::new(&executable).output().unwrap();
    assert!(
        output.status.success(),
        "status {:?}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
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
    use crate::vm::engine::VmEngine;
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
    let dir = std::env::temp_dir().join(format!("jk-aot-jit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
    let object = dir.join("aot.o");
    let bridge = dir.join("bridge.c");
    let library_path = dir.join(if cfg!(target_os = "macos") {
        "aot.dylib"
    } else {
        "aot.so"
    });
    let mut compiler = AotCompiler::new("interop").unwrap();
    compiler.compile_module(&ast).unwrap();
    let slots = compiler.cps_frame_slots("aot_work").unwrap();
    assert!(slots > 1); // Parameters alone cannot hold AOT temporaries.
    compiler.write_object(&object).unwrap();
    // Forward helper calls to the SAME Rust runtime that owns the JIT frames.
    // This avoids a second runtime or a dependency on executable symbol export.
    std::fs::write(
        &bridge,
        r#"
        #include "jk_cps.h"
        static uint64_t *(*slots_fn)(JkCpsFrame *);
        static JkCpsFrame *(*child_fn)(JkCpsFrame *, size_t);
        static void (*release_fn)(JkCpsFrame *);
        static JkCpsEntry wait_fn;
        void bind_runtime(uint64_t *(*s)(JkCpsFrame *),
                          JkCpsFrame *(*c)(JkCpsFrame *, size_t),
                          void (*r)(JkCpsFrame *), JkCpsEntry w) {
            slots_fn = s; child_fn = c; release_fn = r; wait_fn = w;
        }
        uint64_t *__jk_cps_slots(JkCpsFrame *f) { return slots_fn(f); }
        JkCpsFrame *__jk_cps_child(JkCpsFrame *f, size_t n) { return child_fn(f, n); }
        void __jk_cps_release_child(JkCpsFrame *f) { release_fn(f); }
        int32_t wait_value(JkCpsFrame *f) { return wait_fn(f); }
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
        vm.register_aot_suspending_fn("aot_work", entry, slots, library.clone());
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
    vm.reload_source("fn replacement() -> Int32 { 0 }").unwrap();
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
