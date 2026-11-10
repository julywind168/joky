use super::{VmEngine, VmMode};
use crate::runtime::arena::LIVE;
use crate::{FunctionSignature, Type};

thread_local! {
    static EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) };
}

extern "C" fn record_drop(value: i32) {
    EVENTS.with(|events| events.borrow_mut().push(value));
}

const SOURCE: &str = include_str!("../../../../tests/fixtures/multi_owners.jk");

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
        .unwrap_or_else(|e| panic!("{e} at {:?}", e.span()));
    vm
}

#[test]
fn multi_owner_moves_and_cleanup_preserve_order_and_metadata() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        for (entry, failure, expected) in [
            ("multi_scope", false, Vec::<i32>::new()),
            ("multi_move", false, Vec::<i32>::new()),
            ("multi_replace", false, Vec::<i32>::new()),
            ("multi_reinitialize", false, Vec::<i32>::new()),
            ("multi_reinitialize_failure", true, Vec::<i32>::new()),
            ("multi_call", false, Vec::<i32>::new()),
            ("multi_early", false, Vec::<i32>::new()),
            ("multi_loop", false, Vec::<i32>::new()),
            ("multi_nested", false, Vec::<i32>::new()),
            ("multi_partial", true, Vec::<i32>::new()),
            ("multi_partial_return", false, Vec::<i32>::new()),
            ("multi_partial_break", false, Vec::<i32>::new()),
            ("multi_partial_nested", true, Vec::<i32>::new()),
            ("multi_argument_failure", true, Vec::<i32>::new()),
            ("multi_original_failure", true, Vec::<i32>::new()),
            ("multi_references", false, Vec::<i32>::new()),
            ("multi_reuse_budget", false, Vec::<i32>::new()),
        ] {
            EVENTS.with(|events| events.borrow_mut().clear());
            let result = vm.call_int32_0(entry);
            assert_eq!(result.is_err(), failure, "{mode:?} {entry}: {result:?}");
            if !failure {
                assert_eq!(result.as_ref().unwrap(), &42, "{mode:?} {entry}");
            }
            if entry == "multi_original_failure" {
                assert!(result.unwrap_err().message().contains("Shift"));
            }
            EVENTS.with(|events| assert_eq!(*events.borrow(), expected, "{mode:?}: {entry}"));
            LIVE.with(|n| assert_eq!(n.get(), 0, "{mode:?}: {entry}"));
        }
    }
}

fn check_tracked(context: &str) {
    LIVE.with(|n| assert_eq!(n.get(), 0, "{context}"));
}

