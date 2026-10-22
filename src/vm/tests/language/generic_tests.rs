use super::support::{with_suspends, Suspends};
use super::{VmEngine, VmMode};
use crate::{ErrorCode, ErrorPhase, FunctionSignature, Type};

#[test]
fn generic_inference_copy_layouts_and_recursion_match_direct_and_cps_backends() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(
                include_str!("../../../../tests/fixtures/generics.jk"),
                suspends,
                Suspends::Typed,
            );
            let mut vm = VmEngine::with_mode(mode);
            let exports = vm.load_source(&source).unwrap();
            assert!(!exports
                .iter()
                .any(|name| name == "identity" || name.starts_with("__joky_generic_")));
            assert_eq!(vm.function_signature("identity"), None);
            assert_eq!(vm.function_signature("__joky_generic_0_choose"), None);
            let result = if suspends {
                vm.call_async("check", &[]).unwrap().cps_result().unwrap() as i32
            } else {
                vm.call_int32_0("check").unwrap()
            };
            assert_eq!(result, 42, "{mode:?} suspends={suspends}");
            if !suspends {
                assert_eq!(
                    vm.call_int32_1("identity", 42).unwrap_err().code(),
                    ErrorCode::NotFound
                );
            }
        }
    }
}

#[test]
fn generic_templates_are_checked_before_instantiation_even_when_unused() {
    let cases = [
        (
            "fn bad<T>(x: T) -> T { x + x }",
            ErrorCode::TypeMismatch,
            "x + x",
        ),
        (
            "fn bad<T>(x: T) -> Bool { x == x }",
            ErrorCode::TypeMismatch,
            "x == x",
        ),
        ("fn bad<T>(x: T) -> T { ~x }", ErrorCode::TypeMismatch, "~x"),
        (
            "fn bad<T>(x: T) -> Int32 { int32(x) }",
            ErrorCode::TypeMismatch,
            "x",
        ),
        (
            "fn bad<T>(x: T) -> Bool { !x }",
            ErrorCode::TypeMismatch,
            "!x",
        ),
        (
            "fn bad<T>(x: T) -> Int32 { x.field }",
            ErrorCode::TypeMismatch,
            "x.field",
        ),
        (
            "fn bad<T>(x: T) -> T { let (a, b) = x; a }",
            ErrorCode::TypeMismatch,
            "(a, b)",
        ),
        (
            "fn bad<T>(x: T) -> T { var y: T = x; y = 1; y }",
            ErrorCode::TypeMismatch,
            "y = 1",
        ),
        (
            "fn take(n: Int32) -> Int32 { n } fn bad<T>(x: T) -> Int32 { take(x) }",
            ErrorCode::TypeMismatch,
            "x",
        ),
        (
            "fn bad<T>() -> T { 1 }",
            ErrorCode::TypeMismatch,
            "fn bad<T>() -> T { 1 }",
        ),
        (
            "fn bad<T, U>(x: T) -> U { x }",
            ErrorCode::TypeMismatch,
            "fn bad<T, U>(x: T) -> U { x }",
        ),
        (
            "fn id<T>(x: T) -> T { x } fn bad<T, U>(x: T) -> U { id(x) }",
            ErrorCode::TypeMismatch,
            "fn bad<T, U>(x: T) -> U { id(x) }",
        ),
        (
            "struct T { n: Int32 } fn bad<T>() -> Int32 { T { n: 1 }.n }",
            ErrorCode::TypeMismatch,
            "T { n: 1 }",
        ),
        (
            "enum T { A } fn bad<T>() { T.A; }",
            ErrorCode::TypeMismatch,
            "T.A",
        ),
        (
            "fn bad<T>(x: T) { unknown(x) }",
            ErrorCode::UnknownName,
            "unknown(x)",
        ),
        (
            "fn wait<T>(x: T) @suspends -> T { x } fn bad<T>(x: T) -> T { wait(x) }",
            ErrorCode::InvalidCall,
            "wait(x)",
        ),
        (
            "fn bad<T>(x: T) -> T { return x; x }",
            ErrorCode::UnreachableCode,
            "x",
        ),
    ];
    for (body, code, marked) in cases {
        let source = format!("// 🙂\n{body}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source("fn current() -> Int32 { 42 }").unwrap();
            let version = vm.module_version();
            let error = vm.load_source(&source).unwrap_err();
            assert_eq!(error.code(), code, "{source}: {error}");
            assert_eq!(error.phase(), ErrorPhase::TypeCheck);
            assert_eq!(
                error.span().unwrap().slice(&source),
                Some(marked),
                "{source}: {error}"
            );
            assert_eq!(vm.module_version(), version);
            assert_eq!(vm.call_int32_0("current").unwrap(), 42);
        }
        let mut aot = crate::aot::AotCompiler::new("bad_generic").unwrap();
        let error = aot
            .compile_module(&crate::parser::parse(&source).unwrap())
            .unwrap_err();
        assert_eq!(error.code(), code, "{source}: {error}");
        assert_eq!(error.span().unwrap().slice(&source), Some(marked));
    }
}

