//! Borrowed dynamic dispatch and shared front-end boundaries.
use super::{VmEngine, VmMode};

fn rejected(source: &str, expected: &str) {
    let error = super::support::frontend_error(source, &crate::ModuleSources::new());
    assert!(
        error.message().contains(expected),
        "{expected}: {error}\n{source}"
    );
    assert!(error.span().is_some(), "{error}");
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
fn borrowed_dyn_qualified_unused_signatures_compile_without_host_layouts() {
    for borrow in ["&dyn", "&mut dyn"] {
        for result in ["Int32", "String", "(Int32, Bool)", "Unit"] {
            accepted(&format!("trait Read {{ type Item; fn value(&self) @noescape -> Self.Item }} fn unused(value: {borrow} Read<Item = {result}>) {{}}"));
        }
        accepted(&format!("trait Read {{ fn value(&self, other: &Int32) @noescape(self, other) -> Int32 }} fn unused(value: {borrow} Read) {{}}"));
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
        for item in ["&Int32", "(Int32, &Int32)"] {
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
    let base = "import joky/mem\ntrait Read { fn value(&self) @noescape -> Int32; fn change(&mut self) @noescape {} } struct Counter { value: Int32 } impl Read for Counter { fn value(&self) -> Int32 { self.value } } fn use_view(v: &dyn Read) {} fn use_mut(v: &mut dyn Read) {}";
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
        ("fn f(v: &dyn Read) { (*v); }", "Dyn storage"),
        ("fn f(v: &dyn Read) { let c = || { use_view(v) }; c() }", "cannot be captured"),
        ("fn f(v: &dyn Read) { v.change(); }", "exclusive dyn method"),
        ("fn f(v: &dyn Read) { Read.change(v); }", "exclusive dyn method"),
        ("fn f(v: &dyn Read) { v.absent(); }", "Dyn interface has no method"),
        ("fn f() { let c = Counter { value: 1 }; use_mut(&c) }", "shared borrow"),
        ("fn f() { let c = Counter { value: 1 }; use_view(c) }", "explicit borrow"),
        ("fn f() { let c = 1; use_view(&c) }", "no applicable impl"),
        ("fn f(v: &mut dyn Read) { (*(v) = 1); }", "Dyn storage"),
        ("fn f(v: &mut dyn Read) { mem.replace(v, 1); }", "dyn trait"),
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

fn accepted(source: &str) {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        VmEngine::with_mode(mode)
            .load_source(source)
            .unwrap_or_else(|e| panic!("{mode:?}: {e}\n{source}"));
    }
    crate::aot::AotCompiler::new("borrowed-dyn")
        .unwrap()
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap();
}

#[test]
fn borrowed_dyn_v2_executes_default_override_and_exclusive_slots() {
    let source = "trait Read { type Item; fn value(&self) @noescape -> Int32; fn doubled(&self) @noescape -> Int32 { self.value() * 2 } fn change(&mut self, n: Int32) @noescape; } struct Counter { value: Int32 } struct Other { value: Int32 } impl Read for Counter { type Item = Int32; fn value(&self) -> Int32 { self.value } fn change(&mut self, n: Int32) { self.value = n } } impl Read for Other { type Item = Int32; fn value(&self) -> Int32 { self.value + 1 } fn doubled(&self) -> Int32 { self.value() * 2 } fn change(&mut self, n: Int32) { self.value = n - 1 } } fn inspect(v: &dyn Read<Item = Int32>) @noescape -> Int32 { v.doubled() } fn update(v: &mut dyn Read<Item = Int32>) @noescape { v.change(21); Read.change(v, 21) } fn answer() -> Int32 { var a = Counter { value: 1 } var b = Other { value: 1 } let v: &mut dyn Read<Item = Int32> = &mut a; update(v); update(v); inspect(v) + inspect(&b) - 4 }";
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source)
            .unwrap_or_else(|e| panic!("{mode:?}: {e}, span {:?}", e.span()));
        assert_eq!(vm.call_int32_0("answer").unwrap(), 42, "{mode:?}");
    }
    #[cfg(unix)]
    crate::aot::tests::assert_sync_int32(&crate::parser::parse(source).unwrap(), "answer", 42);
}

const DISPATCH: &str = include_str!("../../tests/fixtures/borrowed_dyn_dispatch.jk");
const LIFECYCLE: &str = include_str!("../../tests/fixtures/borrowed_dyn_lifecycle.jk");
const ENTRIES: &[&str] = &[
    "dyn_basic",
    "dyn_mutable",
    "dyn_results",
    "dyn_branch_true",
    "dyn_branch_false",
    "dyn_zero",
    "dyn_enum",
    "dyn_shadow",
];

#[test]
fn borrowed_dyn_v2_executes_signature_results_and_source_matrix() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(DISPATCH)
            .unwrap_or_else(|e| panic!("{mode:?}: {e}, span {:?}", e.span()));
        for entry in ENTRIES {
            assert_eq!(vm.call_int32_0(entry).unwrap(), 42, "{mode:?}: {entry}");
        }
        crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
    }
    #[cfg(unix)]
    crate::aot::tests::assert_sync_results(
        &crate::parser::parse(DISPATCH).unwrap(),
        &ENTRIES
            .iter()
            .map(|&name| (name, Some(42)))
            .collect::<Vec<_>>(),
    );
}