#[test]
fn multi_owner_transfers_are_atomic_at_every_interpreter_pause() {
    let mut vm = vm(VmMode::Interpreter);
    vm.set_interpreter_limits(super::InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    let mut completed = false;
    for pause in 0..4000 {
        EVENTS.with(|events| events.borrow_mut().clear());
        let mut task = vm.call_async("multi_transition_task", &[]).unwrap();
        for _ in 0..pause {
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        completed = task.is_finished();
        drop(task);
        check_tracked(&format!("pause {pause}"));
        if completed {
            break;
        }
    }
    assert!(completed);
}

#[test]
fn multi_owner_cleanup_survives_every_instruction_limit() {
    let mut vm = vm(VmMode::Interpreter);
    let mut completed = false;
    for budget in 1..4000 {
        EVENTS.with(|events| events.borrow_mut().clear());
        vm.set_interpreter_limits(super::InterpreterLimits {
            instruction_budget: Some(budget),
            ..Default::default()
        })
        .unwrap();
        match vm.call_int32_0("multi_transitions") {
            Ok(value) => {
                assert_eq!(value, 42);
                completed = true;
            }
            Err(error) => assert_eq!(error.code(), crate::ErrorCode::ResourceLimit),
        }
        check_tracked(&format!("budget {budget}"));
        if completed {
            break;
        }
    }
    assert!(completed);
}

#[test]
fn multi_owner_cleanup_keeps_code_after_reload_and_vm_drop() {
    for finish in [false, true] {
        let mut vm = vm(VmMode::Interpreter);
        vm.set_interpreter_limits(super::InterpreterLimits {
            poll_budget: Some(1),
            ..Default::default()
        })
        .unwrap();
        EVENTS.with(|events| events.borrow_mut().clear());
        let mut task = vm.call_async("multi_transition_task", &[]).unwrap();
        for _ in 0..4000 {
            if EVENTS.with(|events| events.borrow().contains(&-10)) {
                break;
            }
            assert!(!task.is_finished());
            task.resume().unwrap();
        }
        assert!(EVENTS.with(|events| events.borrow().contains(&-10)));
        vm.load_source("fn replacement() -> Int32 { 7 }").unwrap();
        drop(vm);
        if finish {
            for _ in 0..4000 {
                if task.is_finished() {
                    break;
                }
                task.resume().unwrap();
            }
            assert_eq!(task.cps_result(), Some(42));
        }
        drop(task);
        check_tracked(if finish {
            "reload finish"
        } else {
            "reload cancel"
        });
    }
}

#[test]
fn multi_owner_contracts_reject_partial_moves_and_hidden_borrows() {
    for (body, message) in [
        ("fn bad() { let b = Buffers.new::<Int32>(); let a = b.second; b.version; }", "moved"),
        ("fn bad() { var b = Buffers.new::<Int32>(); let a = b.second; b.first = Array.new::<Int32>() }", "moved"),
        ("fn bad() { let b = Buffers.new::<Int32>(); let a = b.second; let c = &b.first }", "moved"),
        ("struct Wrapper { values: Buffers<Int32> } fn bad() { let w = Wrapper { values: Buffers.new::<Int32>() }; let a = w.values.second; let b = w }", "moved"),
        ("fn bad() { let b = Buffers.new::<Int32>(); let c = b; b.version; }", "moved"),
        ("fn bad() { let b = Buffers.new::<Int32>(); let c = b; let a = &b.second }", "moved"),
        ("fn bad() { var b = Buffers.new::<Int32>(); let a = &b.second; b = Buffers.new::<Int32>(); a.len(); }", "borrow"),
        ("fn bad() { var b = Buffers.new::<Int32>(); let a = &mut b.second; b.first.push(1); a.len(); }", "borrow"),
        ("fn bad() { let b = Buffers.new::<Int32>(); let a = &b.second; let c = b; a.len(); }", "borrow"),
        ("fn bad(b: &Buffers<Int32>) -> Array<Int32> { b.second }", "borrowed wrapper"),
        ("fn bad(b: &Buffers<Int32>) { b.second = Array.new::<Int32>() }", "shared reference"),
        ("fn bad() -> &Array<Int32> { let b = Buffers.new::<Int32>(); &b.second }", "borrowed input"),
        ("fn bad() { var b = Buffers.new::<&Int32>(); let p = 42; b.add(&p) }", "local borrow"),
        ("fn bad() { var b = Buffers.new::<&Int32>(); let p = alloc(42); b.add((&*p)) ; (*(p) = 0) }", "retained"),
        ("type Refs = Buffers<&Int32> fn store(b: &mut Refs, p: &Int32) { b.second.set(0, p); } fn bad() { var b = Buffers.new::<&Int32>(); let p = 42; store(&mut b, &p) }", "local borrow"),
        ("type Refs = Buffers<&Int32> fn store(b: &mut Refs, p: &Int32) { b.second.set(0, p); } fn bad() { var b = Buffers.new::<&Int32>(); let p = alloc(42); store(&mut b, (&*p)) ; (*(p) = 0) }", "retained"),
        ("struct Invalid { a: Array<Int32>, b: Array<Int64>, p: &Int32 }", "reference-free Copy"),
        ("fn bad() { let b = Buffers.new::<Int32>(); let t = (b, 1); let again = b }", "moved"),
        ("fn bad() { let b = Buffers.new::<Int32>(); alloc(b); }", "Copy"),
        ("fn bad() { let b = Buffers.new::<Int32>() ; (*(&b)); }", "non-Copy"),
        ("extern fn host(b: &Buffers<Int32>)", "host ABI"),
    ] {
        let source = format!("{SOURCE}\n{body}").replace("extern fn record_drop(value: Int32)", "fn record_drop(value: Int32) {}");
        let ast = crate::parser::parse(&source).unwrap();
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let error = VmEngine::with_mode(mode).load_module(&ast).expect_err(body);
            assert!(error.message().contains(message), "{mode:?} {body}: {error}");
            assert!(error.span().is_some());
        }
        let error = crate::aot::AotCompiler::new("invalid-multi-owner")
            .unwrap().compile_module(&ast).expect_err(body);
        assert!(error.message().contains(message), "{body}: {error}");
    }
}
