use super::{VmEngine, VmMode};
use crate::{runtime::arena::LIVE, FunctionSignature, Type};

const LIBRARY: &str = include_str!("../../tests/fixtures/map_feasibility_library.jk");
const SOURCE: &str = include_str!("../../tests/fixtures/map_feasibility.jk");
thread_local! {
    static EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) };
}
extern "C" fn record(value: i32) {
    EVENTS.with(|events| events.borrow_mut().push(value));
}
extern "C" fn deny_allocations() {
    crate::runtime::arena::LIMIT.with(|limit| limit.set(0));
}
struct RestoreLimit;
impl Drop for RestoreLimit {
    fn drop(&mut self) {
        crate::runtime::arena::LIMIT.with(|limit| limit.set(crate::runtime::arena::MAX_BYTES));
    }
}
fn modules() -> crate::ModuleSources {
    let mut modules = crate::ModuleSources::new();
    modules.insert("map_feasibility_library", LIBRARY).unwrap();
    modules
}
fn configured(mode: VmMode, source: &str) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    unsafe {
        vm.register_fn(
            "print_int",
            FunctionSignature::new(vec![Type::Int32], Type::Unit),
            record as *const u8,
        )
        .unwrap();
        vm.register_fn(
            "deny_allocations",
            FunctionSignature::new(vec![], Type::Unit),
            deny_allocations as *const u8,
        )
        .unwrap();
    }
    vm.load_source_with_modules(source, &modules())
        .unwrap_or_else(|error| {
            let location = error.span().and_then(|span| {
                error
                    .sources()
                    .get(span.source_id())
                    .map(|file| (file.name(), span.slice(file.text())))
            });
            panic!("{mode:?}: {error}: {location:?}")
        });
    vm
}
#[test]
fn map_feasibility_executes_storage_traits_hashes_and_capacity() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode, SOURCE);
        for name in [
            "probe_copy",
            "probe_hashes",
            "probe_capacity",
            "probe_references",
            "probe_reference_key_admission",
            "probe_owned",
        ] {
            EVENTS.with(|events| events.borrow_mut().clear());
            assert_eq!(vm.call_int32_0(name).unwrap(), 42, "{mode:?}: {name}");
            EVENTS.with(|events| {
                let expected: &[i32] = if name == "probe_owned" {
                    &[-1, -2, -3, 1, 2, 3]
                } else {
                    &[]
                };
                assert_eq!(*events.borrow(), expected, "{mode:?}: {name}");
            });
            LIVE.with(|live| assert_eq!(live.get(), 0));
        }
        EVENTS.with(|events| events.borrow_mut().clear());
        assert_eq!(
            vm.call_int32_0("probe_failure").unwrap_err().message(),
            "Division by zero"
        );
        EVENTS.with(|events| assert_eq!(*events.borrow(), [-1, 1]));
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

fn assert_cleaned() {
    EVENTS.with(|events| {
        let events = events.borrow();
        for id in 1..=4 {
            let drops = events.iter().filter(|&&event| event == id).count();
            assert!(drops <= 1, "duplicate drop: {events:?}");
            if events.contains(&-id) {
                assert_eq!(drops, 1, "lost resource: {events:?}");
            }
        }
    });
    LIVE.with(|live| assert_eq!(live.get(), 0));
}

#[test]
fn map_feasibility_replacement_preserves_cleanup_without_budget_headroom() {
    let _restore = RestoreLimit;
    let source = format!(
        r#"{SOURCE}
        extern fn deny_allocations()
        fn probe_copy_no_allocation() -> Int32 {{
            var values = Array.with_capacity::<Int32>(1)
            values.push(42)
            deny_allocations()
            let old = probe.replace(&mut values, 0, 7)
            if values.capacity() != 1 || values.get(0).unwrap_or(0) != 7 {{ return 0 }}
            old.unwrap_or(0)
        }}
        fn probe_no_allocation() -> Int32 {{
            var values = Array.with_capacity::<Resource>(3)
            values.push(resource(1)) values.push(resource(2)) values.push(resource(3))
            let next = resource(4)
            deny_allocations()
            let old = probe.replace(&mut values, 1, next)
            if values.capacity() != 3 || values.len() != 3 {{ return 0 }}
            if values.borrow_ref(0).id != 1 || values.borrow_ref(1).id != 4 || values.borrow_ref(2).id != 3 {{ return 0 }}
            match old {{ | Some(value) => value.id + 40 | None => 0 }}
        }}
    "#
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        EVENTS.with(|events| events.borrow_mut().clear());
        let vm = configured(mode, &source);
        let copy_result = vm.call_int32_0("probe_copy_no_allocation");
        crate::runtime::arena::LIMIT.with(|limit| limit.set(crate::runtime::arena::MAX_BYTES));
        assert_eq!(copy_result.unwrap(), 42, "{mode:?}");
        LIVE.with(|live| assert_eq!(live.get(), 0));
        let result = vm.call_int32_0("probe_no_allocation");
        crate::runtime::arena::LIMIT.with(|limit| limit.set(crate::runtime::arena::MAX_BYTES));
        // Owner snapshots may still allocate (currently in the interpreter).
        // The source algorithm needs no new element buffer, not zero runtime memory.
        match result {
            Ok(value) => assert_eq!(value, 42, "{mode:?}"),
            Err(error) => assert_eq!(
                error.code(),
                crate::ErrorCode::ResourceLimit,
                "{mode:?}: {error}"
            ),
        }
        assert_cleaned();
    }
}

