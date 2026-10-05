//! Record layout, snapshots, and suspension parity tests.

use super::support::*;
use super::VmMode;
use crate::runtime::{scheduler::Scheduler, suspension::ResumeError};
use crate::{ErrorCode, FunctionSignature};

#[test]
fn records_preserve_values_and_calls_in_direct_and_cps_backends() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(
                include_str!("../../tests/fixtures/records.jk"),
                suspends,
                Suspends::All,
            );
            let vm = parity_vm(&source, mode);
            for (name, expected) in [
                ("snapshots", 87108),
                ("argument_snapshots", 109),
                ("initializer_snapshots", 91),
                ("nested_updates", 389),
                ("conditions", 42),
                ("unit_effects", 3),
                ("calls", 89),
                ("empty_calls", 42),
            ] {
                let actual = if suspends {
                    vm.call_async(name, &[]).unwrap().cps_result().unwrap() as i32
                } else {
                    vm.call_int32_0(name).unwrap()
                };
                assert_eq!(actual, expected, "{mode:?} suspends={suspends} {name}");
            }
            let actual = if suspends {
                vm.call_async("control_flow", &[6])
                    .unwrap()
                    .cps_result()
                    .unwrap() as i32
            } else {
                vm.call_int32_1("control_flow", 6).unwrap()
            };
            assert_eq!(actual, 42);
        }
    }
}

