//! Enum layout, matching, and suspension parity tests.

use super::support::*;
use super::{VmEngine, VmMode};
use crate::runtime::{scheduler::Scheduler, suspension::ResumeError};
use crate::{ErrorCode, FunctionSignature};

#[test]
fn enums_preserve_values_and_matching_in_direct_and_cps_backends() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(
                include_str!("../../tests/fixtures/enums.jk"),
                suspends,
                Suspends::All,
            );
            let vm = parity_vm_with_prelude(&source, mode, false);
            for (name, expected) in [
                ("nested", 7),
                ("replacements", 1086),
                ("effects", 142),
                ("snapshots", 128),
                ("arguments", 11),
                ("aggregate_arms", 49),
                ("loops", 85),
                ("scopes", 921),
                ("calls", 56),
                ("edge_cases", 13),
                ("compute", 2498),
            ] {
                let actual = if suspends {
                    vm.call_async(name, &[]).unwrap().cps_result().unwrap() as i32
                } else {
                    vm.call_int32_0(name).unwrap()
                };
                assert_eq!(actual, expected, "{mode:?} suspends={suspends} {name}");
            }
        }
    }
}

#[test]
fn invalid_enums_and_patterns_are_rejected_before_publication() {
    let cases = [
        ("enum E {}", ErrorCode::InvalidPattern, "at least one"),
        ("enum E { A A }", ErrorCode::DuplicateDefinition, "variant"),
        ("enum E { _ }", ErrorCode::DuplicateDefinition, "reserved"),
        (
            "enum E { A } struct E {}",
            ErrorCode::DuplicateDefinition,
            "already defined",
        ),
        (
            "enum E { A } type E = Int32",
            ErrorCode::DuplicateDefinition,
            "already defined",
        ),
        (
            "enum E { A(Missing) }",
            ErrorCode::UnknownType,
            "Unknown type",
        ),
        (
            "enum E { A(vm.Handle) }",
            ErrorCode::TypeMismatch,
            "Copy value",
        ),
        (
            "enum E { A(vm.Handle) }",
            ErrorCode::TypeMismatch,
            "Copy value",
        ),
        ("enum E { A(E) }", ErrorCode::RecursiveType, "E -> E"),
        (
            "enum E { A(Alias) } type Alias = E",
            ErrorCode::RecursiveType,
            "E -> Alias -> E",
        ),
        (
            "enum E { A(R) } struct R { e: E }",
            ErrorCode::RecursiveType,
            "Infinite inline",
        ),
        (
            "enum E { A } fn f() -> E { E {} }",
            ErrorCode::InvalidCall,
            "qualified variant",
        ),
        (
            "enum E { A } fn f() -> E { E.A() }",
            ErrorCode::InvalidCall,
            "no parentheses",
        ),
        (
            "enum E { A } fn f() -> E { E.Unknown }",
            ErrorCode::UnknownName,
            "no variant",
        ),
        (
            "enum E { A } fn f() -> E { E.Unknown(1) }",
            ErrorCode::UnknownName,
            "no variant",
        ),
        (
            "enum E { A(Int32) } fn f() -> E { E.A }",
            ErrorCode::InvalidCall,
            "one payload",
        ),
        (
            "enum E { A(Int32) } fn f() -> E { E.A() }",
            ErrorCode::InvalidCall,
            "one payload",
        ),
        (
            "enum E { A(Int32) } fn f() -> E { E.A(1, 2) }",
            ErrorCode::InvalidCall,
            "one payload",
        ),
        (
            "enum E { A(Int32) } fn f() -> E { E.A(true) }",
            ErrorCode::TypeMismatch,
            "expects Int32",
        ),
        (
            "enum E { A } type Alias = E fn f() -> E { Alias.A }",
            ErrorCode::UnknownName,
            "Undefined variable",
        ),
        (
            "enum E { A } type Alias = E fn f() -> E { Alias.A() }",
            ErrorCode::UnknownName,
            "Undeclared function",
        ),
        (
            "enum E { A } fn f() -> E { A }",
            ErrorCode::UnknownName,
            "Undefined variable",
        ),
        (
            "enum E { A } fn f() -> E { let E = 1 E.A }",
            ErrorCode::TypeMismatch,
            "has no fields",
        ),
        (
            "enum E { A(Int32) } fn f() -> E { let E = 1 E.A(1) }",
            ErrorCode::UnknownName,
            "has no method",
        ),
        (
            "enum E { A } enum F { A } fn f() -> E { F.A }",
            ErrorCode::TypeMismatch,
            "Return type",
        ),
        (
            "enum E { A B } fn f(e: E) { match e { | A => {} } }",
            ErrorCode::NonExhaustiveMatch,
            "B",
        ),
        (
            "enum E { A } fn f(e: E) { match e {} }",
            ErrorCode::NonExhaustiveMatch,
            "A",
        ),
        (
            "fn f() { match 1 { | _ => {} } }",
            ErrorCode::TypeMismatch,
            "enum value",
        ),
        (
            "enum E { A B } fn f(e: E) { match e { | A => {} | A => {} | _ => {} } }",
            ErrorCode::UnreachableCode,
            "Duplicate match",
        ),
        (
            "enum E { A } fn f(e: E) { match e { | A => {} | _ => {} } }",
            ErrorCode::UnreachableCode,
            "complete coverage",
        ),
        (
            "enum E { A } fn f(e: E) { match e { | _ => {} | A => {} } }",
            ErrorCode::UnreachableCode,
            "complete coverage",
        ),
        (
            "enum E { A } fn f(e: E) { match e { | Unknown => {} } }",
            ErrorCode::InvalidPattern,
            "no variant",
        ),
        (
            "enum E { A } enum F { A } fn f(e: E) { match e { | F.A => {} } }",
            ErrorCode::InvalidPattern,
            "does not belong",
        ),
        (
            "enum E { A } type Alias = E fn f(e: E) { match e { | Alias.A => {} } }",
            ErrorCode::InvalidPattern,
            "does not belong",
        ),
        (
            "enum E { A } fn f(e: E) { match e { | A(x) => {} } }",
            ErrorCode::InvalidPattern,
            "payload arity",
        ),
        (
            "enum E { A(Int32) } fn f(e: E) { match e { | A => {} } }",
            ErrorCode::InvalidPattern,
            "payload arity",
        ),
        (
            "enum E { A(Int32) B } fn f(e: E) -> Int32 { match e { | A(x) => x | B => true } }",
            ErrorCode::TypeMismatch,
            "same type",
        ),
        (
            "enum E { A(Int32) B } fn f(e: E) { match e { | A(x) => {} | B => x; } }",
            ErrorCode::UnknownName,
            "Undefined variable",
        ),
        (
            "enum E { A(Int32) } fn f(e: E) { match e { | A(x) => x = 2 } }",
            ErrorCode::InvalidAssignment,
            "immutable",
        ),
        (
            "enum E { A } fn f(e: E) { match e { | _ => let inside = 1 } inside; }",
            ErrorCode::UnknownName,
            "inside",
        ),
        (
            "enum E { A } fn f(e: E) { match { let inside = e inside } { | _ => {} } inside; }",
            ErrorCode::UnknownName,
            "inside",
        ),
        (
            "enum E { A } fn f(e: E) -> Bool { e == E.A }",
            ErrorCode::TypeMismatch,
            "Comparison operands",
        ),
        (
            "enum E { A } fn f() { E.A.field; }",
            ErrorCode::TypeMismatch,
            "has no fields",
        ),
        (
            "enum E { A } extern fn host(e: E)",
            ErrorCode::AbiError,
            "host ABI",
        ),
        (
            "enum E { A } extern fn host() @suspends -> E",
            ErrorCode::AbiError,
            "host ABI",
        ),
        (
            "enum E { A(Int32) } fn host() @suspends -> Int32 { 1 } fn f() -> E { E.A(host()) }",
            ErrorCode::InvalidCall,
            "@suspends",
        ),
        (
            "enum E { A } fn f(e: E) -> Int32 { match e { | A => return 1 } 2 }",
            ErrorCode::UnreachableCode,
            "Unreachable expression",
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
            let mut aot = crate::aot::AotCompiler::new("invalid_enum").unwrap();
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
fn enum_payloads_and_match_arms_suspend_without_replay_or_lost_ownership() {
    let source = r#"
        struct Pair { x: Int32 flag: Bool }
        enum E { Empty Number(Int32) Pair(Pair) }
        extern fn delayed_value(x: Int32) @suspends -> Int32
        extern fn record(x: Int32) -> Int32
        fn build(n: Int32) @suspends -> E { E.Pair(Pair { x: delayed_value(record(n)) flag: true }) }
        fn original() -> E { E.Number(1) }
        fn pass(e: E) -> E { e }
        fn read(e: E) -> Int32 { match e { | Number(n) => n | Pair(p) => p.x | _ => 0 } }
        fn pending() @suspends -> Int32 {
            var e = original()
            let out = match e {
                | Number(n) => {
                    e = build(7)
                    let p = pass(e)
                    delayed_value(record(n + read(p)))
                }
                | _ => record(99)
            }
            out * 10 + read(e)
        }
        fn failing() @suspends -> Int32 {
            match build(-1) { | Pair(p) => 1 / p.x | _ => record(99) }
        }
        fn child(e: E) @suspends { let p = build(read(e)) record(read(p)); }
        fn scoped() @suspends -> Int32 {
            parallel { | child(E.Number(2)) | child(E.Number(3)) }
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
        vm.load_source("enum E { Empty Number(Int32) } fn original() -> E { E.Empty }")
            .unwrap();
        assert_ne!(old_type, vm.function_signature("original").unwrap().ret);
        assert!(vm.call_int32_0("original").is_err());
        assert!(vm.call_async("original", &[]).is_err());
        let error = unsafe {
            vm.register_fn(
                "host_enum",
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
        assert_eq!(task.cps_result(), Some(108));
        let ResumeError::Failed(error) = failing.resume().unwrap_err() else {
            panic!()
        };
        assert_eq!(error.code(), ErrorCode::RuntimeError);
        assert_eq!(failing.cps_result(), None);
        assert_eq!(failing.resume(), Err(ResumeError::AlreadyFinished));
        for _ in 0..4 {
            if !scoped.is_finished() {
                scoped.resume().unwrap();
            }
        }
        assert_eq!(scoped.cps_result(), Some(42));
        assert_eq!(scheduler.resume_all().unwrap(), 0);
        EVENTS.with(|events| {
            assert_eq!(
                *events.borrow(),
                [7, 7, -1, -1, 2, 2, 3, 3, 7, 7, 9, 9, 3, 4]
            )
        });
    }
}

#[test]
fn enum_constructors_and_match_bodies_respect_import_resolution() {
    let source = r#"
        import joky/vm
        type Count = Int32
        enum E { Empty Value(Count) }
        fn run() -> Int32 {
            let h = vm.create()
            { let vm_text = "fn f() -> Int32 { 42 }" vm.load_source(h, &vm_text) }
            let result = match E.Value({ let vm_text = "f" vm.call_int32(h, &vm_text) }) {
                | Value(n) => { let x: Count = { let vm_text = "f" vm.call_int32(h, &vm_text) } n + x }
                | _ => 0
            }
            vm.close(h)
            result
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        assert_eq!(vm.call_int32_0("run").unwrap(), 84);
        let vm = parity_vm("import joky/vm as runtime enum vm { call_int32(Int32) register_int32(Int32) } fn run() -> Int32 { match vm.call_int32(42) { | call_int32(n) => n | register_int32(n) => n } }", mode);
        assert_eq!(vm.call_int32_0("run").unwrap(), 42);
    }
}

#[test]
fn wide_enums_keep_typed_payload_lanes_across_register_and_stack_arguments() {
    let mut source = "struct Wide { flag: Bool ".to_owned();
    for n in 0..12 {
        source.push_str(&format!("f{n}: Int32 "));
    }
    source.push_str("} enum E { Empty Payload(Wide) Flag(Bool) Number(Int32) }\nfn pass(e: E, n: Int32) -> E { match e { | Payload(p) => { var copy = p copy.f11 = copy.f11 + n E.Payload(copy) } | _ => e } }\nfn run() -> Int32 { var e = E.Payload(Wide { flag: true ");
    for n in 0..12 {
        source.push_str(&format!("f{n}: {n} "));
    }
    source.push_str("}) let saved = e e = E.Empty let empty = pass(e, 0) e = E.Flag(false) let flag = pass(e, 0) e = E.Number(-3) let value = pass(saved, -20) match value { | Payload(p) => if p.flag { p.f0 + p.f7 + p.f11 } else { 0 } | _ => 0 } }");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(&source, suspends, Suspends::All);
            let vm = parity_vm(&source, mode);
            let result = if suspends {
                vm.call_async("run", &[]).unwrap().cps_result().unwrap() as i32
            } else {
                vm.call_int32_0("run").unwrap()
            };
            assert_eq!(result, -2, "{mode:?} suspends={suspends}");
        }
    }
}

#[test]
fn nested_tuple_patterns_bind_enum_payloads() {
    let source = r#"
        enum Message { Empty Pair((Int32, Bool)) Nested((Int32, (Int32, Bool))) }
        fn pair() -> Int32 {
            match Message.Pair((40, true)) {
                | Empty => 0
                | Pair((value, flag)) => if flag { value + 2 } else { 0 }
                | Nested(_) => 0
            }
        }
        fn nested() -> Int32 {
            match Message.Nested((1, (2, true))) {
                | Empty => 0
                | Pair(_) => 0
                | Nested((left, (right, flag))) => if flag { left + right } else { 0 }
            }
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        assert_eq!(vm.call_int32_0("pair").unwrap(), 42, "{mode:?}");
        assert_eq!(vm.call_int32_0("nested").unwrap(), 3, "{mode:?}");
    }
}

#[test]
fn nested_tuple_patterns_validate_shape_names_and_wildcards() {
    let cases = [
        ("enum E { A((Int32, Bool)) } fn f() { match E.A((1, true)) { | A((x, x)) => x | _ => 0 } }", ErrorCode::DuplicateDefinition),
        ("enum E { A((Int32, Bool)) } fn f() { match E.A((1, true)) { | A((x,)) => x | _ => 0 } }", ErrorCode::InvalidPattern),
        ("enum E { A(Int32) } fn f() { match E.A(1) { | A((x, y)) => x | _ => 0 } }", ErrorCode::InvalidPattern),
    ];
    for (source, code) in cases {
        let ast = crate::parser::parse(source).unwrap();
        let error = VmEngine::with_mode(VmMode::Jit)
            .load_module(&ast)
            .unwrap_err();
        assert_eq!(error.code(), code, "{source}: {error}");
    }
}

#[test]
fn recursive_patterns_match_enum_tuple_combinations_and_infer_bindings() {
    let source = r#"
        enum Bits { Off On }
        enum Pair { Value((Bits, Bits)) }
        enum Empty { Value((Unit, (Unit,))) }
        fn read(value: Option<Option<Int32>>) -> Int32 {
            match value { | None => 1 | Some(None) => 2 | Some(Some(n)) => n }
        }
        fn correlated(value: Pair) -> Int32 {
            match value {
                | Value((On, _)) => 10
                | Value((_, On)) => 20
                | Value((Off, Off)) => 30
            }
        }
        fn identity<T>(value: T) -> T { value }
        fn hinted() -> UInt64 {
            match Option.Some((uint64(9), uint64(4))) {
                | Some((n, m)) => identity(n + m)
                | _ => 0
            }
        }
        fn zero() -> Int32 { match Empty.Value(({}, ({},))) { | Value((a, (b,))) => { a; b; 42 } } }
        fn run() -> Int32 {
            read(None) + read(Some(None)) + read(Some(Some(39)))
                + correlated(Pair.Value((Bits.On, Bits.Off)))
                + correlated(Pair.Value((Bits.Off, Bits.On)))
                + correlated(Pair.Value((Bits.Off, Bits.Off)))
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(source, suspends, Suspends::All);
            let vm = parity_vm(&source, mode);
            let result = if suspends {
                vm.call_async("run", &[]).unwrap().cps_result().unwrap() as i32
            } else {
                vm.call_int32_0("run").unwrap()
            };
            assert_eq!(result, 102, "{mode:?} suspends={suspends}");
            let result = if suspends {
                vm.call_async("zero", &[]).unwrap().cps_result().unwrap() as i32
            } else {
                vm.call_int32_0("zero").unwrap()
            };
            assert_eq!(result, 42);
            if !suspends {
                assert_eq!(vm.call_uint64_0("hinted").unwrap(), 13);
            }
        }
    }
}

#[test]
fn recursive_patterns_validate_coverage_and_constructor_identity() {
    let cases = [
        ("fn f(x: Option<Option<Int32>>) { match x { | None => 0 | Some(None) => 1 } }", ErrorCode::NonExhaustiveMatch),
        ("fn f(x: Option<Option<Int32>>) { match x { | Some(_) => 0 | Some(Some(n)) => n | None => 1 } }", ErrorCode::UnreachableCode),
        ("enum B { F T } enum E { V((B, B)) } fn f(x: E) { match x { | V((F, _)) => 0 | V((T, _)) => 1 | V((_, T)) => 2 } }", ErrorCode::UnreachableCode),
        ("enum B { F T } enum E { V((B, B)) } fn f(x: E) { match x { | V((F, F)) => 0 | V((T, T)) => 1 } }", ErrorCode::NonExhaustiveMatch),
        ("enum Other { None } fn f(x: Option<Option<Int32>>) { match x { | Some(Other.None) => 0 | _ => 1 } }", ErrorCode::InvalidPattern),
        ("fn f(x: Option<Option<Int32>>) { match x { | Some(Some) => 0 | _ => 1 } }", ErrorCode::InvalidPattern),
        ("fn f(x: Option<(Int32, Option<Int32>)>) { match x { | Some((n, Some(n))) => n | _ => 0 } }", ErrorCode::DuplicateDefinition),
    ];
    for (source, code) in cases {
        let error = VmEngine::with_mode(VmMode::Jit)
            .load_source(source)
            .unwrap_err();
        assert_eq!(error.code(), code, "{source}: {error}");
    }
    let source = "import joky/option.{type Option as Maybe, Some as Present, None as Absent} fn run() -> Int32 { match Maybe.Some(Maybe.None::<Int32>) { | Present(Absent) => 42 | _ => 0 } }";
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        assert_eq!(parity_vm(source, mode).call_int32_0("run").unwrap(), 42);
    }
}
