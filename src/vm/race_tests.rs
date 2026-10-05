//! Structured first-completion semantics and cancellation across backends.
use super::{support::*, InterpreterLimits, VmEngine, VmMode};
use crate::runtime::{
    arena::LIVE,
    scheduler::Scheduler,
    suspension::{ContinuationStatus, ResumeError},
};

const SOURCE: &str = include_str!("../../tests/fixtures/race.jk");

fn events(expected: &[i32]) {
    EVENTS.with(|events| assert_eq!(&*events.borrow(), expected));
}

fn live(expected: usize) {
    LIVE.with(|count| assert_eq!(count.get(), expected));
}

#[test]
fn race_first_completion_cancels_losers_without_replaying_arguments() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for (name, expected) in [
            ("race_first", vec![1, 2, 101]),
            ("race_later", vec![1, 2, 3, 11, 102, 9]),
            ("race_nested", vec![1, 2, 3, 4, 5, 11, 12, 13, 14, 105, 9]),
            ("race_inside_scope", vec![1, 2, 6, 101, 700, 16, 206]),
            ("race_loop", vec![1, 2, 101, 1, 2, 101, 1, 2, 101]),
            ("race_arguments", vec![1, 2, 2, 1, 3, 101]),
            ("race_single", vec![3, 103]),
            ("race_generic", vec![0, 1, 2]),
            ("race_reference", vec![1, 2, 11, 102, 9]),
            ("race_allocations", vec![1, 2, 11, 102, 9]),
        ] {
            let vm = parity_vm(SOURCE, mode);
            let mut task = vm.call_async(name, &[]).unwrap();
            let before = EVENTS.with(|events| events.borrow().clone());
            for _ in 0..4 {
                assert_eq!(
                    task.status(),
                    ContinuationStatus::Pending,
                    "{mode:?}: {name}"
                );
                task.resume().unwrap();
                events(&before);
            }
            READY.with(|ready| ready.set(true));
            for _ in 0..16 {
                if task.is_finished() {
                    break;
                }
                task.resume().unwrap();
            }
            assert_eq!(task.cps_result(), Some(42), "{mode:?}: {name}");
            events(&expected);
            live(0);
            assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
            events(&expected);
        }
    }
}

#[test]
fn race_immediate_winner_stops_polling_but_evaluates_all_arguments_first() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for (name, expected) in [
            ("race_immediate", vec![1, 2, 3, 1, 2]),
            ("race_skip_failure", vec![1]),
            ("race_return_argument", vec![1]),
        ] {
            let vm = parity_vm(SOURCE, mode);
            let task = vm.call_async(name, &[]).unwrap();
            assert_eq!(task.cps_result(), Some(42), "{mode:?}: {name}");
            events(&expected);
        }
    }
}

#[test]
fn race_cancelled_branch_loans_end_but_allocations_live_until_root_completion() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for (name, allocations) in [("race_reference", 1), ("race_allocations", 2)] {
            let vm = parity_vm(SOURCE, mode);
            let mut task = vm.call_async(name, &[]).unwrap();
            live(allocations);
            READY.with(|ready| ready.set(true));
            task.resume().unwrap();
            assert_eq!(task.status(), ContinuationStatus::Pending);
            events(&[1, 2, 11, 102, 9]);
            live(allocations);
            task.resume().unwrap();
            assert_eq!(task.cps_result(), Some(42));
            live(0);
        }
    }
}

