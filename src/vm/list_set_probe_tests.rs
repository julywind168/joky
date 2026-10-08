//! Retained LS0 Set feasibility; List uses production regression tests.
use super::{VmEngine, VmMode};
use crate::{FunctionSignature, ModuleSources, Type};
use std::cell::RefCell;

const LIBRARY: &str = include_str!("../../tests/fixtures/list_set_probe.jk");
pub(crate) const CASES: &[(&str, &[i32])] = &[
    ("set_strings", &[]),
    ("set_duplicates", &[-1, -2, 2, -3, 100, 1, 200, 3]),
    ("set_transfer", &[-1, -2, 2, 100, 1]),
];
pub(crate) fn modules() -> ModuleSources {
    let mut modules = ModuleSources::new();
    modules.insert("probe", LIBRARY).unwrap();
    modules
}
pub(crate) fn source() -> String {
    let mut source = String::from("import probe\n");
    for (name, _) in CASES {
        source.push_str(&format!("fn {name}() -> Int32 {{ probe.{name}() }}\n"));
    }
    source
}
thread_local! {
    static EVENTS: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
}
extern "C" fn record(value: i32) {
    EVENTS.with(|events| events.borrow_mut().push(value));
}
fn registered(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    // SAFETY: The synchronous callback has the declared Int32 -> Unit ABI.
    unsafe {
        vm.register_fn(
            "record_int",
            FunctionSignature::new(vec![Type::Int32], Type::Unit),
            record as *const u8,
        )
        .unwrap();
    }
    vm
}
#[test]
fn list_set_probe_storage_borrowing_queries_and_cleanup() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = registered(mode);
        vm.set_prelude_enabled(false);
        vm.load_source_with_modules(&source(), &modules())
            .unwrap_or_else(|e| {
                panic!(
                    "{mode:?} Set: {e} at {:?}",
                    e.span().and_then(|span| e
                        .sources()
                        .get(span.source_id())
                        .map(|file| (file.name(), span.slice(file.text()))))
                )
            });
        for (name, expected) in CASES {
            EVENTS.with(|events| events.borrow_mut().clear());
            assert_eq!(vm.call_int32_0(name).unwrap(), 42, "{mode:?} {name}");
            EVENTS.with(|events| {
                assert_eq!(events.borrow().as_slice(), *expected, "{mode:?} {name}")
            });
            crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0));
        }
    }
}
#[test]
fn list_set_probe_rejects_invalid_ownership_views_and_storage() {
    for (body, message) in [
        ("fn bad() { var set = probe.SetProbe.new::<String>() set.insert(\"a\") let view = set.elements() set.remove(\"a\"); slices.len(view); }", "borrow"),
        ("fn bad() { var set = probe.SetProbe.new::<String>() set.insert(\"a\") slices.borrow_mut_ref(set.elements(), 0); }", "type mismatch"),
        ("fn bad() { let set = probe.SetProbe.new::<String>() set.items.len(); }", "private"),
        ("fn bad() { probe.SetProbe.new::<&Int32>(); }", "reference"),
    ] {
        let source = format!("import probe import joky/string import joky/slice as slices {body}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let error = registered(mode)
                .load_source_with_modules(&source, &modules())
                .unwrap_err();
            assert!(error.message().contains(message), "{mode:?}: {body}: {error}");
        }
        let error = crate::TypeRegistry::default()
            .resolve_with_modules(&crate::parser::parse(&source).unwrap(), &modules())
            .and_then(|ast| crate::aot::AotCompiler::new("list-set-probe-negative")?.compile_module(&ast).map(|_| ()))
            .unwrap_err();
        assert!(error.message().contains(message), "AOT: {body}: {error}");
    }
}
