//! A real C event-source adapter, shared with the standalone AOT tests.
use super::{InterpreterLimits, VmEngine, VmMode};
use crate::{
    runtime::{
        arena::LIVE,
        borrowed_host::*,
        cps::{self, CpsEntry, CpsFrame},
        scheduler::Scheduler,
        suspension::{Continuation, ContinuationStatus},
    },
    FunctionSignature, Type,
};
use std::{any::Any, cell::Cell, ffi::c_void, path::PathBuf, process::Command, rc::Rc};

pub(crate) const SOURCE: &str = include_str!("../../tests/fixtures/async_borrowing.jk");
pub(crate) const C_SOURCE: &str = include_str!("../../tests/fixtures/async_borrowing.c");

#[repr(C)]
struct RuntimeApi {
    slots: unsafe extern "C" fn(*mut CpsFrame) -> *mut u64,
    context: unsafe extern "C" fn(*const CpsFrame) -> *mut c_void,
    set_context: unsafe extern "C" fn(*mut CpsFrame, *mut c_void),
    set_result: unsafe extern "C" fn(*mut CpsFrame, u64),
    clear_error: extern "C" fn(),
    division_error: extern "C" fn(usize, usize),
}

struct Fixture {
    library: Rc<libloading::Library>,
    dir: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "joky-async-borrow-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let header = crate::ffi::c_header(&crate::parser::parse(SOURCE).unwrap()).unwrap();
        std::fs::write(dir.join("async_borrowing.h"), header).unwrap();
        std::fs::write(dir.join("host.c"), C_SOURCE).unwrap();
        let path = dir.join("host.so");
        let output = Command::new("cc")
            .args([
                "-std=c11",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-shared",
                "-fPIC",
                "-DASYNC_VM_TEST",
                "-I",
            ])
            .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("include"))
            .arg(dir.join("host.c"))
            .arg("-o")
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let library = Rc::new(unsafe { libloading::Library::new(path).unwrap() });
        unsafe {
            library
                .get::<unsafe extern "C" fn(*const RuntimeApi)>(b"async_bind_runtime")
                .unwrap()(&RuntimeApi {
                slots: cps::slots,
                context: cps::joky_cps_host_context,
                set_context: cps::joky_cps_set_host_context,
                set_result: cps::joky_cps_set_result,
                clear_error: crate::runtime::error::joky_runtime_error_clear,
                division_error: crate::runtime::error::division_by_zero,
            });
        }
        Self { library, dir }
    }
    fn reset(&self) {
        unsafe {
            self.library
                .get::<unsafe extern "C" fn()>(b"async_reset")
                .unwrap()()
        }
    }
    fn ready(&self, read: bool, timer: bool) {
        unsafe {
            self.library
                .get::<unsafe extern "C" fn(i32, i32)>(b"async_ready")
                .unwrap()(read.into(), timer.into())
        }
    }
    fn tick(&self) {
        unsafe {
            self.library
                .get::<unsafe extern "C" fn()>(b"async_tick")
                .unwrap()()
        }
    }
    fn count(&self, which: i32) -> i32 {
        unsafe {
            self.library
                .get::<unsafe extern "C" fn(i32) -> i32>(b"async_count")
                .unwrap()(which)
        }
    }
    fn register(&self, mode: VmMode, owner: Option<Rc<dyn Any>>) -> VmEngine {
        let mut vm = VmEngine::with_mode(mode);
        let reference = |ty, mutable| Type::reference(ty, mutable);
        for (name, params, ret) in [
            (
                "host_io",
                vec![reference(Type::Named("AsyncPoint".into()), true)],
                Type::Unit,
            ),
            (
                "host_read",
                vec![reference(Type::Named("AsyncPoint".into()), false)],
                Type::Int32,
            ),
            (
                "host_four",
                vec![
                    reference(Type::Int32, false),
                    reference(Type::Int64, false),
                    reference(Type::UInt32, false),
                    reference(Type::UInt64, false),
                ],
                Type::UInt64,
            ),
        ] {
            unsafe {
                let descriptor = self
                    .library
                    .get::<*const BorrowingHostV1>(
                        format!("__joky_cps_borrow_v1_{name}").as_bytes(),
                    )
                    .unwrap();
                vm.register_borrowing_suspending_fn(
                    name,
                    FunctionSignature::new(params, ret),
                    **descriptor,
                    Some(owner.clone().unwrap_or_else(|| self.library.clone())),
                )
                .unwrap();
            }
        }
        unsafe {
            vm.register_aot_suspending_fn(
                "timeout",
                FunctionSignature::new(vec![], Type::Unit),
                *self.library.get::<CpsEntry>(b"timeout").unwrap(),
                0,
                self.library.clone(),
            )
            .unwrap();
            vm.register_owned_fn(
                "after_stop",
                FunctionSignature::new(vec![Type::Int32], Type::Int32),
                *self.library.get::<*const u8>(b"after_stop").unwrap(),
                Some(self.library.clone()),
            )
            .unwrap();
        }
        vm
    }
    fn vm(&self, mode: VmMode) -> VmEngine {
        let mut vm = self.register(mode, None);
        vm.load_source(SOURCE).unwrap();
        vm
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn drain(task: &mut Continuation) {
    for _ in 0..1000 {
        if task.is_finished() {
            return;
        }
        task.resume().unwrap();
    }
    panic!("task did not complete");
}
fn live(n: usize) {
    LIVE.with(|live| assert_eq!(live.get(), n));
}

#[test]
fn async_borrowing_completion_race_and_real_c_layouts() {
    let fixture = Fixture::new();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = fixture.vm(mode);
        for (name, read, timer, starts, completed, cancelled) in [
            ("sequential", true, false, 1, 1, 0),
            ("race_timeout", false, true, 1, 0, 1),
            ("race_timeout", true, false, 1, 1, 0),
            ("nested", false, true, 1, 0, 1),
            ("shared", true, false, 2, 2, 0),
            ("distinct", true, false, 2, 2, 0),
        ] {
            fixture.reset();
            let mut task = vm.call_async(name, &[]).unwrap();
            assert_eq!(task.status(), ContinuationStatus::Pending);
            for _ in 0..4 {
                task.resume().unwrap();
                assert_eq!(fixture.count(0), starts);
            }
            fixture.tick();
            fixture.ready(read, timer);
            drain(&mut task);
            assert_eq!(task.cps_result(), Some(42), "{mode:?}: {name}");
            assert_eq!(fixture.count(1), completed);
            assert_eq!(fixture.count(3), cancelled);
            assert_eq!(fixture.count(4), 1);
            assert_eq!(fixture.count(5), 0);
            fixture.tick(); // a late readiness event cannot touch released storage
            live(0);
            assert!(task.resume().is_err());
            drop(task);
            assert_eq!(fixture.count(1) + fixture.count(3), starts);
        }
        for (name, starts, completed, cancelled) in [
            ("unstarted", 0, 0, 0),
            ("immediate", 1, 1, 0),
            ("widths", 1, 1, 0),
            ("repeated", 20, 0, 20),
        ] {
            fixture.reset();
            fixture.ready(false, true);
            let mut task = vm.call_async(name, &[]).unwrap();
            drain(&mut task);
            assert_eq!(task.cps_result(), Some(42), "{mode:?}: {name}");
            assert_eq!(
                (fixture.count(0), fixture.count(1), fixture.count(3)),
                (starts, completed, cancelled)
            );
            live(0);
        }
    }
}

