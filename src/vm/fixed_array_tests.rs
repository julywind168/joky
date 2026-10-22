use super::{VmEngine, VmMode};
use crate::{runtime::arena, ErrorCode, FunctionSignature, Type};
const SOURCE: &str = include_str!("../../tests/fixtures/fixed_array_f2a_cleanup.jk");
thread_local! { static EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) }; }
extern "C" fn note(value: i32) {
    EVENTS.with(|events| events.borrow_mut().push(value));
}
fn configured(mode: VmMode) -> VmEngine {
    configured_source(mode, SOURCE)
}
fn configured_source(mode: VmMode, source: &str) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    unsafe {
        vm.register_fn(
            "note",
            FunctionSignature::new(vec![Type::Int32], Type::Unit),
            note as *const u8,
        )
        .unwrap();
    }
    vm.load_source(source)
        .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
    vm
}
fn reset() {
    EVENTS.with(|events| events.borrow_mut().clear());
}
fn cleaned() {
    arena::LIVE.with(|live| assert_eq!(live.get(), 0));
    EVENTS.with(|events| {
        let events = events.borrow();
        assert!(
            events.iter().filter(|&&n| n == -1).count() <= 1,
            "duplicate cleanup: {events:?}"
        );
        let calls: Vec<_> = events.iter().copied().filter(|&n| n > 0).collect();
        assert!(
            [1, 2, 3, 4].starts_with(&calls),
            "callback replay: {events:?}"
        );
    });
}
struct RestoreLimit(usize);
impl Drop for RestoreLimit {
    fn drop(&mut self) {
        arena::LIMIT.with(|limit| limit.set(self.0));
    }
}

#[test]
fn fixed_array_f2a_failures_and_budget_reuse() {
    let _restore = RestoreLimit(arena::LIMIT.with(|limit| limit.get()));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode);
        for (name, events) in [
            ("construct", vec![1, 2, 3, 4, -1]),
            ("zero_env", vec![-1]),
            ("early", vec![-1]),
        ] {
            reset();
            assert_eq!(vm.call_int32_0(name).unwrap(), 42);
            EVENTS.with(|actual| assert_eq!(*actual.borrow(), events, "{mode:?} {name}"));
            arena::LIVE.with(|live| assert_eq!(live.get(), 0));
        }
        reset();
        assert!(vm.call_int32_0("fail_init").is_err());
        EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 2, 3, -1]));
        cleaned();
        reset();
        assert!(vm.call_int32_0("assignment_fail").is_err());
        EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 2, 3]));
        arena::LIVE.with(|live| assert_eq!(live.get(), 0));
        reset();
        assert!(vm.call_int32_0("fail_cleanup").is_err());
        EVENTS.with(|events| assert_eq!(*events.borrow(), [1, -2]));
        arena::LIVE.with(|live| assert_eq!(live.get(), 0));
        let mut successful = false;
        let mut failed_after_callback = false;
        for budget in (0..8192).step_by(64) {
            arena::LIMIT.with(|limit| limit.set(budget));
            reset();
            match vm.call_int32_0("construct") {
                Ok(value) => {
                    assert_eq!(value, 42);
                    successful = true;
                }
                Err(error) => {
                    assert_eq!(
                        error.code(),
                        ErrorCode::ResourceLimit,
                        "{mode:?} {budget}: {error}"
                    );
                    failed_after_callback |= EVENTS.with(|events| events.borrow().contains(&1));
                }
            }
            cleaned();
        }
        assert!(successful && failed_after_callback);
        arena::LIMIT.with(|limit| limit.set(8192));
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!("../../tests/fixtures/fixed_array_f2a.jk"))
            .unwrap();
        assert_eq!(vm.call_int32_0("reuse").unwrap(), 42);
        arena::LIVE.with(|live| assert_eq!(live.get(), 0));
        arena::LIMIT.with(|limit| limit.set(arena::MAX_BYTES));
    }
}
fn cancellation(sweep: super::support::CancellationSweep) {
    let mut vm = configured(VmMode::Interpreter);
    super::support::assert_cancellation(
        &mut vm,
        "task",
        10000,
        sweep,
        reset,
        || EVENTS.with(|events| events.borrow().clone()),
        cleaned,
    );
}
#[test]
fn fixed_array_f2a_cancellation_boundaries() {
    cancellation(super::support::CancellationSweep::ResourceBoundaries);
}
#[test]
#[ignore = "exhaustive: cancel fixed-array initialization at every interpreter instruction"]
fn exhaustive_fixed_array_f2a_cancellation() {
    cancellation(super::support::CancellationSweep::EveryInstruction);
}