thread_local! { static EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) }; }
unsafe extern "C" fn record(value: i32) -> i32 {
    EVENTS.with(|events| events.borrow_mut().push(value));
    value
}
fn lifecycle_vm(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    unsafe {
        vm.register_fn(
            "record",
            crate::FunctionSignature::new(vec![crate::Type::Int32], crate::Type::Int32),
            record as *const u8,
        )
        .unwrap();
    }
    unsafe {
        vm.register_suspending_fn(
            "delayed_value",
            crate::FunctionSignature::new(vec![crate::Type::Int32], crate::Type::Int32),
            delayed_value,
        )
        .unwrap();
    }
    vm.load_source(LIFECYCLE)
        .unwrap_or_else(|e| panic!("{mode:?}: {e}, span {:?}", e.span()));
    vm
}
#[test]
fn borrowed_dyn_v2_owners_and_arguments_clean_up_on_success_and_failure() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = lifecycle_vm(mode);
        for (entry, expected) in [
            ("dyn_success", &[10, 8, 7][..]),
            ("dyn_order", &[20, 22, 30, 7][..]),
            ("dyn_failure", &[10, 8, 7][..]),
            ("dyn_argument", &[8, 7][..]),
            ("dyn_argument_failure", &[8, 7][..]),
            ("dyn_argument_drop_failure", &[9, 7][..]),
        ] {
            EVENTS.with(|events| events.borrow_mut().clear());
            let result = vm.call_int32_0(entry);
            if entry.contains("failure") {
                assert!(result.unwrap_err().message().contains("zero"));
            } else {
                assert_eq!(result.unwrap(), 42);
            }
            EVENTS.with(|events| assert_eq!(*events.borrow(), expected, "{mode:?}: {entry}"));
            crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
        }
        assert_eq!(
            vm.call_async("dyn_task", &[]).unwrap().cps_result(),
            Some(42)
        );
        crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

#[test]
fn borrowed_dyn_v2_time_slices_cancel_and_preserve_old_code() {
    let mut vm = lifecycle_vm(VmMode::Interpreter);
    vm.set_interpreter_limits(super::InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    for cancel in [true, false] {
        EVENTS.with(|events| events.borrow_mut().clear());
        let mut task = vm.call_async("dyn_task", &[]).unwrap();
        for _ in 0..256 {
            if EVENTS.with(|events| events.borrow().contains(&10)) {
                break;
            }
            assert!(task.cps_result().is_none());
            task.resume().unwrap();
        }
        EVENTS.with(|events| assert_eq!(*events.borrow(), [10]));
        if !cancel {
            assert!(vm.load_source("fn invalid(v: &dyn Missing) {}").is_err());
            vm.load_source(&LIFECYCLE.replace("42 / divisor", "99 / divisor"))
                .unwrap();
            drop(vm);
            for _ in 0..256 {
                if task.cps_result().is_some() {
                    break;
                }
                task.resume().unwrap();
            }
            assert_eq!(task.cps_result(), Some(42));
        }
        drop(task);
        EVENTS.with(|events| assert_eq!(*events.borrow(), [10, 8, 7]));
        crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
        vm = lifecycle_vm(VmMode::Interpreter);
        vm.set_interpreter_limits(super::InterpreterLimits {
            poll_budget: Some(1),
            ..Default::default()
        })
        .unwrap();
    }
}

#[test]
#[ignore = "exhaustive: cancel borrowed dyn calls at every interpreter instruction"]
fn exhaustive_borrowed_dyn_v2_instruction_cancellation() {
    let mut vm = lifecycle_vm(VmMode::Interpreter);
    vm.set_interpreter_limits(super::InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    let mut completed = false;
    for boundary in 0..512 {
        EVENTS.with(|events| events.borrow_mut().clear());
        let mut task = vm.call_async("dyn_task", &[]).unwrap();
        for _ in 0..boundary {
            if task.cps_result().is_some() {
                break;
            }
            task.resume().unwrap();
        }
        completed = task.cps_result().is_some();
        if completed {
            assert_eq!(task.cps_result(), Some(42));
        }
        drop(task);
        EVENTS.with(|events| {
            let events = events.borrow();
            for event in [10, 8, 7] {
                assert!(
                    events.iter().filter(|&&v| v == event).count() <= 1,
                    "boundary {boundary}: {events:?}"
                );
            }
            if completed || events.contains(&10) {
                assert_eq!(*events, [10, 8, 7], "boundary {boundary}");
            }
        });
        crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0, "boundary {boundary}"));
        if completed {
            break;
        }
    }
    assert!(completed);
}