#[test]
fn record_suspensions_keep_snapshots_through_reload_failure_and_cancellation() {
    let source = r#"
        struct Point { x: Int32 y: Int32 }
        struct Empty {}
        extern fn delayed_value(x: Int32) @suspends -> Int32
        extern fn record(x: Int32) -> Int32
        fn original() -> Point { Point { x: 1 y: 2 } }
        fn build(p: Point, n: Int32) @suspends -> Point {
            Point { y: delayed_value(record(n)) x: p.x }
        }
        fn pass(p: Point) -> Point { p }
        fn empty() @suspends -> Empty { delayed_value(0); Empty {} }
        fn pending() @suspends -> Int32 {
            var p = original()
            let q = build(p, { p.x = 9 7 })
            let r = pass(q)
            r.x * 100 + r.y * 10 + p.x
        }
        fn failing() @suspends -> Int32 {
            let p = build(original(), -1)
            let q = Point { x: 7 / p.y y: record(99) }
            q.x
        }
        fn child(p: Point, delta: Int32) @suspends {
            let q = build(p, delta)
            record(q.x + q.y);
        }
        fn scoped() @suspends -> Int32 {
            var p = original()
            parallel { | child(p, 1) | child({ p.x = 9 p }, 2) }
            empty()
            p.x
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm(source, mode);
        let old_type = vm.function_signature("original").unwrap().ret;
        let mut task = vm.call_async("pending", &[]).unwrap();
        let mut failing = vm.call_async("failing", &[]).unwrap();
        let mut scope = vm.call_async("scoped", &[]).unwrap();
        let mut scheduler = Scheduler::new();
        let cancelled = scheduler.suspend(vm.call_async("pending", &[]).unwrap());
        for _ in 0..3 {
            task.resume().unwrap();
            failing.resume().unwrap();
            scope.resume().unwrap();
            scheduler.resume(cancelled).unwrap();
        }
        EVENTS.with(|events| assert_eq!(*events.borrow(), [7, 7, -1, -1, 1, 1, 2, 2, 7, 7]));
        let version = vm.module_version();
        assert!(vm.load_source("struct Point { next: Point }").is_err());
        assert_eq!(vm.module_version(), version);
        vm.load_source(
            "struct Point { x: Int32 y: Int32 } fn original() -> Point { Point { x: 8 y: 9 } }",
        )
        .unwrap();
        assert_ne!(old_type, vm.function_signature("original").unwrap().ret);
        assert!(vm.call_int32_0("original").is_err());
        assert!(vm.call_async("original", &[]).is_err());
        // Canonical source records cannot be reinterpreted as host scalar handles.
        let error = unsafe {
            vm.register_fn(
                "host_record",
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
        assert_eq!(task.cps_result(), Some(189));
        let ResumeError::Failed(error) = failing.resume().unwrap_err() else {
            panic!()
        };
        assert_eq!(error.code(), ErrorCode::RuntimeError);
        assert_eq!(failing.cps_result(), None);
        assert_eq!(failing.resume(), Err(ResumeError::AlreadyFinished));
        for _ in 0..4 {
            if !scope.is_finished() {
                scope.resume().unwrap();
            }
        }
        assert_eq!(scope.cps_result(), Some(9));
        assert_eq!(scheduler.resume_all().unwrap(), 0);
        EVENTS.with(|events| {
            assert_eq!(*events.borrow(), [7, 7, -1, -1, 1, 1, 2, 2, 7, 7, 3, 12, 0])
        });
    }
}

#[test]
fn invalid_records_have_consistent_diagnostics_before_publication() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm("fn current() -> Int32 { 42 }", mode);
        let version = vm.module_version();
        for (source, code, message) in [
            ("struct P { x: Int32 x: Bool }", ErrorCode::DuplicateDefinition, "Duplicate field"),
            ("struct P {} struct P {}", ErrorCode::DuplicateDefinition, "already defined"),
            ("struct P {} type P = Int32", ErrorCode::DuplicateDefinition, "already defined"),
            ("struct P { x: Missing }", ErrorCode::UnknownType, "Unknown type"),
            ("struct P { x: vm.Handle }", ErrorCode::TypeMismatch, "Copy value"),
            ("struct P { x: vm.Handle }", ErrorCode::TypeMismatch, "Copy value"),
            ("struct P { x: P }", ErrorCode::RecursiveRecord, "P -> P"),
            ("struct A { b: B } struct B { a: A }", ErrorCode::RecursiveRecord, "A -> B -> A"),
            ("struct P { x: Alias } type Alias = P", ErrorCode::RecursiveRecord, "P -> Alias -> P"),
            ("struct P {} type A = B type B = A", ErrorCode::CyclicTypeAlias, "A -> B -> A"),
            ("struct P {} fn f() { P {} }", ErrorCode::TypeMismatch, "Return type mismatch"),
            ("struct P { x: Int32 } fn f() -> P { P {} }", ErrorCode::TypeMismatch, "Missing field"),
            ("struct P { x: Int32 } fn f() -> P { P { x: 1 x: 2 } }", ErrorCode::DuplicateDefinition, "Duplicate initializer"),
            ("struct P { x: Int32 } fn f() -> P { P { z: 1 } }", ErrorCode::UnknownName, "no field `z`"),
            ("struct P { x: Int32 } fn f() -> P { P { x: true } }", ErrorCode::TypeMismatch, "expects Int32"),
            ("struct P {} type A = P fn f() -> P { A {} }", ErrorCode::UnknownType, "Unknown record constructor"),
            ("struct P {} struct Q {} fn f() -> P { Q {} }", ErrorCode::TypeMismatch, "Return type mismatch"),
            ("struct P { x: Int32 } fn f() { let p = P { x: 1 } p.x = 2 }", ErrorCode::InvalidAssignment, "immutable binding"),
            ("struct P { x: Int32 } fn f() { var p = P { x: 1 } p.x = true }", ErrorCode::TypeMismatch, "Field assignment"),
            ("struct P { x: Int32 } fn f() -> Int32 { P { x: 1 }.y }", ErrorCode::UnknownName, "no field `y`"),
            ("fn f() -> Int32 { 1.x }", ErrorCode::TypeMismatch, "has no fields"),
            ("struct P {} fn f() -> Bool { P {} == P {} }", ErrorCode::TypeMismatch, "Comparison operands"),
            ("struct P {} extern fn host(p: P)", ErrorCode::AbiError, "host ABI"),
            ("struct P {} extern fn host() @suspends -> P", ErrorCode::AbiError, "host ABI"),
            ("import joky/vm struct P {} fn f() { let vm = P {} vm.create(); }", ErrorCode::UnknownName, "has no method"),
            ("struct P { x: Int32 y: Int32 } fn f() -> P { P { x: return P { x: 1 y: 2 } y: 3 } }", ErrorCode::UnreachableCode, "Unreachable expression"),
        ] {
            let source = format!("// 🙂\n{source}");
            let error = vm.load_source(&source).unwrap_err();
            assert_eq!(error.code(), code, "{mode:?}: {source}: {error}");
            assert!(error.message().contains(message), "{source}: {error}");
            assert!(error.span().is_some_and(|span| span.slice(&source).is_some()), "{error}");
            let mut aot = crate::aot::AotCompiler::new("invalid_record").unwrap();
            let error = aot.compile_module(&crate::parser::parse(&source).unwrap()).unwrap_err();
            assert_eq!(error.code(), code, "AOT: {source}: {error}");
            assert_eq!(vm.module_version(), version);
            assert_eq!(vm.call_int32_0("current").unwrap(), 42);
        }
    }
}

#[test]
fn wide_record_parameters_and_mixed_fields_match_all_call_conventions() {
    let mut source = String::from("struct Wide { before: Unit flag: Bool ");
    for n in 0..12 {
        source.push_str(&format!("f{n}: Int32 "));
    }
    source.push_str("after: Unit }\nfn pass(x: Wide, delta: Int32) -> Wide { var y = x y.f11 = y.f11 + delta y }\nfn run() -> Int32 { let p = Wide { before: {} flag: true ");
    for n in 0..12 {
        source.push_str(&format!("f{n}: {n} "));
    }
    source.push_str(
        "after: {} } let q = pass(p, -20) if q.flag { q.f0 + q.f7 + q.f11 } else { 0 } }",
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(&source, suspends, Suspends::All);
            let vm = parity_vm(&source, mode);
            let value = if suspends {
                vm.call_async("run", &[]).unwrap().cps_result().unwrap() as i32
            } else {
                vm.call_int32_0("run").unwrap()
            };
            assert_eq!(value, -2, "{mode:?} suspends={suspends}");
        }
    }
}