#[test]
fn async_borrowing_failure_cancellation_and_owners() {
    let fixture = Fixture::new();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = fixture.vm(mode);
        for name in ["start_failure", "error_channel"] {
            fixture.reset();
            let error = vm.call_async(name, &[]).unwrap_err();
            assert_eq!(
                error.code(),
                if name == "error_channel" {
                    crate::ErrorCode::RuntimeError
                } else {
                    crate::ErrorCode::HostError
                }
            );
            assert_eq!(fixture.count(2), 1);
            assert_eq!(fixture.count(5), 0);
            live(0);
            fixture.tick();
        }
        for (name, args, read, timer, reason) in [
            ("pending_failure", vec![], true, false, 2),
            ("sibling_failure", vec![0], false, true, 3),
        ] {
            fixture.reset();
            let mut task = vm.call_async(name, &args).unwrap();
            fixture.ready(read, timer);
            assert!(task.resume().is_err());
            assert_eq!(fixture.count(reason), 1);
            assert_eq!(fixture.count(5), 0);
            live(0);
            if name == "sibling_failure" {
                let ContinuationStatus::Failed(crate::runtime::suspension::ResumeError::Failed(
                    error,
                )) = task.status()
                else {
                    panic!("missing original runtime error")
                };
                assert!(error.message().contains("Division by zero"));
            }
            drop(task);
            assert_eq!(fixture.count(reason), 1);
            fixture.tick();
        }
        fixture.reset();
        let mut scheduler = Scheduler::new();
        let cancelled = scheduler.suspend(vm.call_async("nested", &[]).unwrap());
        let pending = vm.call_async("sequential", &[]).unwrap();
        live(2);
        vm.load_source("fn f() -> Int32 { 99 }").unwrap();
        drop(vm);
        scheduler.cancel(cancelled).unwrap();
        assert!(scheduler.cancel(cancelled).is_err());
        live(1);
        assert_eq!(fixture.count(3), 1);
        fixture.tick();
        drop(pending);
        live(0);
        assert_eq!(fixture.count(3), 2);
        fixture.tick();

        struct Owner(Rc<libloading::Library>, Rc<Cell<bool>>);
        impl Drop for Owner {
            fn drop(&mut self) {
                self.1.set(true);
            }
        }
        fixture.reset();
        let dropped = Rc::new(Cell::new(false));
        let owner = Rc::new(Owner(fixture.library.clone(), dropped.clone()));
        assert!(Rc::strong_count(&owner.0) >= 2);
        let mut vm = fixture.register(mode, Some(owner.clone()));
        vm.load_source(SOURCE).unwrap();
        drop(owner);
        let task = vm.call_async("sequential", &[]).unwrap();
        drop(vm);
        assert!(!dropped.get());
        drop(task);
        assert!(dropped.get());
        assert_eq!(fixture.count(3), 1);
        live(0);
    }
}