#[test]
fn borrowed_dyn_v2_budget_failure_cleans_up_dynamic_frames() {
    let mut vm = lifecycle_vm(VmMode::Interpreter);
    for budget in [1, 20, 60, 100, 1000] {
        vm.set_interpreter_limits(super::InterpreterLimits {
            instruction_budget: Some(budget),
            poll_budget: Some(1),
            ..Default::default()
        })
        .unwrap();
        EVENTS.with(|events| events.borrow_mut().clear());
        let mut task = match vm.call_async("dyn_task", &[]) {
            Ok(task) => task,
            Err(error) => {
                assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
                crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
                continue;
            }
        };
        let mut failed = false;
        for _ in 0..1024 {
            if task.cps_result().is_some() {
                break;
            }
            if let Err(crate::runtime::suspension::ResumeError::Failed(error)) = task.resume() {
                assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
                failed = true;
                break;
            }
        }
        assert!(failed || task.cps_result() == Some(42));
        drop(task);
        EVENTS.with(|events| {
            let events = events.borrow();
            for event in [10, 8, 7] {
                assert!(events.iter().filter(|&&v| v == event).count() <= 1);
            }
        });
        crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

thread_local! { static READY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
unsafe extern "C" fn delayed_value(frame: *mut crate::runtime::cps::CpsFrame) -> i32 {
    let frame = &mut *frame;
    if frame.state == 0 {
        record(frame.slots[0] as i32);
        frame.state = 1;
        return 1;
    }
    if !READY.with(|r| r.get()) {
        return 1;
    }
    frame.result = frame.slots[0] + 1;
    0
}
#[test]
fn borrowed_dyn_v2_pending_cancellation_and_reachable_method_code() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = lifecycle_vm(mode);
        READY.with(|r| r.set(false));
        EVENTS.with(|e| e.borrow_mut().clear());
        let mut cancelled = vm.call_async("dyn_pending", &[]).unwrap();
        for _ in 0..3 {
            cancelled.resume().unwrap();
            assert!(cancelled.cps_result().is_none());
        }
        EVENTS.with(|e| assert_eq!(*e.borrow(), [10, 8, 7, 41]));
        drop(cancelled);
        EVENTS.with(|e| assert_eq!(*e.borrow(), [10, 8, 7, 41]));
        crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
        EVENTS.with(|e| e.borrow_mut().clear());
        let mut old = vm.call_async("dyn_after_pending", &[]).unwrap();
        vm.load_source(&LIFECYCLE.replace("42 / divisor", "99 / divisor"))
            .unwrap();
        let mut new = vm.call_async("dyn_after_pending", &[]).unwrap();
        drop(vm);
        READY.with(|r| r.set(true));
        old.resume().unwrap();
        new.resume().unwrap();
        assert_eq!(old.cps_result(), Some(42));
        assert_eq!(new.cps_result(), Some(99));
        drop((old, new));
        EVENTS.with(|e| assert_eq!(*e.borrow(), [41, 41, 10, 8, 7, 10, 8, 7]));
        crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

#[test]
fn borrowed_dyn_v2_method_allocation_failure_restores_budget() {
    struct Reset(usize);
    impl Drop for Reset {
        fn drop(&mut self) {
            crate::runtime::arena::LIMIT.with(|n| n.set(self.0));
        }
    }
    let _reset = Reset(crate::runtime::arena::LIMIT.with(|n| n.get()));
    let source = format!("trait Make {{ fn text(&self) @noescape -> String {{ \"{}\" }} }} struct Empty {{}} impl Make for Empty {{}} fn answer() -> Int32 {{ let e = Empty {{}} let v: &dyn Make = &e let text = v.text() int32(text.byte_length()) }}", "x".repeat(42));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(&source).unwrap();
        crate::runtime::arena::LIMIT.with(|n| n.set(0));
        let error = vm.call_int32_0("answer").unwrap_err();
        assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
        crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
        crate::runtime::arena::LIMIT.with(|n| n.set(_reset.0));
        assert_eq!(vm.call_int32_0("answer").unwrap(), 42);
        crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

#[test]
fn borrowed_dyn_v2_import_aliases_private_signatures_and_diagnostics() {
    let mut modules = crate::ModuleSources::new();
    modules.insert("base", "type Number = Int32; pub trait Read { type Item; fn value(&self, n: Number) @noescape -> Self.Item; fn default_value(&self) @noescape -> Self.Item { self.value(1) } } pub struct Counter { value: Number } impl Read for Counter { type Item = Int32; fn value(&self, n: Number) -> Int32 { self.value / n } } pub fn make() -> Counter { Counter { value: 42 } } pub type View = &dyn Read<Item = Number>;").unwrap();
    let source = "import base as b.{type Read as Renamed}; fn inspect(v: b.View) @noescape -> Int32 { Renamed.default_value(v) } fn answer() -> Int32 { let c = b.make() let v: &dyn Renamed<Item = Int32> = &c inspect(v) } fn failure() -> Int32 { let c = b.make() let v: b.View = &c v.value(0) }";
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source_with_modules(source, &modules)
            .unwrap_or_else(|e| panic!("{mode:?}: {e}, span {:?}", e.span()));
        assert_eq!(vm.call_int32_0("answer").unwrap(), 42);
        let error = vm.call_int32_0("failure").unwrap_err();
        assert!(error.message().contains("zero"));
        let span = error.span().unwrap();
        assert!(error
            .sources()
            .get(span.source_id())
            .unwrap()
            .text()
            .contains("self.value / n"));
    }
    #[cfg(unix)]
    {
        let ast = crate::TypeRegistry::default()
            .resolve_with_modules(&crate::parser::parse(source).unwrap(), &modules)
            .unwrap();
        crate::aot::tests::assert_sync_results(&ast, &[("answer", Some(42)), ("failure", None)]);
    }
}

#[test]
fn borrowed_dyn_v2_rejects_dynamic_call_aliases_and_invalid_arguments() {
    let base = "import joky/mem\ntrait Read { fn value(&self, other: &Int32) @noescape -> Int32; fn change(&mut self, other: &Int32) @noescape; } struct Counter { value: Int32 } impl Read for Counter { fn value(&self, other: &Int32) -> Int32 { (*other) } fn change(&mut self, other: &Int32) { self.value = (*other) } }";
    for (body, message) in [
        ("fn f(v: &dyn Read) { v.value() }", "Argument count mismatch"),
        ("fn f(v: &dyn Read) { v.value(true) }", "Argument type mismatch"),
        ("fn f(v: &dyn Read) { Read.value::<Int32>(v, true) }", "do not accept type arguments"),
        ("fn f() { var c = Counter { value: 1 } let v: &mut dyn Read = &mut c v.change(&c.value) }", "borrow"),
        ("fn f(flag: Bool) { var a = Counter { value: 1 } var b = Counter { value: 2 } let v: &dyn Read = if flag { &a } else { &b } a.value = 3 let n = 1 v.value(&n); }", "borrow"),
        ("fn f(flag: Bool) { var a = Counter { value: 1 } var b = Counter { value: 2 } let v: &dyn Read = if flag { &a } else { &b } b.value = 3 let n = 1 v.value(&n); }", "borrow"),
        ("fn f(v: &dyn Read) { let method = v.value; }", "Bound dyn methods"),
    ] { rejected(&format!("{base} {body}"), message); }
}

#[test]
fn borrowed_dyn_v2_failure_keeps_first_error_during_failing_drop() {
    let source = LIFECYCLE.replace(
        "record(self.id);",
        "record(self.id); let n: Int8 = 127; n + 1;",
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = lifecycle_vm(mode);
        vm.load_source(&source).unwrap();
        EVENTS.with(|e| e.borrow_mut().clear());
        let error = vm.call_int32_0("dyn_failure").unwrap_err();
        assert!(error.message().contains("zero"), "{error}");
        EVENTS.with(|e| assert_eq!(*e.borrow(), [10, 8, 7]));
        crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

const COMPOSITION: &str = include_str!("../../tests/fixtures/borrowed_dyn_composition.jk");

#[test]
fn borrowed_dyn_v3_reborrows_helpers_fields_and_all_sources_execute() {
    let entries = [
        "dyn_reborrow",
        "dyn_generic_reborrow",
        "dyn_fields",
        "dyn_sources_first",
        "dyn_sources_second",
        "dyn_sources_third",
        "dyn_zero_origins",
        "dyn_match_shared_first",
        "dyn_match_shared_second",
        "dyn_match_mutable_first",
        "dyn_match_mutable_second",
        "dyn_match_argument_first",
        "dyn_match_argument_second",
        "dyn_match_assignment_first",
        "dyn_match_assignment_second",
    ];
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(COMPOSITION)
            .unwrap_or_else(|e| panic!("{mode:?}: {e}, span {:?}", e.span()));
        for entry in entries {
            assert_eq!(vm.call_int32_0(entry).unwrap(), 42, "{mode:?}: {entry}");
        }
        crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
    }
    #[cfg(unix)]
    crate::aot::tests::assert_sync_results(
        &crate::parser::parse(COMPOSITION).unwrap(),
        &entries
            .iter()
            .map(|&entry| (entry, Some(42)))
            .collect::<Vec<_>>(),
    );
}

#[test]
fn borrowed_dyn_match_rejects_conflicting_origins_and_permissions() {
    for body in [
        "fn bad(choice: Choice) { var c = Counter { value: 1 } var o = Other { value: 2 } let v: View = match choice { Choice.First => &c, Choice.Second => &o }; c.value = 3; inspect(v); }",
        "fn bad(choice: Choice) { var c = Counter { value: 1 } var o = Other { value: 2 } let v: View = match choice { Choice.First => &c, Choice.Second => &o }; o.value = 3; inspect(v); }",
        "fn bad(choice: Choice) { var c = Counter { value: 1 } var o = Other { value: 2 } let v: MutView = match choice { Choice.First => &mut c, Choice.Second => &mut o }; let shared = &*v; update(&mut *v, 3); inspect(shared); }",
    ] {
        rejected(&format!("{COMPOSITION}\n{body}"), "borrow");
    }
    rejected(
        &format!("{COMPOSITION}\nfn bad(choice: Choice) {{ var c = Counter {{ value: 1 }} let o = Other {{ value: 2 }} let v: MutView = match choice {{ Choice.First => &mut c, Choice.Second => &o }}; update(v, 3); }}"),
        "shared borrow",
    );
}

#[test]
fn borrowed_dyn_v3_reborrows_reject_permission_conflicts_and_escape() {
    let base = "import joky/mem\ntrait Read { fn value(&self) @noescape -> Int32; fn change(&mut self, n: Int32) @noescape; } struct Counter { value: Int32 } impl Read for Counter { fn value(&self) -> Int32 { self.value } fn change(&mut self, n: Int32) { self.value = n } } fn inspect(v: &dyn Read) @noescape -> Int32 { v.value() } fn update(v: &mut dyn Read) @noescape { v.change(21) }";
    for (body, message) in [
        ("fn f(v: &dyn Read) @noescape { update(&mut *v) }", "shared reference as mutable"),
        ("fn f(v: &mut dyn Read) @noescape { let s = &*v; update(v); inspect(s); }", "borrow"),
        ("fn f(v: &mut dyn Read) @noescape { let s = &mut *v; inspect(v); update(s); }", "borrow"),
        ("fn pair(a: &mut dyn Read, b: &dyn Read) @noescape {} fn f(v: &mut dyn Read) @noescape { pair(&mut *v, &*v) }", "borrow"),
        ("fn f() { let v: &dyn Read = { let c = Counter { value: 1 } let v: &dyn Read = &c; &*v }; inspect(v); }", "outlive"),
        ("fn f(v: &dyn Read) @noescape { let capture = || -> Int32 { inspect(&*v) }; capture(); }", "captured"),
        ("fn f() { let c = Counter { value: 1 }; &*c; }", "requires a checked reference"),
        ("fn f(flag: Bool) { var a = Counter { value: 1 } var b = Counter { value: 2 } var c = Counter { value: 3 } let v: &dyn Read = if flag { &a } else { if flag { &b } else { &c } }; c.value = 4; inspect(&*v); }", "borrow"),
        ("fn wait() @suspends {} fn f() @suspends { var c = Counter { value: 1 } let v: &mut dyn Read = &mut c; let s = &*v; wait(); inspect(s); }", "across suspension"),
        ("fn wait() @suspends {} fn f(v: &dyn Read) @noescape { parallel { | inspect(&*v); | wait(); } }", "@suspends"),
        ("fn wait() @suspends {} fn f() @suspends { var c = Counter { value: 1 } let v: &mut dyn Read = &mut c; parallel { | wait(); | wait(); } inspect(&*v); }", "concurrency"),
    ] { rejected(&format!("{base} {body}"), message); }
}

#[test]
fn borrowed_dyn_v3_synchronous_local_segments_in_tasks_clean_up() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = lifecycle_vm(mode);
        EVENTS.with(|e| e.borrow_mut().clear());
        let task = vm.call_async("dyn_local_task", &[]).unwrap();
        assert_eq!(task.cps_result(), Some(42));
        drop(task);
        EVENTS.with(|e| assert_eq!(*e.borrow(), [10, 8]));
        READY.with(|r| r.set(false));
        EVENTS.with(|e| e.borrow_mut().clear());
        let mut task = vm.call_async("dyn_local_between_pending", &[]).unwrap();
        READY.with(|r| r.set(true));
        task.resume().unwrap();
        if task.cps_result().is_none() {
            task.resume().unwrap();
        }
        assert_eq!(task.cps_result(), Some(42));
        drop(task);
        EVENTS.with(|e| assert_eq!(*e.borrow(), [41, 10, 8, 41]));
        crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

#[test]
fn borrowed_dyn_v3_instance_views_keep_original_owners_and_snapshots() {
    use super::{InstanceStatus, InstanceValue as V};
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = lifecycle_vm(mode);
        let old = vm.create_instance("create_state", &[]).unwrap();
        let bytes = vm.instance_bytes();
        for _ in 0..8 {
            EVENTS.with(|e| e.borrow_mut().clear());
            assert_eq!(
                vm.call_instance(old, "instance_read", &[]).unwrap(),
                V::Int32(42)
            );
            assert_eq!(
                vm.call_instance(old, "instance_change", &[]).unwrap(),
                V::Int32(42)
            );
            assert_eq!(vm.instance_bytes(), bytes);
            EVENTS.with(|e| assert_eq!(*e.borrow(), [10, 8, 8]));
        }
        assert!(vm.load_source("fn invalid(v: &dyn Missing) {}").is_err());
        vm.load_source(&LIFECYCLE.replace("42 / divisor", "99 / divisor"))
            .unwrap();
        let new = vm.create_instance("create_state", &[]).unwrap();
        EVENTS.with(|e| e.borrow_mut().clear());
        assert_eq!(
            vm.call_instance(old, "instance_read", &[]).unwrap(),
            V::Int32(42)
        );
        assert_eq!(
            vm.call_instance(new, "instance_read", &[]).unwrap(),
            V::Int32(99)
        );
        let error = vm.call_instance(old, "instance_failure", &[]).unwrap_err();
        assert!(error.message().contains("zero"));
        assert_eq!(vm.instance_status(old).unwrap(), InstanceStatus::Failed);
        assert_eq!(vm.instance_bytes(), bytes);
        assert!(vm.call_instance(old, "instance_read", &[]).is_err());
        vm.close_instance(old).unwrap();
        assert_eq!(
            vm.call_instance(new, "instance_change", &[]).unwrap(),
            V::Int32(99)
        );
        drop(vm);
        EVENTS.with(|e| assert_eq!(*e.borrow(), [10, 8, 10, 8, 10, 8, 7, 8, 7]));
        crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

fn dyn_memory_sweep(step: usize) {
    struct Reset(usize);
    impl Drop for Reset {
        fn drop(&mut self) {
            crate::runtime::arena::LIMIT.with(|n| n.set(self.0));
        }
    }
    let reset = Reset(crate::runtime::arena::LIMIT.with(|n| n.get()));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = lifecycle_vm(mode);
        for entry in ["dyn_success", "dyn_argument"] {
            let mut completed = false;
            let mut failed = false;
            for limit in (0..4096).step_by(step) {
                crate::runtime::arena::LIMIT.with(|n| n.set(limit));
                EVENTS.with(|e| e.borrow_mut().clear());
                match vm.call_int32_0(entry) {
                    Ok(value) => {
                        assert_eq!(value, 42);
                        completed = true;
                    }
                    Err(error) => {
                        assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
                        failed = true;
                    }
                }
                EVENTS.with(|e| {
                    let events = e.borrow();
                    for id in [10, 8, 7] {
                        assert!(
                            events.iter().filter(|&&e| e == id).count() <= 1,
                            "{mode:?}/{entry}/{limit}: {events:?}"
                        );
                    }
                    if completed {
                        assert_eq!(
                            *events,
                            if entry == "dyn_success" {
                                vec![10, 8, 7]
                            } else {
                                vec![8, 7]
                            }
                        );
                    }
                });
                crate::runtime::arena::LIVE
                    .with(|n| assert_eq!(n.get(), 0, "{mode:?}/{entry}/{limit}"));
                if completed {
                    break;
                }
            }
            assert!(completed && failed);
            crate::runtime::arena::LIMIT.with(|n| n.set(reset.0));
            assert_eq!(vm.call_int32_0(entry).unwrap(), 42);
        }
    }
}

#[test]
fn borrowed_dyn_v4_memory_boundaries_restore_budget_and_owners() {
    dyn_memory_sweep(32);
}

#[test]
fn borrowed_dyn_v4_method_capacity_failure_releases_original_owner() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = lifecycle_vm(mode);
        EVENTS.with(|e| e.borrow_mut().clear());
        let error = vm.call_int32_0("dyn_large_failure").unwrap_err();
        assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
        EVENTS.with(|e| assert_eq!(*e.borrow(), [7]));
        crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
        assert_eq!(vm.call_int32_0("dyn_success").unwrap(), 42);
    }
}

fn dyn_instance_budget_sweep(step: usize) {
    use super::{InstanceLimits, InstanceStatus, InstanceValue as V};
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = lifecycle_vm(mode);
        for method in ["instance_read", "instance_change"] {
            let mut failed = false;
            let mut completed = false;
            for limit in (1..4096).step_by(step) {
                vm.set_instance_limits(InstanceLimits::default()).unwrap();
                let id = vm.create_instance("create_state", &[]).unwrap();
                vm.set_instance_limits(InstanceLimits {
                    max_call_bytes: limit,
                    ..Default::default()
                })
                .unwrap();
                EVENTS.with(|e| e.borrow_mut().clear());
                match vm.call_instance(id, method, &[]) {
                    Ok(value) => {
                        assert_eq!(value, V::Int32(42));
                        completed = true;
                    }
                    Err(error) => {
                        assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
                        assert_eq!(vm.instance_status(id).unwrap(), InstanceStatus::Failed);
                        assert!(vm.call_instance(id, method, &[]).is_err());
                        failed = true;
                    }
                }
                vm.close_instance(id).unwrap();
                assert_eq!(vm.instance_bytes(), 0);
                assert_eq!(vm.instance_peak_bytes(), 0);
                crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
                EVENTS.with(|e| {
                    let events = e.borrow();
                    assert_eq!(
                        events.iter().filter(|&&e| e == 7).count(),
                        1,
                        "{mode:?}/{method}/{limit}: {events:?}"
                    );
                    assert!(events.iter().filter(|&&e| e == 8).count() <= 1);
                });
                if completed {
                    break;
                }
            }
            assert!(completed && failed);
        }
    }
}

#[test]
fn borrowed_dyn_v4_instance_call_budgets_release_roots_and_temporary_results() {
    dyn_instance_budget_sweep(32);
}

#[test]
#[ignore = "exhaustive: each byte of dyn instance call allocation budgets"]
fn exhaustive_borrowed_dyn_v4_instance_call_budgets() {
    dyn_instance_budget_sweep(1);
}

#[test]
#[ignore = "exhaustive: each byte of borrowed dyn owner and result allocation budgets"]
fn exhaustive_borrowed_dyn_v4_memory_boundaries() {
    dyn_memory_sweep(1);
}

#[test]
fn borrowed_dyn_v4_frame_limits_fail_and_recover() {
    let mut vm = lifecycle_vm(VmMode::Interpreter);
    let mut failed = false;
    let mut completed = false;
    for max_frames in 1..=16 {
        vm.set_interpreter_limits(super::InterpreterLimits {
            max_frames,
            ..Default::default()
        })
        .unwrap();
        EVENTS.with(|e| e.borrow_mut().clear());
        match vm.call_int32_0("dyn_success") {
            Ok(value) => {
                assert_eq!(value, 42);
                completed = true;
            }
            Err(error) => {
                assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
                failed = true;
            }
        }
        crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
        if completed {
            break;
        }
    }
    assert!(completed && failed);
}

fn dyn_instruction_sweep(exhaustive: bool) {
    let mut vm = lifecycle_vm(VmMode::Interpreter);
    vm.set_interpreter_limits(super::InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    for (entry, final_events) in [
        ("dyn_task", &[10, 8, 7][..]),
        ("dyn_local_task", &[10, 8][..]),
        ("dyn_argument_task", &[8, 7][..]),
        ("dyn_order_task", &[20, 22, 30, 7][..]),
        ("dyn_failure_task", &[10, 8, 7][..]),
    ] {
        let mut completed = false;
        for boundary in (0..1024).step_by(if exhaustive { 1 } else { 11 }) {
            EVENTS.with(|e| e.borrow_mut().clear());
            let mut task = vm.call_async(entry, &[]).unwrap();
            for _ in 0..boundary {
                if task.cps_result().is_some() {
                    completed = true;
                    break;
                }
                if let Err(crate::runtime::suspension::ResumeError::Failed(error)) = task.resume() {
                    assert!(entry.contains("failure"));
                    assert!(error.message().contains("zero"));
                    completed = true;
                    break;
                }
            }
            completed |= task.cps_result().is_some();
            if completed && !entry.contains("failure") {
                assert_eq!(task.cps_result(), Some(42));
            }
            drop(task);
            EVENTS.with(|e| {
                let events = e.borrow();
                for &id in final_events {
                    assert!(
                        events.iter().filter(|&&e| e == id).count() <= 1,
                        "{entry}/{boundary}: {events:?}"
                    );
                }
                if completed {
                    assert_eq!(*events, final_events, "{entry}/{boundary}");
                }
            });
            crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0, "{entry}/{boundary}"));
            if completed {
                break;
            }
        }
        assert!(completed, "{entry}");
    }
}

