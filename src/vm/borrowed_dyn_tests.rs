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
