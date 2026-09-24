use super::*;

#[test]
fn user_drop_runs_with_live_fields_and_without_recursive_receiver_drop() {
    run_program(
        r#"
        class Child { let text: String }
        impl Drop for Child { fn drop(&self) { println(self.text) } }
        class Parent { var child: Child; let text: String }
        impl Drop for Parent { fn drop(&self) { println(self.text); println(self.child.text) } }
        fn main() { let value = Parent(Child("child"), "parent") }
    "#,
    );
}

#[test]
fn user_drop_rejects_invalid_protocols_and_control_flow() {
    for source in [
        "struct C {} impl Drop for C { fn drop(&self) {} }",
        "class C {} impl Drop for C { fn drop(self) {} }",
        "class C {} impl Drop for C { fn drop() {} }",
        "class C {} impl Drop for C { fn drop(&self) -> Int32 { 1 } }",
        "class C {} impl Drop for C { fn drop(&self, x: Int32) {} }",
        "class C {} impl Drop for C { fn drop(&self) {} } impl Drop for C { fn drop(&self) {} }",
        "class C {} impl Drop for C { fn drop(&self) {} } fn bad() { C().drop() }",
        "class C {} impl Drop for C { fn drop(&self) { panic(\"bad\") } }",
        "class C {} impl Drop for C { fn drop(&self) { let bad: Option(Int32) = None; let x = bad! } }",
        "class C {} fn fail() { panic(\"bad\") } impl Drop for C { fn drop(&self) { fail() } }",
        "eff E { @suspends fn wait() -> Unit } class C {} impl Drop for C { fn drop(&self) effects { E } { E.wait() } }",
        "eff E { fn ping() -> Unit } class C {} impl Drop for C { fn drop(&self) effects { E } { E.ping() } }",
        "class C {} impl Drop for C { fn drop(&self) { branch {} } }",
        "class C {} impl Drop for C { fn drop(&self) {} } fn bad() { let c = C(); let f = move fn () -> Unit { c } }",
        "class C {} impl Drop for C { fn drop(&self) {} } fn bad() { let c = Cown.new(C()) }",
        "class C {} impl Drop for C { fn drop(&self) {} } class Box { let c: C } fn bad() { let c = Cown.new(Box(C())) }",
    ] {
        let error = try_run_program(&format!("{source}\nfn main() {{}}")).unwrap_err();
        assert!(!error.to_string().contains("cannot load"), "{source}: {error}");
    }
}

#[test]
fn implicit_drop_effects_cannot_be_erased_by_forward_calls_or_generics() {
    let header = r#"
        eff cleanup { fn release() -> Unit }
        class C {}
        impl Drop for C { fn drop(&self) effects { cleanup } {} }
    "#;
    for body in [
        "fn bad(c: C) {}",
        "fn bad() { let c = C() }",
        "class Box { let c: C } fn bad(b: Box) {}",
        "fn bad() { helper() } fn helper() effects { cleanup } { let c = C() }",
        "fn forget(T: type, value: T) {} fn bad() effects { cleanup } { forget(C, C()) }",
    ] {
        let error = try_run_program(&format!("{header}\n{body}\nfn main() {{}}"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("effect"), "{body}: {error}");
    }
}