#[test]
fn borrowed_dyn_v4_instruction_cancellation_matrix() {
    dyn_instruction_sweep(false);
}

#[test]
#[ignore = "exhaustive: each instruction of reborrows, owning arguments, effects and failures"]
fn exhaustive_borrowed_dyn_v4_instruction_cancellation() {
    dyn_instruction_sweep(true);
}

#[test]
fn borrowed_dyn_v4_old_table_survives_argument_evaluation_and_failed_reload() {
    let mut vm = lifecycle_vm(VmMode::Interpreter);
    vm.set_interpreter_limits(super::InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    EVENTS.with(|e| e.borrow_mut().clear());
    let mut task = vm.call_async("dyn_order_task", &[]).unwrap();
    for _ in 0..1024 {
        if EVENTS.with(|e| e.borrow().contains(&20)) {
            break;
        }
        task.resume().unwrap();
    }
    EVENTS.with(|e| assert_eq!(*e.borrow(), [20]));
    assert!(vm
        .load_source(&LIFECYCLE.replace("left + right", "left + true"))
        .is_err());
    vm.load_source(&LIFECYCLE.replace("left + right", "left + right + 57"))
        .unwrap();
    drop(vm);
    for _ in 0..1024 {
        if task.cps_result().is_some() {
            break;
        }
        task.resume().unwrap();
    }
    assert_eq!(task.cps_result(), Some(42));
    drop(task);
    EVENTS.with(|e| assert_eq!(*e.borrow(), [20, 22, 30, 7]));
    crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
}

#[test]
fn borrowed_dyn_v3_erasure_does_not_allocate() {
    let source = "struct Empty {} trait Read { fn value(&self) @noescape -> Int32 { 42 } } impl Read for Empty {} fn answer() -> Int32 { let e = Empty {} let v: &dyn Read = &e let copy = &*v; copy.value() }";
    struct Reset(usize);
    impl Drop for Reset {
        fn drop(&mut self) {
            crate::runtime::arena::LIMIT.with(|n| n.set(self.0));
        }
    }
    let reset = Reset(crate::runtime::arena::LIMIT.with(|n| n.get()));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        crate::runtime::arena::LIMIT.with(|n| n.set(0));
        assert_eq!(vm.call_int32_0("answer").unwrap(), 42);
        crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
        crate::runtime::arena::LIMIT.with(|n| n.set(reset.0));
    }
    #[cfg(unix)]
    crate::aot::tests::assert_sync_int32(&crate::parser::parse(source).unwrap(), "answer", 42);
}

