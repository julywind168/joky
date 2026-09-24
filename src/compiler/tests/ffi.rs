use super::*;

#[test]
fn foreign_declarations_reject_undefined_abi_shapes_before_loading() {
    for signature in [
        "fn f(value: String) -> Int32",
        "fn f(value: Bool) -> Int32",
        "fn f(value: Bytes) -> Int32",
        "fn f(value: Unit) -> Int32",
        "fn f(value: (Int32, Int32)) -> Int32",
        "fn f(value: List(Int32)) -> Int32",
        "fn f(value: &File) -> Int32",
        "fn f(value: &Int32) -> Int32",
        "fn f(value: Int32) -> String",
        "fn f(value: Int32) -> type",
        "fn f(T: type, value: T) -> T",
        "fn f(self) -> Int32",
        "fn f(value: Int32)",
    ] {
        let source = format!("@extern(c, \"missing-library\", \"f\") {signature}; fn main() {{}}");
        let error = try_run_program(&source).unwrap_err();
        assert!(
            !error.to_string().contains("cannot load C library"),
            "{signature}: {error}"
        );
    }
    assert!(sema::check_program(
        &syntax::parse_program("@extern(c, \"missing\", \"main\") fn main() -> Unit;").unwrap()
    )
    .is_err());
}

#[test]
fn foreign_effect_contracts_are_checked_without_inventing_suspension() {
    let declaration =
        "@extern(c, \"not-loaded\", \"c_call\") fn c_call() -> Int32 effects { native };";
    let effect = "eff native { fn call() -> Unit }";
    let source = format!("{effect} {declaration} fn main() effects {{ native }} {{ c_call() }}");
    let table = sema::check_program(&syntax::parse_program(&source).unwrap()).unwrap();
    assert!(!table
        .function_effects("c_call")
        .unwrap()
        .may_suspend(table.effects()));
    let missing = format!("{effect} {declaration} fn main() {{ c_call() }}");
    assert!(sema::check_program(&syntax::parse_program(&missing).unwrap()).is_err());
    let suspending = source.replace("fn call()", "@suspends fn call()");
    assert!(sema::check_program(&syntax::parse_program(&suspending).unwrap()).is_err());
}

#[test]
fn foreign_pointer_signatures_are_accepted() {
    let source = r#"
        @repr(c) struct Point {
            let x: Int32
            let y: Int32
        }
        @extern(c, "missing", "point") fn point() -> CPtr(Point);
        @extern(c, "missing", "sum") fn sum(value: CPtr(Point)) -> Int32;
        @extern(c, "missing", "set") fn set(value: CMutPtr(Point), x: Int32, y: Int32) -> Unit;
        @extern(c, "missing", "strlen") fn strlen(value: CStr) -> UInt64;
        fn main() {}
    "#;
    assert!(sema::check_program(&syntax::parse_program(source).unwrap()).is_ok());
}

#[test]
fn c_layouts_reject_invalid_and_overflowing_shapes() {
    for (declaration, expected) in [
        (
            "@repr(c) struct Bad { let s: String }",
            "not a C layout type",
        ),
        (
            "struct Inner { let n: Int32 } @repr(c) struct Bad { let s: Inner }",
            "requires @repr(c)",
        ),
        ("@repr(c) struct Bad {}", "empty @repr(c)"),
        ("@repr(c) struct Bad { let n: Int32 = 2 }", "field defaults"),
        (
            "@repr(c) struct Bad { let n: CArray(UInt8, 0) }",
            "greater than zero",
        ),
        (
            "@repr(c) struct Bad { let n: CArray(UInt64, 18446744073709551615) }",
            "addressable object size",
        ),
        (
            "@repr(c) struct Bad { let n: CArray(UInt8, 9223372036854775807); let tail: UInt64 }",
            "addressable object size",
        ),
        (
            "@repr(c) struct Bad { let n: CArray(Bad, 1) }",
            "recursive C value layout",
        ),
        (
            "@repr(c) struct A { let b: CArray(B, 1) } @repr(c) struct B { let a: CArray(A, 1) }",
            "recursive C value layout",
        ),
        ("fn bad(p: CPtr(String)) {}", "not a C layout type"),
        (
            "fn bad(p: CPtr(CArray(String, 2))) {}",
            "not a C layout type",
        ),
        (
            "fn bad(p: CPtr(value: Int32)) {}",
            "positional type arguments",
        ),
    ] {
        let source = format!("{declaration} fn main() {{}}");
        let error = sema::check_program(&syntax::parse_program(&source).unwrap()).unwrap_err();
        assert!(
            error.to_string().contains(expected),
            "{declaration}: {error}"
        );
    }
}

