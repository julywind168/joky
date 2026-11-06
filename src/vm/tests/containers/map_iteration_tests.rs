use super::{VmEngine, VmMode};
use crate::{
    runtime::arena::LIVE, ErrorCode, FunctionSignature, ModuleSources, Type, TypeRegistry,
};

const SOURCE: &str = include_str!("../../../../tests/fixtures/map_iteration.jk");
thread_local! {
    static EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) };
    static EXPORT_LIMIT: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}
extern "C" fn record(n: i32) {
    EVENTS.with(|events| events.borrow_mut().push(n));
    // Arm a fresh limit at export so an earlier import peak cannot hide it.
    if n == 900 {
        if let Some(limit) = EXPORT_LIMIT.with(std::cell::Cell::get) {
            crate::runtime::arena::LIMIT.with(|value| value.set(limit));
        }
    }
}
fn source(entry: &str) -> String {
    format!(
        "{} fn probe() -> Int32 {{ {entry}::<Unit>() }}",
        SOURCE.split("fn main()").next().unwrap()
    )
}
fn configured(mode: VmMode, source: &str) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    unsafe {
        vm.register_fn(
            "record_int",
            FunctionSignature::new(vec![Type::Int32], Type::Unit),
            record as *const u8,
        )
        .unwrap();
    }
    vm.load_source_with_modules(source, &ModuleSources::new())
        .unwrap_or_else(|e| {
            panic!(
                "{mode:?}: {e} {:?}",
                e.span().and_then(|s| e
                    .sources()
                    .get(s.source_id())
                    .map(|f| (f.name(), s.slice(f.text()))))
            )
        });
    vm
}
#[test]
fn map_iteration_executes_views_consumption_and_batches() {
    // Share specializations within each group without exceeding the 256-instance limit.
    let groups: &[&[&str]] = &[
        &["copy_views", "consuming", "batches"],
        &["owned_pipeline"],
        &["references"],
        &["unbounded", "protocol_free_export"],
    ];
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for &names in groups {
            let entries: Vec<_> = names
                .iter()
                .map(|name| (format!("probe_{name}"), *name))
                .collect();
            let source = super::support::generic_fixture_entries(SOURCE, &entries);
            let vm = configured(mode, &source);
            for &name in names {
                EVENTS.with(|events| events.borrow_mut().clear());
                assert_eq!(
                    vm.call_int32_0(&format!("probe_{name}")).unwrap(),
                    42,
                    "{mode:?} {name}"
                );
                EVENTS.with(|events| {
                    assert_eq!(
                        *events.borrow(),
                        if name == "owned_pipeline" {
                            vec![-1, -2, -3, 1, -4, 2, 3, 900, 901, 4]
                        } else if name == "protocol_free_export" {
                            vec![101, 102]
                        } else {
                            vec![]
                        }
                    )
                });
                LIVE.with(|n| assert_eq!(n.get(), 0));
            }
        }
    }
}
fn rejected(body: &str) {
    let source = format!("import joky/map.{{type Map}} import joky/array.{{type Array}} import joky/element.{{type Element}} import joky/slice.{{type Slice}} {body}");
    let error = super::support::frontend_error(&source, &ModuleSources::new());
    assert!(error.span().is_some(), "{body}: {error}");
}
#[test]
fn map_iteration_rejects_conflicting_views_and_reused_consumed_inputs() {
    for body in [
        "fn bad(m: &Map<Int32,Int32>) { slice.set(m.keys(),0,1); }",
        "fn bad(m: &Map<Int32,Int32>) { m.values_mut(); }",
        "fn bad(m: &mut Map<Int32,Int32>) { let v=m.keys(); m.insert(1,2); slice.len(v); }",
        "fn bad(m: &mut Map<Int32,Int32>) { let v=m.values(); m.pop(); slice.len(v); }",
        "fn bad(m: &mut Map<Int32,Int32>) { let v=m.values_mut(); m.keys(); slice.set(v,0,1); }",
        "fn bad(m: &mut Map<Int32,Int32>) { let v=m.values_mut(); m.values_mut(); slice.set(v,0,1); }",
        "fn bad(m: &mut Map<Int32,Int32>) { let v=m.values(); m.extend(Array.new::<(Int32,Int32)>()); slice.len(v); }",
        "fn bad(m: Map<Int32,Int32>) { let v=m.values(); m.into_entries(); slice.len(v); }",
        "fn bad() -> &Slice<Int32> { let m=Map.new::<Int32,Int32>(); m.keys() }",
        "fn bad() -> &Slice<Int32> { let m=Map.new::<Int32,Int32>(); m.values() }",
        "fn bad() { let input=Array.new::<(Int32,Int32)>(); Map.from_entries(input); input.len(); }",
        "fn bad(m: &mut Map<Int32,Int32>) { let input=Array.new::<(Int32,Int32)>(); m.extend(input); input.len(); }",
        "fn bad(m: Map<Int32,Int32>) { m.into_entries(); m.len(); }",
        "fn bad(m: &Map<Int32,Array<Int32>>) { slice.get(m.values(),0); }",
        "fn bad(m: &mut Map<Int32,&Int32>) { let n=42; slice.set(m.values_mut(),0,&n); }",
        "fn bad(m: &mut Map<Int32,&Int32>) { let n=42; var input=Array.new::<(Int32,&Int32)>(); input.push((1,&n)); m.extend(input); }",
        "fn bad() { let p=alloc(42); var input=Array.new::<(Int32,&Int32)>(); input.push((1,(&*p))); let m=Map.from_entries(input); let output=m.into_entries(); output.release() ; (*(p) = 0) }",
    ] { rejected(body); }
}
#[test]
fn map_iteration_drop_failure_stops_before_the_next_input() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode, &source("drop_failure"));
        EVENTS.with(|events| events.borrow_mut().clear());
        let error = vm.call_int32_0("probe").unwrap_err();
        assert_eq!(error.message(), "Batch drop failure");
        EVENTS.with(|events| {
            let events = events.borrow();
            assert_eq!(events.iter().filter(|&&n| n == 101).count(), 2);
            assert!(
                !events.contains(&102) && !events.contains(&103) && !events.contains(&999),
                "{events:?}"
            );
        });
        assert_cleaned();
    }
}
fn assert_cleaned() {
    EVENTS.with(|events| {
        let events = events.borrow();
        for id in [1, 2, 3, 4] {
            let drops = events.iter().filter(|&&n| n == id).count();
            assert!(drops <= 1, "duplicate drop: {events:?}");
            if events.contains(&-id) {
                assert_eq!(drops, 1, "lost owner: {events:?}");
            }
        }
    });
    LIVE.with(|n| assert_eq!(n.get(), 0));
}
#[test]
fn map_iteration_cancellation_and_exhaustion_clean_unprocessed_entries() {
    let mut vm = configured(
        VmMode::Interpreter,
        &format!(
            "{} fn task() @suspends -> Int32 {{ owned_pipeline::<Unit>() }}",
            source("owned_pipeline")
        ),
    );
    super::support::assert_cancellation(
        &mut vm,
        "task",
        16000,
        super::support::CancellationSweep::ResourceBoundaries,
        || EVENTS.with(|events| events.borrow_mut().clear()),
        || EVENTS.with(|events| events.borrow().len()),
        assert_cleaned,
    );
    let mut complete = false;
    for budget in (1..16000).step_by(16) {
        vm.set_interpreter_limits(super::InterpreterLimits {
            instruction_budget: Some(budget),
            ..Default::default()
        })
        .unwrap();
        EVENTS.with(|events| events.borrow_mut().clear());
        match vm.call_int32_0("probe") {
            Ok(n) => {
                assert_eq!(n, 42);
                complete = true;
            }
            Err(e) => assert_eq!(e.code(), ErrorCode::ResourceLimit),
        }
        assert_cleaned();
        if complete {
            break;
        }
    }
    assert!(complete);
}