#[test]
fn map_feasibility_partial_construction_and_index_installation_clean_up_on_oom() {
    let _restore = RestoreLimit;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode, SOURCE);
        let mut completed = false;
        for budget in (0..8192).step_by(16) {
            EVENTS.with(|events| events.borrow_mut().clear());
            crate::runtime::arena::LIMIT.with(|limit| limit.set(budget));
            let result = vm.call_int32_0("probe_owned");
            crate::runtime::arena::LIMIT.with(|limit| limit.set(crate::runtime::arena::MAX_BYTES));
            match result {
                Ok(value) => {
                    assert_eq!(value, 42);
                    completed = true;
                }
                Err(error) => assert_eq!(
                    error.code(),
                    crate::ErrorCode::ResourceLimit,
                    "{mode:?}: {error}"
                ),
            }
            assert_cleaned();
            if completed {
                break;
            }
        }
        assert!(completed, "{mode:?}");
    }
}

#[test]
fn map_feasibility_bucket_layout_uses_the_existing_tagged_enum() {
    use crate::memory_layout::{MemoryLayoutShape, MemoryLayoutTarget};
    let registry = crate::TypeRegistry::new();
    let module = registry
        .resolve_aliases(&crate::parser::parse(LIBRARY).unwrap())
        .unwrap();
    let types = crate::core::types::nominal_types(&registry, &module).unwrap();
    let bucket = &types["Bucket"];
    for align in [4, 8] {
        let layout = bucket
            .memory_layout_for(MemoryLayoutTarget {
                int64_align: align,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            (layout.size, layout.align),
            (if align == 8 { 16 } else { 12 }, align)
        );
        let MemoryLayoutShape::Enum { tag, variants } = layout.shape else {
            panic!("expected tagged enum");
        };
        assert_eq!((tag.offset, tag.size), (0, 4));
        assert!(variants[0].payload.is_none());
        assert!(variants[1].payload.is_none());
        let full = variants[2].payload.as_ref().unwrap();
        assert_eq!((full.offset, full.size), (align, 8));
    }
}
#[test]
fn map_feasibility_cancellation_keeps_each_array_and_temporary_owned() {
    let mut vm = configured(VmMode::Interpreter, SOURCE);
    vm.set_interpreter_limits(super::InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    for pause in 0..3000 {
        EVENTS.with(|events| events.borrow_mut().clear());
        let mut task = vm.call_async("probe_task", &[]).unwrap();
        for _ in 0..pause {
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        let complete = task.is_finished();
        if complete {
            assert_eq!(task.cps_result(), Some(42));
        }
        drop(task);
        assert_cleaned();
        if complete {
            return;
        }
    }
    panic!("probe did not finish within the cancellation sweep");
}

#[test]
fn map_feasibility_rejects_owner_copy_loan_escape_and_conflicting_mutation() {
    let prefix = "import map_feasibility_library as probe.{type Storage, type Hash} import joky/array.{type Array}";
    for (body, message) in [
        ("fn bad(t: Storage<Int32, Int32>) { let moved = t probe.len(&t); }", "moved"),
        ("fn bad(t: &Storage<Int32, Array<Int32>>) { probe.get(t, 0); }", "Copy"),
        ("fn bad(t: &mut Storage<Int32, Array<Int32>>) { let view = probe.view(t, 0) probe.append(t, 1, Array.new::<Int32>()) view.len(); }", "borrow"),
        ("fn bad(t: &mut Storage<Int32, Array<Int32>>) { let view = probe.view_mut(t, 0) probe.len(t); view.clear(); }", "borrow"),
        ("fn bad() -> &Array<Int32> { var t = probe.empty::<Int32, Array<Int32>>() probe.append(&mut t, 1, Array.new::<Int32>()) probe.view(&t, 0) }", "borrow"),
        ("fn bad(t: &Storage<Int32, Array<Int32>>) -> Option<&Array<Int32>> { Some(probe.view(t, 0)) }", "Copy"),
        ("fn bad() { var t = probe.empty::<Int32, &Int32>() let local = 1 probe.append(&mut t, 1, &local) }", "local borrow"),
        ("fn bad() { var t = probe.empty::<Int32, &Int32>() let p = alloc(1) probe.append(&mut t, 1, share(p)) let result = probe.get(&t, 0) probe.release(t) memory.write(p, 2); }", "borrow"),
        ("struct OwnedKey { data: Array<Int32> } impl Hash for OwnedKey { fn hash(self) -> UInt64 { self.data.len() } } fn bad() { var t = probe.empty::<OwnedKey, Int32>() probe.append(&mut t, OwnedKey { data: Array.new::<Int32>() }, 1) }", "moved"),
    ] {
        let source = format!("{prefix}\n{body}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let error = VmEngine::with_mode(mode).load_source_with_modules(&source, &modules()).unwrap_err();
            assert!(error.message().contains(message), "{mode:?}: {source}: {error}");
            assert!(error.span().is_some());
        }
        let result = crate::TypeRegistry::default().resolve_with_modules(&crate::parser::parse(&source).unwrap(), &modules());
        let error = match result {
            Ok(ast) => crate::aot::AotCompiler::new("invalid-map-probe").unwrap().compile_module(&ast).unwrap_err(),
            Err(error) => error,
        };
        assert!(error.message().contains(message), "{source}: {error}");
    }
}
