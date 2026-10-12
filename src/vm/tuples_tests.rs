//! Tuple layout, destructuring, and suspension parity tests.

use super::support::*;
use super::VmMode;
use crate::runtime::{scheduler::Scheduler, suspension::ResumeError};
use crate::{ErrorCode, FunctionSignature};

#[test]
fn tuples_preserve_structure_snapshots_and_bindings_in_direct_and_cps_backends() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(
                include_str!("../../tests/fixtures/tuples.jk"),
                suspends,
                Suspends::Typed,
            );
            let vm = parity_vm(&source, mode);
            for (name, expected) in [
                ("basic", 735),
                ("nested", 5),
                ("snapshots", 1934),
                ("mutable_leaves", 18567),
                ("shadowing", 225),
                ("effects", 1114),
                ("sequence_boundaries", 12),
                ("zero_calls", 42),
                ("calls", 91),
                ("loops", 85),
                ("abrupt", 3),
                ("compute", 22813),
            ] {
                let value = if suspends {
                    vm.call_async(name, &[]).unwrap().cps_result().unwrap() as i32
                } else {
                    vm.call_int32_0(name).unwrap()
                };
                assert_eq!(value, expected, "{mode:?} suspends={suspends} {name}");
            }
        }
    }
}

#[test]
fn invalid_tuples_and_binding_patterns_preserve_the_previous_module() {
    let cases = [
        (
            "fn f() -> (Int32,) { (true,) }",
            ErrorCode::TypeMismatch,
            "Return type mismatch",
        ),
        (
            "fn f() -> (Int32,) { 1 }",
            ErrorCode::TypeMismatch,
            "Return type mismatch",
        ),
        (
            "fn f() -> Int32 { (1,) }",
            ErrorCode::TypeMismatch,
            "Return type mismatch",
        ),
        (
            "fn f() { let (x,) = 1 }",
            ErrorCode::TypeMismatch,
            "requires a tuple",
        ),
        (
            "fn f() { let (x, y) = (1,) }",
            ErrorCode::TypeMismatch,
            "has 2 elements",
        ),
        (
            "fn f() { let (x,) = (1, 2) }",
            ErrorCode::TypeMismatch,
            "has 1 elements",
        ),
        (
            "fn f() { let (x, (y,)) = (1, 2) }",
            ErrorCode::TypeMismatch,
            "requires a tuple",
        ),
        (
            "fn f() { let (x, x) = (1, 2) }",
            ErrorCode::DuplicateDefinition,
            "Duplicate binding",
        ),
        (
            "fn f() { var (x, (x,)) = (1, (2,)) }",
            ErrorCode::DuplicateDefinition,
            "Duplicate binding",
        ),
        (
            "fn f() { let (x, y) = (1, 2) x = 3 }",
            ErrorCode::InvalidAssignment,
            "immutable",
        ),
        (
            "fn f() { var (x, y) = (1, 2) x = true }",
            ErrorCode::TypeMismatch,
            "Assignment type",
        ),
        (
            "fn f() { let (x, y): (Int32, Bool) = (1, 2) }",
            ErrorCode::TypeMismatch,
            "annotation expects",
        ),
        (
            "fn f() { let _: Bool = 1 }",
            ErrorCode::TypeMismatch,
            "annotation expects",
        ),
        (
            "fn f() { let _ = 1 _; }",
            ErrorCode::UnknownName,
            "Undefined variable",
        ),
        (
            "fn f() { let (_, _) = (1, 2) _ = 3 }",
            ErrorCode::UnknownName,
            "Undefined variable",
        ),
        (
            "fn f() { { let (x, y) = (1, 2) } x; }",
            ErrorCode::UnknownName,
            "Undefined variable",
        ),
        (
            "fn f() { let (x, y) = (1, x) }",
            ErrorCode::UnknownName,
            "Undefined variable",
        ),
        (
            "fn f(h: vm.Handle) { let _ = (h,) }",
            ErrorCode::TypeMismatch,
            "copyable aggregate",
        ),
        (
            "fn f(h: vm.Handle) { let _ = (h,) }",
            ErrorCode::TypeMismatch,
            "copyable aggregate",
        ),
        (
            "fn f(p: (vm.Handle,)) {}",
            ErrorCode::TypeMismatch,
            "copyable aggregate",
        ),
        (
            "type Bad = (Missing,)",
            ErrorCode::UnknownType,
            "Unknown type",
        ),
        (
            "type Bad = (vm.Handle,)",
            ErrorCode::TypeMismatch,
            "copyable aggregate",
        ),
        ("type A = (A,)", ErrorCode::CyclicTypeAlias, "A -> A"),
        (
            "type A = (Int32, B) type B = (A,)",
            ErrorCode::CyclicTypeAlias,
            "A -> B -> A",
        ),
        (
            "struct R { pair: (R,) }",
            ErrorCode::RecursiveRecord,
            "R -> R",
        ),
        ("enum E { More((E,)) }", ErrorCode::RecursiveType, "E -> E"),
        (
            "struct R { pair: (Alias,) } type Alias = R",
            ErrorCode::RecursiveRecord,
            "R -> Alias -> R",
        ),
        (
            "enum E { More((R,)) } struct R { value: E }",
            ErrorCode::RecursiveType,
            "Infinite inline",
        ),
        (
            "struct R { pair: (vm.Handle,) }",
            ErrorCode::TypeMismatch,
            "copyable aggregate",
        ),
        (
            "enum E { Value((vm.Handle,)) }",
            ErrorCode::TypeMismatch,
            "copyable aggregate",
        ),
        (
            "struct R {} struct S {} fn f() -> (R,) { (S {},) }",
            ErrorCode::TypeMismatch,
            "Return type mismatch",
        ),
        (
            "fn f() -> Bool { (1,) == (1,) }",
            ErrorCode::TypeMismatch,
            "Comparison operands",
        ),
        (
            "fn f() { let p = (1,) p.x; }",
            ErrorCode::TypeMismatch,
            "has no fields",
        ),
        (
            "extern fn host(p: (Int32, Bool))",
            ErrorCode::AbiError,
            "host ABI",
        ),
        (
            "extern fn host() @suspends -> (Unit,)",
            ErrorCode::AbiError,
            "host ABI",
        ),
        (
            "fn f() -> (Int32, Int32) { (return (1, 2), 3) }",
            ErrorCode::UnreachableCode,
            "Unreachable expression",
        ),
        (
            "fn f() -> (Int32, Int32) { if true { (1, 2) } else { (1, false) } }",
            ErrorCode::TypeMismatch,
            "same type",
        ),
        (
            "fn inner() @suspends -> (Int32,) { (1,) } fn f() { let (x,) = inner() }",
            ErrorCode::InvalidCall,
            "@suspends",
        ),
    ];
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm("fn current() -> Int32 { 42 }", mode);
        let version = vm.module_version();
        for (source, code, message) in cases {
            let source = format!("// 🙂\n{source}");
            let error = vm.load_source(&source).unwrap_err();
            assert_eq!(error.code(), code, "{mode:?}: {source}: {error}");
            assert!(error.message().contains(message), "{source}: {error}");
            assert!(
                error
                    .span()
                    .is_some_and(|span| span.slice(&source).is_some()),
                "{error}"
            );
            let mut aot = crate::aot::AotCompiler::new("invalid_tuple").unwrap();
            let error = aot
                .compile_module(&crate::parser::parse(&source).unwrap())
                .unwrap_err();
            assert_eq!(error.code(), code, "AOT: {source}: {error}");
            assert_eq!(vm.module_version(), version);
            assert_eq!(vm.call_int32_0("current").unwrap(), 42);
        }
    }
}