#[test]
fn fixed_array_f2a_rejects_unopened_contexts() {
    use crate::core::{prepared::PreparedModule, resolver::BindingRegistry};
    let types = crate::TypeRegistry::default();
    for body in [
        "fn bad(x: FixedArray<FixedArray<Int32,2>,2>) @suspends {}",
        "fn bad() { let a = alloc(fixed.repeat::<Int32,2>(1)) }",
        "import joky/arena.{type Arena} fn bad() { let arena = Arena.new(); let a = arena.alloc(fixed.repeat::<Int32,2>(1)) }",
        "fn bad() { let a = fixed.repeat::<Int32,2>(1); let f = || { a.len() }; f(); }",
        "fn bad() @suspends { let a = fixed.repeat::<Int32,2>(1) }",
        "fn bad() -> &FixedArray<Int32,2> { let a = fixed.repeat::<Int32,2>(1) &a }",
        "fn bad() { var a = fixed.repeat::<Int32,2>(1); let r = &a; a.set(0,2); r.len(); }",
        "fn bad() { let a = fixed.repeat::<String,0>(\"no\") }",
        "fn bad(x:&Int32) { let a = fixed.repeat::<&Int32,0>(x) }",
        "fn bad() { let text = \"once\"; let f = move |i:UInt64| { let owned = text; int32(i) }; fixed.from_fn::<Int32,2>(f); }",
        "fn wait(i:UInt64) @suspends -> Int32 { int32(i) } fn bad() { fixed.from_fn::<Int32,2>(wait); }",
        "extern fn foreign(a: FixedArray<Int32,2>)",
        "fn bad(){ let a=fixed.repeat::<FixedArray<Unit,65536>,1>(fixed.repeat::<Unit,65536>({})); a.get(0); }",
        "fn bad()->Option<FixedArray<Unit,65536>> {Some(fixed.repeat::<Unit,65536>({}))}",
        "struct Bad<T> { a:T, r:&Int32 } fn bad(x:&Bad<FixedArray<Int32,2>>) {}",

        "import joky/array.{type Array} struct Outer { data: Array<FixedArray<Int32,2>> } fn bad(x: Outer) {}",
        "fn bad() { let a = (fixed.repeat::<Int32,2>(1), \"owner\") }",
        "import joky/list.{type List} fn bad() { let a = List.new::<FixedArray<Int32,2>>() }",
        "fn bad(x: FixedArray<Int32,2>) -> &FixedArray<Int32,2> { &x }",
        "struct Packet { data:FixedArray<Int32,2> } fn bad() { let p=Packet{data:fixed.repeat::<Int32,2>(1)}; let f=||{p.data.len()}; f(); }",
        "fn f(x:FixedArray<Int32,2>)->FixedArray<Int32,2>{x} fn bad(){let g=f; g(fixed.repeat::<Int32,3>(1));}",

    ] {
        let source = format!("import joky/fixed_array as fixed.{{type FixedArray}}\n{body}");
        let result = crate::parser::parse(&source).map_err(crate::Error::from)
            .and_then(|module| PreparedModule::new(&module, &types, &BindingRegistry::default()))
            .and_then(|prepared| prepared.plan(&Default::default()));
        assert!(result.is_err(), "accepted: {source}");
    }
}

#[test]
fn fixed_array_f2b_values_execute() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!("../../tests/fixtures/fixed_array_f2b.jk"))
            .unwrap_or_else(|e| {
                panic!(
                    "{mode:?}: {e} {:?}",
                    e.span().and_then(|s| e
                        .sources()
                        .get(s.source_id())
                        .map(|f| (f.name(), s.slice(f.text()))))
                )
            });
        for name in [
            "nested",
            "records",
            "tuples",
            "variants",
            "indirect",
            "dynamic",
            "generated",
            "deeper",
            "empty_nested",
            "pattern",
            "borrowed",
            "aggregate_indirect",
            "aggregate_replace",
            "generated_variants",
        ] {
            assert_eq!(
                vm.call_int32_0(name)
                    .unwrap_or_else(|e| panic!("{mode:?} {name}: {e}")),
                42,
                "{mode:?} {name}"
            );
            arena::LIVE.with(|live| assert_eq!(live.get(), 0));
        }
    }
}