#[test]
fn generic_call_errors_reject_ambiguity_noncopy_values_and_mismatches() {
    let prelude = "fn id<T>(x: T) -> T { x } fn same<T>(a: T, b: T) -> T { a } fn tag<T>() -> Int32 { 42 } fn plain(n: Int32) -> Int32 { n } ";
    for (body, code) in [
        ("fn f() { id() }", ErrorCode::InvalidCall),
        ("fn f() { id(1, 2) }", ErrorCode::InvalidCall),
        ("fn f() { id::<Int32, Bool>(1) }", ErrorCode::InvalidCall),
        ("fn f() { tag() }", ErrorCode::TypeMismatch),
        ("fn f() { id(\"text\") }", ErrorCode::TypeMismatch),
        ("fn f() { tag::<String>() }", ErrorCode::TypeMismatch),
        ("fn f(n: vm.Function) { id(n) }", ErrorCode::TypeMismatch),
        ("fn f() { tag::<vm.Function>() }", ErrorCode::TypeMismatch),
        ("fn f() { tag::<Unknown>() }", ErrorCode::UnknownType),
        ("fn f() { same(1, true) }", ErrorCode::TypeMismatch),
        ("fn f(n: Int64) { id::<Int32>(n) }", ErrorCode::TypeMismatch),
        ("fn f() -> Int64 { id(1) }", ErrorCode::TypeMismatch),
        ("fn f() { id(2147483648) }", ErrorCode::TypeMismatch),
        ("fn f() { id::<UInt32>(-1) }", ErrorCode::TypeMismatch),
        ("fn f() { plain::<Int32>(1) }", ErrorCode::InvalidCall),
        ("fn f() { int32::<Int32>(1) }", ErrorCode::InvalidCall),
        (
            "enum E { V(Int32) } fn f() { E.V::<Int32>(1) }",
            ErrorCode::InvalidCall,
        ),
        (
            "fn f() @suspends { parallel { | id(1) } }",
            ErrorCode::InvalidCall,
        ),
        (
            "fn f() @suspends { parallel { | tag::<Bool>() } }",
            ErrorCode::InvalidCall,
        ),
        ("fn __joky_generic_0_id() {}", ErrorCode::ReservedName),
        ("fn id<T>(n: T) -> T { n }", ErrorCode::DuplicateDefinition),
    ] {
        let source = format!("{prelude}{body}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let error = VmEngine::with_mode(mode).load_source(&source).unwrap_err();
            assert_eq!(error.code(), code, "{source}: {error}");
            assert!(error.span().is_some());
        }
        let mut aot = crate::aot::AotCompiler::new("bad_generic_call").unwrap();
        let error = aot
            .compile_module(&crate::parser::parse(&source).unwrap())
            .unwrap_err();
        assert_eq!(error.code(), code, "{source}: {error}");
    }
}

