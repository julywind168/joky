//! The same C fixture exercises actual native pointers in all three backends.
use super::{VmEngine, VmMode};
use crate::{FunctionSignature, Type};

pub(crate) const SOURCE: &str = include_str!("../../tests/fixtures/c_borrowing.jk");
pub(crate) const C_SOURCE: &str = include_str!("../../tests/fixtures/c_borrowing.c");
pub(crate) const CASES: &[&str] = &[
    "ffi_arena",
    "ffi_local",
    "ffi_nested",
    "ffi_fields",
    "ffi_shared",
    "ffi_widths",
    "ffi_helpers",
    "ffi_flow",
    "ffi_ptr_roundtrip",
    "ffi_ptr_null",
    "ffi_ptr_memory",
    "ffi_ptr_flow",
];

fn reference(name: &str, mutable: bool) -> Type {
    Type::reference(Type::Named(name.into()), mutable)
}

#[cfg(unix)]
pub(crate) fn register_fixture(vm: &mut VmEngine, library: std::rc::Rc<libloading::Library>) {
    let r = |ty, mutable| Type::reference(ty, mutable);
    for (name, params, ret) in [
        ("c_ptr_create", vec![], Type::Ptr),
        ("c_ptr_identity", vec![Type::Ptr], Type::Ptr),
        ("c_ptr_two", vec![Type::Ptr, Type::Ptr], Type::Ptr),
        (
            "c_ptr_three",
            vec![Type::Ptr, Type::Ptr, Type::Ptr],
            Type::Ptr,
        ),
        (
            "c_ptr_four",
            vec![
                Type::Ptr,
                r(Type::Int32, false),
                Type::Ptr,
                r(Type::Int32, false),
            ],
            Type::Ptr,
        ),
        ("c_ptr_bump", vec![Type::Ptr], Type::Unit),
        ("c_ptr_read", vec![Type::Ptr], Type::Int32),
        ("c_ptr_destroy", vec![Type::Ptr], Type::Unit),
        ("c_ptr_live", vec![], Type::Int32),
        ("c_inspect", vec![reference("CPoint", false)], Type::Int64),
        ("c_update", vec![reference("CPoint", true)], Type::Unit),
        ("c_packet", vec![reference("Packet", true)], Type::UInt64),
        ("c_zero", vec![], Type::Int32),
        (
            "c_pair",
            vec![r(Type::Int32, true), r(Type::Int32, true)],
            Type::Unit,
        ),
        (
            "c_sum",
            vec![r(Type::Int32, false), r(Type::Int32, false)],
            Type::Int32,
        ),
        (
            "c_three",
            vec![
                r(Type::Int32, false),
                r(Type::Int64, false),
                r(Type::UInt32, false),
            ],
            Type::UInt32,
        ),
        (
            "c_four",
            vec![
                r(Type::Int32, false),
                r(Type::Int64, false),
                r(Type::UInt32, false),
                r(Type::UInt64, false),
            ],
            Type::UInt64,
        ),
    ] {
        unsafe {
            let symbol = library.get::<*const u8>(name.as_bytes()).unwrap();
            vm.register_c_fn(
                name,
                FunctionSignature::new(params, ret),
                *symbol,
                Some(library.clone()),
            )
            .unwrap();
        }
    }
}