const F2B_CLEANUP: &str = include_str!("../../tests/fixtures/fixed_array_f2b_cleanup.jk");
#[test]
fn fixed_array_f2b_failures_and_budget_reuse() {
    let _restore = RestoreLimit(arena::LIMIT.with(|limit| limit.get()));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured_source(mode, F2B_CLEANUP);
        reset();
        assert_eq!(vm.call_int32_0("exercise").unwrap(), 42);
        EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 2, 3, 4, -1]));
        cleaned();
        reset();
        assert!(vm.call_int32_0("fail_init").is_err());
        EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 2, 3, -1]));
        cleaned();
        let mut succeeded = false;
        let mut failed_after_callback = false;
        for budget in (0..32768).step_by(128) {
            arena::LIMIT.with(|limit| limit.set(budget));
            reset();
            match vm.call_int32_0("exercise") {
                Ok(n) => {
                    assert_eq!(n, 42);
                    succeeded = true;
                }
                Err(error) => {
                    assert_eq!(
                        error.code(),
                        ErrorCode::ResourceLimit,
                        "{mode:?} {budget}: {error}"
                    );
                    failed_after_callback |= EVENTS.with(|events| events.borrow().contains(&1));
                }
            }
            cleaned();
        }
        assert!(succeeded && failed_after_callback);
        arena::LIMIT.with(|limit| limit.set(32768));
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!("../../tests/fixtures/fixed_array_f2b.jk"))
            .unwrap();
        assert_eq!(vm.call_int32_0("reuse").unwrap(), 42);
        arena::LIVE.with(|live| assert_eq!(live.get(), 0));
        arena::LIMIT.with(|limit| limit.set(arena::MAX_BYTES));
    }
}
fn f2b_cancellation(sweep: super::support::CancellationSweep) {
    let mut vm = configured_source(VmMode::Interpreter, F2B_CLEANUP);
    super::support::assert_cancellation(
        &mut vm,
        "task",
        20000,
        sweep,
        reset,
        || EVENTS.with(|events| events.borrow().clone()),
        cleaned,
    );
}
#[test]
fn fixed_array_f2b_cancellation_boundaries() {
    f2b_cancellation(super::support::CancellationSweep::ResourceBoundaries);
}
#[test]
#[ignore = "exhaustive: cancel array aggregates, indirect results and dynamic elements at every instruction"]
fn exhaustive_fixed_array_f2b_cancellation() {
    f2b_cancellation(super::support::CancellationSweep::EveryInstruction);
}

#[test]
fn fixed_array_f3_views_execute() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!("../../tests/fixtures/fixed_array_f3.jk"))
            .unwrap_or_else(|e| {
                panic!(
                    "{mode:?}: {e} {:?}",
                    e.span().map(|s| (
                        s,
                        s.slice(include_str!("../../tests/fixtures/fixed_array_f3.jk"))
                    ))
                )
            });
        for name in [
            "elements",
            "slice_values",
            "nested",
            "dynamic",
            "empty",
            "returned",
            "indirect",
            "generic",
            "shared_views",
            "element_views",
            "aligned",
            "noescape",
            "fields",
            "alias_methods",
            "record_method",
            "enum_view",
            "slice_operations",
        ] {
            assert_eq!(
                vm.call_int32_0(name)
                    .unwrap_or_else(|e| panic!("{mode:?} {name}: {e}")),
                42,
                "{mode:?} {name}"
            );
            arena::LIVE.with(|live| assert_eq!(live.get(), 0));
        }
        for name in [
            "bad_index",
            "bad_mut_index",
            "bad_range",
            "bad_end",
            "bad_empty",
            "bad_zero_size",
        ] {
            let error = vm.call_int32_0(name).unwrap_err();
            assert_eq!(
                error.code(),
                ErrorCode::RuntimeError,
                "{mode:?} {name}: {error}"
            );
            assert!(
                error
                    .to_string()
                    .contains("FixedArray index or slice is out of bounds"),
                "{error}"
            );
            assert!(error.span().is_some());
            arena::LIVE.with(|live| assert_eq!(live.get(), 0));
        }
    }
}

