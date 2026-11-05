//! Interop AOT fixtures.
use super::*;

#[test]
#[cfg(unix)]
fn injected_module_symbols_link_without_inheriting_the_export_prefix() {
    use crate::{
        core::resolver::BindingSpec,
        modules::{ModuleRegistry, NativeBinding},
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
    let fixture = ScratchDir::new("custom-registry");
    let dir = fixture.path();
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
    link(
        link_runtime(
            Command::new("cc")
                .args(["-Wall", "-Wextra", "-Werror"])
                .arg(&host)
                .arg(&object),
        ),
        &executable,
    );
    assert!(Command::new(&executable).status().unwrap().success());
}

#[test]
fn registry_symbols_cannot_alias_source_exports() {
    use crate::{
        core::resolver::BindingSpec,
        modules::{ModuleRegistry, NativeBinding},
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
        fn aot_divide(a: Int32, b: Int32) -> Int32 { a / b }
        fn aot_shift(x: Int32) @suspends -> Int32 { 1 << wait_value(x) | wait_value(99) }
        fn aot_division(x: Int32) @suspends -> Int32 {
            let divisor = wait_value(x)
            return aot_divide(7, divisor) + wait_value(99)
        }
        fn aot_failure(x: Int32) @suspends { 7 / wait_value(x); }
        fn aot_division_scope(x: Int32) @suspends -> Int32 {
            join { aot_failure(x), loop_branch(99) }
            wait_value(100)
        }
        fn aot_expression(x: Int32) @suspends -> Int32 {
            var active = (x < 0) == !(wait_value(x) > 0)
            var result = -x
            while active { result = result + -wait_value(2) * 2; active = false }
            result
        }
        fn aot_conditional(x: Int32) @suspends -> Int32 {
            let skipped = false && wait_value(99) > 0
            let selected = if x == 99 || wait_value(x) > 0 {
                wait_value(10)
            } else { wait_value(20) }
            selected + wait_value(30)
        }
        fn loop_branch(x: Int32) @suspends { wait_value(x); {} }
        fn aot_jumps(n: Int32) @suspends -> Int32 {
            var i = 0; var total = 0
            while wait_value(i) <= n {
                i = i + 1
                if i == 2 { continue }
                if i == 4 { break }
                join { loop_branch(i), loop_branch(i + 10) }
                total = total + wait_value(i)
            }
            return total + wait_value(40)
        }
        fn aot_loop(n: Int32) @suspends -> Int32 {
            var i = 0
            var total = 0
            var checks = 0;
            while (checks = checks + 1; wait_value(i) <= n) {
                join { loop_branch(i), loop_branch(i + 10) }
                total = total + wait_value({ i = i + 1; i })
            }
            total + (checks - (n + 1))
        }
    "#,
    )
    .unwrap();
    let fixture = ScratchDir::new("jit");
    let dir = fixture.path();
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
    let loop_slots = compiler.cps_frame_slots("aot_loop").unwrap();
    let expression_slots = compiler.cps_frame_slots("aot_expression").unwrap();
    let conditional_slots = compiler.cps_frame_slots("aot_conditional").unwrap();
    let jump_slots = compiler.cps_frame_slots("aot_jumps").unwrap();
    let division_slots = compiler.cps_frame_slots("aot_division").unwrap();
    let shift_slots = compiler.cps_frame_slots("aot_shift").unwrap();
    let division_scope_slots = compiler.cps_frame_slots("aot_division_scope").unwrap();
    assert!(slots > 1); // Parameters alone cannot hold AOT temporaries.
    compiler.write_object(&object).unwrap();
    // Forward helper calls to the SAME Rust runtime that owns the JIT frames.
    // This avoids a second runtime or a dependency on executable symbol export.
    std::fs::write(
        &bridge,
        r#"
        #include "joky_cps.h"
        #include "joky_runtime.h"
        static void (*checkpoint_fn)(void);
        void bind_checkpoint(void (*f)(void)) { checkpoint_fn = f; }
        void __joky_task_checkpoint_v1(void) { checkpoint_fn(); }
        static int32_t (*error_code_fn)(void);
        static uint64_t (*float_enter_fn)(void);
        static void (*float_reset_fn)(void);
        static void (*float_restore_fn)(uint64_t);
        void bind_float_runtime(uint64_t (*e)(void), void (*r)(void), void (*s)(uint64_t)) {
            float_enter_fn = e; float_reset_fn = r; float_restore_fn = s;
        }
        uint64_t __joky_float_env_enter(void) { return float_enter_fn(); }
        void __joky_float_env_reset(void) { float_reset_fn(); }
        void __joky_float_env_restore(uint64_t saved) { float_restore_fn(saved); }
        static void (*division_fn)(size_t, size_t);
        static void (*shift_fn)(size_t, size_t);
        static void (*call_failed_fn)(uint64_t, size_t, size_t);
        void bind_call_runtime(void (*f)(uint64_t, size_t, size_t)) { call_failed_fn = f; }
        void __joky_runtime_call_failed(uint64_t source, size_t start, size_t end) {
            call_failed_fn(source, start, end);
        }
        void bind_shift_runtime(void (*s)(size_t, size_t)) { shift_fn = s; }
        void __joky_runtime_invalid_shift(size_t start, size_t end) { shift_fn(start, end); }
        void bind_error_runtime(int32_t (*e)(void), void (*d)(size_t, size_t)) {
            error_code_fn = e; division_fn = d;
        }
        int32_t joky_runtime_error_code(void) { return error_code_fn(); }
        void __joky_runtime_division_by_zero(size_t start, size_t end) { division_fn(start, end); }
        static uint64_t *(*slots_fn)(JokyCpsFrame *);
        static JokyCpsFrame *(*child_fn)(JokyCpsFrame *, size_t);
        static void (*release_fn)(JokyCpsFrame *);
        static JokyCpsFrame *(*child_at_fn)(JokyCpsFrame *, size_t, size_t);
        static void (*release_children_fn)(JokyCpsFrame *);
        static void (*finish_fn)(JokyCpsFrame *);
        void bind_finish_runtime(void (*f)(JokyCpsFrame *)) { finish_fn = f; }
        void __joky_cps_finish(JokyCpsFrame *f) { finish_fn(f); }
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
        void bind_scope_runtime(JokyCpsFrame *(*c)(JokyCpsFrame *, size_t, size_t),
                                void (*r)(JokyCpsFrame *)) {
            child_at_fn = c; release_children_fn = r;
        }
        JokyCpsFrame *__joky_cps_child_at(JokyCpsFrame *f, size_t i, size_t n) {
            return child_at_fn(f, i, n);
        }
        void __joky_cps_release_children(JokyCpsFrame *f) { release_children_fn(f); }
        static uint64_t (*task_entry_fn)(JokyCpsFrame *, const JokyTaskEntryV1 *, const size_t *);
        static JokyCpsFrame *(*task_prepare_fn)(JokyCpsFrame *, size_t, uint64_t);
        static int32_t (*task_poll_fn)(JokyCpsFrame *);
        void bind_task_runtime(uint64_t (*e)(JokyCpsFrame *, const JokyTaskEntryV1 *, const size_t *),
                               JokyCpsFrame *(*p)(JokyCpsFrame *, size_t, uint64_t),
                               int32_t (*r)(JokyCpsFrame *)) {
            task_entry_fn = e; task_prepare_fn = p; task_poll_fn = r;
        }
        uint64_t __joky_task_entry_v1(JokyCpsFrame *f, const JokyTaskEntryV1 *e, const size_t *a) {
            return task_entry_fn(f, e, a);
        }
        JokyCpsFrame *__joky_cps_concurrent_prepare_v1(JokyCpsFrame *f, size_t i, uint64_t e) {
            return task_prepare_fn(f, i, e);
        }
        int32_t __joky_cps_task_poll_v1(JokyCpsFrame *f) { return task_poll_fn(f); }
        static void (*protocol_context)(JokyCpsFrame *f);
        static uint64_t (*protocol_current)(uint32_t k);
        static uint64_t (*protocol_open)(uint32_t k, size_t n);
        static uint64_t (*protocol_prepare)(uint64_t s, uint64_t e, const uint64_t *p, size_t n);
        static int32_t (*protocol_publish)(uint64_t s, const uint64_t *p, size_t n);
        static int32_t (*protocol_join)(uint64_t s);
        static int32_t (*protocol_take)(uint64_t s, size_t i, uint64_t *p, size_t n);
        static int32_t (*protocol_cancel)(uint64_t s);
        static int32_t (*protocol_drain)(uint64_t s);
        static int32_t (*protocol_release)(uint64_t s);
        void bind_task_protocol(const void *const *functions) {
            protocol_context = (void (*)(JokyCpsFrame *f))functions[0];
            protocol_current = (uint64_t (*)(uint32_t k))functions[1];
            protocol_open = (uint64_t (*)(uint32_t k, size_t n))functions[2];
            protocol_prepare = (uint64_t (*)(uint64_t s, uint64_t e, const uint64_t *p, size_t n))functions[3];
            protocol_publish = (int32_t (*)(uint64_t s, const uint64_t *p, size_t n))functions[4];
            protocol_join = (int32_t (*)(uint64_t s))functions[5];
            protocol_take = (int32_t (*)(uint64_t s, size_t i, uint64_t *p, size_t n))functions[6];
            protocol_cancel = (int32_t (*)(uint64_t s))functions[7];
            protocol_drain = (int32_t (*)(uint64_t s))functions[8];
            protocol_release = (int32_t (*)(uint64_t s))functions[9];
        }
        void __joky_task_context_v1(JokyCpsFrame *f) { protocol_context(f); }
        uint64_t __joky_task_current_scope_v1(uint32_t k) { return protocol_current(k); }
        uint64_t __joky_task_scope_open_v1(uint32_t k, size_t n) { return protocol_open(k, n); }
        uint64_t __joky_task_child_prepare_v1(uint64_t s, uint64_t e, const uint64_t *p, size_t n) { return protocol_prepare(s, e, p, n); }
        int32_t __joky_task_publish_v1(uint64_t s, const uint64_t *p, size_t n) { return protocol_publish(s, p, n); }
        int32_t __joky_task_join_v1(uint64_t s) { return protocol_join(s); }
        int32_t __joky_task_take_result_v1(uint64_t s, size_t i, uint64_t *p, size_t n) { return protocol_take(s, i, p, n); }
        int32_t __joky_task_request_cancel_v1(uint64_t s) { return protocol_cancel(s); }
        int32_t __joky_task_drain_v1(uint64_t s) { return protocol_drain(s); }
        int32_t __joky_task_scope_release_v1(uint64_t s) { return protocol_release(s); }
        static int32_t (*concurrent_open_fn)(JokyCpsFrame *, uint32_t, size_t);
        static int64_t (*concurrent_next_fn)(JokyCpsFrame *);
        static int64_t (*concurrent_next_result_fn)(JokyCpsFrame *);
        static int32_t (*concurrent_drain_fn)(JokyCpsFrame *);
        static int32_t (*concurrent_polled_fn)(JokyCpsFrame *, size_t, int32_t);
        static JokyCpsFrame *(*concurrent_result_fn)(JokyCpsFrame *, size_t, size_t);
        static JokyCpsFrame *(*concurrent_take_fn)(JokyCpsFrame *, size_t);
        void bind_concurrent_runtime(int32_t (*o)(JokyCpsFrame *, uint32_t, size_t),
                                     int64_t (*n)(JokyCpsFrame *),
                                     int32_t (*p)(JokyCpsFrame *, size_t, int32_t),
                                     JokyCpsFrame *(*r)(JokyCpsFrame *, size_t, size_t),
                                     JokyCpsFrame *(*t)(JokyCpsFrame *, size_t),
                                     int64_t (*nr)(JokyCpsFrame *),
                                     int32_t (*d)(JokyCpsFrame *)) {
            concurrent_open_fn = o; concurrent_next_fn = n; concurrent_polled_fn = p;
            concurrent_result_fn = r; concurrent_take_fn = t; concurrent_next_result_fn = nr; concurrent_drain_fn = d;
        }
        int32_t __joky_cps_concurrent_open_v1(JokyCpsFrame *f, uint32_t k, size_t n) {
            return concurrent_open_fn(f, k, n);
        }
        JokyCpsFrame *__joky_cps_concurrent_result_v1(JokyCpsFrame *f, size_t i, size_t n) {
            return concurrent_result_fn(f, i, n);
        }
        JokyCpsFrame *__joky_cps_concurrent_take_v1(JokyCpsFrame *f, size_t i) {
            return concurrent_take_fn(f, i);
        }
        int32_t __joky_cps_concurrent_drain_v1(JokyCpsFrame *f) { return concurrent_drain_fn(f); }
        int64_t __joky_cps_concurrent_next_result_v1(JokyCpsFrame *f) { return concurrent_next_result_fn(f); }
        int64_t __joky_cps_concurrent_next_v1(JokyCpsFrame *f) { return concurrent_next_fn(f); }
        int32_t __joky_cps_concurrent_polled_v1(JokyCpsFrame *f, size_t i, int32_t s) {
            return concurrent_polled_fn(f, i, s);
        }
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
    type BindScope = unsafe extern "C" fn(
        unsafe extern "C" fn(*mut cps::CpsFrame, usize, usize) -> *mut cps::CpsFrame,
        unsafe extern "C" fn(*mut cps::CpsFrame),
    );
    unsafe {
        type BindCheckpoint = unsafe extern "C" fn(extern "C" fn());
        library.get::<BindCheckpoint>(b"bind_checkpoint").unwrap()(
            crate::runtime::error::checkpoint,
        );
        type BindFloat = unsafe extern "C" fn(
            extern "C" fn() -> u64,
            extern "C" fn(),
            unsafe extern "C" fn(u64),
        );
        library.get::<BindFloat>(b"bind_float_runtime").unwrap()(
            crate::runtime::float_env::enter,
            crate::runtime::float_env::reset,
            crate::runtime::float_env::restore,
        );
        type BindShift = unsafe extern "C" fn(extern "C" fn(usize, usize));
        type BindCall = unsafe extern "C" fn(extern "C" fn(u64, usize, usize));
        library.get::<BindCall>(b"bind_call_runtime").unwrap()(crate::runtime::error::call_failed);
        type BindFinish = unsafe extern "C" fn(unsafe extern "C" fn(*mut cps::CpsFrame));
        library.get::<BindFinish>(b"bind_finish_runtime").unwrap()(cps::finish);
        library.get::<BindShift>(b"bind_shift_runtime").unwrap()(
            crate::runtime::error::invalid_shift,
        );
        type BindError = unsafe extern "C" fn(extern "C" fn() -> i32, extern "C" fn(usize, usize));
        library.get::<BindError>(b"bind_error_runtime").unwrap()(
            crate::runtime::error::joky_runtime_error_code,
            crate::runtime::error::division_by_zero,
        );
        library.get::<Bind>(b"bind_runtime").unwrap()(
            cps::slots,
            cps::child,
            cps::release_child,
            wait_value,
        );
        library.get::<BindScope>(b"bind_scope_runtime").unwrap()(
            cps::child_at,
            cps::release_children,
        );
        type BindTask = unsafe extern "C" fn(
            unsafe extern "C" fn(
                *mut cps::CpsFrame,
                *const cps::task::NativeTaskEntryV2,
                *const usize,
            ) -> u64,
            unsafe extern "C" fn(*mut cps::CpsFrame, usize, u64) -> *mut cps::CpsFrame,
            unsafe extern "C" fn(*mut cps::CpsFrame) -> i32,
        );
        library.get::<BindTask>(b"bind_task_runtime").unwrap()(
            cps::task::register,
            cps::task::prepare_static,
            cps::task::poll,
        );
        type BindProtocol = unsafe extern "C" fn(*const *const u8);
        let protocol = [
            cps::protocol::context as *const u8,
            cps::protocol::current as *const u8,
            cps::protocol::open as *const u8,
            cps::protocol::prepare as *const u8,
            cps::protocol::publish as *const u8,
            cps::protocol::join as *const u8,
            cps::protocol::take as *const u8,
            cps::protocol::cancel as *const u8,
            cps::protocol::drain as *const u8,
            cps::protocol::release as *const u8,
        ];
        library.get::<BindProtocol>(b"bind_task_protocol").unwrap()(protocol.as_ptr());
        type BindConcurrent = unsafe extern "C" fn(
            unsafe extern "C" fn(*mut cps::CpsFrame, u32, usize) -> i32,
            unsafe extern "C" fn(*mut cps::CpsFrame) -> i64,
            unsafe extern "C" fn(*mut cps::CpsFrame, usize, i32) -> i32,
            unsafe extern "C" fn(*mut cps::CpsFrame, usize, usize) -> *mut cps::CpsFrame,
            unsafe extern "C" fn(*mut cps::CpsFrame, usize) -> *mut cps::CpsFrame,
            unsafe extern "C" fn(*mut cps::CpsFrame) -> i64,
            unsafe extern "C" fn(*mut cps::CpsFrame) -> i32,
        );
        library
            .get::<BindConcurrent>(b"bind_concurrent_runtime")
            .unwrap()(
            cps::concurrent::open,
            cps::concurrent::next,
            cps::concurrent::polled,
            cps::concurrent::result,
            cps::concurrent::take,
            cps::concurrent::next_result,
            cps::concurrent::drain,
        );
    }
    let entry = unsafe { *library.get::<cps::CpsEntry>(b"aot_work").unwrap() };
    let weak = Rc::downgrade(&library);
    let mut vm = VmEngine::new();
    unsafe {
        for (name, slots) in [
            ("aot_division", division_slots),
            ("aot_division_scope", division_scope_slots),
        ] {
            vm.register_aot_suspending_fn(
                name,
                FunctionSignature::new(vec![Type::Int32], Type::Int32),
                *library.get::<cps::CpsEntry>(name.as_bytes()).unwrap(),
                slots,
                library.clone(),
            )
            .unwrap();
        }
        vm.register_aot_suspending_fn(
            "aot_work",
            FunctionSignature::new(vec![Type::Int32], Type::Int32),
            entry,
            slots,
            library.clone(),
        )
        .unwrap();
        vm.register_aot_suspending_fn(
            "aot_loop",
            FunctionSignature::new(vec![Type::Int32], Type::Int32),
            *library.get::<cps::CpsEntry>(b"aot_loop").unwrap(),
            loop_slots,
            library.clone(),
        )
        .unwrap();
        vm.register_aot_suspending_fn(
            "aot_expression",
            FunctionSignature::new(vec![Type::Int32], Type::Int32),
            *library.get::<cps::CpsEntry>(b"aot_expression").unwrap(),
            expression_slots,
            library.clone(),
        )
        .unwrap();
        vm.register_aot_suspending_fn(
            "aot_conditional",
            FunctionSignature::new(vec![Type::Int32], Type::Int32),
            *library.get::<cps::CpsEntry>(b"aot_conditional").unwrap(),
            conditional_slots,
            library.clone(),
        )
        .unwrap();
        vm.register_aot_suspending_fn(
            "aot_jumps",
            FunctionSignature::new(vec![Type::Int32], Type::Int32),
            *library.get::<cps::CpsEntry>(b"aot_jumps").unwrap(),
            jump_slots,
            library.clone(),
        )
        .unwrap();
    }
    vm.load_source(
        r#"
        extern fn aot_work(x: Int32) @suspends -> Int32
        extern fn aot_loop(n: Int32) @suspends -> Int32
        extern fn aot_expression(x: Int32) @suspends -> Int32
        extern fn aot_conditional(x: Int32) @suspends -> Int32
        extern fn aot_jumps(n: Int32) @suspends -> Int32
        extern fn aot_division(x: Int32) @suspends -> Int32
        extern fn aot_division_scope(x: Int32) @suspends -> Int32
        fn compute(x: Int32) @suspends -> Int32 { 100 + aot_work(x) }
        fn compute_loop(n: Int32) @suspends -> Int32 { aot_loop(n) }
        fn compute_expression(x: Int32) @suspends -> Int32 { aot_expression(x) }
        fn compute_conditional(x: Int32) @suspends -> Int32 { aot_conditional(x) }
        fn compute_jumps(n: Int32) @suspends -> Int32 { return aot_jumps(n) }
        fn compute_division(x: Int32) @suspends -> Int32 { return aot_division(x) }
        fn compute_division_scope(x: Int32) @suspends -> Int32 { return aot_division_scope(x) }
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

    READY.with(|v| v.set(false));
    let starts = STARTS.with(Cell::get);
    let mut task = vm.call_async("compute_loop", &[3]).unwrap();
    for _ in 0..3 {
        task.resume().unwrap();
    }
    assert_eq!(STARTS.with(Cell::get), starts + 1);
    READY.with(|v| v.set(true));
    for _ in 0..20 {
        if task.is_finished() {
            break;
        }
        task.resume().unwrap();
    }
    assert!(task.is_finished());
    assert_eq!(task.cps_result(), Some(9));
    // Four condition calls, six scope branches, and three body calls.
    assert_eq!(STARTS.with(Cell::get), starts + 13);
    drop(task);

    READY.with(|v| v.set(false));
    let starts = STARTS.with(Cell::get);
    let mut task = vm.call_async("compute_expression", &[4]).unwrap();
    for _ in 0..3 {
        task.resume().unwrap();
    }
    assert_eq!(STARTS.with(Cell::get), starts + 1);
    READY.with(|v| v.set(true));
    task.resume().unwrap();
    assert!(!task.is_finished());
    READY.with(|v| v.set(false));
    for _ in 0..3 {
        task.resume().unwrap();
    }
    assert_eq!(STARTS.with(Cell::get), starts + 2);
    READY.with(|v| v.set(true));
    task.resume().unwrap();
    assert_eq!(task.cps_result().unwrap() as i32, -10);
    assert_eq!(STARTS.with(Cell::get), starts + 2);
    drop(task);

    for (input, expected, calls) in [(-2, 52, 3), (1, 42, 3), (99, 42, 2)] {
        READY.with(|v| v.set(false));
        let starts = STARTS.with(Cell::get);
        let mut task = vm.call_async("compute_conditional", &[input]).unwrap();
        for call in 1..=calls {
            assert!(!task.is_finished());
            for _ in 0..3 {
                task.resume().unwrap();
            }
            assert_eq!(STARTS.with(Cell::get), starts + call);
            READY.with(|v| v.set(true));
            task.resume().unwrap();
            READY.with(|v| v.set(false));
        }
        assert_eq!(task.cps_result(), Some(expected));
        assert_eq!(STARTS.with(Cell::get), starts + calls);
    }
    for (input, expected, calls) in [(0, 41, 2), (1, 43, 6), (3, 47, 11), (10, 47, 11)] {
        READY.with(|v| v.set(false));
        let starts = STARTS.with(Cell::get);
        let mut task = vm.call_async("compute_jumps", &[input]).unwrap();
        for _ in 0..20 {
            if task.is_finished() {
                break;
            }
            let pending_starts = STARTS.with(Cell::get);
            for _ in 0..3 {
                task.resume().unwrap();
            }
            assert_eq!(STARTS.with(Cell::get), pending_starts);
            READY.with(|v| v.set(true));
            task.resume().unwrap();
            READY.with(|v| v.set(false));
        }
        assert_eq!(task.cps_result(), Some(expected));
        assert_eq!(STARTS.with(Cell::get), starts + calls);
    }
    READY.with(|v| v.set(true));

    // Fail while computing an explicit return, after leaving the loop.
    let mut failed_return = vm.call_async("compute_jumps", &[0]).unwrap();
    failed_return.resume().unwrap();
    assert!(!failed_return.is_finished());
    FAIL.with(|v| v.set(true));
    assert_eq!(
        failed_return.resume(),
        Err(ResumeError::InvalidCpsStatus(7))
    );
    assert_eq!(failed_return.cps_result(), None);
    drop(failed_return);
    FAIL.with(|v| v.set(false));

    let failed_id = scheduler.suspend(vm.call_async("compute_conditional", &[1]).unwrap());
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
    for name in ["compute_division", "compute_division_scope"] {
        READY.with(|v| v.set(false));
        let starts = STARTS.with(Cell::get);
        let mut task = vm.call_async(name, &[-1]).unwrap();
        for _ in 0..3 {
            task.resume().unwrap();
        }
        let calls = if name.ends_with("scope") { 2 } else { 1 };
        assert_eq!(STARTS.with(Cell::get), starts + calls);
        READY.with(|v| v.set(true));
        let ResumeError::Failed(error) = task.resume().unwrap_err() else {
            panic!("expected structured AOT failure")
        };
        assert_eq!(error.code(), crate::ErrorCode::RuntimeError);
        assert!(error.span().is_some());
        assert_eq!(error.message(), "Division by zero");
        if name == "compute_division" {
            assert!(!error.native_call_sites().is_empty());
        }
        assert_eq!(task.cps_result(), None);
        assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
        assert_eq!(STARTS.with(Cell::get), starts + calls);
    }
    // The interpreter must also preserve failures from generated AOT entries.
    for name in ["aot_division", "aot_division_scope"] {
        let mut interpreted = VmEngine::new_interpreter();
        unsafe {
            interpreted
                .register_aot_suspending_fn(
                    name,
                    FunctionSignature::new(vec![Type::Int32], Type::Int32),
                    *library.get::<cps::CpsEntry>(name.as_bytes()).unwrap(),
                    if name.ends_with("scope") {
                        division_scope_slots
                    } else {
                        division_slots
                    },
                    library.clone(),
                )
                .unwrap();
        }
        interpreted.load_source(&format!("extern fn {name}(x: Int32) @suspends -> Int32 fn run(x: Int32) @suspends -> Int32 {{ {name}(x) }}")).unwrap();
        READY.with(|v| v.set(false));
        let mut task = interpreted.call_async("run", &[-1]).unwrap();
        for _ in 0..3 {
            task.resume().unwrap();
        }
        READY.with(|v| v.set(true));
        let ResumeError::Failed(error) = task.resume().unwrap_err() else {
            panic!("expected structured AOT failure")
        };
        assert_eq!(error.message(), "Division by zero");
        assert_eq!(error.code(), crate::ErrorCode::RuntimeError);
    }
    for mode in [crate::vm::VmMode::Jit, crate::vm::VmMode::Interpreter] {
        let mut shifted = VmEngine::with_mode(mode);
        unsafe {
            shifted
                .register_aot_suspending_fn(
                    "aot_shift",
                    FunctionSignature::new(vec![Type::Int32], Type::Int32),
                    *library.get::<cps::CpsEntry>(b"aot_shift").unwrap(),
                    shift_slots,
                    library.clone(),
                )
                .unwrap();
        }
        shifted.load_source("extern fn aot_shift(n: Int32) @suspends -> Int32 fn run(n: Int32) @suspends -> Int32 { aot_shift(n) }").unwrap();
        for count in [-2, 31] {
            READY.with(|v| v.set(false));
            let starts = STARTS.with(Cell::get);
            let mut task = shifted.call_async("run", &[count]).unwrap();
            for _ in 0..3 {
                task.resume().unwrap();
            }
            assert_eq!(STARTS.with(Cell::get), starts + 1);
            READY.with(|v| v.set(true));
            let ResumeError::Failed(error) = task.resume().unwrap_err() else {
                panic!("expected shared shift failure")
            };
            assert_eq!(error.code(), crate::ErrorCode::RuntimeError);
            assert!(error.message().contains("Shift count"));
            assert_eq!(task.cps_result(), None);
            assert_eq!(STARTS.with(Cell::get), starts + 1);
        }
    }
    let cancelled_id = scheduler.suspend(vm.call_async("compute_conditional", &[3]).unwrap());
    let survivor = scheduler.suspend(vm.call_async("compute_conditional", &[4]).unwrap());
    let cancelled_jump = scheduler.suspend(vm.call_async("compute_jumps", &[0]).unwrap());
    scheduler.resume(cancelled_jump).unwrap(); // Pending inside the return expression.
    let surviving_jump = scheduler.suspend(vm.call_async("compute_jumps", &[10]).unwrap());
    READY.with(|v| v.set(false));
    let cancelled_division = scheduler.suspend(vm.call_async("compute_division", &[-1]).unwrap());
    let failed_division = scheduler.suspend(vm.call_async("compute_division", &[-1]).unwrap());
    vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
    drop(vm);
    drop(library);
    assert!(weak.upgrade().is_some());
    scheduler.cancel(cancelled_id).unwrap();
    scheduler.cancel(cancelled_jump).unwrap();
    scheduler.cancel(cancelled_division).unwrap();
    READY.with(|v| v.set(true));
    let SchedulerError::Resume(ResumeError::Failed(error)) =
        scheduler.resume(failed_division).unwrap_err()
    else {
        panic!("expected division failure after reload")
    };
    assert_eq!(error.message(), "Division by zero");
    drop(scheduler.take_failed(failed_division).unwrap());
    scheduler.resume(survivor).unwrap();
    scheduler.resume(survivor).unwrap();
    scheduler.resume(survivor).unwrap();
    let task = scheduler.take_completed(survivor).unwrap();
    assert_eq!(task.cps_result(), Some(42));
    drop(task);
    for _ in 0..12 {
        if scheduler
            .continuation(surviving_jump)
            .unwrap()
            .is_finished()
        {
            break;
        }
        scheduler.resume(surviving_jump).unwrap();
    }
    let task = scheduler.take_completed(surviving_jump).unwrap();
    assert_eq!(task.cps_result(), Some(47));
    drop(task);
    assert!(
        weak.upgrade().is_none(),
        "AOT library must unload after its last task"
    );
}

#[test]
#[cfg(unix)]
fn ffi_c_compiler_layout_and_pointer_roundtrips_agree_in_all_backends() {
    use crate::vm::{
        tests::ffi_tests::{register_fixture, CASES, C_SOURCE, SOURCE},
        VmEngine, VmMode,
    };
    let fixture = ScratchDir::new("ffi-borrowing");
    let dir = fixture.path();
    let ast = crate::parser::parse(SOURCE).unwrap();
    std::fs::write(dir.join("bindings.h"), crate::ffi::c_header(&ast).unwrap()).unwrap();
    std::fs::write(dir.join("fixture.c"), C_SOURCE).unwrap();
    let library_path = dir.join(if cfg!(target_os = "macos") {
        "fixture.dylib"
    } else {
        "fixture.so"
    });
    let output = Command::new("cc")
        .args([
            "-std=c11", "-Wall", "-Wextra", "-Werror", "-fPIC", "-shared",
        ])
        .arg(dir.join("fixture.c"))
        .arg("-o")
        .arg(&library_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let library = Rc::new(unsafe { libloading::Library::new(&library_path) }.unwrap());
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        register_fixture(&mut vm, library.clone());
        vm.load_source(SOURCE)
            .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
        for _ in 0..2 {
            for name in CASES {
                assert_eq!(vm.call_int32_0(name).unwrap(), 42, "{mode:?}: {name}");
                crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
            }
        }
        assert!(vm.call_int32_1("ffi_fail", 0).is_err());
        crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
        assert_eq!(vm.call_int32_1("ffi_fail", 1).unwrap(), 42);
    }
    // A symbol prefix applies only to Joky functions, never C imports.
    let mut aot = AotCompiler::with_symbol_prefix("ffi-borrowing", "test_").unwrap();
    aot.compile_module(&ast).unwrap();
    let object = dir.join("program.o");
    aot.write_object(&object).unwrap();
    let mut main = String::from("#include <stdint.h>\n");
    for name in CASES {
        main.push_str(&format!("extern int32_t test_{name}(void);\n"));
    }
    main.push_str("int main(void) {\n");
    for (index, name) in CASES.iter().enumerate() {
        main.push_str(&format!("if (test_{name}() != 42) return {};\n", index + 1));
    }
    main.push_str("return 0; }\n");
    std::fs::write(dir.join("main.c"), main).unwrap();
    let executable = dir.join("program");
    link(
        link_runtime(
            Command::new("cc")
                .arg("-std=c11")
                .arg(dir.join("main.c"))
                .arg(dir.join("fixture.c"))
                .arg(&object),
        ),
        &executable,
    );
    assert!(Command::new(executable).status().unwrap().success());
}

#[test]
#[cfg(unix)]
fn opaque_ptr_native_pending_returns_preserve_values_without_automatic_release() {
    let source = r#"
        unsafe extern "C" fn ptr_source() -> Ptr
        unsafe extern "C" fn ptr_read(p: Ptr) -> Int32
        unsafe extern "C" fn ptr_release(p: Ptr)
        extern fn ptr_wait() @suspends
        fn pass(p: Ptr) @suspends -> (Ptr, Ptr) { ptr_wait(); (p, Ptr.null()) }
        fn pending() @suspends -> Int32 {
            let p = unsafe { ptr_source() }
            let (q, empty) = pass(p)
            let value = if Ptr.is_null(empty) && !Ptr.is_null(q) { unsafe { ptr_read(q) } } else { 0 }
            unsafe { ptr_release(p) }
            value
        }
        fn failing(n: Int32) @suspends -> Int32 {
            let p = unsafe { ptr_source() }
            let (q, _) = pass(p)
            unsafe { ptr_read(q) } / n
        }
    "#;
    let mut compiler = AotCompiler::new("ptr-pending").unwrap();
    let ast = crate::parser::parse(source).unwrap();
    compiler.compile_module(&ast).unwrap();
    let pending = compiler.cps_frame_slots("pending").unwrap();
    let failing = compiler.cps_frame_slots("failing").unwrap();
    let fixture = ScratchDir::new("ptr-pending");
    let dir = fixture.path();
    let object = dir.join("program.o");
    compiler.write_object(&object).unwrap();
    std::fs::write(dir.join("bindings.h"), crate::ffi::c_header(&ast).unwrap()).unwrap();
    let harness = r#"
        #include "bindings.h"
        #include "joky_cps.h"
        #include "joky_runtime.h"
        static int ready, creates, releases;
        static int32_t value = 42;
        void *ptr_source(void) { ++creates; return &value; }
        int32_t ptr_read(void *p) { return p == &value ? *(int32_t *)p : 0; }
        void ptr_release(void *p) { if (p == &value) ++releases; }
        int32_t ptr_wait(JokyCpsFrame *frame) { (void)frame; return ready ? 0 : 1; }
        extern int32_t pending(JokyCpsFrame *), failing(JokyCpsFrame *);
        int main(void) {
            JokyCpsFrame *p = joky_cps_frame_new(PENDING_SLOTS);
            JokyCpsFrame *f = joky_cps_frame_new(FAILING_SLOTS);
            JokyCpsFrame *cancelled = joky_cps_frame_new(PENDING_SLOTS);
            __joky_cps_slots(f)[0] = 0;
            for (int i = 0; i < 3; ++i) {
                if (pending(p) != 1 || failing(f) != 1 || pending(cancelled) != 1) return 1;
            }
            if (creates != 3 || releases != 0) return 2;
            joky_cps_frame_free(cancelled);
            if (releases != 0) return 3;
            ready = 1;
            if (pending(p) != 0 || joky_cps_result(p) != 42 || releases != 1) return 4;
            if (failing(f) != JOKY_CPS_RUNTIME_ERROR || !joky_runtime_error_code()) return 5;
            joky_cps_frame_free(p);
            joky_cps_frame_free(f);
            return creates != 3 || releases != 1;
        }
    "#
    .replace("PENDING_SLOTS", &pending.to_string())
    .replace("FAILING_SLOTS", &failing.to_string());
    let host = dir.join("host.c");
    std::fs::write(&host, harness).unwrap();
    let executable = dir.join("program");
    link(
        link_runtime(
            Command::new("cc")
                .arg(&host)
                .arg(&object)
                .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-I"])
                .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("include")),
        ),
        &executable,
    );
    let output = Command::new(executable).output().unwrap();
    assert!(output.status.success(), "{output:?}");
}
