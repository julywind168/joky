//! Sequential task scope and branch semantics: dynamic registration, body
//! continuation, join polling order, captures, abrupt exits, and cancellation
//! across the JIT, interpreter and AOT backends.
use super::support::{self, parity_vm, EVENTS, READY};
use super::{InterpreterLimits, VmMode};
use crate::runtime::{
    arena::{LIMIT, LIVE},
    scheduler::Scheduler,
    suspension::ResumeError,
};

const HOSTS: &str = "extern fn record(x: Int32) -> Int32
extern fn delayed_value(x: Int32) @suspends -> Int32
extern fn fail_now() @suspends
struct Counter { value: Int32 }
fn bump(counter: &mut Counter, id: Int32) @suspends {
    counter.value = counter.value + delayed_value(id)
    record(counter.value);
}
fn read(counter: &Counter, id: Int32) @suspends {
    record(counter.value)
    delayed_value(id);
}
fn send(text: String) @suspends { delayed_value(int32(text.byte_length())); }
fn keep(counter: &mut Counter) @suspends { delayed_value(counter.value); }
fn child_a() @suspends { delayed_value(1); record(101); }
fn child_b() @suspends { delayed_value(2); record(102); }
";

fn events(expected: &[i32]) {
    EVENTS.with(|events| assert_eq!(&*events.borrow(), expected));
}

#[test]
fn scheduling_rounds_poll_nested_bodies_and_children_once() {
    unsafe extern "C" fn pending(frame: *mut crate::runtime::cps::CpsFrame) -> i32 {
        EVENTS.with(|events| events.borrow_mut().push((&(*frame).slots)[0] as i32));
        (*frame).state = 1;
        1
    }
    for entry in [
        "join { | helper(9, 1) | helper(8, 2) }",
        "scope { branch { helper(9, 1) }; branch { helper(8, 2) } }",
        "race { | helper(9, 1) | helper(8, 2) }",
    ] {
        let source = format!(
            "extern fn pending(id: Int32) @suspends
             fn helper(body: Int32, child: Int32) @suspends {{
                 scope {{ branch {{ pending(child) }}; pending(body) }}
             }}
             fn main() @suspends {{ {entry} }}"
        );
        for (mode, slice) in [
            (VmMode::Jit, None),
            (VmMode::Interpreter, None),
            (VmMode::Interpreter, Some(1)),
        ] {
            EVENTS.with(|events| events.borrow_mut().clear());
            let mut vm = crate::vm::VmEngine::with_mode(mode);
            if mode == VmMode::Interpreter {
                vm.set_interpreter_limits(InterpreterLimits {
                    poll_budget: slice,
                    ..Default::default()
                })
                .unwrap();
            }
            unsafe {
                vm.register_suspending_fn(
                    "pending",
                    crate::FunctionSignature::new(vec![crate::Type::Int32], crate::Type::Unit),
                    pending,
                )
                .unwrap();
            }
            vm.load_source(&source).unwrap();
            let mut task = vm.call_async("main", &[]).unwrap();
            for _ in 0..500 {
                if EVENTS.with(|events| events.borrow().len()) >= 12 {
                    break;
                }
                task.resume().unwrap();
            }
            events(&[9, 8, 1, 2, 9, 8, 1, 2, 9, 8, 1, 2]);
            drop(task);
            LIVE.with(|live| assert_eq!(live.get(), 0));
        }
    }
}

/// Drive a fixture to completion with the READY-gated hosts: resumes alternate
/// between letting the body and children make progress and releasing READY
/// when a full round makes no progress. Asserts the side-effect trace, result
/// value, and that cancellation released every arena object.
fn drive(source: &str, entry: &str, expected: &[i32], result: Option<i32>, mode: VmMode) {
    READY.with(|ready| ready.set(false));
    EVENTS.with(|events| events.borrow_mut().clear());
    let vm = parity_vm(source, mode);
    let mut task = vm.call_async(entry, &[]).unwrap();
    let mut last_len = EVENTS.with(|events| events.borrow().len());
    for _ in 0..256 {
        if task.is_finished() {
            break;
        }
        match task.resume() {
            Ok(()) => {}
            Err(ResumeError::Failed(error)) => panic!("{mode:?}: {entry} failed: {error}"),
            Err(error) => panic!("{mode:?}: {entry}: {error:?}"),
        }
        let len = EVENTS.with(|events| events.borrow().len());
        if len == last_len && !task.is_finished() {
            READY.with(|ready| ready.set(true));
        }
        last_len = len;
    }
    assert!(task.is_finished(), "{mode:?}: {entry} did not finish");
    events(expected);
    if let Some(result) = result {
        assert_eq!(task.cps_result(), Some(result as u64), "{mode:?}: {entry}");
    }
    LIVE.with(|live| assert_eq!(live.get(), 0, "{mode:?}: {entry}"));
}

