//! Run with `cargo run --example host_tasks`. No game engine or wall-clock sleep.
use joky::{
    runtime::{error, executor::Clock},
    vm::{call_current_instance, InstanceValue as V, VmEngine},
    FunctionSignature, Type,
};
use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

struct GameClock(Cell<Instant>);
impl Clock for GameClock {
    fn now(&self) -> Instant {
        self.0.get()
    }
}

unsafe extern "C" fn increment() -> i32 {
    match call_current_instance("increment_state", &[]) {
        Ok(V::Int32(value)) => value,
        Ok(_) => unreachable!("checked method returns Int32"),
        Err(failure) => {
            error::record_error(failure);
            0
        }
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let clock = Rc::new(GameClock(Cell::new(Instant::now())));
    let mut vm = VmEngine::new_interpreter();
    vm.configure_tasks(clock.clone(), 32)?;
    // Exact synchronous scalar ABI. The callback retains no receiver or pointer.
    unsafe {
        vm.register_fn(
            "increment",
            FunctionSignature::new(vec![], Type::Int32),
            increment as *const u8,
        )?;
    }
    vm.load_source(
        r#"
        import joky/task
        struct State { count: Int32 }
        fn create() -> State { State { count: 0 } }
        fn increment_state(state: &mut State) @noescape -> Int32 {
            state.count = state.count + 1; state.count
        }
        fn read(state: &State) @noescape -> Int32 { state.count }
        extern fn increment() -> Int32
        fn work() @suspends -> Int32 { increment(); task.sleep(16ms); increment() }
    "#,
    )?;
    let owner = vm.create_instance("create", &[])?;
    vm.spawn_instance_task(owner, "work", &[])?;
    assert_eq!(vm.poll_tasks(8)?.polled, 1);
    assert_eq!(vm.call_instance(owner, "read", &[])?, V::Int32(1));
    assert_eq!(vm.poll_tasks(8)?.polled, 0); // paused game clock: no timer polling
    clock.0.set(clock.now() + Duration::from_millis(16));
    let report = vm.poll_tasks(8)?;
    assert_eq!(report.completed[0].result, Ok(2));
    vm.close_instance(owner)?;
    println!("Instance task completed: 2");
    Ok(())
}