#[test]
fn ffi_attributes_and_unsafe_boundaries_reject_invalid_programs() {
    for source in [
        "#repr(C) struct Empty {}",
        "#repr(C) struct G<T> { x: T }",
        "#repr(C) struct P { b: Bool }",
        "#repr(C) struct P { unit: Unit }",
        "#repr(C) struct P { text: String }",
        "#repr(C) struct P { ref: &Int32 }",
        "#repr(C) struct P { pair: (Int32, Int32) }",
        "enum E { A } #repr(C) struct P { value: E }",
        "struct N { x: Int32 } #repr(C) struct P { nested: N }",
        "#repr(C) #repr(C) struct P { x: Int32 }",
        "#repr(c) struct P { x: Int32 }",
        "#repr(C()) struct P { x: Int32 }",
        "#repr(value: C) struct P { x: Int32 }",
        "#repr(C, C) struct P { x: Int32 }",
        "#repr struct P { x: Int32 }",
        "#derive(Eq) struct P { x: Int32 }",
        "#cfg(all(feature: true)) struct P { x: Int32 }",
        "#repr(C) fn f() {}",
        "pub #repr(C) struct P { x: Int32 }",
        "@repr(C) struct P { x: Int32 }",
        "extern \"C\" fn c_zero() -> Int32",
        "unsafe extern \"other\" fn c_zero() -> Int32",
        "unsafe extern \"C\" fn c_zero() @suspends -> Int32",
        "unsafe extern \"C\" fn c_zero<T>() -> Int32",
        "unsafe extern \"C\" fn c_zero() -> &Int32",
        "unsafe extern \"C\" fn c_zero() -> Bool",
        "unsafe extern \"C\" fn c_zero(x: Int32) -> Int32",
        "struct P { x: Int32 } unsafe extern \"C\" fn c_zero(x: &P) -> Int32",
        "unsafe extern \"C\" fn c_zero(x: &Bool) -> Int32",
        "unsafe extern \"C\" fn c_zero(a: &Int32, b: &Int32, c: &Int32, d: &Int32, e: &Int32) -> Int32",
        "unsafe extern \"C\" fn c_zero() -> Int32 fn f() -> Int32 { c_zero() }",
        "unsafe extern \"C\" fn c_zero() -> Int32 fn f() -> Int32 { unsafe { c_zero(); }; c_zero() }",
        "unsafe extern \"C\" fn c_zero() -> Int32 fn unused<T>(x: T) -> Int32 { c_zero() }",
        "fn f() -> &Int32 { unsafe { let n = 1; &n } }",
    ] {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            unsafe { vm.register_c_fn("c_zero", FunctionSignature::new(vec![], Type::Int32), zero as *const u8, None).unwrap(); }
            let error = vm.load_source(source).expect_err(source);
            assert!(error.span().is_some(), "{mode:?}: {source}: {error}");
        }
        if let Ok(ast) = crate::parser::parse(source) {
            assert!(crate::aot::AotCompiler::new("ffi-invalid").unwrap().compile_module(&ast).is_err(), "{source}");
        }
    }
}
extern "C" fn zero() -> i32 {
    0
}

#[test]
fn ffi_unsafe_keeps_borrow_checks_and_registration_is_separate() {
    unsafe extern "C" fn pair(_a: *mut i32, _b: *mut i32) {}
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        let signature =
            FunctionSignature::new(vec![Type::reference(Type::Int32, true); 2], Type::Unit);
        assert!(unsafe { vm.register_fn("pair", signature.clone(), pair as *const u8) }.is_err());
        unsafe { vm.register_c_fn("pair", signature, pair as *const u8, None) }.unwrap();
        for body in [
            "let p = alloc(1); unsafe { pair(p, p) }",
            "let p = alloc(1); let view = share(p); unsafe { pair(p, alloc(2)) }; memory.read(view)",
            "let p = 1; unsafe { pair(&p, alloc(2)) }",
        ] {
            let source = format!("unsafe extern \"C\" fn pair(a: &mut Int32, b: &mut Int32) fn f() {{ {body}; }}");
            let error = vm.load_source(&source).expect_err(body);
            assert_eq!(error.phase(), crate::ErrorPhase::TypeCheck, "{error}");
            let ast = crate::parser::parse(&source).unwrap();
            assert!(crate::aot::AotCompiler::new("ffi-loans").unwrap().compile_module(&ast).is_err());
        }
        vm.load_source("fn f() -> Int32 { 42 }").unwrap();
        let version = vm.module_version();
        unsafe {
            vm.register_fn(
                "c_zero",
                FunctionSignature::new(vec![], Type::Int32),
                zero as *const u8,
            )
        }
        .unwrap();
        assert!(vm
            .load_source("unsafe extern \"C\" fn c_zero() -> Int32")
            .is_err());
        unsafe {
            vm.register_c_fn(
                "c_zero",
                FunctionSignature::new(vec![], Type::Int32),
                zero as *const u8,
                None,
            )
        }
        .unwrap();
        assert!(vm.load_source("extern fn c_zero() -> Int32").is_err());
        assert_eq!(vm.module_version(), version);
        assert_eq!(vm.call_int32_0("f").unwrap(), 42);
        assert!(unsafe {
            vm.register_c_fn(
                "c_zero",
                FunctionSignature::new(vec![], Type::Bool),
                zero as *const u8,
                None,
            )
        }
        .is_err());
        assert!(unsafe {
            vm.register_c_fn(
                "c_zero",
                FunctionSignature::new(vec![], Type::Int32),
                std::ptr::null(),
                None,
            )
        }
        .is_err());
        vm.load_source(
            "unsafe extern \"C\" fn c_zero() -> Int32 fn f() -> Int32 { unsafe { c_zero() } }",
        )
        .unwrap();
        assert_eq!(vm.call_int32_0("f").unwrap(), 0);
    }
}