#[test]
fn tuple_construction_and_destructuring_resume_once_after_reload_and_cancellation() {
    let source = r#"
        struct Marker { value: Int32 }
        extern fn delayed_value(x: Int32) @suspends -> Int32
        extern fn record(x: Int32) -> Int32
        fn original() -> (Marker,) { (Marker { value: 1 },) }
        fn direct(n: Int32) -> (Int32, Bool) { (n, true) }
        fn build(n: Int32) @suspends -> (Int32, Bool) { (delayed_value(record(n)), true) }
        fn empty() @suspends -> (Unit, Unit) { (delayed_value(0);, {}) }
        fn pending() @suspends -> Int32 {
            var n = 1
            var (saved, (value, flag), unit) = (n, build({ n = 9 7 }), {})
            saved = saved + 1
            let (again, _) = direct(value)
            let _ = empty()
            if flag { saved * 100 + again * 10 + n } else { 0 }
        }
        fn failing() @suspends -> Int32 {
            let (value, _) = build(-1)
            let (_, _) = (1 / value, record(99))
            42
        }
        fn child(pair: (Int32, Bool)) @suspends {
            let (n, flag) = pair
            let (value, _) = build(n)
            if flag { record(value); }
        }
        fn scoped() @suspends -> Int32 {
            parallel { | child((2, true)) | child((3, true)) }
            42
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm(source, mode);
        let old_type = vm.function_signature("original").unwrap().ret;
        let mut task = vm.call_async("pending", &[]).unwrap();
        let mut failing = vm.call_async("failing", &[]).unwrap();
        let mut scoped = vm.call_async("scoped", &[]).unwrap();
        let mut scheduler = Scheduler::new();
        let cancelled = scheduler.suspend(vm.call_async("pending", &[]).unwrap());
        for _ in 0..3 {
            task.resume().unwrap();
            failing.resume().unwrap();
            scoped.resume().unwrap();
            scheduler.resume(cancelled).unwrap();
        }
        EVENTS.with(|events| assert_eq!(*events.borrow(), [7, 7, -1, -1, 2, 2, 3, 3, 7, 7]));
        vm.load_source(
            "struct Marker { value: Int32 } fn original() -> (Marker,) { (Marker { value: 2 },) }",
        )
        .unwrap();
        assert_ne!(old_type, vm.function_signature("original").unwrap().ret);
        assert!(vm.call_int32_0("original").is_err());
        assert!(vm.call_async("original", &[]).is_err());
        let error = unsafe {
            vm.register_fn(
                "host_tuple",
                FunctionSignature::new(vec![], old_type),
                record as *const u8,
            )
        }
        .unwrap_err();
        assert_eq!(error.code(), ErrorCode::AbiError);
        scheduler.cancel(cancelled).unwrap();
        drop(vm);
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        assert!(!task.is_finished());
        task.resume().unwrap();
        assert_eq!(task.cps_result(), Some(289));
        let ResumeError::Failed(error) = failing.resume().unwrap_err() else {
            panic!()
        };
        assert_eq!(error.code(), ErrorCode::RuntimeError);
        assert_eq!(error.span().unwrap().slice(source), Some("1 / value"));
        assert_eq!(failing.cps_result(), None);
        assert_eq!(failing.resume(), Err(ResumeError::AlreadyFinished));
        for _ in 0..4 {
            if !scoped.is_finished() {
                scoped.resume().unwrap();
            }
        }
        assert_eq!(scoped.cps_result(), Some(42));
        assert_eq!(scheduler.resume_all().unwrap(), 0);
        EVENTS
            .with(|events| assert_eq!(*events.borrow(), [7, 7, -1, -1, 2, 2, 3, 3, 7, 7, 0, 3, 4]));
    }
}