#[test]
fn fixed_array_f3_rejects_invalid_view_origins_and_access() {
    use crate::core::{prepared::PreparedModule, resolver::BindingRegistry};
    let types = crate::TypeRegistry::default();
    let prefix = "import joky/fixed_array as fixed.{type FixedArray} import joky/slice as slices.{type Slice} import joky/array.{type Array} import joky/mem type Row=FixedArray<Int32,3> fn row()->Row { fixed.repeat::<Int32,3>(42) }";
    for body in [
        "fn bad()->&Int32 { let a=row(); a.borrow_ref(0) }",
        "fn bad()->&Slice<Int32> { let a=row(); a.slice(0,0) }",
        "fn bad(a:Row)->&Int32 { a.borrow_ref(0) }",
        "fn bad(a:Row)->&Slice<Int32> { a.slice(0,3) }",
        "fn bad()->&Int32 { let a=row(); let f=fixed.borrow_ref::<Int32,3>; f(&a,0) }",
        "fn view<T: Copy,const N:UInt64>(a:&FixedArray<T,N>)->&Slice<T>{a.slice(0,N)} fn bad()->&Slice<Int32>{let a=row(); view(&a)}",
        "struct Packet{data:Row} fn bad()->&Int32{let p=Packet{data:row()}; p.data.borrow_ref(0)}",
        "fn bad()->&Slice<Int32>{var a=Array.new::<Row>(); a.push(row()); a.borrow_ref(0).slice(0,3)}",
        "fn bad(){var a=row(); let r=a.borrow_ref(0); a=row(); let n=*r}",
        "fn bad(){var a=row(); let r=a.slice(0,0); a=row(); slices.len(r);}",
        "fn bad(){var a=row(); let r=a.borrow_ref(0); a.set(1,9); let n=*r}",
        "fn bad(){var a=row(); let r=a.borrow_mut_ref(0); let n=*a.borrow_ref(1); *r=9}",
        "fn bad(){var a=row(); let r=a.borrow_mut_ref(0); let copy=a; *r=9}",
        "fn bad(){var a=row(); let r=a.slice_mut(0,0); let copy=a; slices.len(r);}",
        "fn bad(){var a=row(); let r=a.borrow_mut_ref(0); let s=a.borrow_mut_ref(1); *r=1; *s=2}",
        "fn bad(){var a=fixed.repeat::<Unit,3>({}); let r=a.slice_mut(0,0); let s=a.slice_mut(3,3); slices.len(r); slices.len(s);}",
        "fn bad(){var a=fixed.repeat::<Int32,0>(1); let s=a.slice(0,0); a=fixed.repeat::<Int32,0>(2); slices.len(s);}",
        "fn bad(){let r={let a=row(); a.borrow_ref(0)}; let n=*r}",
        "fn bad(){var a=Array.new::<Row>(); a.push(row()); let r=a.borrow_ref(0).borrow_ref(1); a.reserve(30); let n=*r}",
        "fn bad(){var a=Array.new::<Row>(); a.push(row()); let r=a.borrow_ref(0).slice(0,0); a.set(0,row()); slices.len(r);}",
        "fn bad(){var a=Array.new::<Row>(); a.push(row()); let r=a.borrow_ref(0).borrow_ref(1); a.take_last(); let n=*r}",
        "fn bad(){var a=Array.new::<Row>(); a.push(row()); let r=a.borrow_ref(0).slice(0,0); a.release(); slices.len(r);}",
        "fn bad(){var a=row(); let s=a.slice_mut(0,3); let r=slices.borrow_mut_ref(s,0); slices.set(s,1,9); *r=2}",
        "fn bad(){let a=row(); let s=a.slice(0,3); slices.set(s,0,9);}",
        "fn bad(other:&Row)->&Slice<Int32>{let a=row(); a.slice(0,3)}",
        "struct Holder{r:&Int32} fn bad(a:&Row)->&Holder{alloc(Holder{r:a.borrow_ref(0)})}",
        "struct Holder{r:&Int32} fn first(a:&Row)->&Int32{a.borrow_ref(0)} fn bad(a:&Row)->&Holder{alloc(Holder{r:first(a)})}",
        "fn bad(a:&Row,b:&mut Array<&Int32>){b.push(a.borrow_ref(0))}",
        "struct P{tag:Int32,data:Row} struct Holder{r:&Int32} fn bad(p:&P)->&Holder{alloc(Holder{r:&(*p).tag})}",

        "struct P{data:Row} fn bad(p:&P)->&mut Int32{p.data.borrow_mut_ref(0)}",
        "struct P{data:Row} impl P{fn view(&self)->&Slice<Int32>{self.data.slice(0,3)}} fn bad(p:P)->&Slice<Int32>{p.view()}",
        "fn at(a:&Row)->&Int32{a.borrow_ref(0)} fn bad(){var a=Array.new::<Row>(); a.push(row()); let f=at; let r=f(a.borrow_ref(0)); a.reserve(20); let n=*r}",
        "fn bad(){var a=row(); let f=fixed.slice::<Int32,3>; let s=f(&a,0,0); a=row(); slices.len(s);}",
        "struct P{data:Row} fn bad(){var p=P{data:row()}; let r=p.data.borrow_mut_ref(0); let copy=p; *r=1}",

    ] {
        let source = format!("{prefix} {body}");
        let module = crate::parser::parse(&source).unwrap_or_else(|e|panic!("{body}: {e}"));
        let result = PreparedModule::new(&module, &types, &BindingRegistry::default())
            .and_then(|prepared| prepared.plan(&Default::default()));
        let error = result.err().unwrap_or_else(|| panic!("accepted: {body}"));
        assert!(matches!(error.code(), ErrorCode::InvalidAssignment | ErrorCode::TypeMismatch | ErrorCode::InvalidCall), "{body}: {error}");
        assert!(!error.to_string().contains("not supported in this context"), "view was gated instead of borrow checked: {body}: {error}");
    }
}

