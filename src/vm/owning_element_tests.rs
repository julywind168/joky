use super::{VmEngine, VmMode};
use crate::{runtime::arena::LIVE, ErrorCode, FunctionSignature, Type};

thread_local! {
    static EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) };
}
extern "C" fn record_drop(value: i32) {
    EVENTS.with(|events| events.borrow_mut().push(value));
    if value == -99 {
        crate::runtime::arena::LIMIT.with(|limit| limit.set(1));
    }
}
const SOURCE: &str = include_str!("../../tests/fixtures/owning_elements.jk");
fn vm(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    unsafe {
        vm.register_fn(
            "record_drop",
            FunctionSignature::new(vec![Type::Int32], Type::Unit),
            record_drop as *const u8,
        )
        .unwrap();
    }
    vm.load_source(SOURCE)
        .unwrap_or_else(|error| panic!("{error} at {:?}", error.span()));
    vm
}

#[test]
fn owning_elements_move_and_drop_in_both_execution_modes() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        for (entry, failure, events) in [
            ("elements_growth", false, vec![5, 4, 3, 2, 1]),
            ("elements_generic_metadata", false, vec![1]),
            ("elements_set", false, vec![2, 5, 3, 4, 1]),
            ("elements_release", false, vec![2, 1]),
            ("elements_overwrite", false, vec![2, 1, 3]),
            ("elements_nested", false, vec![3, 2, 1]),
            ("elements_groups", false, vec![10, 1, 2, 20, 3, 4, 30, 5, 6]),
            ("enums_borrow_boundary", false, vec![1]),
            ("enums_phantom_reference", false, vec![]),
            ("enums_phantom", false, vec![]),
            ("enums_buffers", false, vec![]),
            ("enums_field", false, vec![1, 2]),
            ("enums_array_set", false, vec![1, 5, 10, 2, 3, 4]),
            ("enums_methods", false, vec![1, 2, 4, 5, 7, 6, 3]),
            ("enums_empty_replacement_failure", true, vec![2, 4, 3, 1]),
            ("enums_custom", false, vec![10, 2, 3, 1]),
            ("enums_pop", false, vec![3, 1, 2]),
            ("enums_result", false, vec![2, 3, 10, 4, 5, 1]),
            ("enums_overwrite", false, vec![1, 10, 2, 3]),
            ("enums_nested_array", false, vec![40, 30, 20, 10, 1]),
            ("enums_array", false, vec![10, 2, 3, 1]),
            ("enums_wildcard", false, vec![10, 1, 2]),
            ("enums_early_exit", false, vec![1]),
            ("enums_failure", true, vec![1]),
            ("enums_cleanup_failure", true, vec![3, 2, 1]),
            ("elements_empty", true, vec![]),
            ("elements_rhs_failure", true, vec![3, 2, 1]),
            ("elements_drop_failure", true, vec![2, 4, 3, 1]),
            ("elements_cleanup_failure", true, vec![3, 2, 1]),
        ] {
            EVENTS.with(|events| events.borrow_mut().clear());
            let result = vm.call_int32_0(entry);
            assert_eq!(result.is_err(), failure, "{mode:?} {entry}: {result:?}");
            if !failure {
                assert_eq!(result.unwrap(), 42, "{mode:?} {entry}");
            }
            EVENTS.with(|actual| assert_eq!(*actual.borrow(), events, "{mode:?} {entry}"));
            LIVE.with(|count| assert_eq!(count.get(), 0, "{mode:?} {entry}"));
        }
    }
}
fn tracked(context: &str) {
    EVENTS.with(|events| {
        let events = events.borrow();
        for id in 1..=6 {
            let drops = events.iter().filter(|&&event| event == id).count();
            assert!(drops <= 1, "{context}: duplicate drop {events:?}");
            if events.contains(&-id) {
                assert_eq!(drops, 1, "{context}: lost owner {events:?}");
            }
        }
    });
    LIVE.with(|count| assert_eq!(count.get(), 0, "{context}"));
}
#[test]
fn owning_element_transfers_survive_every_interpreter_pause() {
    let mut vm = vm(VmMode::Interpreter);
    vm.set_interpreter_limits(super::InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    let mut completed = false;
    for pause in 0..2000 {
        EVENTS.with(|events| events.borrow_mut().clear());
        let mut task = vm.call_async("elements_task", &[]).unwrap();
        for _ in 0..pause {
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        completed = task.is_finished();
        drop(task);
        tracked(&format!("pause {pause}"));
        if completed {
            break;
        }
    }
    assert!(completed);
}
#[test]
fn owning_element_transfers_survive_every_instruction_budget() {
    let mut vm = vm(VmMode::Interpreter);
    let mut completed = false;
    for budget in 1..2000 {
        EVENTS.with(|events| events.borrow_mut().clear());
        vm.set_interpreter_limits(super::InterpreterLimits {
            instruction_budget: Some(budget),
            ..Default::default()
        })
        .unwrap();
        match vm.call_int32_0("elements_transitions") {
            Ok(value) => {
                assert_eq!(value, 42);
                completed = true;
            }
            Err(error) => assert_eq!(error.code(), ErrorCode::ResourceLimit),
        }
        tracked(&format!("budget {budget}"));
        if completed {
            break;
        }
    }
    assert!(completed);
}
#[test]
fn owning_elements_reject_copy_reads_views_and_hidden_capabilities() {
    let prefix = "import joky/array.{type Array} struct Resource { values: Array<Int32> } ";
    for body in [
        "fn bad(a: &Array<Resource>) { a.get(0); }",
        "fn bad(a: &Array<Resource>) { a.borrow(0); }",
        "fn bad(a: &mut Array<Resource>) { a.borrow_mut(0); }",
        "fn bad(a: &Array<Resource>) { a.slice(0, 1); }",
        "fn bad(a: &mut Array<Resource>) { a.slice_mut(0, 1); }",
        "fn bad() { var a = Array.new::<Resource>() let value = Resource { values: Array.new::<Int32>() } a.push(value) a.push(value) }",
        "fn bad(a: &Array<Resource>) -> Resource { a.take_last() }",
        "fn bad(a: Array<Resource>) @suspends {}",
        "fn bad(a: Array<Resource>) { let reference = &a a.release() reference.len(); }",
        "fn bad() { var a = Array.new::<Resource>() a.push(Resource { values: Array.new::<Int32>() }) let reference = &a a.take_last(); reference.len(); }",
        "fn bad() { var a = Array.new::<Resource>() let view = &a a.set(0, Resource { values: Array.new::<Int32>() }) view.len(); }",
        "fn bad(a: Array<&Array<Int32>>) {}",
        "struct SharedOwner { values: Array<&Int32> } fn bad(a: Array<SharedOwner>) {}",
        "fn bad(a: Array<Resource>) { alloc(a); }",
    ] {
        let ast = crate::parser::parse(&format!("{prefix}{body}")).unwrap();
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let error = VmEngine::with_mode(mode).load_module(&ast).expect_err(body);
            assert!(error.span().is_some(), "{body}: {error}");
        }
        crate::aot::AotCompiler::new("invalid-owning-element").unwrap().compile_module(&ast).expect_err(body);
    }
}

#[test]
fn owning_elements_keep_drop_code_after_reload_and_vm_destruction() {
    for finish in [false, true] {
        let mut vm = vm(VmMode::Interpreter);
        vm.set_interpreter_limits(super::InterpreterLimits {
            poll_budget: Some(1),
            ..Default::default()
        })
        .unwrap();
        EVENTS.with(|events| events.borrow_mut().clear());
        let mut task = vm.call_async("elements_task", &[]).unwrap();
        while !EVENTS.with(|events| events.borrow().contains(&-5)) {
            task.resume().unwrap();
        }
        vm.load_source("fn replacement() -> Int32 { 7 }").unwrap();
        drop(vm);
        if finish {
            while !task.is_finished() {
                task.resume().unwrap();
            }
            assert_eq!(task.cps_result(), Some(42));
        }
        drop(task);
        tracked(if finish {
            "reload completion"
        } else {
            "reload cancellation"
        });
    }
}
struct RestoreLimit;
impl Drop for RestoreLimit {
    fn drop(&mut self) {
        crate::runtime::arena::LIMIT.with(|limit| limit.set(crate::runtime::arena::MAX_BYTES));
    }
}
#[test]
fn owning_elements_cleanup_after_allocation_and_snapshot_failures() {
    let _restore = RestoreLimit;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        let mut completed = false;
        for budget in (1..4000).step_by(16) {
            EVENTS.with(|events| events.borrow_mut().clear());
            crate::runtime::arena::LIMIT.with(|limit| limit.set(budget));
            match vm.call_int32_0("elements_transitions") {
                Ok(value) => {
                    assert_eq!(value, 42);
                    completed = true;
                }
                Err(error) => assert_eq!(error.code(), ErrorCode::ResourceLimit),
            }
            tracked(&format!("{mode:?} allocation budget {budget}"));
            if completed {
                break;
            }
        }
        assert!(completed, "{mode:?}");
        crate::runtime::arena::LIMIT.with(|limit| limit.set(crate::runtime::arena::MAX_BYTES));
    }
    let mut vm = vm(VmMode::Interpreter);
    vm.load_source(&format!("{SOURCE}\n fn snapshot_failure() -> Int32 {{ var values = Array.new::<ElementResource>() values.push(element_resource(1)) record_drop(-99) let result = values.take_last() result.id }}")).unwrap();
    EVENTS.with(|events| events.borrow_mut().clear());
    assert_eq!(
        vm.call_int32_0("snapshot_failure").unwrap_err().code(),
        ErrorCode::ResourceLimit
    );
    EVENTS.with(|events| assert_eq!(*events.borrow(), vec![-99, 1]));
    LIVE.with(|count| assert_eq!(count.get(), 0));
}
#[test]
fn owning_elements_preserve_consumer_identity_through_generic_modules() {
    let mut modules = crate::ModuleSources::new();
    modules.insert("collections", r#"
        import joky/array.{type Array}
        pub type Batch<T> = Array<T>
        pub fn make<T>(value: T) -> Batch<T> { var values = Array.new::<T>() values.push(value) values }
        pub fn take<T>(values: &mut Batch<T>) -> T { values.take_last() }
        pub enum Parcel<T> { Empty Item(T) }
        pub fn wrap<T>(value: T) -> Parcel<T> { Parcel.Item(value) }
        pub fn unwrap<T>(value: Parcel<T>) -> Option<T> { match value { | Empty => None | Item(item) => Some(item) } }
        pub fn pop<T>(values: &mut Batch<T>) -> Option<T> { values.pop() }
    "#).unwrap();
    let source = r#"
        import joky/array.{type Array}
        import joky/drop.{type Drop}
        import collections as c
        extern fn record_drop(value: Int32)
        struct Resource { private values: Array<Int32> id: Int32 }
        impl Drop for Resource { fn drop(&mut self) { record_drop(self.id) } }
        fn check() -> Int32 {
            var inner = Array.new::<Int32>()
            inner.push(42)
            var values = c.make(Resource { values: inner id: 1 })
            let nested = c.unwrap(c.wrap(c.pop(&mut values)))
            let result = match nested { | None => return 0 | Some(inner) => match inner { | None => return 0 | Some(item) => item } }
            result.values.get(0).unwrap_or(0)
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = vm(mode);
        vm.load_source_with_modules(source, &modules).unwrap();
        EVENTS.with(|events| events.borrow_mut().clear());
        assert_eq!(vm.call_int32_0("check").unwrap(), 42);
        EVENTS.with(|events| assert_eq!(*events.borrow(), vec![1]));
        LIVE.with(|count| assert_eq!(count.get(), 0));
    }
    let ast = crate::parser::parse(source).unwrap();
    let ast = crate::TypeRegistry::default()
        .resolve_with_modules(&ast, &modules)
        .unwrap();
    crate::aot::AotCompiler::new("owning-elements-modules")
        .unwrap()
        .compile_module(&ast)
        .unwrap();
}

#[test]
fn owning_enums_survive_interpreter_boundaries_and_allocation_failure() {
    let _restore = RestoreLimit;
    let mut vm = vm(VmMode::Interpreter);
    vm.set_interpreter_limits(super::InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    let mut completed = false;
    for pause in 0..2000 {
        EVENTS.with(|e| e.borrow_mut().clear());
        let mut task = vm.call_async("enums_task", &[]).unwrap();
        for _ in 0..pause {
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        completed = task.is_finished();
        drop(task);
        tracked(&format!("enum pause {pause}"));
        if completed {
            break;
        }
    }
    assert!(completed);
    completed = false;
    for budget in 1..2000 {
        EVENTS.with(|e| e.borrow_mut().clear());
        vm.set_interpreter_limits(super::InterpreterLimits {
            instruction_budget: Some(budget),
            ..Default::default()
        })
        .unwrap();
        match vm.call_int32_0("enums_transitions") {
            Ok(value) => {
                assert_eq!(value, 42);
                completed = true;
            }
            Err(error) => assert_eq!(error.code(), ErrorCode::ResourceLimit),
        }
        tracked(&format!("enum instruction budget {budget}"));
        if completed {
            break;
        }
    }
    assert!(completed);
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = super::owning_element_tests::vm(mode);
        completed = false;
        for budget in (1..5000).step_by(16) {
            EVENTS.with(|e| e.borrow_mut().clear());
            crate::runtime::arena::LIMIT.with(|limit| limit.set(budget));
            match vm.call_int32_0("enums_transitions") {
                Ok(value) => {
                    assert_eq!(value, 42);
                    completed = true;
                }
                Err(error) => assert_eq!(error.code(), ErrorCode::ResourceLimit),
            }
            tracked(&format!("enum {mode:?} allocation {budget}"));
            if completed {
                break;
            }
        }
        assert!(completed, "{mode:?}");
        crate::runtime::arena::LIMIT.with(|limit| limit.set(crate::runtime::arena::MAX_BYTES));
    }
}
#[test]
fn owning_enums_reject_reuse_borrowed_matching_and_invalid_payloads() {
    let prefix = "import joky/array.{type Array} import joky/drop.{type Drop} enum Message { Empty Item(Array<Int32>) } ";
    for body in [
        "fn bad(value: Message) { let next = value let again = value }",
        "fn bad(value: Message) { match value { | Empty => {} | Item(_) => {} } let again = value }",
        "fn bad(value: Message) { match value { | _ => value } }",
        "fn bad(value: Message) { match value { | Empty => {} | Item(item) => { let first = item let second = item } } }",
        "fn bad(value: &Message) { match value { | Empty => {} | Item(_) => {} } }",
        "enum Bad { Owned(Array<Int32>) Shared(&Int32) } fn bad() {}",
        "enum Bad { View(&Array<Int32>) } fn bad() {}",
        "struct Hidden { view: &Option<Array<Int32>> } fn bad() {}",
        "struct Hidden<T> { view: &Option<T> } fn bad(value: Hidden<Array<Int32>>) {}",
        "enum Hidden<T> { View(&Option<T>) } fn bad(value: Hidden<Array<Int32>>) {}",
        "type Hidden<T> = &Option<T> struct Stored<T> { value: Hidden<T> } fn bad(value: Stored<Array<Int32>>) {}",
        "type Hidden = &Option<Array<Int32>> struct Stored { value: Hidden } fn bad() {}",
        "struct Hidden { view: &Later<Array<Int32>> } enum Later<T> { Empty Item(T) } fn bad() {}",
        "enum Hidden { View(&Later<Array<Int32>>) } enum Later<T> { Empty Item(T) } fn bad() {}",
        "struct Hidden { view: &Later<Array<Int32>> } struct Later<T> { value: T } fn bad() {}",
        "fn bad(value: Message) { let pair = (value, 1) }",
        "fn bad(value: Option<Array<Int32>>) { value.zip(Option.Some(42)); }",
        "fn bad(value: Message) { alloc(value); }",
        "fn bad(value: Message) @suspends {}",
        "impl Drop for Message { fn drop(&mut self) {} } fn bad() {}",
    ] {
        let source = format!("{prefix}{body}");
        let ast = crate::parser::parse(&source).unwrap();
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let error = VmEngine::with_mode(mode).load_module(&ast).expect_err(body);
            assert!(error.span().is_some(), "{body}: {error}");
        }
        crate::aot::AotCompiler::new("invalid-owning-enum").unwrap().compile_module(&ast).expect_err(body);
    }
}
#[test]
fn owning_enum_drop_code_survives_reload_and_vm_destruction() {
    for finish in [false, true] {
        let mut vm = vm(VmMode::Interpreter);
        vm.set_interpreter_limits(super::InterpreterLimits {
            poll_budget: Some(1),
            ..Default::default()
        })
        .unwrap();
        EVENTS.with(|e| e.borrow_mut().clear());
        let mut task = vm.call_async("enums_task", &[]).unwrap();
        while !EVENTS.with(|e| e.borrow().iter().filter(|&&v| v == -1).count() >= 2) {
            task.resume().unwrap();
        }
        vm.load_source("fn replacement() -> Int32 { 7 }").unwrap();
        drop(vm);
        if finish {
            while !task.is_finished() {
                task.resume().unwrap();
            }
            assert_eq!(task.cps_result(), Some(42));
        }
        drop(task);
        tracked("enum reload");
    }
}

#[test]
fn owning_enum_snapshots_and_failed_return_preserve_drop_responsibility() {
    let _restore = RestoreLimit;
    let mut vm = vm(VmMode::Interpreter);
    vm.load_source(&format!(r#"{SOURCE}
        struct LimitTrigger {{ values: Array<Int32> }}
        impl Drop for LimitTrigger {{ fn drop(&mut self) {{ record_drop(-99) }} }}
        fn send_enum() -> Message {{
            let trigger = LimitTrigger {{ values: Array.new::<Int32>() }}
            Message.Resource(element_resource(1))
        }}
        fn enum_result_snapshot_failure() -> Int32 {{ let value = send_enum() match value {{ | _ => 42 }} }}
        fn enum_match_snapshot_failure() -> Int32 {{
            let value = Message.Resource(element_resource(1))
            record_drop(-99)
            match value {{ | Resource(item) => item.id | _ => 0 }}
        }}
        fn enum_failed_return() -> Message {{
            let failure = failing_element(2)
            Message.Resource(element_resource(1))
        }}
        fn enum_failed_return_entry() -> Int32 {{ match enum_failed_return() {{ | _ => 42 }} }}
    "#)).unwrap();
    for entry in [
        "enum_result_snapshot_failure",
        "enum_match_snapshot_failure",
    ] {
        EVENTS.with(|e| e.borrow_mut().clear());
        crate::runtime::arena::LIMIT.with(|l| l.set(crate::runtime::arena::MAX_BYTES));
        assert_eq!(
            vm.call_int32_0(entry).unwrap_err().code(),
            ErrorCode::ResourceLimit
        );
        EVENTS.with(|e| assert_eq!(*e.borrow(), vec![-99, 1], "{entry}"));
        LIVE.with(|l| assert_eq!(l.get(), 0));
    }
    crate::runtime::arena::LIMIT.with(|l| l.set(crate::runtime::arena::MAX_BYTES));
    EVENTS.with(|e| e.borrow_mut().clear());
    assert!(vm.call_int32_0("enum_failed_return_entry").is_err());
    EVENTS.with(|e| assert_eq!(*e.borrow(), vec![2, 1]));
    LIVE.with(|l| assert_eq!(l.get(), 0));
}