#[test]
fn race_failure_stops_later_branches_and_releases_the_whole_task() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for (name, expected, expression) in [
            (
                "race_failure",
                vec![1, 2, 3, 11],
                "memory.read(p) / divisor",
            ),
            (
                "race_argument_failure",
                vec![1],
                "delayed_value(1) / divisor",
            ),
        ] {
            let vm = parity_vm(SOURCE, mode);
            let mut task = vm.call_async(name, &[0]).unwrap();
            live(1);
            READY.with(|ready| ready.set(true));
            let ResumeError::Failed(error) = task.resume().unwrap_err() else {
                panic!("expected runtime failure")
            };
            assert_eq!(error.code(), crate::ErrorCode::RuntimeError);
            assert_eq!(error.span().unwrap().slice(SOURCE), Some(expression));
            assert_eq!(task.cps_result(), None);
            events(&expected);
            live(0);
            assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
            drop(task);
            live(0);
        }
        let vm = parity_vm(SOURCE, mode);
        assert!(vm.call_async("race_host_failure", &[]).is_err());
        events(&[1]);
        live(0);
    }
}

#[test]
fn race_cancellation_reload_and_vm_drop_keep_tasks_independent() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm(SOURCE, mode);
        let mut scheduler = Scheduler::new();
        let cancelled = scheduler.suspend(vm.call_async("race_allocations", &[]).unwrap());
        let mut resumed = vm.call_async("race_reference", &[]).unwrap();
        live(3);
        assert!(vm.load_source("fn broken(").is_err());
        vm.load_source("fn race_reference() @suspends -> Int32 { 99 }")
            .unwrap();
        assert_eq!(
            vm.call_async("race_reference", &[]).unwrap().cps_result(),
            Some(99)
        );
        drop(vm);
        scheduler.cancel(cancelled).unwrap();
        assert!(scheduler.cancel(cancelled).is_err());
        live(1);
        READY.with(|ready| ready.set(true));
        resumed.resume().unwrap();
        assert!(!resumed.is_finished());
        live(1);
        drop(resumed);
        live(0);
    }
}

#[test]
fn race_requires_suspending_unit_calls_in_all_backends() {
    for (source, message) in [
        (
            "fn f() { race { | f() } }",
            "race must be inside an @suspends function",
        ),
        (
            "fn f() @suspends { race {} }",
            "race requires at least one branch",
        ),
        (
            "fn f() @suspends { race { | 1 } }",
            "race branches must be function calls",
        ),
        (
            "fn branch_task() {} fn f() @suspends { race { | branch_task() } }",
            "must be an @suspends function returning Unit",
        ),
        (
            "fn branch_task() @suspends -> Int32 { 1 } fn f() @suspends { race { | branch_task() } }",
            "must be an @suspends function returning Unit",
        ),
        (
            "fn f() @suspends { race { | int32(1) } }",
            "race branches must be @suspends functions returning Unit",
        ),
    ] {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let error = VmEngine::with_mode(mode).load_source(source).unwrap_err();
            assert!(error.message().contains(message), "{mode:?}: {error}");
            assert!(error.span().is_some());
        }
        let error = crate::aot::AotCompiler::new("race-rejection")
            .unwrap()
            .compile_module(&crate::parser::parse(source).unwrap())
            .unwrap_err();
        assert!(error.message().contains(message), "{error}");
    }
}

