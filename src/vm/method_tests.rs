use super::support::{with_suspends, Suspends};
use super::{VmEngine, VmMode};
use crate::{ErrorCode, FunctionSignature, Type};

#[test]
fn concrete_branch_hints_precede_defaults_in_methods_and_generics() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(
                include_str!("../../tests/fixtures/inference.jk"),
                suspends,
                Suspends::All,
            );
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(&source).unwrap();
            let result = if suspends {
                vm.call_async("check", &[]).unwrap().cps_result().unwrap() as i32
            } else {
                vm.call_int32_0("check").unwrap()
            };
            assert_eq!(result, 42, "{mode:?}, suspends={suspends}");
        }
    }
}

#[test]
fn inherent_methods_constants_and_checked_conversions_match_backends() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = include_str!("../../tests/fixtures/methods.jk");
            let source = with_suspends(source, suspends, Suspends::Typed);
            let mut vm = VmEngine::with_mode(mode);
            let exports = vm.load_source(&source).unwrap();
            assert!(!exports.iter().any(|n| n.contains('.')));
            let result = if suspends {
                vm.call_async("check", &[]).unwrap().cps_result().unwrap() as i32
            } else {
                vm.call_int32_0("check").unwrap()
            };
            assert_eq!(result, 42, "{mode:?}, suspends={suspends}");
            let wide = if suspends {
                vm.call_async_uint64("wide", &[])
                    .unwrap()
                    .cps_result()
                    .unwrap()
            } else {
                vm.call_uint64_0("wide").unwrap()
            };
            assert_eq!(wide, u64::MAX);
            assert_eq!(vm.function_signature("Int32.to_u64"), None);
        }
    }
}

#[test]
fn checked_integer_methods_cover_all_sixteen_pairs_and_boundaries() {
    let source = include_str!("../../tests/fixtures/checked_conversions.jk");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let text = with_suspends(source, suspends, Suspends::Named("check"));
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(&text).unwrap();
            let result = if suspends {
                vm.call_async("check", &[]).unwrap().cps_result().unwrap() as i32
            } else {
                vm.call_int32_0("check").unwrap()
            };
            assert_eq!(
                result, 0,
                "{mode:?}, suspends={suspends}, boundary case={result}"
            );
        }
    }
}

