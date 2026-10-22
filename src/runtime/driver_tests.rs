use super::*;
use crate::{
    runtime::{
        arena::LIVE,
        borrowed_host::{BorrowingHostV1, STOP_CANCELLED},
        cps::{self, DONE, FAILED},
    },
    vm::{InterpreterLimits, VmEngine, VmMode},
    ErrorCode, FunctionSignature, Type,
};
use std::{cell::Cell, time::Duration};

thread_local! {
    static POLLS: Cell<usize> = const { Cell::new(0) };
    static STOPS: Cell<usize> = const { Cell::new(0) };
    static READY: Cell<bool> = const { Cell::new(false) };
}

unsafe extern "C" fn external_wait(frame: *mut CpsFrame) -> i32 {
    POLLS.with(|count| count.set(count.get() + 1));
    if READY.with(Cell::get) {
        (*frame).state = DONE;
        0
    } else {
        1
    }
}

unsafe extern "C" fn stop_borrow(frame: *mut CpsFrame, reason: u32) {
    assert_eq!(reason, STOP_CANCELLED);
    assert_eq!(*((&(*frame).slots)[0] as *const i32), 42);
    STOPS.with(|count| count.set(count.get() + 1));
}

#[test]
fn blocking_driver_handles_completed_tasks_and_interpreter_slices() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        if mode == VmMode::Interpreter {
            vm.set_interpreter_limits(InterpreterLimits {
                poll_budget: Some(1),
                instruction_budget: Some(20000),
                ..Default::default()
            })
            .unwrap();
        }
        vm.load_source(
            r#"
            import joky/task
            fn immediate() @suspends -> Int32 { 42 }
            fn unit() @suspends {}
            fn work() @suspends -> Int32 {
                var i = 0
                while i < 100 { i = i + 1 }
                task.yield_now()
                task.sleep_ms(1)
                i
            }
        "#,
        )
        .unwrap();
        assert_eq!(
            block_on(vm.call_async("immediate", &[]).unwrap()).unwrap(),
            42
        );
        assert_eq!(block_on(vm.call_async("unit", &[]).unwrap()).unwrap(), 0);
        assert_eq!(block_on(vm.call_async("work", &[]).unwrap()).unwrap(), 100);
    }
}

#[test]
fn blocking_driver_does_not_poll_unknown_waits_but_manual_resume_still_works() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        POLLS.with(|count| count.set(0));
        READY.with(|ready| ready.set(false));
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_suspending_fn(
                "wait",
                FunctionSignature::new(vec![], Type::Unit),
                external_wait,
            )
            .unwrap();
        }
        vm.load_source("extern fn wait() @suspends fn main() @suspends { wait() }")
            .unwrap();
        let error = block_on(vm.call_async("main", &[]).unwrap()).unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidState);
        assert!(error.message().contains("without a supported wake source"));
        assert_eq!(POLLS.with(Cell::get), 1);
        let mut task = vm.call_async("main", &[]).unwrap();
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        assert_eq!(task.cps_result(), Some(0));
        assert_eq!(POLLS.with(Cell::get), 3);
    }
}

#[test]
fn driver_failure_stops_borrowing_hosts_before_releasing_arena() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for body in [
            "wait(value)",
            "race { | task.yield_now() | task.sleep_ms(60000) }; wait(value)",
            "parallel { | wait(value) | task.sleep_ms(1) }",
            "parallel { | wait(value) | fail() }",
        ] {
            STOPS.with(|count| count.set(0));
            READY.with(|ready| ready.set(false));
            let mut vm = VmEngine::with_mode(mode);
            unsafe {
                vm.register_borrowing_suspending_fn(
                    "wait",
                    FunctionSignature::new(vec![Type::reference(Type::Int32, true)], Type::Unit),
                    BorrowingHostV1 {
                        abi_version: 1,
                        poll: Some(external_wait),
                        stop: Some(stop_borrow),
                    },
                    None,
                )
                .unwrap();
            }
            vm.load_source(&format!(
                r#"
                import joky/task
                extern fn wait(value: &mut Int32) @suspends
                fn fail() @suspends {{ task.yield_now(); let ignored = 1 / 0 }}
                fn main() @suspends {{ let value = alloc(42); {body} }}
            "#
            ))
            .unwrap();
            let error = block_on(vm.call_async("main", &[]).unwrap()).unwrap_err();
            assert!(error.message().contains(if body.contains("fail()") {
                "Division by zero"
            } else {
                "without a supported wake source"
            }));
            assert_eq!(STOPS.with(Cell::get), 1);
            LIVE.with(|live| assert_eq!(live.get(), 0));
        }
    }
}