#[test]
fn generic_instantiation_limits_stop_type_growth_without_rejecting_cached_recursion() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        // The rigid definition is valid, but no concrete instance is requested.
        vm.load_source("fn grow<T>(n: T) { grow((n,)) }").unwrap();
        vm.load_source("fn current() -> Int32 { 42 }").unwrap();
        let version = vm.module_version();
        let source = "fn grow<T>(n: T) { grow((n,)) } fn run() { grow({}) }";
        let error = vm.load_source(source).unwrap_err();
        assert_eq!(error.code(), ErrorCode::ResourceLimit);
        assert_eq!(
            error.message(),
            "Generated function expansion exceeds 32 dependency levels (requested depth 33)"
        );
        assert_eq!(error.span().unwrap().slice(source), Some("grow((n,))"));
        assert_eq!(vm.module_version(), version);
        assert_eq!(vm.call_int32_0("current").unwrap(), 42);
        let mut source = "fn discard<T>(x: T) { let _ = x }".to_owned();
        for i in 0..4097 {
            source.push_str(&format!(
                " struct R{i} {{}} fn use_{i}() {{ discard(R{i} {{}}) }}"
            ));
        }
        let error = vm.load_source(&source).unwrap_err();
        assert_eq!(error.code(), ErrorCode::ResourceLimit);
        assert_eq!(error.message(), "Generated function limit 4096 exceeded (4096 generic instances + 0 closure entries, requesting 1 more)");
    }
    let mut aot = crate::aot::AotCompiler::new("growing_generic").unwrap();
    let source = "fn grow<T>(n: T) { grow((n,)) } fn run() { grow({}) }";
    assert_eq!(
        aot.compile_module(&crate::parser::parse(source).unwrap())
            .unwrap_err()
            .code(),
        ErrorCode::ResourceLimit
    );
}

#[test]
fn generic_inference_handles_literal_tuples_unit_and_local_type_shadowing() {
    let source = r#"
        type T = Int32
        fn id<T>(x: T) -> T { x }
        fn head<T>(pair: (Int64, T)) -> T { let (_, x) = pair; x }
        fn pair<T, U>(x: (T, U), y: (U, T)) -> (T, U) { x }
        fn run() -> Int32 {
            let a = id(if true { 42 } else { 0 })
            let b = head((4294967295, true))
            let wide: Int64 = 4294967295
            let (c, d) = pair((1, wide), (2, 3))
            let unit = id(let local = 7)
            let empty = id(if true {} else {})
            let (e, f) = id::<((Int32,), Unit)>(((42,), {}))
            let (g,) = e
            let text = id::<Bool>(true)
            if a == 42 && b && c == 1 && d == wide && local == 7 && g == 42 && text { 42 } else { -1 }
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        assert_eq!(vm.call_int32_0("run").unwrap(), 42);
    }
}

thread_local! {
    static READY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static EVENTS: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
}
extern "C" fn record(n: u64) -> u64 {
    EVENTS.with(|events| events.borrow_mut().push(n));
    n
}
unsafe extern "C" fn delay(frame: *mut crate::runtime::cps::CpsFrame) -> i32 {
    let frame = &mut *frame;
    if frame.state == 0 {
        record(frame.slots[0]);
        frame.state = 1;
        return 1;
    }
    if !READY.with(std::cell::Cell::get) {
        return 1;
    }
    frame.result = frame.slots[0];
    0
}