#[test]
fn async_borrowing_interpreter_limits_stop_active_operations() {
    let fixture = Fixture::new();
    for limits in [
        InterpreterLimits {
            poll_budget: Some(1),
            instruction_budget: Some(90),
            ..Default::default()
        },
        InterpreterLimits {
            max_frames: 3,
            ..Default::default()
        },
    ] {
        fixture.reset();
        let mut vm = fixture.register(VmMode::Interpreter, None);
        vm.set_interpreter_limits(limits).unwrap();
        vm.load_source(&format!("{SOURCE} fn recurse() @suspends {{ recurse() }} fn limited() @suspends -> Int32 {{ let p = alloc(AsyncPoint {{ value: 1, mode: 0 }}); parallel {{ | host_io(p) | recurse() }} 0 }}")).unwrap();
        let result = vm.call_async("limited", &[]);
        if let Ok(mut task) = result {
            for _ in 0..500 {
                if task.resume().is_err() {
                    break;
                }
            }
            assert!(matches!(task.status(), ContinuationStatus::Failed(_)));
        }
        assert_eq!(fixture.count(0), 1);
        assert_eq!(fixture.count(3), 1);
        assert_eq!(fixture.count(5), 0);
        live(0);
        fixture.tick();
    }
}

#[test]
fn async_borrowing_rejects_conflicts_bad_layouts_and_legacy_bindings() {
    let fixture = Fixture::new();
    let invalid = [
        "fn bad() @suspends { let p = alloc(AsyncPoint { value: 1, mode: 0 }); race { | host_io(p) | host_io(p) } }",
        "fn bad() @suspends { let p = alloc(AsyncPoint { value: 1, mode: 0 }); parallel { | host_io(p) | read_unit(p) } }",
        "fn touch(p: &mut AsyncPoint) -> &mut AsyncPoint { p.value = 9; p } fn bad() @suspends { let p = alloc(AsyncPoint { value: 1, mode: 0 }); race { | host_io(p) | host_io(touch(p)) } }",
        "fn bad() @suspends { var p = AsyncPoint { value: 1, mode: 0 }; host_io(&mut p) }",
        "fn bad() { let p = alloc(AsyncPoint { value: 1, mode: 0 }); host_io(p) }",
        "extern fn other(p: &AsyncPoint) @suspends -> &AsyncPoint",
        "extern fn other(p: &AsyncPoint, raw: Ptr) @suspends",
        "extern fn other(p: &AsyncPoint) @suspends -> Ptr",
        "extern fn other(p: &Bool) @suspends",
        "struct Plain { value: Int32 } extern fn other(p: &Plain) @suspends",
        "extern fn other(a: &Int32, b: &Int32, c: &Int32, d: &Int32, e: &Int32) @suspends",
    ];
    for body in invalid {
        let source = format!("{SOURCE}\n{body}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = fixture.register(mode, None);
            assert!(vm.load_source(&source).is_err(), "{mode:?}: {body}");
        }
        let ast = crate::parser::parse(&source).unwrap();
        assert!(
            crate::aot::AotCompiler::new("invalid-borrow")
                .unwrap()
                .compile_module(&ast)
                .is_err(),
            "{body}"
        );
    }
    unsafe extern "C" fn poll(_: *mut CpsFrame) -> i32 {
        0
    }
    unsafe extern "C" fn stop(_: *mut CpsFrame, _: u32) {}
    let valid = BorrowingHostV1 {
        abi_version: 1,
        poll: Some(poll),
        stop: Some(stop),
    };
    let signature = FunctionSignature::new(vec![Type::reference(Type::Int32, true)], Type::Unit);
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_borrowing_suspending_fn("host", signature.clone(), valid, None)
                .unwrap();
            for bad in [
                BorrowingHostV1 {
                    abi_version: 0,
                    ..valid
                },
                BorrowingHostV1 {
                    poll: None,
                    ..valid
                },
                BorrowingHostV1 {
                    stop: None,
                    ..valid
                },
            ] {
                assert!(vm
                    .register_borrowing_suspending_fn("host", signature.clone(), bad, None)
                    .is_err());
            }
            for bad in [
                FunctionSignature::new(vec![], Type::Unit),
                FunctionSignature::new(vec![Type::Ptr], Type::Unit),
                FunctionSignature::new(vec![Type::reference(Type::Int32, true)], Type::String),
            ] {
                assert!(vm
                    .register_borrowing_suspending_fn("host", bad, valid, None)
                    .is_err());
            }
            assert!(vm
                .register_suspending_fn("host", signature.clone(), poll)
                .is_err());
        }
        // Failed registration is atomic, and cannot replace the valid descriptor.
        vm.load_source("extern fn host(p: &mut Int32) @suspends fn run() @suspends -> Int32 { let p = alloc(42); host(p); memory.read(p) }").unwrap();
        assert_eq!(vm.call_async("run", &[]).unwrap().cps_result(), Some(42));
        assert!(vm.load_source("extern fn host(p: &mut Int32)").is_err());
        assert!(vm
            .load_source("extern fn host(p: &Int32) @suspends")
            .is_err());
        unsafe {
            vm.register_suspending_fn(
                "host",
                FunctionSignature::new(vec![Type::Int32], Type::Unit),
                poll,
            )
            .unwrap();
        }
        assert!(vm
            .load_source("extern fn host(p: &mut Int32) @suspends")
            .unwrap_err()
            .message()
            .contains("borrowing host v1"));
    }
}