#[test]
fn inherent_methods_and_constants_reject_invalid_definitions_even_unused() {
    let cases = [
        (
            "impl Int32 { fn f(self) -> Bool { self } }",
            ErrorCode::TypeMismatch,
        ),
        ("impl Unknown {}", ErrorCode::TypeMismatch),
        ("struct P {} impl P { fn f() -> Int32 { 42 } } fn bad<P>(value: P) -> Int32 { (P).f() }", ErrorCode::TypeMismatch),
        ("impl Ptr {}", ErrorCode::TypeMismatch),
        ("impl vm.Function {}", ErrorCode::TypeMismatch),
        ("type Alias = Int32 impl Alias {}", ErrorCode::TypeMismatch),
        (
            "impl Int32 { fn f(self) {} } impl Int32 { fn f() {} }",
            ErrorCode::DuplicateDefinition,
        ),
        (
            "impl Int32 { fn f(self) {} const f: Int32 = 1 }",
            ErrorCode::DuplicateDefinition,
        ),
        (
            "enum E { A } impl E { fn A() {} }",
            ErrorCode::DuplicateDefinition,
        ),
        (
            "impl Int32 { const BAD: Int32 = true }",
            ErrorCode::TypeMismatch,
        ),
        (
            "impl Int32 { const BAD: Int32 = 2147483648 }",
            ErrorCode::TypeMismatch,
        ),
        (
            "impl Int32 { const BAD: String = \"text\" }",
            ErrorCode::TypeMismatch,
        ),
        (
            "impl Int32 { const A: Int32 = Int32.B const B: Int32 = Int32.A }",
            ErrorCode::InvalidState,
        ),
        (
            "impl Int32 { const A: Int32 = 1 / 0 }",
            ErrorCode::InvalidState,
        ),
        (
            "impl Int32 { const A: Int32 = 1 << 32 }",
            ErrorCode::InvalidState,
        ),
        (
            "impl Int32 { const A: Int32 = 1 << -1 }",
            ErrorCode::InvalidState,
        ),
        (
            "fn f() -> Int32 { 1 } impl Int32 { const A: Int32 = f() }",
            ErrorCode::InvalidState,
        ),
        (
            "impl Int32 { const A: Int32 = Int32.MISSING }",
            ErrorCode::UnknownName,
        ),
        (
            "impl Int32 { fn f() {} } fn test() { let n = 1; n.f() }",
            ErrorCode::InvalidCall,
        ),
        (
            "impl Int32 { fn f(self) {} } fn test() { Int32.f() }",
            ErrorCode::InvalidCall,
        ),
        (
            "impl Int32 { fn f(self) {} } fn test() { true.f() }",
            ErrorCode::UnknownName,
        ),
        (
            "impl Int32 { fn f(self) {} } fn test() { 1.f::<Int32>() }",
            ErrorCode::InvalidCall,
        ),
        (
            "impl Int32 { fn f(self) {} } fn test<T>(n: T) { n.f() }",
            ErrorCode::UnknownName,
        ),
        (
            "impl Int32 { fn f(self) @suspends {} } fn test() { 1.f() }",
            ErrorCode::InvalidCall,
        ),
        (
            "import joky/integer enum Option<T> { None, Some(T) }",
            ErrorCode::DuplicateDefinition,
        ),
        (
            "import joky/integer import joky/integer",
            ErrorCode::InvalidImport,
        ),
        (
            "import joky/integer fn test() { Int32.MAX = 1 }",
            ErrorCode::UnknownName,
        ),
        (
            "import joky/integer fn test() { Int32.MAX() }",
            ErrorCode::UnknownName,
        ),
        (
            "import joky/integer fn test() -> Option<UInt64> { 2147483648.to_u64() }",
            ErrorCode::TypeMismatch,
        ),
    ];
    for (source, code) in cases {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source("fn current() -> Int32 { 42 }").unwrap();
            let error = vm.load_source(source).unwrap_err();
            assert_eq!(error.code(), code, "{source}: {error}");
            assert!(error.span().is_some(), "{source}");
            assert_eq!(vm.call_int32_0("current").unwrap(), 42);
        }
        let mut aot = crate::aot::AotCompiler::new("bad_method").unwrap();
        let error = aot
            .compile_module(&crate::parser::parse(source).unwrap())
            .unwrap_err();
        assert_eq!(error.code(), code, "{source}: {error}");
    }
}

#[test]
fn method_receivers_preserve_snapshot_order_context_shadowing_and_recursion() {
    let source = r#"
        import joky/integer
        struct Pair { x: Int32 }
        impl Pair {
            fn add(self, other: Int32) -> Int32 { self.x + other }
            fn recurse(self, n: Int32) -> Int32 { if n == 0 { self.x } else { self.recurse(n - 1) } }
        }
        impl Int32 { fn add(self, other: Int32) -> Int32 { self + other } }
        fn check() -> Int32 {
            var pair = Pair { x: 20 }
            if pair.add({ pair.x = 1; 22 }) != 42 { return 1 }
            if pair.x != 1 { return 2 }
            if { pair.x = 2; pair }.add(40) != 42 { return 3 }
            if pair.x.add({ pair.x = 1; 40 }) != 42 { return 4 }
            let Pair = pair
            if Pair.add(41) != 42 { return 5 }
            if Pair.recurse(20) != 1 { return 6 }
            negative_conversion()
        }
        fn negative_conversion() -> Int32 {
            match (-1).to_u64() { None => 42, Some(n) => 0 }
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        assert_eq!(vm.call_int32_0("check").unwrap(), 42);
    }
}