#[test]
fn map_iteration_allocation_failures_clean_imported_and_exported_owners() {
    struct RestoreLimit;
    impl Drop for RestoreLimit {
        fn drop(&mut self) {
            crate::runtime::arena::LIMIT.with(|limit| limit.set(crate::runtime::arena::MAX_BYTES));
            EXPORT_LIMIT.with(|limit| limit.set(None));
        }
    }
    let _restore = RestoreLimit;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode, &source("owned_pipeline"));
        let mut complete = false;
        let mut failed_after_import = false;
        for budget in (0..65536).step_by(16) {
            EVENTS.with(|events| events.borrow_mut().clear());
            crate::runtime::arena::LIMIT.with(|limit| limit.set(budget));
            let result = vm.call_int32_0("probe");
            crate::runtime::arena::LIMIT.with(|limit| limit.set(crate::runtime::arena::MAX_BYTES));
            match result {
                Ok(n) => {
                    assert_eq!(n, 42);
                    complete = true;
                }
                Err(error) => {
                    assert_eq!(error.code(), ErrorCode::ResourceLimit, "{mode:?}: {error}");
                    failed_after_import |= EVENTS.with(|events| events.borrow().contains(&1));
                }
            }
            assert_cleaned();
            if complete {
                break;
            }
        }
        assert!(complete && failed_after_import, "{mode:?}");
        let mut complete = false;
        let mut failed_during_export = false;
        for budget in (0..65536).step_by(16) {
            EVENTS.with(|events| events.borrow_mut().clear());
            EXPORT_LIMIT.with(|limit| limit.set(Some(budget)));
            let result = vm.call_int32_0("probe");
            EXPORT_LIMIT.with(|limit| limit.set(None));
            crate::runtime::arena::LIMIT.with(|limit| limit.set(crate::runtime::arena::MAX_BYTES));
            match result {
                Ok(n) => {
                    assert_eq!(n, 42);
                    complete = true;
                }
                Err(error) => {
                    assert_eq!(error.code(), ErrorCode::ResourceLimit, "{mode:?}: {error}");
                    failed_during_export |= EVENTS.with(|events| {
                        let events = events.borrow();
                        events.contains(&900) && !events.contains(&901)
                    });
                }
            }
            assert_cleaned();
            if complete {
                break;
            }
        }
        assert!(complete && failed_during_export, "{mode:?}");
    }
}