#[test]
fn generic_pending_calls_keep_snapshots_failures_and_owners_after_reload_and_cancel() {
    use crate::runtime::{scheduler::Scheduler, suspension::ResumeError};
    let source = r#"
        extern fn record(n: UInt64) -> UInt64
        extern fn delay(n: UInt64) @suspends -> UInt64
        struct Pair { a: UInt64, b: UInt64 }
        fn id<T>(value: T) -> T { value }
        enum Deferred<T> { Empty, Value(T) }
            fn hold_enum<T>(value: Deferred<T>, n: UInt64) @suspends -> Deferred<T> { delay(n); value }
            fn delayed<T: Copy>(value: T, n: UInt64) @suspends -> T { let wrapped = hold_enum(Deferred.Value(value), n); match wrapped { Empty => value, Value(saved) => saved } }
        fn first<T>(a: T, b: T) -> T { a }
        fn failing<T>(value: T, n: UInt64) -> T { 1 << n; value }
        fn nested_failure<T>(value: T, n: UInt64) @suspends -> T { failing(value, delay(n)) }
        fn branch_task<T: Copy>(value: T, n: UInt64) @suspends { let _ = delayed(value, n); record(n); }
        fn bad_branch<T>(value: T) @suspends { nested_failure(value, 64); record(99); }
        fn pending(n: UInt64) @suspends -> UInt64 {
            var p = Pair { a: n, b: 1 }
            let old = first(p, delayed({ p.a = 0; p }, record(n)))
            let new = delayed(id(old), 3)
            new.a + new.b + p.a
        }
        fn failure(n: UInt64) @suspends -> UInt64 { nested_failure(id(42), n); record(99) }
        fn scoped() @suspends -> UInt64 { parallel { | branch_task(Pair { a: 1, b: 2 }, 7) | branch_task::<Unit>({}, 8) }; 42 }
        fn failed_scope() @suspends -> UInt64 { parallel { | bad_branch(false) | branch_task({}, 9) }; record(99) }
        fn sync(n: UInt64) -> UInt64 { failing(42, n); record(99) }
        fn lazy(n: UInt64) -> UInt64 { if true || failing(42, n) == 0 { 42 } else { 0 } }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        READY.with(|ready| ready.set(false));
        EVENTS.with(|events| events.borrow_mut().clear());
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_fn(
                "record",
                FunctionSignature::new(vec![Type::UInt64], Type::UInt64),
                record as *const u8,
            )
            .unwrap();
            vm.register_suspending_fn(
                "delay",
                FunctionSignature::new(vec![Type::UInt64], Type::UInt64),
                delay,
            )
            .unwrap();
        }
        vm.load_source(source).unwrap();
        let error = vm.call_uint64_1("sync", 64).unwrap_err();
        assert_eq!(error.span().unwrap().slice(source), Some("1 << n"));
        assert_eq!(vm.call_uint64_1("lazy", 64).unwrap(), 42);
        EVENTS.with(|events| assert!(events.borrow().is_empty()));
        let high = 0x8000_0000_ffff_ffff;
        let mut task = vm.call_async_uint64("pending", &[high]).unwrap();
        let mut failure = vm.call_async_uint64("failure", &[64]).unwrap();
        let mut scoped = vm.call_async_uint64("scoped", &[]).unwrap();
        let mut failed_scope = vm.call_async_uint64("failed_scope", &[]).unwrap();
        let mut scheduler = Scheduler::new();
        let cancelled = scheduler.suspend(vm.call_async_uint64("pending", &[7]).unwrap());
        for _ in 0..3 {
            task.resume().unwrap();
            failure.resume().unwrap();
            scoped.resume().unwrap();
            failed_scope.resume().unwrap();
            scheduler.resume(cancelled).unwrap();
        }
        let initial = [high, high, 64, 7, 8, 64, 9, 7, 7];
        EVENTS.with(|events| assert_eq!(*events.borrow(), initial));
        scheduler.cancel(cancelled).unwrap();
        vm.load_source("fn id<T>(value: T) -> T { value } fn pending(n: UInt64) @suspends -> UInt64 { id(n + 2) }").unwrap();
        assert_eq!(
            vm.call_async_uint64("pending", &[high])
                .unwrap()
                .cps_result(),
            Some(high + 2)
        );
        drop(vm);
        READY.with(|ready| ready.set(true));
        for _ in 0..3 {
            if !task.is_finished() {
                task.resume().unwrap();
            }
        }
        assert_eq!(task.cps_result(), Some(high + 1));
        for task in [&mut failure, &mut failed_scope] {
            let ResumeError::Failed(error) = task.resume().unwrap_err() else {
                panic!()
            };
            assert_eq!(error.code(), ErrorCode::RuntimeError);
            assert_eq!(error.span().unwrap().slice(source), Some("1 << n"));
            assert_eq!(task.cps_result(), None);
            assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
        }
        scoped.resume().unwrap();
        assert_eq!(scoped.cps_result(), Some(42));
        assert_eq!(scheduler.resume_all().unwrap(), 0);
        let mut expected = initial.to_vec();
        expected.extend([3, 7, 8]);
        EVENTS.with(|events| assert_eq!(*events.borrow(), expected));
    }
}