#[test]
fn c_layout_metadata_survives_serialization_and_preserves_type_identity() {
    let source = r#"
        @repr(c) struct Packet { let tag: UInt8; let samples: CArray(UInt32, 3); let next: CPtr(Packet) }
        fn a(p: CPtr(UInt32)) {}
        fn b(p: CPtr(UInt64)) {}
        fn c(p: CMutPtr(UInt32)) {}
        fn d(p: CPtr(CArray(UInt8, 4))) {}
        fn e(p: CPtr(CArray(UInt8, 8))) {}
        fn main() {}
    "#;
    let program = syntax::parse_program(source).unwrap();
    let table = sema::check_program(&program).unwrap();
    let encoded = bincode::serialize(&table.module.types).unwrap();
    let mut restored: sema::TypeTable = bincode::deserialize(&encoded).unwrap();
    restored.validate_c_layouts().unwrap();
    let keys = program
        .functions
        .iter()
        .filter_map(|f| f.parameters.first())
        .map(|p| {
            let ty = table.checked_annotation(&p.ty, &[], None).unwrap();
            restored.stable_type_key(ty, crate::module::StableId(1))
        })
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(keys.len(), 5);
}

#[test]
fn c_arrays_cannot_reach_the_joky_value_abi() {
    for declaration in [
        "fn bad(value: CArray(UInt8, 4)) {}",
        "fn bad(value: Option(CArray(UInt8, 4))) {}",
        "@repr(c) struct Array { let data: CArray(UInt8, 4) } fn bad(value: Array) {}",
        "class Bad { let data: CArray(UInt8, 4) }",
    ] {
        let error = try_run_program(&format!("{declaration} fn main() {{}}")).unwrap_err();
        assert!(
            error.to_string().contains("CArray is a native layout type"),
            "{declaration}: {error}"
        );
    }
}

#[test]
fn c_null_pointers_are_pure_typed_copy_values() {
    run_program(
        r#"
        @repr(c) struct Point { let x: Int32 }
        fn check(p: CPtr(Int32)) -> Bool { p.is_null() }
        fn main() {
            let p = CPtr.null(Int32)
            if !check(p) || !p.is_null() { panic("const null") }
            if !CMutPtr.null(UInt8).is_null() { panic("mut null") }
            if !CPtr.null(Unit).is_null() || !CMutPtr.null(Unit).is_null() { panic("void null") }
            if !CStr.null().is_null() { panic("string null") }
            if !CPtr.null(Point).is_null() { panic("struct null") }
            if !CPtr.null(CArray(UInt8, 4)).is_null() { panic("array null") }
            let alias: type = CArray(Point, 2)
            if !CMutPtr.null(alias).is_null() { panic("alias null") }
        }
    "#,
    );
}

#[test]
fn c_null_pointers_reject_invalid_construction_and_conversion() {
    for body in [
        "CPtr.null()",
        "CPtr.null(Int32, UInt8)",
        "CMutPtr.null()",
        "CPtr.null(T: Int32)",
        "CPtr.null(1)",
        "CStr.null(UInt8)",
        "CPtr.null(String)",
        "CPtr.null(Bool)",
        "CPtr.null(CArray(UInt8, 0))",
        "CPtr.null(CArray(String, 4))",
        "CPtr.null(CArray(UInt64, 18446744073709551615))",
        "CPtr.null(Int32).is_null(1)",
        "CStr.null().is_null(1)",
        "1.is_null()",
        "let p: CPtr(Int32) = 0",
        "let p: CPtr(Int32) = CMutPtr.null(Int32)",
        "let p: CMutPtr(Int32) = CPtr.null(Int32)",
        "let p: CStr = CPtr.null(UInt8)",
        "let p: CPtr(UInt8) = CPtr.null(Int32)",
        "let Int32 = 1; CPtr.null(Int32)",
    ] {
        let source = format!("fn main() {{ {body} }}");
        let parsed = syntax::parse_program(&source).unwrap();
        assert!(sema::check_program(&parsed).is_err(), "accepted: {body}");
    }
}