#[test]
fn map_iteration_pop_preserves_dense_indices_and_tombstones() {
    let source = r#"
        import joky/map.{type Map}
        import joky/check.{assert}
        import joky/eq.{type Eq}
        import joky/hash.{type Hash}
        struct Key { id: Int32 }
        impl Eq for Key { fn eq(&self, other: &Key) -> Bool { self.id == other.id } }
        impl Hash for Key { fn hash(&self) -> UInt64 { 0 } }
        fn test() -> Int32 {
            var m = Map.new::<Key, Int32>()
            var round = 0
            while round < 12 {
                var i = 0
                while i < 9 { m.insert(Key { id: i }, i * 10); i = i + 1 }
                m.remove(Key { id: 4 })
                { let assertion_condition = map.test_invariant(&m); let assertion_message = "Before pop"; assert(assertion_condition, &assertion_message) }
                while !m.is_empty() {
                    match m.pop() {
                        Some((key, value)) => {
                            { let assertion_condition = value == key.id * 10 && !m.contains_key(key); let assertion_message = "Popped contents"; assert(assertion_condition, &assertion_message) }
                            { let assertion_condition = map.test_invariant(&m); let assertion_message = "After pop"; assert(assertion_condition, &assertion_message) }
                        }
                        None => { { let assertion_condition = false; let assertion_message = "Unexpected empty map"; assert(assertion_condition, &assertion_message) } }
                    }
                }
                { let assertion_condition = m.pop().is_none() && map.test_invariant(&m); let assertion_message = "After draining"; assert(assertion_condition, &assertion_message) }
                m.reserve(20)
                { let assertion_condition = map.test_invariant(&m); let assertion_message = "After rehash"; assert(assertion_condition, &assertion_message) }
                m.insert(Key { id: 10 }, 42); m.clear()
                { let assertion_condition = map.test_invariant(&m); let assertion_message = "After reuse"; assert(assertion_condition, &assertion_message) }
                round = round + 1
            }
            42
        }
    "#;
    let registry = TypeRegistry::default();
    registry
        .standard
        .set(crate::core::imports::map_test_library())
        .unwrap();
    let ast = registry
        .resolve_with_modules(
            &crate::parser::parse(source).unwrap(),
            &ModuleSources::new(),
        )
        .unwrap();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_module(&ast).unwrap();
        assert_eq!(vm.call_int32_0("test").unwrap(), 42);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
    #[cfg(unix)]
    crate::backends::aot::tests::assert_sync_int32(&ast, "test", 42);
}

#[test]
#[ignore = "exhaustive: cancel at every instruction; run by the stress check in Release"]
fn exhaustive_map_iteration_cancels_at_each_instruction() {
    let mut vm = configured(
        VmMode::Interpreter,
        &format!(
            "{} fn task() @suspends -> Int32 {{ owned_pipeline::<Unit>() }}",
            source("owned_pipeline")
        ),
    );
    super::support::assert_cancellation(
        &mut vm,
        "task",
        16000,
        super::support::CancellationSweep::EveryInstruction,
        || EVENTS.with(|events| events.borrow_mut().clear()),
        || EVENTS.with(|events| events.borrow().len()),
        assert_cleaned,
    );
}
