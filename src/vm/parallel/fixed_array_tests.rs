use super::*;
use crate::vm::{VmEngine, VmMode};
pub(crate) const SOURCE: &str = include_str!("../../../tests/fixtures/task_fixed_arrays.jk");
pub(crate) fn input(ty: &Type, variant: usize) -> TaskValue {
    let (tag, payload) = ty
        .enum_variant(if variant == 0 { "Empty" } else { "Data" })
        .unwrap();
    TaskValue::Enum {
        ty: ty.clone(),
        variant: tag,
        payload: payload.map(|ty| {
            let fields = ty.tuple_element_types().unwrap();
            let packet = &fields[0];
            let rows = packet.record_field_type("rows").unwrap();
            let (row, _) = rows.fixed_array_element_type().unwrap();
            Box::new(TaskValue::Tuple(vec![
                TaskValue::Record {
                    ty: packet.clone(),
                    fields: vec![
                        TaskValue::Scalar(InstanceValue::UInt8(7)),
                        TaskValue::FixedArray {
                            ty: rows.clone(),
                            elements: vec![
                                TaskValue::FixedArray {
                                    ty: row.clone(),
                                    elements: vec![TaskValue::Scalar(InstanceValue::Int32(-42)); 3]
                                };
                                2
                            ],
                        },
                        TaskValue::Scalar(InstanceValue::Int64(-9)),
                    ],
                },
                TaskValue::Scalar(InstanceValue::Bool(true)),
            ]))
        }),
    }
}
#[test]
fn task_fixed_array_admission_and_frozen_copy() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_source(SOURCE).unwrap_or_else(|e| panic!("{e}"));
        let sig = vm.parallel_signature("echo").unwrap();
        let value = TaskValue::FixedArray {
            ty: sig.params[0].clone(),
            elements: vec![TaskValue::Scalar(InstanceValue::Int32(42)); 3],
        };
        for elements in [
            vec![],
            vec![TaskValue::Scalar(InstanceValue::UInt32(42)); 3],
            vec![TaskValue::Scalar(InstanceValue::Int32(42)); 4],
        ] {
            assert!(vm
                .prepare_parallel_task(
                    "echo",
                    vec![TaskValue::FixedArray {
                        ty: sig.params[0].clone(),
                        elements
                    }],
                    vec![]
                )
                .is_err());
        }
        for stage in 0..3 {
            let budget = TaskBudget::new(1 << 20);
            let prepared = vm
                .prepare_parallel_task("echo", vec![value.clone()], vec![budget.clone()])
                .unwrap();
            if stage == 0 {
                drop(prepared);
            } else {
                let mut task = prepared.start().unwrap();
                assert!(!task.resume().unwrap());
                if stage == 2 {
                    while !task.resume().unwrap() {}
                }
                drop(task);
            }
            assert_eq!(budget.used(), 0, "{mode:?} dropped stage={stage}");
        }
        let budget = TaskBudget::new(1 << 20);
        let prepared = vm
            .prepare_parallel_task("echo", vec![value.clone()], vec![budget.clone()])
            .unwrap();
        drop(vm);
        let output = std::thread::spawn(move || {
            let mut task = prepared.start().unwrap();
            while !task.resume().unwrap() {}
            let output = task.take_result().unwrap();
            assert!(task.take_result().is_err());
            output
        })
        .join()
        .unwrap();
        assert_eq!(output.value(), &value);
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}

fn attempt(vm: &VmEngine, name: &str, bytes: usize, workers: usize, cancel: bool) -> bool {
    let budget = TaskBudget::new(bytes);
    let pool = crate::runtime::parallel::ParallelExecutor::new(workers, 64).unwrap();
    let sig = vm.parallel_signature(name).unwrap();
    let outcome = vm
        .prepare_parallel_task(name, vec![input(&sig.params[0], 1)], vec![budget.clone()])
        .and_then(|prepared| prepared.submit(&pool))
        .and_then(|task| {
            if cancel {
                task.request_cancel();
            }
            task.wait()?;
            task.take_result()
        });
    let success = outcome.is_ok();
    drop((outcome, pool));
    assert_eq!(budget.used(), 0, "{name}, bytes={bytes}, workers={workers}");
    success
}

#[test]
fn task_fixed_array_failure_cancellation_and_budget_boundaries() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_source(SOURCE).unwrap_or_else(|e| panic!("{e}"));
        for workers in [1, 3] {
            assert!(!attempt(&vm, "root", 512, workers, false));
            assert!(attempt(&vm, "root", 1 << 20, workers, false));
            assert!(!attempt(&vm, "failed", 1 << 20, workers, false));
            assert!(!attempt(&vm, "cancelled", 1 << 20, workers, true));
        }
    }
}

#[test]
fn task_fixed_array_cancel_after_children_install_their_captures() {
    use std::sync::{Condvar, Mutex, OnceLock};
    static ENTERED: OnceLock<(Mutex<usize>, Condvar)> = OnceLock::new();
    unsafe extern "C" fn entered(_: *mut crate::runtime::cps::CpsFrame) -> i32 {
        let (count, wake) = ENTERED.get_or_init(|| (Mutex::new(0), Condvar::new()));
        *count.lock().unwrap() += 1;
        wake.notify_all();
        0
    }
    let source = format!(
        "{SOURCE}\n{}",
        r#"
        extern fn entered() @suspends
        fn observed(value: Choice) @suspends -> Choice { entered(); cancelled(value) }
        fn active(value: Choice) @suspends -> Choice {
            let (a, b) = join { | observed(value) | observed(value) }
            a
        }
    "#
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        unsafe {
            vm.register_parallel_suspending_fn(
                "entered",
                FunctionSignature::new(vec![], Type::Unit),
                entered,
                0,
                Arc::new(()),
            )
            .unwrap();
        }
        vm.load_source(&source).unwrap_or_else(|e| panic!("{e}"));
        let sig = vm.parallel_signature("active").unwrap();
        for workers in [1, 3] {
            let (count, wake) = ENTERED.get_or_init(|| (Mutex::new(0), Condvar::new()));
            *count.lock().unwrap() = 0;
            let pool = crate::runtime::parallel::ParallelExecutor::new(workers, 32).unwrap();
            let budget = TaskBudget::new(1 << 20);
            let task = vm
                .prepare_parallel_task(
                    "active",
                    vec![input(&sig.params[0], 1)],
                    vec![budget.clone()],
                )
                .unwrap()
                .submit(&pool)
                .unwrap();
            let (count, timeout) = wake
                .wait_timeout_while(
                    count.lock().unwrap(),
                    std::time::Duration::from_secs(5),
                    |count| *count != 2,
                )
                .unwrap();
            assert!(
                !timeout.timed_out() || *count == 2,
                "children did not receive their Copy captures"
            );
            drop(count);
            task.request_cancel();
            task.wait().unwrap();
            assert_eq!(
                task.take_result().err().unwrap().code(),
                ErrorCode::Cancelled
            );
            drop((task, pool));
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
#[ignore = "exhaustive: Copy packet preparation, publication and result budget failures"]
fn exhaustive_task_fixed_array_budget_failures() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_source(SOURCE).unwrap_or_else(|e| panic!("{e}"));
        let mut completed = false;
        for bytes in (0..262144).step_by(64) {
            if attempt(&vm, "root", bytes, 2, false) {
                completed = true;
                break;
            }
        }
        assert!(completed);
    }
}