thread_local! {
    static READY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) };
}
extern "C" fn record(n: i32) -> i32 {
    EVENTS.with(|events| events.borrow_mut().push(n));
    n
}
unsafe extern "C" fn delay(frame: *mut crate::runtime::cps::CpsFrame) -> i32 {
    let frame = &mut *frame;
    if frame.state == 0 {
        record(frame.slots[0] as i32);
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
fn methods_preserve_pending_receivers_snapshots_failures_scopes_and_owners() {
    use crate::runtime::{scheduler::Scheduler, suspension::ResumeError};
    let source = r#"
        import joky/integer
        extern fn record(n: Int32) -> Int32
        extern fn delay(n: Int32) @suspends -> Int32
        struct Pair { x: Int32 }
        impl Pair {
            fn add(self, other: Int32) @suspends -> Pair {
                Pair { x: self.x + delay(other) }
            }
        }
        impl Int32 {
            fn receive(self, other: Int32) @suspends -> Option<UInt64> {
                delay(other)
                self.to_u64()
            }
            fn branch_task(self) @suspends { record(delay(self)); }
            fn fail(self) @suspends -> Int32 { let divisor = delay(self); 1 / divisor }
        }
        fn pending(n: Int32) @suspends -> Int32 {
            var pair = Pair { x: record(n) }
            let result = pair.add({ pair.x = 99; record(2) })
            result.x + pair.x
        }
        fn receiver() @suspends -> Int32 {
            match delay(42).receive(record(7)) { Some(n) => int32(n), None => -1 }
        }
        fn failure() @suspends -> Int32 { 0.fail() + record(99) }
        fn scoped() @suspends -> Int32 { parallel { | 1.branch_task() | 2.branch_task() }; 42 }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        READY.with(|v| v.set(false));
        EVENTS.with(|v| v.borrow_mut().clear());
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_fn(
                "record",
                FunctionSignature::new(vec![Type::Int32], Type::Int32),
                record as *const u8,
            )
            .unwrap();
            vm.register_suspending_fn(
                "delay",
                FunctionSignature::new(vec![Type::Int32], Type::Int32),
                delay,
            )
            .unwrap();
        }
        vm.load_source(source).unwrap();
        let mut pending = vm.call_async("pending", &[40]).unwrap();
        let mut receiver = vm.call_async("receiver", &[]).unwrap();
        let mut failure = vm.call_async("failure", &[]).unwrap();
        let mut scoped = vm.call_async("scoped", &[]).unwrap();
        let mut scheduler = Scheduler::new();
        let cancelled = scheduler.suspend(vm.call_async("pending", &[5]).unwrap());
        for _ in 0..3 {
            pending.resume().unwrap();
            receiver.resume().unwrap();
            failure.resume().unwrap();
            scoped.resume().unwrap();
            scheduler.resume(cancelled).unwrap();
        }
        let initial = [40, 2, 2, 42, 0, 1, 2, 5, 2, 2];
        EVENTS.with(|v| assert_eq!(*v.borrow(), initial));
        scheduler.cancel(cancelled).unwrap();
        vm.load_source("fn pending(n: Int32) -> Int32 { n }")
            .unwrap();
        drop(vm);
        READY.with(|v| v.set(true));
        for _ in 0..5 {
            if !pending.is_finished() {
                pending.resume().unwrap();
            }
            if !receiver.is_finished() {
                receiver.resume().unwrap();
            }
            if !scoped.is_finished() {
                scoped.resume().unwrap();
            }
        }
        assert_eq!(pending.cps_result(), Some(141));
        assert_eq!(receiver.cps_result(), Some(42));
        assert_eq!(scoped.cps_result(), Some(42));
        let ResumeError::Failed(error) = failure.resume().unwrap_err() else {
            panic!()
        };
        assert_eq!(error.span().unwrap().slice(source), Some("1 / divisor"));
        assert_eq!(failure.cps_result(), None);
        assert_eq!(scheduler.resume_all().unwrap(), 0);
        let mut expected = initial.to_vec();
        expected.extend([7, 7, 1, 2]);
        EVENTS.with(|v| assert_eq!(*v.borrow(), expected));
    }
}

