//! String aggregates, containers and bulk text source algorithms.
use super::{VmEngine, VmMode};
use crate::runtime::arena::LIVE;
use crate::{FunctionSignature, Type};
thread_local! { static EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) }; }
extern "C" fn record(value: i32) {
    EVENTS.with(|events| events.borrow_mut().push(value));
}

fn configured(source: &str, mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    // SAFETY: Static non-unwinding callback with the exact Int32 -> Unit ABI.
    unsafe {
        vm.register_fn(
            "record_drop",
            FunctionSignature::new(vec![Type::Int32], Type::Unit),
            record as *const u8,
        )
        .unwrap();
    }
    vm.load_source(source).unwrap_or_else(|error| {
        let location = error.span().and_then(|span| {
            error
                .sources()
                .get(span.source_id())
                .map(|file| (file.name(), span.slice(file.text())))
        });
        panic!("{mode:?}: {error} {location:?}");
    });
    vm
}

fn assert_events(expected: &[i32], context: &str) {
    EVENTS.with(|events| {
        let events = events.borrow();
        let mut created: Vec<_> = events.iter().filter(|&&id| id < 0).map(|id| -id).collect();
        let mut dropped: Vec<_> = events.iter().copied().filter(|&id| id > 0).collect();
        created.sort_unstable();
        dropped.sort_unstable();
        assert_eq!(created, expected, "{context}: {events:?}");
        assert_eq!(dropped, expected, "{context}: {events:?}");
    });
    LIVE.with(|live| assert_eq!(live.get(), 0));
}

#[test]
fn string_s5_aggregates_containers_and_builders_preserve_ownership() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut groups = crate::string_collection_model::ownership_groups();
        groups.push(crate::string_collection_model::bulk_group());
        for group in groups {
            let vm = configured(&group.source, mode);
            for entry in group.entries {
                EVENTS.with(|events| events.borrow_mut().clear());
                let context = format!("{mode:?}: {}", entry.name);
                if let Some(expected) = entry.text {
                    assert_eq!(
                        vm.call_string_0(&entry.name).unwrap(),
                        expected,
                        "{context}"
                    );
                } else if entry.failure {
                    assert_eq!(
                        vm.call_int32_0(&entry.name).unwrap_err().code(),
                        crate::ErrorCode::RuntimeError,
                        "{context}"
                    );
                } else {
                    assert_eq!(vm.call_int32_0(&entry.name).unwrap(), 42, "{context}");
                }
                assert_events(&entry.drops, &context);
            }
        }
    }
}

#[test]
fn string_s5_rejects_copy_get_moves_conflicting_views_and_suspension() {
    for source in [
        "import joky/array.{type Array} fn f() { let a = Array.new::<String>() a.get(0); }",
        "import joky/array.{type Array} fn f() { let a = Array.new::<StringBuilder>() a.get(0); }",
        "import joky/map.{type Map} fn f() { let a = Map.new::<Int32, String>() a.get(0); }",
        "import joky/array.{type Array} fn f() { let a = Array.new::<String>() let b = a a.release() b.release() }",
        "import joky/array.{type Array} fn f() { var a = Array.new::<String>() a.push(\"中\") let b = a.borrow_ref(0) a.clear() b.byte_length(); }",
        "import joky/array.{type Array} import joky/slice fn f() { var a = Array.new::<String>() a.push(\"中\") let b = a.borrow_ref(0).bytes() a.clear() slice.len(b); }",
        "import joky/array.{type Array} fn f() -> &String { var a = Array.new::<String>() a.push(\"中\") a.borrow_ref(0) }",
        "struct Mixed { text: String reference: &Int32 } fn f() {}",
        "fn pass<T>(x: T) -> T { x } fn f() { let x = Some(\"中\") pass(x); x.is_some(); }",
        "import joky/array.{type Array} fn f(a: Array<String>) @suspends {}",
        "struct Pair { left: String right: String } fn f(a: Pair) @suspends {}",
        "import joky/string.{type StringBuilder} fn f(a: Option<StringBuilder>) @suspends {}",
        "import joky/map.{type Map} fn f() { Map.new::<String, Int32>(); }",
        "unsafe extern \"C\" fn f(a: String)",
    ] {
        // StringBuilder is explicitly imported only by cases that need it.
        let source = if source.contains("Array.new::<StringBuilder>") { format!("import joky/string.{{type StringBuilder}} {source}") } else { source.to_owned() };
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            assert!(VmEngine::with_mode(mode).load_source(&source).is_err(), "{mode:?}: {source}");
        }
        assert!(crate::aot::AotCompiler::new("string-owner-negative").unwrap().compile_module(&crate::parser::parse(&source).unwrap()).is_err(), "AOT: {source}");
    }
}