fn assert_rejected(source: &str, message: &str) {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let error = prepare(source, mode).unwrap_err();
        assert!(
            error.message().contains(message),
            "{mode:?}: {error} (expected {message})"
        );
    }
}

/// Build an engine with the recording host bindings and load `source`.
fn prepare(source: &str, mode: VmMode) -> Result<(), crate::Error> {
    READY.with(|ready| ready.set(false));
    EVENTS.with(|events| events.borrow_mut().clear());
    let mut vm = crate::vm::VmEngine::with_mode(mode);
    unsafe {
        vm.register_suspending_fn(
            "delayed_value",
            crate::FunctionSignature::new(vec![crate::Type::Int32], crate::Type::Int32),
            support::delayed_value,
        )
        .unwrap();
        vm.register_fn(
            "record",
            crate::FunctionSignature::new(vec![crate::Type::Int32], crate::Type::Int32),
            support::record as *const u8,
        )
        .unwrap();
        vm.register_suspending_fn(
            "fail_now",
            crate::FunctionSignature::new(vec![], crate::Type::Unit),
            support::fail_now,
        )
        .unwrap();
    }
    vm.load_source(source).map(|_| ())
}

#[test]
fn body_continues_after_registration_and_join_polls_in_registration_order() {
    let source = format!(
        "{HOSTS}
fn main() @suspends {{
    scope {{
        branch {{ child_a() }}
        branch {{ child_b() }}
        delayed_value(9);
        record(9);
    }}
    record(42);
}}
"
    );
    // The body registers both children, suspends on the host call, both
    // children start in registration order (1, 2), the body resumes and
    // records 9, then the join completes the children in registration order
    // before 42.
    drive(
        &source,
        "main",
        &[9, 1, 2, 9, 101, 102, 42],
        None,
        VmMode::Interpreter,
    );
}

#[test]
fn captures_snapshot_copy_values_and_move_owners() {
    let copy_snapshot = format!(
        "{HOSTS}
fn main() @suspends {{
    scope {{
        var count = 1
        branch {{ delayed_value(count); }}
        count = 2
        delayed_value(count);
    }}
}}
"
    );
    // The body resumes first (2), then the join runs the child, which kept
    // its registration-time snapshot (1).
    drive(&copy_snapshot, "main", &[2, 1], None, VmMode::Interpreter);

    let owner_move = format!(
        "{HOSTS}
fn main() @suspends {{
    scope {{
        let text = \"hello\"
        branch {{ send(text) }}
        delayed_value(7);
    }}
}}
"
    );
    // The owning capture moves out of the parent at registration; the child
    // observes its length (5) when the join polls it.
    drive(&owner_move, "main", &[7, 5], None, VmMode::Interpreter);
}

#[test]
fn nested_scopes_complete_descendants_before_reporting_success() {
    let source = format!(
        "{HOSTS}
fn main() @suspends {{
    scope {{
        branch {{
            scope {{
                branch {{ delayed_value(2); record(202); }}
            }}
        }}
        delayed_value(1);
    }}
    record(42);
}}
"
    );
    drive(&source, "main", &[1, 2, 202, 42], None, VmMode::Interpreter);
}

#[test]
fn abrupt_exits_join_crossed_scopes_before_leaving() {
    let return_across = format!(
        "{HOSTS}
fn early() @suspends -> Int32 {{
    scope {{
        branch {{ child_a() }}
        delayed_value(5);
        return 7
    }}
}}
fn main() @suspends -> Int32 {{ early() }}
"
    );
    // The return value is saved, the crossed scope joins (child completes),
    // and then the function returns 7.
    drive(
        &return_across,
        "main",
        &[5, 1, 101],
        Some(7),
        VmMode::Interpreter,
    );

    let break_across = format!(
        "{HOSTS}
fn main() @suspends {{
    var n = 0
    while n < 3 {{
        scope {{
            branch {{ delayed_value(n); record(100 + n); }}
            if n == 1 {{ break }}
        }}
        n = n + 1
    }}
    record(42);
}}
"
    );
    drive(
        &break_across,
        "main",
        &[0, 100, 1, 101, 42],
        None,
        VmMode::Interpreter,
    );

    let continue_across = format!(
        "{HOSTS}
fn main() @suspends {{
    var n = 0
    while n < 4 {{
        n = n + 1
        scope {{
            branch {{ delayed_value(n); record(100 + n); }}
            if n == 2 {{ continue }}
            record(200 + n);
        }}
    }}
}}
"
    );
    drive(
        &continue_across,
        "main",
        &[201, 1, 101, 2, 102, 203, 3, 103, 204, 4, 104],
        None,
        VmMode::Interpreter,
    );
}

