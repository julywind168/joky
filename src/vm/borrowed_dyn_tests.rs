//! Front-end qualification and the explicit pre-dispatch execution gate.
use super::{VmEngine, VmMode};

fn rejected(source: &str, expected: &str) {
    let check = |error: crate::Error| {
        assert!(
            error.message().contains(expected),
            "{expected}: {error}\n{source}"
        );
        assert!(error.span().is_some(), "{error}");
    };
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        check(VmEngine::with_mode(mode).load_source(source).unwrap_err());
    }
    check(
        crate::aot::AotCompiler::new("borrowed-dyn-rejection")
            .unwrap()
            .compile_module(&crate::parser::parse(source).unwrap())
            .unwrap_err(),
    );
}

#[test]
fn borrowed_dyn_rejects_incompatible_unused_interfaces() {
    for (method, diagnostic) in [
        (
            "fn value(self) @noescape -> Int32",
            "requires &self or &mut self",
        ),
        ("fn value() -> Int32", "requires a borrowed receiver"),
        ("fn value(&self) @suspends -> Int32", "must be synchronous"),
        ("fn value(&self) -> Int32", "declared @noescape"),
        ("fn value(&self) -> Int32 { 42 }", "declared @noescape"),
        (
            "fn value(&self, other: &Self) @noescape -> Bool",
            "Self outside its receiver",
        ),
        ("fn value(&self) @noescape -> Self", "returns Self"),
        (
            "fn value(&self) @noescape -> &Int32",
            "cannot return references",
        ),
        (
            "fn value(&self, other: &Int32) @noescape(self) -> Int32",
            "declared @noescape",
        ),
    ] {
        rejected(
            &format!("trait Read {{ {method} }} fn unused(value: &dyn Read) {{}}"),
            diagnostic,
        );
    }
}

#[test]
fn borrowed_dyn_requires_complete_valid_associated_equalities() {
    for (binding, expected) in [
        ("", "explicit equality for 'Item'"),
        ("<Other = Int32>", "Unknown dyn associated type 'Other'"),
        (
            "<Item = Int32, Item = Int32>",
            "Duplicate dyn associated equality",
        ),
        ("<Item = Missing>", "Unknown type"),
        ("<Item = &Int32>", "cannot return references"),
        ("<Item = (Int32, &Int32)>", "cannot return references"),
    ] {
        rejected(&format!("trait Read {{ type Item; fn value(&self) @noescape -> Self.Item }} fn unused(value: &dyn Read{binding}) {{}}"), expected);
    }
    rejected("trait Mark {} trait Read { type Item: Mark; fn value(&self) @noescape -> Self.Item } fn unused(value: &dyn Read<Item = Int32>) {}", "declared capabilities");
}

#[test]
fn borrowed_dyn_qualified_types_never_reach_backend_layouts() {
    for borrow in ["&dyn", "&mut dyn"] {
        for result in ["Int32", "String", "(Int32, Bool)", "Unit"] {
            rejected(&format!("trait Read {{ type Item; fn value(&self) @noescape -> Self.Item }} fn unused(value: {borrow} Read<Item = {result}>) {{}}"), "dynamic dispatch requires V2");
        }
        rejected(&format!("trait Read {{ fn value(&self, other: &Int32) @noescape(self, other) -> Int32 }} fn unused(value: {borrow} Read) {{}}"), "dynamic dispatch requires V2");
    }
    rejected(
        "trait Read { fn value(&self) @noescape -> Int32 } fn unused(value: dyn Read) {}",
        "Bare dyn trait types",
    );
    rejected("fn unused(value: &dyn Missing) {}", "Unknown trait bound");
}

#[test]
fn borrowed_dyn_keyword_and_type_syntax_are_checked() {
    for source in [
        "fn dyn() {}",
        "fn main() { let dyn = 1 }",
        "struct dyn {}",
        "fn f(value: &dyn Read<Int32>) {}",
        "fn f(value: &dyn Read<>) {}",
        "fn f(value: &dyn Read<Item = Int32 Other = Int32>) {}",
        "fn f(value: &dyn Read + Write) {}",
    ] {
        assert!(crate::parser::parse(source).is_err(), "{source}");
    }
    let parsed =
        crate::parser::parse("fn f(value: &mut dyn Read<Item = Int32, Other = (Bool,),>) {}")
            .unwrap();
    let crate::parser::ast::Item::FnDef(f) = &parsed.items[0] else {
        panic!("function")
    };
    assert_eq!(
        f.params[0].ty.to_string(),
        "&mut dyn Read<Item = Int32, Other = (Bool,)>"
    );
}

#[test]
fn borrowed_dyn_qualification_shares_the_projection_budget() {
    let mut target = "Int32".to_owned();
    for _ in 0..40 {
        target = format!("&dyn Read<Item = {target}>");
    }
    rejected(&format!("trait Read {{ type Item; fn read(&self) @noescape -> Int32 }} fn unused(value: {target}) {{}}"), "32 dependency levels");
}