const F3_CLEANUP: &str = include_str!("../../tests/fixtures/fixed_array_f3_cleanup.jk");
#[test]
fn fixed_array_f3_failures_headers_and_payload_reuse() {
    let _restore = RestoreLimit(arena::LIMIT.with(|limit| limit.get()));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured_source(mode, F3_CLEANUP);
        reset();
        assert_eq!(vm.call_int32_0("exercise").unwrap(), 42);
        EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 2, 3, 4, -1]));
        cleaned();
        reset();
        let error = vm.call_int32_0("failure").unwrap_err();
        assert_eq!(error.code(), ErrorCode::RuntimeError);
        assert!(error
            .to_string()
            .contains("FixedArray index or slice is out of bounds"));
        EVENTS.with(|events| assert_eq!(*events.borrow(), [1, -1]));
        cleaned();
        let mut succeeded = false;
        let mut failed_with_view = false;
        for budget in (0..16384).step_by(128) {
            arena::LIMIT.with(|limit| limit.set(budget));
            reset();
            match vm.call_int32_0("exercise") {
                Ok(n) => {
                    assert_eq!(n, 42);
                    succeeded = true;
                }
                Err(error) => {
                    assert_eq!(
                        error.code(),
                        ErrorCode::ResourceLimit,
                        "{mode:?} {budget}: {error}"
                    );
                    failed_with_view |= EVENTS.with(|events| events.borrow().contains(&2));
                }
            }
            cleaned();
        }
        assert!(succeeded && failed_with_view);
        arena::LIMIT.with(|limit| limit.set(65536));
        reset();
        assert_eq!(vm.call_int32_0("payload_reuse").unwrap(), 42);
        cleaned();
        assert_eq!(vm.call_int32_0("references").unwrap(), 42);
        cleaned();
        assert_eq!(
            vm.call_int32_0("headers").unwrap_err().code(),
            ErrorCode::ResourceLimit
        );
        cleaned();
        arena::LIMIT.with(|limit| limit.set(arena::MAX_BYTES));
        assert_eq!(vm.call_int32_0("headers").unwrap(), 42);
        cleaned();
    }
}
fn f3_cancellation(sweep: super::support::CancellationSweep) {
    let mut vm = configured_source(VmMode::Interpreter, F3_CLEANUP);
    super::support::assert_cancellation(
        &mut vm,
        "task",
        20000,
        sweep,
        reset,
        || EVENTS.with(|events| events.borrow().clone()),
        cleaned,
    );
}
#[test]
fn fixed_array_f3_cancellation_boundaries() {
    f3_cancellation(super::support::CancellationSweep::ResourceBoundaries);
}
#[test]
#[ignore = "exhaustive: cancel fixed-array views, nested loans and returned slice headers at every instruction"]
fn exhaustive_fixed_array_f3_cancellation() {
    f3_cancellation(super::support::CancellationSweep::EveryInstruction);
}