#[test]
fn string_as_cstr_rejects_non_string_receivers_and_arguments() {
    for body in [
        "\"text\".as_cstr(1)",
        "1.as_cstr()",
        "let flag = true; flag.as_cstr()",
        "let bytes = Bytes.from_string(\"x\"); bytes.as_cstr()",
        "CStr.null().as_cstr()",
    ] {
        let source = format!("fn main() {{ {body} }}");
        let parsed = syntax::parse_program(&source).unwrap();
        assert!(sema::check_program(&parsed).is_err(), "accepted: {body}");
    }
}

#[test]
fn cstr_to_string_rejects_non_cstr_receivers_and_arguments() {
    for body in [
        "CStr.null().to_string(1)",
        "CPtr.null(Int32).to_string()",
        "CMutPtr.null(UInt8).to_string()",
        "\"text\".to_string()",
        "1.to_string()",
        "let value: Option(String) = CStr.null().to_string(); value.to_string()",
    ] {
        let source = format!("fn main() {{ {body} }}");
        let parsed = syntax::parse_program(&source).unwrap();
        assert!(sema::check_program(&parsed).is_err(), "accepted: {body}");
    }
}

#[test]
fn tmpfile_wrapper_rejects_repeated_close_and_use_after_close() {
    let wrapper = include_str!("../../../examples/ffi/tmpfile.jk")
        .split_once("\nfn main()")
        .unwrap()
        .0
        .replace("/usr/lib/libSystem.B.dylib", "missing-tmpfile-test-library");
    for body in [
        "let closed = file.close(); let again = file.close()",
        "let closed = file.close(); let flushed = file.flush()",
        "let owner = file; let flushed = file.flush()",
        "let closed = file.close(); match closed { Ok(_) => (), Err(_) => { let retry = file.close() } }",
    ] {
        let source = format!(
            "{wrapper}\nfn misuse(file: TempFile) effects {{ stdio }} {{ {body} }}\nfn main() {{}}"
        );
        let error = try_run_program(&source).unwrap_err().to_string();
        assert!(error.contains("after move"), "{body}: {error}");
    }
}

#[test]
fn tmpfile_wrapper_requires_effects_and_does_not_consume_a_borrow() {
    let wrapper = include_str!("../../../examples/ffi/tmpfile.jk")
        .split_once("\nfn main()")
        .unwrap()
        .0
        .replace("/usr/lib/libSystem.B.dylib", "missing-tmpfile-test-library");
    for (declaration, expected) in [
        (
            "fn misuse(file: TempFile) { let flushed = file.flush() }",
            "effect",
        ),
        (
            "fn misuse(file: TempFile) { let closed = file.close() }",
            "effect",
        ),
        ("fn misuse() { let opened = open_temp_file() }", "effect"),
        (
            "fn misuse(file: &TempFile) effects { stdio } { let closed = file.close() }",
            "takes non-owned local",
        ),
    ] {
        let source = format!("{wrapper}\n{declaration}\nfn main() {{}}");
        let error = try_run_program(&source).unwrap_err().to_string();
        assert!(
            error.contains(expected) && !error.contains("cannot load C library"),
            "{declaration}: {error}"
        );
    }
}

