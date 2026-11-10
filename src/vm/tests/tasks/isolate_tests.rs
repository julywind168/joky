use super::*;
use crate::runtime::arena::LIVE;

const SOURCE: &str = include_str!("../../../../tests/fixtures/isolate.jk");

#[test]
fn isolate_values_failures_and_cancellation() {
    for (mode, slice) in [
        (VmMode::Jit, None),
        (VmMode::Interpreter, None),
        (VmMode::Interpreter, Some(1)),
    ] {
        let mut vm = VmEngine::with_mode(mode);
        if mode == VmMode::Interpreter {
            vm.set_interpreter_limits(InterpreterLimits {
                poll_budget: slice,
                ..Default::default()
            })
            .unwrap();
        }
        vm.load_source(SOURCE).unwrap();
        let mut task = vm.call_async("main", &[]).unwrap();
        for _ in 0..50000 {
            if task.is_finished() {
                break;
            }
            task.resume()
                .unwrap_or_else(|error| panic!("{mode:?}/{slice:?}: {error:?}"));
        }
        assert!(task.is_finished(), "{mode:?}/{slice:?}");
        drop(task);
        LIVE.with(|live| assert_eq!(live.get(), 0, "{mode:?}/{slice:?}"));
    }
}

#[test]
fn isolate_rejects_escaping_control_flow_and_borrows() {
    for (source, message) in [
        ("fn main() { isolate { 1 }; }", "@suspends"),
        (
            "fn main() @suspends { isolate { return }; }",
            "cannot leave isolate",
        ),
        (
            "fn main() @suspends { while true { isolate { break }; } }",
            "cannot leave isolate",
        ),
        (
            "fn main() @suspends { while true { isolate { continue }; } }",
            "cannot leave isolate",
        ),
        (
            "fn main() @suspends { let x=alloc(1); isolate {x}; }",
            "reference-free",
        ),
        (
            "fn main() @suspends { let x=\"moved\"; isolate {x}; x; }",
            "moved",
        ),
    ] {
        let source = format!("import joky/task\n{source}");
        let error = support::frontend_error(&source, &crate::ModuleSources::new());
        assert!(error.message().contains(message), "{source}: {error}");
    }
}

thread_local! { static DROPS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) }; }
extern "C" fn record(id: i32) {
    DROPS.with(|drops| drops.borrow_mut().push(id));
}
const CLEANUP: &str = r#"
import joky/task
import joky/check
extern fn record(id:Int32)
struct Owner {id:Int32,text:String}

fn failing() @suspends {
    let owner=Owner{id:1,text:"failed child"}
    task.yield_now()
    check.panic("child failed")
}
fn survivor() @suspends {
    let owner=Owner{id:2,text:"cancelled sibling"}
    task.sleep(100s)
}
fn work() @suspends ->Int32 {
    let captured=Owner{id:3,text:"captured"}
    let result:Result<Unit,task.Failure>=isolate {
        let local=captured
        scope {branch {survivor()}; branch {failing()}}
    }
    check.assert(result.is_err(),"caught")
    record(4)
    42
}
"#;
fn cleanup_vm(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    unsafe {
        vm.register_fn(
            "record",
            crate::FunctionSignature::new(vec![crate::Type::Int32], crate::Type::Unit),
            record as *const u8,
        )
        .unwrap();
    }
    vm.load_source(CLEANUP).unwrap();
    vm
}
#[test]
fn isolate_drains_owned_captures_and_siblings_before_handler() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        DROPS.with(|drops| drops.borrow_mut().clear());
        let vm = cleanup_vm(mode);
        let mut task = vm.call_async("work", &[]).unwrap();
        for _ in 0..1000 {
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        assert_eq!(task.cps_result(), Some(42));
        drop(task);
        DROPS.with(|drops| {
            let events = drops.borrow();
            assert_eq!(events.last(), Some(&4), "{mode:?}: {events:?}");
            assert_eq!(*events, [4]);
        });
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}
fn cancellation(sweep: support::CancellationSweep) {
    let mut vm = cleanup_vm(VmMode::Interpreter);
    support::assert_cancellation(
        &mut vm,
        "work",
        10000,
        sweep,
        || DROPS.with(|drops| drops.borrow_mut().clear()),
        || DROPS.with(|drops| drops.borrow().clone()),
        || {
            LIVE.with(|live| assert_eq!(live.get(), 0));
            DROPS.with(|drops| {
                let mut events = drops.borrow().clone();
                events.sort();
                let n = events.len();
                events.dedup();
                assert_eq!(n, events.len(), "double cleanup");
            });
        },
    );
}
#[test]
fn isolate_cancellation_at_resource_boundaries() {
    cancellation(support::CancellationSweep::ResourceBoundaries)
}
#[test]
#[ignore = "exhaustive: cancel isolated subtrees at every interpreted instruction"]
fn exhaustive_isolate_cancellation() {
    cancellation(support::CancellationSweep::EveryInstruction)
}