#[test]
fn driver_cancels_unknown_race_loser_after_supported_wake() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        READY.with(|ready| ready.set(false));
        STOPS.with(|count| count.set(0));
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_borrowing_suspending_fn(
                "wait",
                FunctionSignature::new(vec![Type::reference(Type::Int32, true)], Type::Unit),
                BorrowingHostV1 {
                    abi_version: 1,
                    poll: Some(external_wait),
                    stop: Some(stop_borrow),
                },
                None,
            )
            .unwrap();
        }
        vm.load_source(
            r#"
            import joky/task
            extern fn wait(value: &mut Int32) @suspends
            fn main() @suspends -> Int32 {
                let value = alloc(42)
                race { | wait(value) | task.sleep_ms(1) }
                42
            }
        "#,
        )
        .unwrap();
        assert_eq!(block_on(vm.call_async("main", &[]).unwrap()).unwrap(), 42);
        assert_eq!(STOPS.with(Cell::get), 1);
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

#[test]
fn timer_deadline_survives_early_manual_polls_and_waits_until_ready() {
    let mut frame = CpsFrame::new(1);
    frame.slots[0] = 10000;
    unsafe {
        assert_eq!(crate::stdlib::task::sleep_ms(&mut frame), 1);
        let Some(Wake::Deadline(deadline)) = frame.wake else {
            panic!("missing timer");
        };
        assert_eq!(crate::stdlib::task::sleep_ms(&mut frame), 1);
        assert!(matches!(frame.wake, Some(Wake::Deadline(value)) if value == deadline));
        let deadline = Instant::now() + Duration::from_millis(2);
        frame.wake = Some(Wake::Deadline(deadline));
        frame.wait_plan().wait().unwrap();
        assert!(Instant::now() >= deadline);
        assert_eq!(crate::stdlib::task::sleep_ms(&mut frame), 0);
        assert_eq!(frame.state, DONE);
        assert!(frame.wake.is_none());
    }
}

#[test]
fn frame_wait_plan_ignores_terminal_and_cancelled_children() {
    let mut root = CpsFrame::new(0);
    let early = Instant::now() + Duration::from_secs(1);
    let late = early + Duration::from_secs(1);
    unsafe {
        let first = cps::child_at(&mut root, 0, 0);
        (*first).wake = Some(Wake::Deadline(late));
        let second = cps::child_at(&mut root, 1, 0);
        (*second).wake = Some(Wake::Deadline(early));
        assert!(matches!(root.wait_plan().0, Some(Wake::Deadline(value)) if value == early));
        (*first).wake = Some(Wake::Immediate);
        assert!(matches!(root.wait_plan().0, Some(Wake::Immediate)));
        (*first).state = DONE;
        assert!(matches!(root.wait_plan().0, Some(Wake::Deadline(value)) if value == early));
        cps::release_children(&mut root);
    }
    assert!(root.wait_plan().0.is_none());
}

#[test]
fn native_driver_checks_completion_errors_and_releases_every_terminal_path() {
    unsafe extern "C" fn malformed(frame: *mut CpsFrame) -> i32 {
        (*frame).result = 42;
        0
    }
    unsafe extern "C" fn failure(frame: *mut CpsFrame) -> i32 {
        (*frame).state = DONE;
        7
    }
    unsafe extern "C" fn pending_done(frame: *mut CpsFrame) -> i32 {
        (*frame).state = DONE;
        1
    }
    unsafe extern "C" fn detailed(frame: *mut CpsFrame) -> i32 {
        error::division_by_zero(4, 9);
        (*frame).state = DONE;
        0
    }
    READY.with(|ready| ready.set(false));
    for (entry, status, message) in [
        (malformed as CpsEntry, RUNTIME_ERROR, "did not set DONE"),
        (failure as CpsEntry, 7, "failure status 7"),
        (pending_done as CpsEntry, RUNTIME_ERROR, "terminal state"),
        (
            external_wait as CpsEntry,
            RUNTIME_ERROR,
            "without a supported wake source",
        ),
        (detailed as CpsEntry, RUNTIME_ERROR, "Division by zero"),
    ] {
        let mut frame = CpsFrame::new(0);
        let mut value: i32 = 42;
        STOPS.with(|count| count.set(0));
        unsafe {
            let child = cps::child(&mut frame, 1);
            (&mut (*child).slots)[0] = (&mut value as *mut i32) as u64;
            (*child).host_stop = Some(stop_borrow);
        }
        let error = error::checked(|| unsafe {
            assert_eq!(joky_cps_block_on(entry, &mut frame), status);
        })
        .unwrap_err();
        assert!(error.message().contains(message), "{error}");
        assert_eq!(frame.state, FAILED);
        assert!(frame.wait_plan().0.is_none());
        assert_eq!(STOPS.with(Cell::get), 1);
        drop(frame);
        assert_eq!(STOPS.with(Cell::get), 1);
    }
    let mut frame = CpsFrame::new(0);
    assert_eq!(
        unsafe { joky_cps_block_on(crate::stdlib::task::yield_now, &mut frame) },
        0
    );
    assert_eq!(frame.state, DONE);
    assert_eq!(frame.result, 0);
}