#[test]
fn tuple_elements_and_destructured_initializers_resolve_imported_calls() {
    let source = r#"
        import joky/vm
        type Count = Int32
        fn run() -> Int32 {
            let h = vm.create()
            { let vm_text = "fn f() -> Int32 {{ 21 }}" vm.load_source(h, &vm_text) }
            let (a, (b,)): (Count, (Count,)) = ({ let vm_text = "f" vm.call_int32(h, &vm_text) }, ({ let vm_text = "f" vm.call_int32(h, &vm_text) },))
            vm.close(h)
            a + b
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        assert_eq!(vm.call_int32_0("run").unwrap(), 42);
    }
}

#[test]
fn wide_tuples_keep_parameter_order_and_mutable_leaves_across_backends() {
    let fields = (0..14).map(|_| "Int32").collect::<Vec<_>>().join(", ");
    let names = (0..14)
        .map(|n| format!("x{n}"))
        .collect::<Vec<_>>()
        .join(", ");
    let values = (0..14)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let source = format!("type Wide = (Unit, Bool, ({fields}), Unit)\nfn pass(value: Wide, delta: Int32) -> Wide {{ var (unit, flag, ({names}), tail) = value x13 = x13 + delta; (unit, flag, ({names}), tail) }}\nfn run() -> Int32 {{ let original = ({{}}, true, ({values}), {{}}) let (_, flag, ({names}), _) = pass(original, -20) let (_, _, (first, _, _, _, _, _, _, _, _, _, _, _, _, last), _) = original if flag {{ first + last + x0 + x7 + x13 }} else {{ 0 }} }}");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(&source, suspends, Suspends::Typed);
            let vm = parity_vm(&source, mode);
            let value = if suspends {
                vm.call_async("run", &[]).unwrap().cps_result().unwrap() as i32
            } else {
                vm.call_int32_0("run").unwrap()
            };
            assert_eq!(value, 13, "{mode:?} suspends={suspends}");
        }
    }
}

#[test]
fn owning_tuples_move_through_option_zip_and_destructure() {
    let source = r#"
        import joky/option.{type Option}
        import joky/array.{type Array}
        fn f() -> Int32 {
            let pair = Option.Some(Array.new::<Int32>()).zip(Option.Some(Array.new::<Int32>()))
            let (left, right) = match pair { Some(pair) => pair, None => return 0 }
            left.release()
            right.release()
            42
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        assert_eq!(vm.call_int32_0("f").unwrap(), 42, "{mode:?}");
    }
}