#[test]
fn isolate_failure_value_needs_no_arena_allocation_and_does_not_catch_cancel() {
    use crate::runtime::arena::LIMIT;
    extern "C" fn limit() {
        LIMIT.with(|limit| limit.set(0));
    }
    extern "C" fn restore() {
        LIMIT.with(|limit| limit.set(usize::MAX));
    }
    extern "C" fn cancel() {
        crate::runtime::error::record_error(crate::Error::new_with_code(
            crate::ErrorPhase::Runtime,
            crate::ErrorCode::Cancelled,
            "external cancellation",
        ));
    }
    extern "C" fn abi() {
        crate::runtime::error::record_error(crate::Error::new_with_code(
            crate::ErrorPhase::Abi,
            crate::ErrorCode::AbiError,
            "invalid host contract",
        ));
    }
    let source = r#"import joky/task
        extern fn limit() extern fn restore() extern fn cancel() extern fn abi()
        fn budget() @suspends ->Int32 {
            let outcome:Result<String,task.Failure>=isolate {limit(); "cannot allocate"}
            restore()
            match outcome {Err(ResourceLimit)=>42,_=>0}
        }
        fn cancelled() @suspends {let caught=isolate {cancel()}; restore()}
        fn invalid() @suspends {let caught=isolate {abi()}; restore()}
    "#;
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            restore();
        }
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let _reset = Reset;
        let mut vm = VmEngine::with_mode(mode);
        for (name, entry) in [
            ("limit", limit as *const u8),
            ("restore", restore as *const u8),
            ("cancel", cancel as *const u8),
            ("abi", abi as *const u8),
        ] {
            unsafe {
                vm.register_fn(
                    name,
                    crate::FunctionSignature::new(vec![], crate::Type::Unit),
                    entry,
                )
                .unwrap();
            }
        }
        vm.load_source(source).unwrap();
        let mut task = vm.call_async("budget", &[]).unwrap();
        while !task.is_finished() {
            task.resume().unwrap();
        }
        assert_eq!(task.cps_result(), Some(42), "{mode:?}");
        drop(task);
        for (entry, code) in [
            ("cancelled", crate::ErrorCode::Cancelled),
            ("invalid", crate::ErrorCode::AbiError),
        ] {
            let error = match vm.call_async(entry, &[]) {
                Err(error) => error,
                Ok(mut task) => loop {
                    match task.resume() {
                        Err(crate::runtime::suspension::ResumeError::Failed(error)) => break error,
                        Ok(()) => assert!(!task.is_finished(), "failure swallowed"),
                        Err(error) => panic!("{error:?}"),
                    }
                },
            };
            assert_eq!(error.code(), code, "{mode:?}");
        }
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

#[test]
fn reservation_sample_reclaims_cancelled_holds_and_rejects_late_replies() {
    let source = format!(
        "{}\nfn main() @suspends {{demo({{}})}}",
        include_str!("../../../../examples/reservation_logic.jk")
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(&source).unwrap();
        crate::runtime::driver::block_on(vm.prepare_async("main", &[]).unwrap())
            .unwrap_or_else(|error| panic!("{mode:?}: {error}"));
        drop(vm);
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

#[test]
fn isolate_catches_legacy_host_status_and_handler_failure_propagates() {
    unsafe extern "C" fn failed(_: *mut crate::runtime::cps::CpsFrame) -> i32 {
        7
    }
    let source = r#"import joky/task import joky/check
        extern fn failed() @suspends
        fn run() @suspends ->Int32 {
            let first=isolate {failed()}
            let second=isolate {scope {branch {failed()}}}
            match (first,second) {(Err(Host),Err(Host))=>42,_=>0}
        }
        fn report() @suspends {
            let first=isolate {failed()}
            if first.is_err() {check.panic("handler failed")}
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_suspending_fn(
                "failed",
                crate::FunctionSignature::new(vec![], crate::Type::Unit),
                failed,
            )
            .unwrap();
        }
        vm.load_source(source).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            crate::runtime::driver::block_on(vm.prepare_async("run", &[]).unwrap())
                .unwrap_or_else(|e| panic!("{mode:?}: {e}")),
            42
        );
        let error =
            crate::runtime::driver::block_on(vm.prepare_async("report", &[]).unwrap()).unwrap_err();
        assert!(
            error.message().contains("handler failed"),
            "{mode:?}: {error}"
        );
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}