#[test]
fn failure_cancels_unpolled_children_and_releases_captures() {
    let source = format!(
        "{HOSTS}
fn main() @suspends {{
    scope {{
        let text = \"owned\"
        branch {{ send(text) }}
        fail_now();
        record(42);
    }}
}}
"
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        LIVE.with(|live| assert_eq!(live.get(), 0));
        // The body fails before any child is polled; cancellation must still
        // release the moved capture. Eager JIT drives may surface the failure
        // from the initial call itself.
        let failure = match parity_vm(&source, mode).call_async("main", &[]) {
            Err(error) => error,
            Ok(mut task) => {
                let ResumeError::Failed(error) = task.resume().unwrap_err() else {
                    panic!("{mode:?}: expected runtime failure")
                };
                error
            }
        };
        assert_eq!(failure.code(), crate::ErrorCode::HostError, "{failure}");
        events(&[]);
        LIVE.with(|live| assert_eq!(live.get(), 0, "{mode:?}"));
    }
}

#[test]
fn branch_requires_an_enclosing_scope_in_the_same_function() {
    let bare = format!(
        "{HOSTS}
fn main() @suspends {{ branch {{ delayed_value(1); }} }}
"
    );
    assert_rejected(
        &bare,
        "branch requires an enclosing scope in the same function",
    );

    let called_helper = format!(
        "{HOSTS}
fn helper() @suspends {{ branch {{ delayed_value(1); }} }}
fn main() @suspends {{ scope {{ helper() }} }}
"
    );
    assert_rejected(
        &called_helper,
        "branch requires an enclosing scope in the same function",
    );
}

#[test]
fn scope_and_branch_require_suspending_callers() {
    let sync_scope = format!(
        "{HOSTS}
fn main() {{ scope {{ record(1); }} }}
"
    );
    assert_rejected(&sync_scope, "scope must be inside an @suspends function");

    let sync_branch = format!(
        "{HOSTS}
fn main() {{ branch {{ record(1); }} }}
"
    );
    assert_rejected(&sync_branch, "branch must be inside an @suspends function");
}

#[test]
fn parent_access_conflicting_with_child_borrows_is_rejected() {
    let exclusive_after_branch = format!(
        "{HOSTS}
fn main() @suspends {{
    scope {{
        let counter = alloc(Counter {{ value: 1 }})
        branch {{ keep(counter) }}
        bump(counter, 2);
    }}
}}
"
    );
    assert_rejected(
        &exclusive_after_branch,
        "Reference access conflicts with a borrow retained by the task arena",
    );

    let second_branch_conflict = format!(
        "{HOSTS}
fn main() @suspends {{
    scope {{
        let counter = alloc(Counter {{ value: 1 }})
        branch {{ keep(counter) }}
        branch {{ bump(counter, 2) }}
    }}
}}
"
    );
    assert_rejected(
        &second_branch_conflict,
        "Reference access conflicts with a borrow retained by the task arena",
    );
}

#[test]
fn cross_iteration_conflicts_are_rejected_across_iterations() {
    let conflicting_loop = format!(
        "{HOSTS}
fn main() @suspends {{
    scope {{
        let counter = alloc(Counter {{ value: 0 }})
        var i = 0
        while i < 4 {{
            branch {{ keep(counter) }}
            bump(counter, i);
            i = i + 1
        }}
    }}
}}
"
    );
    assert_rejected(
        &conflicting_loop,
        "Reference access conflicts with a borrow retained by the task arena",
    );
}