#[test]
fn async_borrowing_aliases_generics_modules_and_nested_layouts() {
    #[repr(C)]
    struct Point {
        small: i32,
        wide: i64,
    }
    #[repr(C)]
    struct Packet {
        point: Point,
        unsigned: u64,
    }
    unsafe extern "C" fn poll(frame: *mut CpsFrame) -> i32 {
        let frame = &mut *frame;
        if frame.state == 0 {
            frame.host_context = frame.slots[0] as *mut c_void;
            frame.state = 1;
            return 1;
        }
        let packet = &mut *frame.host_context.cast::<Packet>();
        assert_eq!(
            (packet.point.small, packet.point.wide, packet.unsigned),
            (-1, -2, 45)
        );
        packet.point.small = 1;
        frame.result =
            (i64::from(packet.point.small) + packet.point.wide + packet.unsigned as i64) as u64;
        0
    }
    unsafe extern "C" fn stop(frame: *mut CpsFrame, reason: u32) {
        assert_eq!(reason, STOP_COMPLETED);
        assert_eq!((*(*frame).host_context.cast::<Packet>()).point.small, 1);
    }
    let library = r#"
        #repr(C) pub struct Point { pub small: Int32, pub wide: Int64 }
        #repr(C) pub struct Packet { pub point: Point, pub unsigned: UInt64 }
        type View = &mut Packet
        extern fn process(packet: View) @suspends -> Int64
        pub fn wrapper<T>(packet: View, ignored: T) @suspends -> Int64 { process(packet) }
    "#;
    let source = r#"import geometry.{type Packet, type Point, wrapper}
        fn run() @suspends -> Int32 {
            let p = alloc(Packet { point: Point { small: -1, wide: int64(-2) }, unsigned: uint64(45) })
            let answer = wrapper(p, true)
            p.unsigned = uint64(0)
            int32(answer - int64(2))
        }"#;
    let mut modules = crate::ModuleSources::new();
    modules.insert("geometry", library).unwrap();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_borrowing_suspending_fn(
                "process",
                FunctionSignature::new(
                    vec![Type::reference(Type::Named("geometry.Packet".into()), true)],
                    Type::Int64,
                ),
                BorrowingHostV1 {
                    abi_version: 1,
                    poll: Some(poll),
                    stop: Some(stop),
                },
                None,
            )
            .unwrap();
        }
        vm.load_source_with_modules(source, &modules).unwrap();
        let mut task = vm.call_async("run", &[]).unwrap();
        live(1);
        vm.load_source("fn f() -> Int32 { 0 }").unwrap();
        drop(vm);
        drain(&mut task);
        assert_eq!(task.cps_result(), Some(42));
        live(0);
    }
    let ast = crate::TypeRegistry::default()
        .resolve_with_modules(&crate::parser::parse(source).unwrap(), &modules)
        .unwrap();
    crate::aot::AotCompiler::new("nested-borrow")
        .unwrap()
        .compile_module(&ast)
        .unwrap();
}

