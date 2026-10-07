use super::{VmEngine, VmMode};
use crate::{
    runtime::arena::LIVE, ErrorCode, FunctionSignature, ModuleSources, Type, TypeRegistry,
};

const SOURCE: &str = include_str!("../../tests/fixtures/map_borrowing.jk");
thread_local! { static EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) }; }
extern "C" fn record(n: i32) {
    EVENTS.with(|events| events.borrow_mut().push(n));
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
        .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
    vm
}
#[test]
fn map_borrowing_executes_views_replacement_and_shared_contents() {
    // Share specializations within each group without exceeding the 256-instance limit.
    let groups: &[&[&str]] = &[
        &["copy_views"],
        &["owned_views"],
        &["nested_views"],
        &["reference_values", "copied_reference", "returned_reference"],
        &["shared_aggregate"],
        &["aggregate_references"],
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
                        if name == "owned_views" {
                            vec![-1, -2, 2, 1]
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
#[test]
fn map_borrowing_missing_keys_report_calls_and_clean_stored_owners() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for (name, expression) in [
            ("missing_copy", "m.borrow(2)"),
            ("missing_mut", "m.borrow_mut(2)"),
            ("missing_owner", "m.borrow_ref(2)"),
            ("missing_owner_mut", "m.borrow_mut_ref(2)"),
        ] {
            let vm = configured(mode, &source(name));
            EVENTS.with(|events| events.borrow_mut().clear());
            let error = vm.call_int32_0("probe").unwrap_err();
            assert_eq!(error.code(), ErrorCode::RuntimeError);
            assert_eq!(error.message(), "Map key not found");
            let has_call = error
                .span()
                .into_iter()
                .chain(error.runtime_trace().iter().map(|f| f.span))
                .chain(error.native_call_sites().iter().copied())
                .any(|s| {
                    error
                        .sources()
                        .get(s.source_id())
                        .and_then(|f| s.slice(f.text()))
                        == Some(expression)
                });
            assert!(has_call, "{mode:?} {name}: missing Map call location");
            EVENTS.with(|events| {
                assert_eq!(
                    *events.borrow(),
                    if name.contains("owner") {
                        vec![-1, 1]
                    } else {
                        vec![]
                    }
                )
            });
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}
fn rejected(body: &str) {
    let source = format!("import joky/map.{{type Map}} import joky/array.{{type Array}} import joky/element as element.{{type Element}} {body}");
    let ast = match TypeRegistry::default().resolve_with_modules(
        &crate::parser::parse(&source).unwrap(),
        &ModuleSources::new(),
    ) {
        Ok(ast) => ast,
        Err(error) => {
            assert!(error.span().is_some(), "{body}: {error}");
            return;
        }
    };
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let error = VmEngine::with_mode(mode).load_module(&ast).expect_err(body);
        assert!(error.span().is_some(), "{mode:?} {body}: {error}");
    }
    crate::aot::AotCompiler::new("invalid-map-view")
        .unwrap()
        .compile_module(&ast)
        .expect_err(body);
}
#[test]
fn map_borrowing_rejects_conflicts_escape_and_invalid_view_representations() {
    for body in [
        "fn bad(m: &Map<Int32,Int32>) { m.borrow_mut(1); }",
        "fn bad(m: &mut Map<Int32,Int32>) { let a=m.borrow_mut(1) let b=m.borrow_mut(2) element.write(a,1) element.write(b,2) }",
        "fn bad(m: &mut Map<Int32,Int32>) { let a=m.borrow_mut(1) m.get(2) element.write(a,1) }",
        "fn bad(m: &mut Map<Int32,Int32>) { let a=m.borrow(1) m.insert(2,3) element.read(a); }",
        "fn bad(m: &mut Map<Int32,Int32>) { let a=m.borrow(1) m.remove(2) element.read(a); }",
        "fn bad(m: &mut Map<Int32,Int32>) { let a=m.borrow(1) m.reserve(0) element.read(a); }",
        "fn bad(m: &mut Map<Int32,Int32>) { let a=m.borrow(1) m.clear() element.read(a); }",
        "fn bad(m: Map<Int32,Int32>) { let a=m.borrow(1) m.release() element.read(a); }",
        "fn bad(m: Map<Int32,Int32>) { let a=m.borrow(1) let moved=m element.read(a); }",
        "fn bad(m: &mut Map<Int32,Array<Int32>>) { let a=m.borrow_ref(1) m.clear() a.len(); }",
        "fn bad(m: &Map<Int32,Array<Int32>>) { m.borrow(1); }",
        "fn bad() -> &Element<Int32> { var m=Map.new::<Int32,Int32>() m.insert(1,2) m.borrow(1) }",
        "fn bad() -> &Array<Int32> { var m=Map.new::<Int32,Array<Int32>>() m.insert(1,Array.new::<Int32>()) m.borrow_ref(1) }",
        "fn bad(m: &Map<Int32,Int32>) { Some(m.borrow(1)); }",
        "fn bad(m: &Map<Int32,Int32>) { (m.borrow(1), 0); }",
        "fn bad(m: &Map<Int32,Array<Int32>>) { Some(m.borrow_ref(1)); }",
        "fn pause() @suspends {} fn bad() @suspends -> Int32 { var m=Map.new::<Int32,Int32>() m.insert(1,2) let v=m.borrow(1) pause() element.read(v) }",
        "struct View { value: &Element<Int32> } fn bad(m: &Map<Int32,Int32>) { View { value: m.borrow(1) }; }",
        "extern fn host(v: &Element<Int32>) fn bad(m: &Map<Int32,Int32>) { host(m.borrow(1)) }",
        "fn bad(m: &Map<Int32,Int32>) @suspends -> Int32 { element.read(m.borrow(1)) }",
        "fn bad(m: Map<Int32,Int32>) { m.release() m.borrow(1); }",
    ] { rejected(body); }
}
#[test]
fn map_borrowing_rejects_stack_stores_and_retains_actual_reference_sources() {
    for body in [
        "fn bad(m: &mut Map<Int32,&Int32>) { let n=42 m.insert(1,&n); }",
        "fn bad(m: &mut Map<Int32,&Int32>) { let n=42 element.write(m.borrow_mut(1),&n); }",
        "struct Shared { value: &Int32 } fn bad(m: &mut Map<Int32,Shared>) { let n=42 m.insert(1,Shared { value: &n }); }",
        "fn bad() { var m=Map.new::<Int32,&Int32>() let p=alloc(42) m.insert(1,share(p)) m.release() memory.write(p,0) }",
        "fn bad() { var m=Map.new::<Int32,&Int32>() let p=alloc(42) m.insert(1,share(alloc(1))) element.write(m.borrow_mut(1),share(p)) m.release() memory.write(p,0) }",
        "struct Shared { value: &Int32 } fn bad() { var m=Map.new::<Int32,Shared>() let p=alloc(42) m.insert(1,Shared { value: share(p) }) let value=m.remove(1) m.release() memory.write(p,0) }",
        "struct Owned { data: Array<Int32>, value: &Int32 } fn bad() { Map.new::<Int32,Owned>(); }",
        "fn bad() { Map.new::<Int32,&mut Int32>(); }",
    ] { rejected(body); }
}

#[test]
fn map_borrowing_cross_module_generic_aliases_preserve_view_and_content_origins() {
    let mut modules = ModuleSources::new();
    modules
        .insert(
            "map_views_library",
            include_str!("../../tests/fixtures/map_views_library.jk"),
        )
        .unwrap();
    let prefix = include_str!("../../tests/fixtures/map_views_modules.jk")
        .split("fn main()")
        .next()
        .unwrap();
    // CLI tests cover standalone AOT for these same source-module entries.
    for entry in ["copies", "owners", "references", "iteration"] {
        let source = format!("{prefix} fn test() -> Int32 {{ {entry}::<Unit>() }}")
            .replace("extern fn record_int(value: Int32)", "");
        let ast = TypeRegistry::default()
            .resolve_with_modules(&crate::parser::parse(&source).unwrap(), &modules)
            .unwrap();
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_module(&ast).unwrap();
            assert_eq!(vm.call_int32_0("test").unwrap(), 42, "{mode:?} {entry}");
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
    for body in [
        "fn bad(m: &mut views.Dictionary<Int32,Int32>) { let v=views.keys(m) m.pop() slices.len(v); }",
        "fn bad(m: &mut views.Dictionary<Int32,&Int32>) { let n=42 slices.set(views.values_mut(m),0,&n); }",
        "fn bad() { let p=alloc(42) var input=Array.new::<(Int32,&Int32)>() input.push((1,share(p))) let m=views.from_entries(input) let output=views.export(m) output.release() memory.write(p,0) }",
        "fn bad(m: &mut views.Dictionary<Int32,Int32>) { let v=views.view(m,1) m.insert(2,3) element.read(v); }",
        "fn bad() { var m=Map.new::<Int32,&Int32>() let p=alloc(42) views.put(&mut m,1,share(p)) let r=views.copied(&m,1) m.release() memory.write(p,0) }",
        "fn bad() { var m=Map.new::<Int32,&Int32>() let p=alloc(42) m.insert(1,share(alloc(0))) element.write(views.edit(&mut m,1),share(p)) m.release() memory.write(p,0) }",
        "fn bad(m: &mut views.Dictionary<Int32,&Int32>) { let n=42 views.put(m,1,&n); }",
    ] {
        let source = format!("import map_views_library as views import joky/map.{{type Map}} import joky/element as element import joky/slice as slices import joky/array.{{type Array}} {body}");
        let ast = TypeRegistry::default().resolve_with_modules(&crate::parser::parse(&source).unwrap(), &modules).unwrap();
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            VmEngine::with_mode(mode).load_module(&ast).expect_err(body);
        }
        crate::aot::AotCompiler::new("invalid-module-map-view").unwrap().compile_module(&ast).expect_err(body);
    }
}

fn assert_cleaned() {
    EVENTS.with(|events| {
        let events = events.borrow();
        for id in [1, 2] {
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
fn map_borrowing_cancellation_and_exhaustion_clean_replaced_owners() {
    let mut vm = configured(VmMode::Interpreter, &source("owned_views"));
    super::support::assert_cancellation(
        &mut vm,
        "task",
        6000,
        super::support::CancellationSweep::ResourceBoundaries,
        || EVENTS.with(|events| events.borrow_mut().clear()),
        || EVENTS.with(|events| events.borrow().len()),
        assert_cleaned,
    );
    let mut complete = false;
    for budget in (1..6000).step_by(16) {
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
fn map_borrowing_allocation_failures_clean_each_replacement_owner() {
    struct RestoreLimit;
    impl Drop for RestoreLimit {
        fn drop(&mut self) {
            crate::runtime::arena::LIMIT.with(|limit| limit.set(crate::runtime::arena::MAX_BYTES));
        }
    }
    let _restore = RestoreLimit;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode, &source("owned_views"));
        let mut complete = false;
        let mut failed_after_borrow = false;
        for budget in (0..16384).step_by(16) {
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
                    failed_after_borrow |= EVENTS.with(|events| events.borrow().contains(&-2));
                }
            }
            assert_cleaned();
            if complete {
                break;
            }
        }
        assert!(complete && failed_after_borrow, "{mode:?}");
    }
}

#[test]
#[ignore = "exhaustive: cancel at every instruction; run by the stress check in Release"]
fn exhaustive_map_borrowing_cancels_at_each_instruction() {
    let mut vm = configured(VmMode::Interpreter, &source("owned_views"));
    super::support::assert_cancellation(
        &mut vm,
        "task",
        6000,
        super::support::CancellationSweep::EveryInstruction,
        || EVENTS.with(|events| events.borrow_mut().clear()),
        || EVENTS.with(|events| events.borrow().len()),
        assert_cleaned,
    );
}