#[test]
fn ffi_calls_preserve_pending_cleanup_owners_and_runtime_failures() {
    use super::support::{delayed_value, EVENTS, READY};
    use crate::runtime::{arena::LIVE, suspension::ContinuationStatus};
    use std::{cell::Cell, rc::Rc};
    thread_local! { static CALLS: Cell<i32> = const { Cell::new(0) }; }
    unsafe extern "C" fn bump(value: *mut i32) {
        *value += 1;
        CALLS.with(|calls| calls.set(calls.get() + 1));
    }
    unsafe extern "C" fn fail(value: *const i32) -> i32 {
        crate::runtime::error::record_error(crate::Error::new_with_code(
            crate::ErrorPhase::Runtime,
            crate::ErrorCode::HostError,
            "C host failed",
        ));
        *value
    }
    struct Owner(Rc<Cell<bool>>);
    impl Drop for Owner {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }
    let source = r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        unsafe extern "C" fn bump(value: &mut Int32)
        unsafe extern "C" fn fail(value: &Int32) -> Int32
        fn run() @suspends -> Int32 {
            let p = alloc(40)
            unsafe { bump(p) }
            delayed_value(memory.read(p))
            unsafe { bump(p) }
            memory.read(p)
        }
        fn failure() -> Int32 { let p = alloc(7); unsafe { fail(p) } }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        CALLS.with(|calls| calls.set(0));
        READY.with(|ready| ready.set(false));
        EVENTS.with(|events| events.borrow_mut().clear());
        let dropped = Rc::new(Cell::new(false));
        let owner = Rc::new(Owner(dropped.clone()));
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_suspending_fn(
                "delayed_value",
                FunctionSignature::new(vec![Type::Int32], Type::Int32),
                delayed_value,
            )
            .unwrap();
            vm.register_c_fn(
                "bump",
                FunctionSignature::new(vec![Type::reference(Type::Int32, true)], Type::Unit),
                bump as *const u8,
                Some(owner.clone()),
            )
            .unwrap();
            vm.register_c_fn(
                "fail",
                FunctionSignature::new(vec![Type::reference(Type::Int32, false)], Type::Int32),
                fail as *const u8,
                None,
            )
            .unwrap();
        }
        drop(owner);
        vm.load_source(source).unwrap();
        let error = vm.call_int32_0("failure").unwrap_err();
        assert_eq!(error.code(), crate::ErrorCode::HostError);
        LIVE.with(|n| assert_eq!(n.get(), 0));
        let mut task = vm.call_async("run", &[]).unwrap();
        let cancelled = vm.call_async("run", &[]).unwrap();
        CALLS.with(|calls| assert_eq!(calls.get(), 2));
        for _ in 0..3 {
            assert_eq!(task.status(), ContinuationStatus::Pending);
            task.resume().unwrap();
            CALLS.with(|calls| assert_eq!(calls.get(), 2));
        }
        drop(cancelled);
        LIVE.with(|n| assert_eq!(n.get(), 1));
        vm.load_source("fn f() -> Int32 { 99 }").unwrap();
        drop(vm);
        assert!(!dropped.get());
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        assert_eq!(task.cps_result(), Some(42));
        CALLS.with(|calls| assert_eq!(calls.get(), 3));
        LIVE.with(|n| assert_eq!(n.get(), 0));
        drop(task);
        assert!(dropped.get());
    }
}