#[test]
fn suspending_function_values_stop_borrowing_hosts_before_cancellation_releases_storage() {
    let fixture = Fixture::new();
    let source = format!(
        r#"{SOURCE}
        fn timer_wrapper() @suspends {{ timeout() }}
        fn indirect_race() @suspends -> Int32 {{
            let p = share(alloc(AsyncPoint {{ value: 42, mode: 0 }}))
            let read = read_unit
            let timer = timer_wrapper
            race {{ | read(p) | timer() }}
            after_stop(42)
        }}
    "#
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = fixture.register(mode, None);
        vm.load_source(&source).unwrap();
        for completed in [false, true] {
            fixture.reset();
            let mut task = vm.call_async("indirect_race", &[]).unwrap();
            for _ in 0..3 {
                task.resume().unwrap();
                assert_eq!(fixture.count(0), 1);
            }
            fixture.ready(completed, !completed);
            drain(&mut task);
            assert_eq!(task.cps_result(), Some(42));
            assert_eq!(fixture.count(1), i32::from(completed));
            assert_eq!(fixture.count(3), i32::from(!completed));
            assert_eq!(fixture.count(4), 1);
            assert_eq!(fixture.count(5), 0);
            live(0);
            fixture.tick();
        }
        fixture.reset();
        let task = vm.call_async("indirect_race", &[]).unwrap();
        vm.load_source("fn new_module() -> Int32 { 1 }").unwrap();
        drop(vm);
        drop(task);
        assert_eq!(fixture.count(3), 1);
        assert_eq!(fixture.count(5), 0);
        live(0);
        fixture.tick();
    }
}