#[test]
fn associated_constants_fold_scalar_operators_and_bound_dependencies() {
    let source = r#"
        import joky/integer
        impl Bool {
            const YES: Bool = !false == true
            const WRAP: UInt64 = uint64(-1)
            const MIN_DIV: Int32 = Int32.MIN / -1
            const MIN_NEG: Int32 = -Int32.MIN
            const SHIFT: UInt32 = (1 << 31) | 3
            const MASK: UInt64 = ~uint64(0)
            const OVERFLOW: Int32 = Int32.MAX + 1
        }
        fn check() -> Int32 {
            if !Bool.YES { return 1 }
            if Bool.WRAP != UInt64.MAX { return 2 }
            if Bool.MIN_DIV != Int32.MIN { return 3 }
            if Bool.MIN_NEG != Int32.MIN { return 4 }
            if Bool.SHIFT != 2147483651 { return 5 }
            if Bool.MASK != UInt64.MAX { return 6 }
            if Bool.OVERFLOW != Int32.MIN { return 7 }
            42
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        assert_eq!(vm.call_int32_0("check").unwrap(), 42);
        let mut deep = String::from("impl Int32 { const C0: Int32 = 1 ");
        for i in 1..33 {
            deep += &format!("const C{i}: Int32 = Int32.C{} ", i - 1);
        }
        deep += "}";
        let error = vm.load_source(&deep).unwrap_err();
        assert_eq!(error.code(), ErrorCode::ResourceLimit);
        let mut shared = String::from("impl Int32 { const C0: Int32 = 0 ");
        for i in 1..32 {
            shared += &format!("const C{i}: Int32 = Int32.C{p} + Int32.C{p} ", p = i - 1);
        }
        shared += "} fn check() -> Int32 { Int32.C31 }";
        vm.load_source(&shared).unwrap();
        assert_eq!(vm.call_int32_0("check").unwrap(), 0);
    }
}

#[test]
fn methods_resolve_local_vm_names_and_calls_inside_implementations() {
    let source = r#"
        import joky/vm
        import joky/integer
        struct P { n: Int32 }
        impl P {
            const ANSWER: Int32 = 42
            fn call_int32(self, n: Int32) -> Int32 { self.n + n }
            fn register_int32(self) -> Int32 { self.n }
            fn use_vm(self) -> Int32 {
                let handle = vm.Vm.interpreter()
                { let vm_text = "fn answer() -> Int32 {{ 42 }}"; vm.load_source(handle, &vm_text) }
                let result = { let vm_text = "answer"; vm.call_int32_0(handle, &vm_text) }
                vm.close(handle)
                result
            }
        }
        enum E { A }
        impl E { fn answer(self) -> Int32 { 42 } }
        fn local(vm: P) -> Int32 { vm.call_int32(1) }
        fn check() -> Int32 {
            let vm = P { n: 41 }
            if vm.call_int32(1) != 42 { return 1 }
            if vm.register_int32() != 41 { return 2 }
            if local(vm) != 42 { return 3 }
            if vm.use_vm() != 42 { return 4 }
            if E.A.answer() != 42 { return 5 }
            match P.ANSWER.to_u64() { Some(n) => int32(n), None => -1 }
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        assert_eq!(vm.call_int32_0("check").unwrap(), 42);
    }
}