#[test]
fn ffi_attributes_and_unsafe_wrappers_survive_source_modules_and_aliases() {
    #[repr(C)]
    struct Point {
        x: i32,
        y: i64,
    }
    unsafe extern "C" fn inspect(p: *const Point) -> i64 {
        i64::from((*p).x) + (*p).y
    }
    let library = r#"
        #repr(C) pub struct Point { pub x: Int32, pub y: Int64 }
        type View = &Point
        unsafe extern "C" fn inspect(p: View) -> Int64
        pub fn read<T>(p: &Point, ignored: T) -> Int64 { unsafe { inspect(p) } }
    "#;
    let source = "import geometry.{type Point, read} fn f() -> Int32 { let p = alloc(Point { x: 19 ,y: 23 }); int32(read(p, 0)) }";
    let mut modules = crate::ModuleSources::new();
    modules.insert("geometry", library).unwrap();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_c_fn(
                "inspect",
                FunctionSignature::new(vec![reference("geometry.Point", false)], Type::Int64),
                inspect as *const u8,
                None,
            )
            .unwrap();
        }
        vm.load_source_with_modules(source, &modules)
            .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
        assert_eq!(vm.call_int32_0("f").unwrap(), 42);
        let version = vm.module_version();
        let mut invalid = crate::ModuleSources::new();
        invalid
            .insert(
                "geometry",
                library.replace("unsafe { inspect(p) }", "inspect(p)"),
            )
            .unwrap();
        let error = vm.load_source_with_modules(source, &invalid).unwrap_err();
        assert!(error.message().contains("unsafe block"), "{error}");
        assert_eq!(vm.module_version(), version);
        assert_eq!(vm.call_int32_0("f").unwrap(), 42);
    }
    let ast = crate::TypeRegistry::default()
        .resolve_with_modules(&crate::parser::parse(source).unwrap(), &modules)
        .unwrap();
    crate::aot::AotCompiler::new("ffi-imported")
        .unwrap()
        .compile_module(&ast)
        .unwrap();
    let header = crate::ffi::c_header(&ast).unwrap();
    assert!(header.contains("geometry_Point"));
}

#[test]
fn ffi_attributes_preserve_spans_and_bound_recursive_metadata() {
    use crate::parser::ast::{AttributeValue, Literal};
    let source =
        "// é\n#tool.check(all(level: 2), note: \"hé\", enabled: true,) pub struct P { x: Int32 }";
    let ast = crate::parser::parse(source).unwrap();
    let attribute = &ast.records[0].attributes[0];
    assert_eq!(attribute.path, "tool.check");
    assert_eq!(
        attribute.span.slice(source),
        Some("#tool.check(all(level: 2), note: \"hé\", enabled: true,)")
    );
    let arguments = attribute.arguments.as_ref().unwrap();
    assert!(matches!(&arguments[0].value, AttributeValue::Name(n) if n.path == "all"));
    assert!(
        matches!(&arguments[1].value, AttributeValue::Literal(Literal::String(s)) if s == "hé")
    );
    assert!(crate::aot::AotCompiler::new("ffi-unknown-attribute")
        .unwrap()
        .compile_module(&ast)
        .is_err());
    for source in [
        format!(
            "#a({}C{}) struct P {{ x: Int32 }}",
            "a(".repeat(33),
            ")".repeat(33)
        ),
        "#a(name: 1, name: 2) struct P { x: Int32 }".into(),
        "#a(name: 1, C) struct P { x: Int32 }".into(),
        "#a(1 + 2) struct P { x: Int32 }".into(),
    ] {
        assert!(crate::parser::parse(&source).is_err(), "{source}");
    }
    let ast = crate::parser::parse("#repr(C,) pub struct P { x: Int32 }").unwrap();
    crate::aot::AotCompiler::new("ffi-trailing-comma")
        .unwrap()
        .compile_module(&ast)
        .unwrap();
}

#[test]
fn ffi_c_imports_cannot_be_exported_as_safe_function_references() {
    let source = r#"
        import joky/vm
        unsafe extern "C" fn c_zero() -> Int32
        fn f(handle: vm.Handle) { { let vm_text = "unsafe" vm.register_int32(handle, &vm_text, c_zero) }; }
    "#;
    let ast = crate::parser::parse(source).unwrap();
    let error = crate::aot::AotCompiler::new("ffi-function-ref")
        .unwrap()
        .compile_module(&ast)
        .unwrap_err();
    assert_eq!(error.code(), crate::ErrorCode::InvalidCall);
}