#[test]
fn generic_recursion_uses_iterative_interpreter_frames_on_a_small_native_stack() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            for attribute in ["", "@suspends"] {
                let mut vm = VmEngine::new_interpreter();
                vm.set_interpreter_limits(super::InterpreterLimits {
                    max_frames: 3000,
                    ..Default::default()
                })
                .unwrap();
                let source = format!(
                    "fn descend<T>(x: T, n: Int32) {attribute} -> T {{ if n == 0 {{ x }} else {{ descend(x, n - 1) }} }}
                     fn run(n: Int32) {attribute} -> Int32 {{ let (x, valid) = descend((42, true), n); if valid {{ x }} else {{ 0 }} }}"
                );
                vm.load_source(&source).unwrap();
                if attribute.is_empty() {
                    assert_eq!(vm.call_int32_1("run", 2000).unwrap(), 42);
                } else {
                    assert_eq!(vm.call_async("run", &[2000]).unwrap().cps_result(), Some(42));
                }
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn generic_aggregate_recursion_preserves_poll_and_resource_limits() {
    use super::InterpreterLimits;
    use crate::runtime::suspension::ResumeError;
    let mut vm = VmEngine::new_interpreter();
    vm.load_source(
        "fn descend<T>(x: T, n: Int32) @suspends -> T { if n == 0 { x } else { descend(x, n - 1) } }
         fn run(n: Int32) @suspends -> Int32 { let (x, valid) = descend((42, true), n); if valid { x } else { 0 } }",
    ).unwrap();
    vm.set_interpreter_limits(InterpreterLimits {
        max_frames: 64,
        instruction_budget: Some(5000),
        poll_budget: Some(1),
    })
    .unwrap();
    let mut task = vm.call_async("run", &[20]).unwrap();
    let mut polls = 0;
    while !task.is_finished() {
        task.resume().unwrap();
        polls += 1;
        assert!(polls < 5000);
    }
    assert!(polls > 20);
    assert_eq!(task.cps_result(), Some(42));
    for limits in [
        InterpreterLimits {
            max_frames: 4,
            instruction_budget: None,
            poll_budget: Some(1),
        },
        InterpreterLimits {
            max_frames: 64,
            instruction_budget: Some(20),
            poll_budget: Some(1),
        },
    ] {
        vm.set_interpreter_limits(limits).unwrap();
        let mut task = vm.call_async("run", &[20]).unwrap();
        for _ in 0..5000 {
            if let Err(ResumeError::Failed(error)) = task.resume() {
                assert_eq!(error.code(), ErrorCode::ResourceLimit);
                break;
            }
            assert!(!task.is_finished());
        }
        assert!(task.is_finished());
        assert_eq!(task.cps_result(), None);
        assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
    }
}

#[test]
fn generic_templates_need_concrete_wrappers_at_native_function_boundaries() {
    let invalid = "import joky/vm fn id<T>(x: T) -> T { x }
        fn run() { let child = vm.Vm.new(); vm.register_int32_1(child, \"id\", id) }";
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        assert!(VmEngine::with_mode(mode).load_source(invalid).is_err());
    }
    let mut aot = crate::aot::AotCompiler::new("generic_reference").unwrap();
    assert!(aot
        .compile_module(&crate::parser::parse(invalid).unwrap())
        .is_err());
    let mut vm = VmEngine::new();
    vm.load_source(
        r#"
        import joky/vm
        fn id<T>(x: T) -> T { x }
        fn concrete(x: Int32) -> Int32 { id(x) }
        fn run() -> Int32 {
            let child = vm.Vm.interpreter()
            { let vm_text = "id"; vm.register_int32_1(child, &vm_text, concrete) }
            { let vm_text = "extern fn id(x: Int32) -> Int32 fn run() -> Int32 {{ id(42) }}"; vm.load_source(child, &vm_text) }
            let result = { let vm_text = "run"; vm.call_int32_0(child, &vm_text) }
            vm.close(child)
            result
        }
    "#,
    )
    .unwrap();
    assert_eq!(vm.call_int32_0("run").unwrap(), 42);
}

#[test]
fn generic_parameters_are_rejected_by_host_registration_without_replacing_entries() {
    use crate::parser::ast::TypeParameter;
    let parameter = Type::Parameter(std::sync::Arc::new(TypeParameter::new(
        "T".into(),
        crate::Span::empty(0),
    )));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_fn(
                "host",
                FunctionSignature::new(vec![Type::UInt64], Type::UInt64),
                record as *const u8,
            )
            .unwrap();
            for signature in [
                FunctionSignature::new(vec![parameter.clone()], Type::UInt64),
                FunctionSignature::new(vec![Type::UInt64], parameter.clone()),
            ] {
                assert_eq!(
                    vm.register_fn("host", signature.clone(), record as *const u8)
                        .unwrap_err()
                        .code(),
                    ErrorCode::AbiError
                );
                assert_eq!(
                    vm.register_suspending_fn("host", signature, delay)
                        .unwrap_err()
                        .code(),
                    ErrorCode::AbiError
                );
            }
        }
        vm.load_source(
            "extern fn host(n: UInt64) -> UInt64 fn run(n: UInt64) -> UInt64 { host(n) }",
        )
        .unwrap();
        assert_eq!(vm.call_uint64_1("run", u64::MAX).unwrap(), u64::MAX);
    }
}