#[test]
fn timer_range_checks_do_not_panic_or_require_a_driver() {
    let mut frame = CpsFrame::new(1);
    frame.slots[0] = u64::MAX;
    match error::checked(|| unsafe { crate::stdlib::task::sleep_ms(&mut frame) }) {
        Ok(status) => {
            // Some operating systems represent this entire UInt64 duration.
            assert_eq!(status, 1);
            assert!(matches!(frame.wake, Some(Wake::Deadline(_))));
        }
        Err(error) => {
            assert!(error.message().contains("monotonic clock range"));
            assert!(frame.wake.is_none());
        }
    }
    drop(frame);
    let mut zero = CpsFrame::new(1);
    assert_eq!(
        unsafe { joky_cps_block_on(crate::stdlib::task::sleep_ms, &mut zero) },
        0
    );
    assert_eq!(zero.state, DONE);
}

#[test]
fn pending_builtin_task_keeps_code_alive_after_vm_drop() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source("import joky/task fn main() @suspends -> Int32 { task.yield_now(); 42 }")
            .unwrap();
        let task = vm.call_async("main", &[]).unwrap();
        assert!(!task.is_finished());
        drop(vm);
        assert_eq!(block_on(task).unwrap(), 42);
    }
}

struct WakeOperation {
    ready: std::sync::Arc<std::sync::atomic::AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
    control: Option<std::sync::mpsc::Sender<bool>>,
}
thread_local! {
    static WAKE_MODE: Cell<u8> = const {Cell::new(0)};
    static HOOK_ARMED: Cell<bool> = const {Cell::new(false)};
    static HOOK_CHANNEL: std::cell::RefCell<Option<(std::sync::mpsc::Sender<bool>,std::sync::mpsc::Receiver<()>)>> = const {std::cell::RefCell::new(None)};
    static WAKE_POLLS: Cell<usize> = const {Cell::new(0)};
    static WAKE_STOPS: Cell<usize> = const {Cell::new(0)};
}
unsafe extern "C" fn wake_poll(frame: *mut CpsFrame) -> i32 {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    WAKE_POLLS.with(|n| n.set(n.get() + 1));
    if (*frame).host_context.is_null() {
        let waker = crate::runtime::executor::current_waker().unwrap();
        let ready = Arc::new(AtomicBool::new(false));
        let mut operation = WakeOperation {
            ready: ready.clone(),
            worker: None,
            control: None,
        };
        match WAKE_MODE.with(Cell::get) {
            0 => {
                ready.store(true, Ordering::Release);
                waker.wake();
            }
            1 => {
                operation.worker = Some(std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(2));
                    ready.store(true, Ordering::Release);
                    waker.wake();
                }));
            }
            2 => {
                let (tx, rx) = std::sync::mpsc::channel();
                let (ack_tx, ack_rx) = std::sync::mpsc::channel();
                operation.control = Some(tx.clone());
                HOOK_CHANNEL.with(|c| *c.borrow_mut() = Some((tx, ack_rx)));
                HOOK_ARMED.with(|a| a.set(true));
                operation.worker = Some(std::thread::spawn(move || {
                    if rx.recv().unwrap_or(false) {
                        ready.store(true, Ordering::Release);
                        waker.wake();
                    }
                    let _ = ack_tx.send(());
                }));
            }
            _ => unreachable!(),
        }
        (*frame).host_context = Box::into_raw(Box::new(operation)).cast();
        return 1;
    }
    if (*(*frame).host_context.cast::<WakeOperation>())
        .ready
        .load(Ordering::Acquire)
    {
        (*frame).state = DONE;
        0
    } else {
        1
    }
}
unsafe extern "C" fn wake_stop(frame: *mut CpsFrame, _: u32) {
    assert_eq!(*((&(*frame).slots)[0] as *const i32), 42);
    let mut operation = Box::from_raw((*frame).host_context.cast::<WakeOperation>());
    if let Some(control) = operation.control.take() {
        let _ = control.send(false);
    }
    if let Some(worker) = operation.worker.take() {
        worker.join().unwrap();
    }
    HOOK_ARMED.with(|a| a.set(false));
    HOOK_CHANNEL.with(|c| c.borrow_mut().take());
    (*frame).host_context = std::ptr::null_mut();
    WAKE_STOPS.with(|n| n.set(n.get() + 1));
}
const WAKE_HOST: BorrowingHostV1 = BorrowingHostV1 {
    abi_version: 1,
    poll: Some(wake_poll),
    stop: Some(wake_stop),
};
fn wake_vm(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    unsafe {
        vm.register_borrowing_suspending_fn(
            "wait",
            FunctionSignature::new(vec![Type::reference(Type::Int32, true)], Type::Unit),
            WAKE_HOST,
            None,
        )
        .unwrap();
    }
    vm.load_source("import joky/task extern fn wait(value:&mut Int32) @suspends fn main() @suspends -> Int32 {let value=alloc(42); wait(value); 42} fn timer_race() @suspends -> Int32 {let value=alloc(42); race { | wait(value) | task.sleep_ms(10000) }; 42}").unwrap();
    vm
}
fn reset_wake(mode: u8) {
    WAKE_MODE.with(|m| m.set(mode));
    WAKE_POLLS.with(|n| n.set(0));
    WAKE_STOPS.with(|n| n.set(0));
}
#[test]
fn blocking_driver_waits_for_external_wakes_before_and_after_sleep() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = wake_vm(mode);
        for (entry, notify) in [
            ("main", 0),
            ("main", 1),
            ("timer_race", 0),
            ("timer_race", 1),
        ] {
            reset_wake(notify);
            assert_eq!(block_on(vm.prepare_async(entry, &[]).unwrap()).unwrap(), 42);
            assert_eq!(WAKE_POLLS.with(Cell::get), 2);
            assert_eq!(WAKE_STOPS.with(Cell::get), 1);
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}
struct RaceClock;
impl crate::runtime::executor::Clock for RaceClock {
    fn now(&self) -> Instant {
        if HOOK_ARMED.with(|a| a.replace(false)) {
            let (signal, ack) = HOOK_CHANNEL.with(|c| c.borrow_mut().take().unwrap());
            signal.send(true).unwrap();
            ack.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        Instant::now()
    }
    fn allows_blocking_wait(&self) -> bool {
        true
    }
}
#[test]
fn blocking_driver_does_not_lose_a_wake_between_queue_check_and_kernel_wait() {
    use crate::runtime::executor::{HostExecutor, TaskGroup};
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        reset_wake(2);
        let vm = wake_vm(mode);
        let executor = HostExecutor::new(std::rc::Rc::new(RaceClock), 8);
        let group = TaskGroup::new();
        executor
            .spawn(&group, vm.prepare_async("main", &[]).unwrap())
            .unwrap();
        assert_eq!(executor.poll(1).unwrap().polled, 1);
        executor.wait().unwrap();
        assert_eq!(executor.poll(1).unwrap().completed[0].result, Ok(42));
        assert_eq!(WAKE_POLLS.with(Cell::get), 2);
        assert_eq!(WAKE_STOPS.with(Cell::get), 1);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}
#[test]
fn native_blocking_driver_captures_external_wakers_on_the_first_poll() {
    unsafe extern "C" fn root(frame: *mut CpsFrame) -> i32 {
        crate::runtime::borrowed_host::joky_cps_poll_borrowing_v1(frame, &WAKE_HOST)
    }
    for notify in [0, 1] {
        reset_wake(notify);
        let mut marker = 42i32;
        let mut frame = CpsFrame::new(1);
        frame.slots[0] = (&mut marker as *mut i32) as usize as u64;
        assert_eq!(unsafe { joky_cps_block_on(root, &mut frame) }, 0);
        assert_eq!(frame.state, DONE);
        assert_eq!(WAKE_POLLS.with(Cell::get), 2);
        assert_eq!(WAKE_STOPS.with(Cell::get), 1);
    }
}