#[test]
fn borrowed_dyn_qualification_depth_boundary() {
    for depth in [32, 33] {
        let mut target = "Int32".to_owned();
        for _ in 0..depth {
            target = format!("&dyn Read<Item = {target}>");
        }
        let source = format!("trait Read {{ type Item; fn read(&self) @noescape -> Int32 }} fn unused(value: &dyn Read<Item = {target}>) {{}} fn answer() -> Int32 {{ 42 }}");
        if depth == 32 {
            accepted(&source);
        } else {
            rejected(&source, "32 dependency levels");
        }
    }
}

#[test]
fn borrowed_dyn_qualification_shares_exact_obligation_boundary() {
    let mut source = String::from("trait M {} struct P<T, U> {} impl<T: M, U: M> M for P<T, U> {} struct Q<T> {} impl<T: M> M for Q<T> {} trait Read { type Item: M; fn read(&self) @noescape -> Int32 { 42 } } struct Empty {}\n");
    let mut level = Vec::new();
    for i in 0..128 {
        source.push_str(&format!("struct L{i} {{}} impl M for L{i} {{}}\n"));
        level.push(format!("L{i}"));
    }
    while level.len() > 1 {
        level = level
            .chunks(2)
            .map(|pair| format!("P<{}, {}>", pair[0], pair[1]))
            .collect();
    }
    for overflow in [false, true] {
        let root = if overflow {
            format!("Q<{}>", level[0])
        } else {
            level[0].clone()
        };
        let program = format!("{source} impl Read for Empty {{ type Item = {root} }} fn answer() -> Int32 {{ let e = Empty {{}} let v: &dyn Read<Item = {root}> = &e; v.read() }}");
        if overflow {
            rejected(&program, "256 obligations");
        } else {
            for mode in [VmMode::Jit, VmMode::Interpreter] {
                let mut vm = VmEngine::with_mode(mode);
                vm.load_source(&program)
                    .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
                assert_eq!(vm.call_int32_0("answer").unwrap(), 42);
            }
            #[cfg(unix)]
            crate::aot::tests::assert_sync_int32(
                &crate::parser::parse(&program).unwrap(),
                "answer",
                42,
            );
        }
    }
}