#[test]
fn borrowed_dyn_failed_preparation_preserves_previous_module() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source("fn answer() -> Int32 { 42 }").unwrap();
        for item in ["Int32", "String", "&Int32"] {
            let source = format!("trait Read {{ type Item; fn read(&self) @noescape -> Self.Item }} fn unused(value: &dyn Read<Item = {item}>) {{}}");
            assert!(vm.load_source(&source).is_err());
            assert_eq!(vm.call_int32_0("answer").unwrap(), 42);
        }
    }
}

#[test]
fn borrowed_dyn_qualification_does_not_restrict_static_traits() {
    let source = "trait Read { type Item; fn value(self) -> Self.Item } struct Counter { value: Int32 } impl Read for Counter { type Item = Int32; fn value(self) -> Int32 { self.value } } fn answer() -> Int32 { Counter { value: 42 }.value() }";
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        assert_eq!(vm.call_int32_0("answer").unwrap(), 42);
    }
    #[cfg(unix)]
    crate::aot::tests::assert_sync_int32(&crate::parser::parse(source).unwrap(), "answer", 42);
}

#[test]
fn borrowed_dyn_v1_boundaries_are_shared_by_all_backends() {
    let base = "trait Read { fn value(&self) @noescape -> Int32; fn change(&mut self) @noescape {} } struct Counter { value: Int32 } impl Read for Counter { fn value(&self) -> Int32 { self.value } } fn use_view(v: &dyn Read) {} fn use_mut(v: &mut dyn Read) {}";
    for (body, expected) in [
        ("fn f() { var c = Counter { value: 1 } let v: &dyn Read = &c; c.value = 2; use_view(v) }", "borrow"),
        ("fn f() { var c = Counter { value: 1 } let v: &mut dyn Read = &mut c; let w = v; use_mut(v); use_mut(w) }", "moved"),
        ("fn f() { let v: &dyn Read = &Counter { value: 1 } }", "temporary borrowing"),
        ("fn f() -> &dyn Read { let c = Counter { value: 1 } &c }", "cannot be returned"),
        ("struct Hidden { v: &dyn Read }", "Dyn views cannot be stored"),
        ("enum Hidden { Some(&dyn Read) }", "Dyn views cannot be stored"),
        ("fn f(v: (&dyn Read, Int32)) {}", "Dyn views cannot be stored"),
        ("fn f(v: fn(&dyn Read) -> Unit) {}", "Dyn views cannot be stored"),
        ("fn f(v: &dyn Read) @suspends {}", "synchronous function"),
        ("extern fn f(v: &dyn Read)", "host ABI"),
        ("fn f(v: &dyn Read) { memory.read(v); }", "Dyn storage"),
        ("fn f(v: &dyn Read) { let c = fn() { use_view(v) }; c() }", "cannot be captured"),
        ("fn f(v: &dyn Read) { v.change(); }", "exclusive dyn method"),
        ("fn f(v: &dyn Read) { Read.change(v); }", "exclusive dyn method"),
        ("fn f(v: &dyn Read) { v.absent(); }", "Dyn interface has no method"),
        ("fn f(v: &dyn Read) { v.value(); }", "requires V2"),
        ("fn f(v: &dyn Read) { Read.value(v); }", "requires V2"),
        ("fn f() { let c = Counter { value: 1 } use_view(&c) }", "requires V2"),
        ("fn f() { let c = Counter { value: 1 }; use_mut(&c) }", "shared borrow"),
        ("fn f() { let c = Counter { value: 1 }; use_view(c) }", "explicit borrow"),
        ("fn f() { let c = 1; use_view(&c) }", "no applicable impl"),
        ("fn f(v: &mut dyn Read) { memory.write(v, 1); }", "Dyn storage"),
        ("fn f(v: &mut dyn Read) { memory.replace(v, 1); }", "Dyn storage"),
        ("fn f(v: &dyn Read) { alloc(v); }", "alloc requires Copy data"),
        ("fn needs<T: Read>(v: T) {} fn f(v: &dyn Read) { needs(v) }", "does not satisfy"),
        ("trait Other { fn value(&self) @noescape -> Int32; } fn f(v: &dyn Read) { Other.value(v); }", "different trait"),
        ("fn pause() @suspends {} fn f() @suspends { let c = alloc(Counter { value: 1 }); let v: &dyn Read = c; pause(); use_view(v) }", "dyn view cannot remain live"),
    ] {
        rejected(&format!("{base} {body}"), expected);
    }
}

#[test]
fn borrowed_dyn_v1_rechecks_instantiated_function_boundaries() {
    let base = "trait Mark {} impl Mark for Int32 {}";
    for (body, expected) in [
        ("fn never<T>() -> T { never::<T>() } fn f() { let v: &dyn Mark = never::<&dyn Mark>(); }", "Dyn views cannot be returned"),
        ("fn identity<T>(v: T) -> T { v } fn f(v: &dyn Mark) { identity(v); }", "Dyn views cannot be returned"),
        ("fn wait<T>(v: T) @suspends {} fn f() @suspends { let p = alloc(1); let v: &dyn Mark = p; wait(v) }", "Dyn parameters require a synchronous function"),
    ] {
        rejected(&format!("{base} {body}"), expected);
    }
}