#[test]
fn owner_captures_move_and_reject_later_uses() {
    let moved = format!(
        "{HOSTS}
fn main() @suspends {{
    scope {{
        let text = \"hello\"
        branch {{ send(text) }}
        send(text);
    }}
}}
"
    );
    let error = prepare(&moved, VmMode::Jit).unwrap_err();
    assert_eq!(error.code(), crate::ErrorCode::InvalidAssignment, "{error}");

    let conditional = format!(
        "{HOSTS}
fn main() @suspends {{
    scope {{
        let text = \"hello\"
        if true {{
            branch {{ send(text) }}
        }} else {{
            send(text);
        }}
    }}
}}
"
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        prepare(&conditional, mode).unwrap();
    }
}

#[test]
fn branch_results_stay_unit() {
    let value_body = format!(
        "{HOSTS}
fn main() @suspends {{
    scope {{ branch {{ delayed_value(1); 42 }} }}
}}
"
    );
    let error = prepare(&value_body, VmMode::Jit).unwrap_err();
    assert!(error.message().contains("Unit"), "{error}");
}

#[test]
fn branch_bodies_cannot_jump_to_parent_loops() {
    let break_out = format!(
        "{HOSTS}
fn main() @suspends {{
    scope {{
        var i = 0
        while i < 3 {{
            branch {{
                if i == 1 {{ break }}
                delayed_value(i);
            }}
            i = i + 1
        }}
    }}
}}
"
    );
    assert_rejected(&break_out, "break must be inside a while loop");
}

#[test]
fn registration_respects_the_interpreter_frame_quota() {
    let source = format!(
        "{HOSTS}
fn main() @suspends {{
    scope {{
        branch {{ child_a() }}
        branch {{ child_b() }}
        branch {{ child_a() }}
        branch {{ child_b() }}
        branch {{ child_a() }}
        delayed_value(9);
    }}
}}
"
    );
    let mut vm = parity_vm(&source, VmMode::Interpreter);
    vm.set_interpreter_limits(InterpreterLimits {
        max_frames: 3,
        ..Default::default()
    })
    .unwrap();
    let failure = match vm.call_async("main", &[]) {
        Err(error) => error,
        Ok(mut task) => {
            let ResumeError::Failed(error) = task.resume().unwrap_err() else {
                panic!("expected the frame quota to fail registration")
            };
            error
        }
    };
    assert_eq!(failure.code(), crate::ErrorCode::ResourceLimit, "{failure}");
    LIVE.with(|live| assert_eq!(live.get(), 0));
}