#[test]
fn c_cells_reject_non_c_content_and_wrong_receivers() {
    for body in [
        "CMutPtr.alloc()",
        "CMutPtr.alloc(1, 2)",
        "CMutPtr.alloc(label: 1)",
        "CMutPtr.alloc(\"text\")",
        "let bytes = Bytes.from_string(\"x\"); CMutPtr.alloc(bytes)",
        "CMutPtr.alloc(CArray(UInt8, 2))",
        "CPtr.alloc(1)",
        "CStr.alloc(1)",
        "1.read()",
        "\"text\".read()",
        "CPtr.null(Int32).read()",
        "CStr.null().read()",
        "CMutPtr.null(Int32).read(1)",
        "CMutPtr.null(Int32).write(1.5)",
        "CMutPtr.null(Int32).write()",
        "CPtr.null(Int32).write(1)",
        "CStr.null().free()",
        "1.free()",
        "CMutPtr.null(Int32).free(1)",
    ] {
        let source = format!("fn main() {{ {body} }}");
        let parsed = syntax::parse_program(&source).unwrap();
        assert!(sema::check_program(&parsed).is_err(), "accepted: {body}");
    }
}

#[test]
fn nested_c_pointer_types_and_nulls_are_accepted() {
    for body in [
        "let p: CPtr(CPtr(Int32)) = CPtr.null(CPtr(Int32))",
        "let p: CMutPtr(CMutPtr(Unit)) = CMutPtr.null(CMutPtr(Unit))",
        "let p: CPtr(CStr) = CPtr.null(CStr)",
        "let p: CMutPtr(CPtr(UInt8)) = CMutPtr.null(CPtr(UInt8))",
        "let p: CPtr(CPtr(CPtr(Int32))) = CPtr.null(CPtr(CPtr(Int32)))",
    ] {
        let source = format!("fn main() {{ {body} }}");
        let parsed = syntax::parse_program(&source).unwrap();
        assert!(
            sema::check_program(&parsed).is_ok(),
            "rejected: {body}: {:?}",
            sema::check_program(&parsed).err()
        );
    }
}

#[test]
fn native_callbacks_reject_effects_captures_and_non_closures() {
    let declaration = "@extern(c, \"missing-library\", \"sort\") fn sort(values: CPtr(Unit), compare: fn(CMutPtr(Int32), CMutPtr(Int32)) -> Int32) -> Unit; \
         eff ping { fn hit() -> Unit }";
    for body in [
        // The callback body uses an effect: C cannot service Joky effects.
        "fn misuse(values: CPtr(Unit)) effects { ping } { sort(values, fn (x: CMutPtr(Int32), y: CMutPtr(Int32)) -> Int32 { ping.hit(); 0 }) }",
        // Suspending callbacks cannot run under a synchronous C call.
        "eff time { @suspends fn sleep(duration: Duration) -> Unit } fn misuse(values: CPtr(Unit)) effects { time } { sort(values, fn (x: CMutPtr(Int32), y: CMutPtr(Int32)) -> Int32 { time.sleep(1ms); 0 }) }",
        // Capturing callbacks have no lifetime protocol yet.
        "fn misuse(values: CPtr(Unit)) { let bound: Int32 = 1; sort(values, fn (x: CMutPtr(Int32), y: CMutPtr(Int32)) -> Int32 { x.read() - y.read() - bound }) }",
        // Region-owned Cowns must not escape through callback captures.
        "class State { var value: Int32 = 0 } fn misuse(values: CPtr(Unit)) { region { let state = Cown.new(State()); sort(values, fn (x: CMutPtr(Int32), y: CMutPtr(Int32)) -> Int32 { when (state) |s| { s.value } }) } }",
        // Only closure literals are accepted at the call site.
        "fn misuse(values: CPtr(Unit), later: fn(CMutPtr(Int32), CMutPtr(Int32)) -> Int32) { sort(values, later) }",
    ] {
        let source = format!("{declaration}fn main() {{}} {body}");
        let error = try_run_program(&source).unwrap_err().to_string();
        assert!(
            error.contains("not supported yet")
                || error.contains("capture")
                || error.contains("closure")
                || error.contains("no effects"),
            "{body}: {error}"
        );
    }
    // Cross-module callbacks stay rejected until imports re-resolve types.
    let public_source = "@extern(c, \"missing-library\", \"sort\") pub fn sort(values: CPtr(Unit), compare: fn(CMutPtr(Int32), CMutPtr(Int32)) -> Int32) -> Unit; fn main() {}";
    assert!(sema::check_program(&syntax::parse_program(public_source).unwrap()).is_err());
}