#[test]
fn generic_copy_contracts_reject_invalid_templates_before_instantiation() {
    for (source, message) in [
        ("fn bad<T>(x: T) -> (T, T) { (x, x) }", "moved"),
        ("fn bad<T>(x: T) -> T { let alias = x; x }", "moved"),
        ("fn bad<T>(x: &T) -> T { *x }", "Copy"),
        ("fn duplicate<T: Copy>(x: T) -> (T, T) { (x, x) } fn bad<T>(x: T) -> (T, T) { duplicate(x) }", "Copy bound"),
        ("fn consume<T>(x: T) {} fn bad<T>(x: T, n: Int32) { var i = 0; while i < n { consume(x); i += 1 } }", "moved"),
        ("fn bad<T>(x: T, flag: Bool) -> T { if flag { let _ = x }; x }", "moved"),
        ("struct Box<T> { value: T } fn bad<T>(x: Box<T>) -> (Box<T>, Box<T>) { (x, x) }", "moved"),
        ("trait Item { type Value } fn bad<T: Item>(x: T.Value) -> (T.Value, T.Value) { (x, x) }", "moved"),
        ("fn duplicate<T: Copy>(x: T) -> (T, T) { (x, x) } fn bad() { let pair = duplicate(\"owned\") }", "Copy bound"),
        ("struct Box<T: Copy> { value: T } fn bad(x: Box<String>) {}", "Copy bound"),
        ("enum Choice<T: Copy> { Empty, Value(T) } fn bad(x: Choice<String>) {}", "Copy bound"),
        ("type Pair<T: Copy> = (T, T) fn bad(x: Pair<String>) {}", "Copy bound"),
        ("trait Mark {} impl<T: Copy> Mark for T {} fn need<T: Mark>(x: T) {} fn bad() { need(\"owned\") }", "does not satisfy trait bound"),
        ("struct Box<T> { value: T } impl<T: Copy> Box<T> { fn copied(&self) -> T { self.value } } fn bad(x: &Box<String>) { x.copied() }", "Copy bound"),
        ("fn bad<F: Copy + Fn() -> Int32>(f: F) {}", "Copy cannot be combined"),
        ("import joky/fixed_array.{type FixedArray} fn bad<T>(values: FixedArray<T, 2>) -> T { let [first, last] = values; first }", "Copy"),
    ] {
        let error = super::support::frontend_error(source, &crate::ModuleSources::new());
        assert!(error.message().contains(message), "{source}: {error}");
    }
}