#[test]
fn long_registration_loops_recycle_frames_and_return_to_baseline() {
    let source = format!(
        "{HOSTS}
fn main() @suspends {{
    var i = 0
    while i < 4000 {{
        scope {{
            branch {{ delayed_value(i); }}
            delayed_value(0);
        }}
        i = i + 1
    }}
}}
"
    );
    // Each iteration registers, suspends, and joins one child under a tight
    // frame budget, proving completed children recycle their frames; a second
    // identical run proves no metadata or slot growth accumulates.
    for _ in 0..2 {
        EVENTS.with(|events| events.borrow_mut().clear());
        let mut vm = parity_vm(&source, VmMode::Interpreter);
        READY.with(|ready| ready.set(true));
        vm.set_interpreter_limits(InterpreterLimits {
            max_frames: 6,
            ..Default::default()
        })
        .unwrap();
        let mut task = vm.call_async("main", &[]).unwrap();
        for _ in 0..32_000 {
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        assert!(task.is_finished());
        assert_eq!(EVENTS.with(|events| events.borrow().len()), 8000);
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

#[test]
fn deep_dynamic_nesting_completes_and_cancels_iteratively() {
    let source = format!(
        "{HOSTS}
fn nest(n: Int32) @suspends {{
    if n > 0 {{
        scope {{
            branch {{ nest(n - 1) }}
            delayed_value(n);
        }}
    }}
}}
fn main() @suspends {{ nest(150) }}
"
    );
    // Complete the whole chain with READY released.
    EVENTS.with(|events| events.borrow_mut().clear());
    let vm = parity_vm(&source, VmMode::Interpreter);
    READY.with(|ready| ready.set(true));
    let mut task = vm.call_async("main", &[]).unwrap();
    for _ in 0..2048 {
        if task.is_finished() {
            break;
        }
        task.resume().unwrap();
    }
    assert!(task.is_finished());
    assert_eq!(EVENTS.with(|events| events.borrow().len()), 150);
    LIVE.with(|live| assert_eq!(live.get(), 0));

    // Cancel while every level is suspended: the iterative walk must release
    // the whole tree without unbounded host recursion.
    READY.with(|ready| ready.set(false));
    EVENTS.with(|events| events.borrow_mut().clear());
    let vm = parity_vm(&source, VmMode::Interpreter);
    let mut task = vm.call_async("main", &[]).unwrap();
    task.resume().unwrap();
    assert!(!task.is_finished());
    drop(task);
    LIVE.with(|live| assert_eq!(live.get(), 0));
}

#[test]
fn scope_cancellation_reload_and_vm_drop_keep_tasks_independent() {
    let source = format!(
        "{HOSTS}
fn allocations() @suspends {{
    scope {{
        let counter = alloc(Counter {{ value: 1 }})
        branch {{ bump(counter, 2) }}
        delayed_value(3);
    }}
}}
"
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm(&source, mode);
        let mut scheduler = Scheduler::new();
        let cancelled = scheduler.suspend(vm.call_async("allocations", &[]).unwrap());
        // Registration, capture transfer and the first polling round all ran;
        // every level is suspended on its host call now.
        EVENTS.with(|events| assert_eq!(events.borrow().len(), 2));
        vm.load_source("fn replacement() {}").unwrap();
        drop(vm);
        scheduler.cancel(cancelled).unwrap();
        assert!(scheduler.cancel(cancelled).is_err());
        LIVE.with(|live| assert_eq!(live.get(), 0, "{mode:?}"));
    }
}

#[test]
fn child_allocation_failure_propagates_through_the_join_and_cleans_up() {
    struct Reset(usize);
    impl Drop for Reset {
        fn drop(&mut self) {
            LIMIT.with(|limit| limit.set(self.0));
        }
    }
    let _reset = Reset(LIMIT.with(|limit| limit.get()));
    let source = format!(
        "{HOSTS}
fn grow(counter: &mut Counter) @suspends {{
    delayed_value(1)
    let extra = alloc(Counter {{ value: 5 }})
    counter.value = extra.value
}}
fn main() @suspends {{
    scope {{
        let counter = alloc(Counter {{ value: 0 }})
        branch {{ grow(counter) }}
        delayed_value(9);
    }}
}}
"
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        LIMIT.with(|limit| limit.set(usize::MAX));
        READY.with(|ready| ready.set(false));
        EVENTS.with(|events| events.borrow_mut().clear());
        let vm = parity_vm(&source, mode);
        let mut task = vm.call_async("main", &[]).unwrap();
        // The parent allocation and registration run first; both levels then
        // suspend on their host calls.
        for _ in 0..8 {
            if EVENTS.with(|events| events.borrow().len()) >= 2 {
                break;
            }
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        eprintln!(
            "{mode:?} events after start: {:?}",
            EVENTS.with(|events| events.borrow().clone())
        );
        EVENTS.with(|events| assert_eq!(&*events.borrow(), &[9, 1]));
        // Exhaust the task arena before the child allocates its own object.
        LIMIT.with(|limit| limit.set(0));
        READY.with(|ready| ready.set(true));
        let failure = loop {
            match task.resume() {
                Ok(()) if task.is_finished() => panic!("{mode:?}: expected allocation failure"),
                Ok(()) => continue,
                Err(ResumeError::Failed(error)) => break error,
                Err(error) => panic!("{mode:?}: {error:?}"),
            }
        };
        assert_eq!(failure.code(), crate::ErrorCode::ResourceLimit, "{failure}");
        drop(task);
        LIVE.with(|live| assert_eq!(live.get(), 0, "{mode:?}"));
    }
}

#[test]
#[ignore = "exhaustive: cancel at every instruction; run by the Debug stress check"]
fn exhaustive_task_scope_cancels_at_every_instruction() {
    let source = format!(
        "{HOSTS}
fn main() @suspends -> Int32 {{
    scope {{
        branch {{ child_a() }}
        branch {{ child_b() }}
        delayed_value(9);
    }}
    record(42)
}}
"
    );
    let mut vm = parity_vm(&source, VmMode::Interpreter);
    super::support::assert_cancellation(
        &mut vm,
        "main",
        6000,
        super::support::CancellationSweep::EveryInstruction,
        || {
            READY.with(|ready| ready.set(true));
            EVENTS.with(|events| events.borrow_mut().clear());
        },
        || EVENTS.with(|events| events.borrow().len()),
        || LIVE.with(|live| assert_eq!(live.get(), 0)),
    );
}