#[test]
fn methods_compile_and_recurse_on_a_small_native_stack() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let source = r#"
            import joky/integer
            struct P { n: Int32 }
            impl P {
                fn descend(self, n: Int32) -> Int32 {
                    if n == 0 { self.n } else { self.descend(n - 1) }
                }
            }
            fn check(n: Int32) -> Int32 { P { n: 42 }.descend(n) }
        "#;
            let mut vm = VmEngine::new_interpreter();
            vm.set_interpreter_limits(super::InterpreterLimits {
                max_frames: 6000,
                ..Default::default()
            })
            .unwrap();
            vm.load_source(source).unwrap();
            assert_eq!(vm.call_int32_1("check", 5000).unwrap(), 42);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn impl_parser_preserves_spans_and_rejects_unsupported_forms() {
    let source = "// 🙂\nimpl Int32 { const MAX: Int32 = 2147483647 fn add(self, n: Int32,) -> Self { self + n } } fn check() -> Int32 { (1).add(41) }";
    let ast = crate::parser::parse(source).unwrap();
    let implementation = &ast.implementations[0];
    assert_eq!(implementation.target, Type::Int32);
    assert_eq!(
        implementation.constants[0].span.slice(source),
        Some("const MAX: Int32 = 2147483647")
    );
    assert_eq!(implementation.methods[0].params[0].ty, Type::Int32);
    let crate::parser::ast::Item::FnDef(function) = &ast.items[0] else {
        panic!()
    };
    let crate::parser::ast::Expr::Block { exprs, .. } = &function.body else {
        panic!()
    };
    assert_eq!(exprs[0].span().slice(source), Some("(1).add(41)"));
    for source in [
        "impl Int32 { fn f(n: Int32, self) {} }",
        "impl Int32 { fn f(self: Int32) {} }",
        "impl Int32 { fn f<Self>(self, value: Self) {} }",
        "impl<T: Copy +> Int32 {}",
        "impl Option<Int32> {}",
        "impl (Int32, Int32) {}",
        "impl Int32 { extern fn f() }",
        "impl Int32 { const VALUE = 1 }",
        "impl Int32 { fn f(self) {} } import joky/vm",
        "impl Int32 { fn f(Self: Int32) {} }",
        "impl Int32 { fn f(self) { let Self = 1 } }",
        "fn f() { let impl = 1 }",
        "fn const() {}",
        "impl Int32 { fn f(self other: Int32) {} }",
        "impl Int32 { fn f(self) {} } fn run() { (1).f(1 2) }",
    ] {
        assert!(crate::parser::parse(source).is_err(), "{source}");
    }
}

#[test]
fn source_integer_library_normalizes_once_and_keeps_nominal_identity() {
    let source = "// 🙂\nimport joky/integer\nfn f(n: Int32) -> Option<UInt64> { n.to_u64() }";
    let types = crate::TypeRegistry::new().with_prelude(false);
    let first = types
        .resolve_aliases(&crate::parser::parse(source).unwrap())
        .unwrap();
    let twice = types.resolve_aliases(&first).unwrap();
    assert!(first.imports.is_empty());
    assert_eq!(first.enums.len(), 1);
    assert_eq!(twice.enums.len(), 1);
    assert_eq!(twice.implementations.len(), 10);
    let ret = |m: &crate::parser::ast::Module| {
        let crate::parser::ast::Item::FnDef(f) = &m.items[0] else {
            panic!()
        };
        f.ret_ty.clone()
    };
    assert_eq!(ret(&first), ret(&twice));
    let reload = types
        .resolve_aliases(&crate::parser::parse(source).unwrap())
        .unwrap();
    assert_eq!(ret(&first), ret(&reload));
    // The appended enum keeps its stdlib source identity across reloads.
    assert_eq!(first.enums[0].span, reload.enums[0].span);
    let file = first.sources.get(first.enums[0].span.source_id()).unwrap();
    assert_eq!(file.name(), "joky/option.jk");
    assert!(!types.contains("Option"));
    let mut registry = crate::core::resolver::BindingRegistry::default();
    assert_eq!(
        registry
            .register_module("joky/integer", [])
            .unwrap_err()
            .code(),
        ErrorCode::DuplicateDefinition
    );
}

#[test]
fn option_type_import_exposes_the_enum_constructor_namespace() {
    let source = "import joky/option.{type Option} fn value() -> Option<Int32> { Option.Some(7) } fn check() -> Int32 { match value() { Option.Some(n) => n, Option.None => 0 } }";
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        assert_eq!(vm.call_int32_0("check").unwrap(), 7, "{mode:?}");
    }
}

#[test]
fn option_value_imports_enable_bare_enum_constructors() {
    let source = "import joky/option.{type Option, Some, None} fn value() -> Option<Int32> { Some(7) } fn check() -> Int32 { match value() { Some(n) => n, None => 0 } }";
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        assert_eq!(vm.call_int32_0("check").unwrap(), 7, "{mode:?}");
    }
}