#[test]
fn race_interpreter_reclaims_nested_frames_before_the_next_iteration() {
    let source = format!(
        r#"{SOURCE}
        fn repeat() @suspends -> Int32 {{
            var i = 0
            while i < 100 {{ race_nested(); i = i + 1; }}
            42
        }}
    "#
    );
    let mut vm = parity_vm(&source, VmMode::Interpreter);
    vm.set_interpreter_limits(InterpreterLimits {
        max_frames: 32,
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    let mut task = vm.call_async("repeat", &[]).unwrap();
    READY.with(|ready| ready.set(true));
    for _ in 0..50_000 {
        if task.is_finished() {
            break;
        }
        task.resume().unwrap();
    }
    assert_eq!(task.cps_result(), Some(42));
    let one = [1, 2, 3, 4, 5, 11, 12, 13, 14, 105, 9];
    events(&one.repeat(100));
}

#[test]
fn race_interpreter_resource_limits_cancel_all_branches() {
    for limits in [
        InterpreterLimits {
            max_frames: 4,
            ..Default::default()
        },
        InterpreterLimits {
            instruction_budget: Some(100),
            poll_budget: Some(1),
            ..Default::default()
        },
    ] {
        let mut vm = parity_vm(SOURCE, VmMode::Interpreter);
        vm.set_interpreter_limits(limits).unwrap();
        match vm.call_async("race_allocations", &[]) {
            Err(error) => assert_eq!(error.code(), crate::ErrorCode::ResourceLimit),
            Ok(mut task) => {
                for _ in 0..200 {
                    match task.resume() {
                        Err(ResumeError::Failed(error)) => {
                            assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
                            break;
                        }
                        Ok(()) => assert!(!task.is_finished()),
                        other => panic!("unexpected resume: {other:?}"),
                    }
                }
                assert!(matches!(task.status(), ContinuationStatus::Failed(_)));
                live(0);
            }
        }
        live(0);
    }
}

#[test]
fn race_interpreter_cancels_deep_losers_iteratively() {
    let source = format!(
        r#"{SOURCE}
        fn deep(n: Int32) @suspends {{
            if n == 0 {{ slow(1) }} else {{ deep(n - 1) }}
        }}
        fn run() @suspends -> Int32 {{
            race {{ | deep(4000) | wait(2) }}
            42
        }}
    "#
    );
    let mut vm = parity_vm(&source, VmMode::Interpreter);
    vm.set_interpreter_limits(InterpreterLimits {
        max_frames: 5000,
        ..Default::default()
    })
    .unwrap();
    let mut task = vm.call_async("run", &[]).unwrap();
    events(&[1, 2]);
    READY.with(|ready| ready.set(true));
    task.resume().unwrap();
    assert_eq!(task.cps_result(), Some(42));
    events(&[1, 2, 11, 102]);
}

#[test]
fn race_imports_keep_generic_borrows_and_first_completion_semantics() {
    let mut sources = crate::ModuleSources::new();
    sources
        .insert(
            "workers",
            r#"
        pub struct Point { x: Int32 }
        pub fn write(p: &mut Point, value: Int32) @suspends { p.x = value; }
        pub fn keep<T>(value: &T) @suspends { alloc((value,)); }
        pub fn nested<T>(value: &T) @suspends { race { | keep(value) } }
    "#,
        )
        .unwrap();
    let prefix = "import workers as w.{write as change}\n";
    let source = format!(
        r#"{prefix}
        fn run() @suspends -> Int32 {{
            let p = alloc(w.Point {{ x: 0 }})
            let q = alloc(w.Point {{ x: 0 }})
            race {{ | change(p, 42) | change(q, 99) }}
            p.x + q.x
        }}
    "#
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source_with_modules(&source, &sources).unwrap();
        assert_eq!(vm.call_async("run", &[]).unwrap().cps_result(), Some(42));
        live(0);
    }
    let ast = crate::TypeRegistry::default()
        .resolve_with_modules(&crate::parser::parse(&source).unwrap(), &sources)
        .unwrap();
    crate::aot::AotCompiler::new("race-imports")
        .unwrap()
        .compile_module(&ast)
        .unwrap();
    for body in [
        "race { | change(p, 1) | change(p, 2) }",
        "race { | w.nested(p) }; p.x = 2;",
    ] {
        let source =
            format!("{prefix} fn f() @suspends {{ let p = alloc(w.Point {{ x: 0 }}); {body} }}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let error = VmEngine::with_mode(mode)
                .load_source_with_modules(&source, &sources)
                .unwrap_err();
            assert_eq!(error.code(), crate::ErrorCode::InvalidAssignment, "{error}");
        }
        let ast = crate::TypeRegistry::default()
            .resolve_with_modules(&crate::parser::parse(&source).unwrap(), &sources)
            .unwrap();
        let error = crate::aot::AotCompiler::new("race-import-rejection")
            .unwrap()
            .compile_module(&ast)
            .unwrap_err();
        assert_eq!(error.code(), crate::ErrorCode::InvalidAssignment, "{error}");
    }
}