struct RestoreLimit(usize);
impl Drop for RestoreLimit {
    fn drop(&mut self) {
        crate::runtime::arena::LIMIT.with(|limit| limit.set(self.0));
    }
}
fn assert_partial_events(context: &str) {
    EVENTS.with(|events| {
        let events = events.borrow();
        assert!(
            !events.contains(&999),
            "{context}: invalid Drop field access: {events:?}"
        );
        for id in 1..100 {
            let created = events.iter().filter(|&&value| value == -id).count();
            let dropped = events.iter().filter(|&&value| value == id).count();
            assert!(
                created <= 1 && dropped <= 1,
                "{context}: duplicate owner: {events:?}"
            );
            if created != 0 {
                assert_eq!(dropped, 1, "{context}: lost owner: {events:?}");
            }
        }
    });
    LIVE.with(|live| assert_eq!(live.get(), 0, "{context}"));
}

#[test]
fn string_s5_budget_failures_release_nested_and_partial_bulk_outputs() {
    use crate::runtime::arena;
    let _restore = RestoreLimit(arena::LIMIT.with(|limit| limit.get()));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut groups = crate::string_collection_model::ownership_groups();
        groups.push(crate::string_collection_model::Group {
            source: format!(
                "{} fn work() -> Int32 {{ bulk_work::<Unit>() }}",
                crate::string_collection_model::SOURCE
            ),
            entries: vec![crate::string_collection_model::Entry {
                name: "work".into(),
                text: None,
                drops: vec![],
                failure: false,
            }],
        });
        for group in groups
            .into_iter()
            .filter(|group| !group.entries.iter().any(|entry| entry.failure))
        {
            let vm = configured(&group.source, mode);
            let mut successes = 0;
            let mut failures = 0;
            for budget in (0..8192).step_by(64) {
                arena::LIMIT.with(|limit| limit.set(budget));
                for entry in &group.entries {
                    EVENTS.with(|events| events.borrow_mut().clear());
                    let result = if entry.text.is_some() {
                        vm.call_string_0(&entry.name).map(|_| ())
                    } else {
                        vm.call_int32_0(&entry.name)
                            .map(|value| assert_eq!(value, 42))
                    };
                    match result {
                        Ok(()) => successes += 1,
                        Err(error) => {
                            assert_eq!(
                                error.code(),
                                crate::ErrorCode::ResourceLimit,
                                "{mode:?}, {}, {budget}",
                                entry.name
                            );
                            failures += 1;
                        }
                    }
                    assert_partial_events(&format!("{mode:?}, {}, {budget}", entry.name));
                }
            }
            assert!(successes > 0 && failures > 0);
            arena::LIMIT.with(|limit| limit.set(arena::MAX_BYTES));
        }
    }
}

#[test]
fn string_s5_interpreter_cancellation_reclaims_containers_and_bulk_parts() {
    use super::support::{assert_cancellation, CancellationSweep};
    for entry in [
        "array_records",
        "enum_arrays",
        "nested",
        "map_values",
        "builders",
        "bulk_work",
    ] {
        let source = format!(
            "{} fn run() @suspends -> Int32 {{ {entry}::<Unit>() }}",
            crate::string_collection_model::SOURCE
        );
        let mut vm = configured(&source, VmMode::Interpreter);
        assert_cancellation(
            &mut vm,
            "run",
            50_000,
            CancellationSweep::ResourceBoundaries,
            || EVENTS.with(|events| events.borrow_mut().clear()),
            || LIVE.with(|live| live.get()),
            || assert_partial_events(entry),
        );
    }
}

#[test]
fn string_s5_join_length_checks_do_not_wrap_or_require_huge_allocations() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let registry = std::rc::Rc::new(crate::modules::ModuleRegistry::default());
        assert!(registry
            .types()
            .standard
            .set(crate::core::imports::string_test_library())
            .is_ok());
        let mut vm = VmEngine::with_mode_and_registry(mode, registry);
        vm.load_source("import joky/string fn probe() -> Int32 { string.probe_join_length() }")
            .unwrap();
        assert_eq!(vm.call_int32_0("probe").unwrap(), 42);
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}