#[test]
fn fixed_array_f5_rejects_recursive_wrappers_at_closed_boundaries() {
    use crate::core::{prepared::PreparedModule, resolver::BindingRegistry};
    let prefix = r#"
        import joky/fixed_array as fixed.{type FixedArray}
        import joky/array.{type Array}
        import joky/arena.{type Arena}
        import joky/list.{type List}
        import joky/map.{type Map}
        import joky/set.{type Set}
        type Row=FixedArray<Int32,2>
        struct Wrapper<T>{value:T}
        type Nested=Wrapper<Option<(Row,Int32)>>
        fn make()->Nested {Wrapper{value:Some((fixed.repeat::<Int32,2>(42),0))}}
    "#;
    let prepare = |source: &str| {
        let ast = crate::parser::parse(source).unwrap();
        PreparedModule::new(
            &ast,
            &crate::TypeRegistry::default(),
            &BindingRegistry::default(),
        )
        .and_then(|p| p.plan(&Default::default()))
    };
    prepare(prefix).unwrap();
    for body in [
        "fn bad(){let p=alloc(make())}",
        "fn bad(){let p=graph{let p=graph.alloc(make()); p}}",
        "fn bad(){let arena=Arena.new(); let p=arena.alloc(make())}",
        "fn bad(){let p=make(); let f=||{p.value;}; f();}",
        "fn bad(){let p=make(); let f=move ||{p.value;}; f();}",
        "fn bad(p:&Nested){let f=||{p.value;}; f();}",
        "fn bad(){let values=List.new::<Nested>()}",
        "fn bad(){let values=Map.new::<Int32,Nested>()}",
        "fn bad(){let values=Set.new::<Nested>()}",
        "fn bad(){let values=List.new::<Array<Row>>()}",
        "fn bad(){let values=Wrapper{value:Array.new::<Row>()}}",
        "fn bad(){let values=Some(Array.new::<Row>())}",
        "fn bad(p:Nested) @suspends {}",
        "fn bad(p:&Nested) @suspends {}",
        "fn bad(p:Array<Row>) @suspends {}",
        "fn bad() @suspends {parallel{|let p=make()|let p=make()}}",
        "fn bad() @suspends {race{|let p=make()|let p=make()}}",
        "extern fn foreign(p:Nested)",
        "extern fn foreign(p:&Nested) @suspends",
        "fn bad()->&Nested{let p=make(); &p}",
        "fn bad(){let p=fixed.repeat::<Wrapper<String>,0>(Wrapper{value:\"owned\"})}",
    ] {
        let source = format!("{prefix}\n{body}");
        let error = prepare(&source)
            .err()
            .unwrap_or_else(|| panic!("accepted: {body}"));
        assert!(error.span().is_some(), "{body}: {error}");
        assert!(
            !matches!(
                error.code(),
                ErrorCode::UnknownName | ErrorCode::UnknownType
            ),
            "wrong rejection: {body}: {error}"
        );
    }
}

#[test]
fn fixed_array_f5_wrapped_values_cannot_enter_persistent_or_public_abis() {
    use crate::core::{prepared::PreparedModule, resolver::BindingRegistry};
    let source = r#"
        import joky/fixed_array as fixed.{type FixedArray}
        type Row=FixedArray<Int32,2>
        struct Wrapper<T>{value:T}
        struct State{nested:Wrapper<Option<Row>>}
        struct Plain{value:Int32}
        fn make()->State{State{nested:Wrapper{value:Some(fixed.repeat::<Int32,2>(42))}}}
        fn migrate(old:&Plain)->State{make()}
        fn read(state:&State)->Int32{42}
        fn run()->Int32{let value=make(); read(&value)}
    "#;
    let ast = crate::parser::parse(source).unwrap();
    let prepared = PreparedModule::new(
        &ast,
        &crate::TypeRegistry::default(),
        &BindingRegistry::default(),
    )
    .unwrap();
    let plan = prepared.plan(&Default::default()).unwrap();
    let state_source = source.replace("struct State", "#state struct State");
    let state_ast = crate::parser::parse(&state_source).unwrap();
    assert!(PreparedModule::new(
        &state_ast,
        &crate::TypeRegistry::default(),
        &BindingRegistry::default()
    )
    .and_then(|p| p.plan(&Default::default()))
    .is_err());
    let root = &plan.functions["make"].signature.ret;
    assert!(!crate::core::instance::persistent(root));
    assert!(crate::core::script::schema(root).is_none());
    for name in ["make", "migrate", "read"] {
        assert!(!plan.functions[name].instance_entry, "{name}");
        assert!(!plan.functions[name].domain_entry, "{name}");
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        assert_eq!(vm.call_int32_0("run").unwrap(), 42);
        assert!(vm.create_instance("make", &[]).is_err());
        assert!(vm.create_domain("make", &[]).is_err());
        assert!(vm.call_int32_0("make").is_err());
        assert!(vm.call_async("make", &[]).is_err());
        assert_eq!(vm.instance_count(), 0);
        assert_eq!(vm.instance_bytes(), 0);
        arena::LIVE.with(|n| assert_eq!(n.get(), 0));
    }
    let mut aot = crate::aot::AotCompiler::new("fixed-array-private").unwrap();
    let exports = aot.compile_module(&ast).unwrap();
    for name in ["make", "migrate", "read"] {
        assert!(!exports.iter().any(|export| export == name), "{name}");
    }
}

#[test]
fn fixed_array_f5_graph_allocation_rejects_nested_arrays_before_lowering() {
    let source = r#"
        import joky/fixed_array as fixed.{type FixedArray}
        struct Wrap<T>{value:T}
        type Row=FixedArray<Int32,2>
        fn bad(){let p=graph{let p=graph.alloc(Wrap{value:Some(fixed.repeat::<Int32,2>(42))}); p}}
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        let error = vm.load_source(source).unwrap_err();
        assert_eq!(error.code(), ErrorCode::TypeMismatch);
        assert!(error.message().contains("graph.alloc"));
        assert!(error
            .span()
            .unwrap()
            .slice(source)
            .unwrap()
            .contains("graph.alloc"));
    }
    let error = crate::aot::AotCompiler::new("graph-array-rejected")
        .unwrap()
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::TypeMismatch);
    assert!(error.message().contains("graph.alloc"));
}

#[test]
fn fixed_array_f5_imported_wrappers_preserve_graph_rejection() {
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "payload",
            r#"
        import joky/fixed_array as fixed.{type FixedArray}
        pub struct Wrap<T>{pub value:T}
        pub type Payload=Wrap<Option<FixedArray<Int32,2>>>
        pub fn make()->Payload{Wrap{value:Some(fixed.repeat::<Int32,2>(42))}}
    "#,
        )
        .unwrap();
    let source = "import payload.{make} fn bad(){let p=graph{let p=graph.alloc(make()); p}}";
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        let error = vm.load_source_with_modules(source, &modules).unwrap_err();
        assert_eq!(error.code(), ErrorCode::TypeMismatch);
        assert!(error.message().contains("graph.alloc"));
    }
    let ast = crate::TypeRegistry::default()
        .resolve_with_modules(&crate::parser::parse(source).unwrap(), &modules)
        .unwrap();
    let error = crate::aot::AotCompiler::new("imported-array-graph")
        .unwrap()
        .compile_module(&ast)
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::TypeMismatch);
    assert!(error.message().contains("graph.alloc"));
}

#[test]
fn fixed_array_f5_migration_cannot_publish_wrapped_array_candidates() {
    use crate::vm::state_domains::migration;
    let source = r#"
        import joky/fixed_array as fixed.{type FixedArray}
        struct Wrap<T>{value:T}
        struct State{payload:Wrap<Option<FixedArray<Int32,2>>>}
        fn upgrade(old:&PreviousState)->State{State{payload:Wrap{value:Some(fixed.repeat::<Int32,2>(old.value))}}}
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(
            "struct Plain{value:Int32} fn make()->Plain{Plain{value:42}} fn version()->Int32{42}",
        )
        .unwrap();
        let domain = vm.create_domain("make", &[]).unwrap();
        let owners = vec![vm.script_code_owner().unwrap()];
        let schema = migration::schema(&vm.domains, &owners).unwrap();
        let usage = vm.domains.borrow().usage();
        let version = vm.module_version();
        let result = migration::prepare(&vm, &vm.domains, &owners, schema, source, "upgrade");
        assert!(
            result.is_err(),
            "accepted wrapped array migration: {mode:?}"
        );
        assert_eq!(vm.module_version(), version);
        assert_eq!(migration::schema(&vm.domains, &owners).unwrap(), schema);
        assert_eq!(vm.domains.borrow().usage(), usage);
        assert_eq!(vm.call_int32_0("version").unwrap(), 42);
        domain.close();
        drop(vm);
        arena::LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}
