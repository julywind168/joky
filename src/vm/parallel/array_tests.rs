use super::*;
use crate::vm::{VmEngine, VmMode};

pub(crate) const SOURCE: &str = include_str!("../../../tests/fixtures/task_arrays.jk");
pub(crate) fn input(ty: &Type, mode: usize) -> TaskValue {
    let payload = ty.array_element_type().unwrap();
    let (items, array) = payload.enum_variant("Items").unwrap();
    let array = array.unwrap();
    let item = array.array_element_type().unwrap();
    let elements = if mode == 0 {
        vec![]
    } else {
        vec![
            TaskValue::Enum {
                ty: payload.clone(),
                variant: payload.enum_variant("Empty").unwrap().0,
                payload: None,
            },
            TaskValue::Enum {
                ty: payload.clone(),
                variant: items,
                payload: Some(Box::new(TaskValue::Array {
                    ty: array.clone(),
                    elements: vec![TaskValue::Record {
                        ty: item.clone(),
                        fields: vec![
                            TaskValue::Scalar(InstanceValue::Int8(-7)),
                            TaskValue::String("nested owner".into()),
                            TaskValue::Scalar(InstanceValue::Bool(true)),
                        ],
                    }],
                })),
            },
            TaskValue::Enum {
                ty: payload.clone(),
                variant: payload.enum_variant("Text").unwrap().0,
                payload: Some(Box::new(TaskValue::String("tail owner".into()))),
            },
        ]
    };
    TaskValue::Array {
        ty: ty.clone(),
        elements,
    }
}
pub(crate) fn expected(sig: &FunctionSignature, input: TaskValue) -> TaskValue {
    TaskValue::Tuple(vec![
        input,
        TaskValue::Array {
            ty: sig.ret.tuple_element_types().unwrap()[1].clone(),
            elements: vec![
                TaskValue::String("first".into()),
                TaskValue::String("second".into()),
            ],
        },
    ])
}
#[test]
fn task_parallel_array_admission_and_frozen_nominal_identity() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_source(SOURCE).unwrap();
        let sig = vm.parallel_signature("identity").unwrap();
        let valid = input(&sig.params[0], 1);
        let budget = TaskBudget::new(1 << 20);
        let prepared = vm
            .prepare_parallel_task("identity", vec![valid.clone()], vec![budget.clone()])
            .unwrap();
        let wrong_values = [
            TaskValue::Array {
                ty: sig.params[0].clone(),
                elements: vec![TaskValue::String("wrong element".into())],
            },
            TaskValue::Tuple(vec![]),
            TaskValue::Record {
                ty: sig.params[0].clone(),
                fields: vec![],
            },
        ];
        for wrong in wrong_values {
            assert!(vm
                .prepare_parallel_task("identity", vec![wrong], vec![])
                .is_err());
        }
        // Identical source reloaded into another nominal universe cannot accept
        // a value tagged with the retired declaration's identity.
        vm.load_source(SOURCE).unwrap();
        assert!(vm
            .prepare_parallel_task("identity", vec![valid.clone()], vec![])
            .is_err());
        drop(vm);
        let output = std::thread::spawn(move || {
            let mut task = prepared.start().unwrap();
            while !task.resume().unwrap() {}
            let result = task.take_result().unwrap();
            assert!(task.take_result().is_err());
            result
        })
        .join()
        .unwrap();
        assert_eq!(output.value(), &valid);
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
fn task_parallel_array_failure_cancellation_and_budget_boundaries() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_source(SOURCE).unwrap();
        for workers in [1, 3] {
            assert!(!attempt(&vm, "root", 512, workers, false));
            assert!(attempt(&vm, "root", 1 << 20, workers, false));
            assert!(!attempt(&vm, "failed", 1 << 20, workers, false));
            assert!(!attempt(&vm, "cancelled", 1 << 20, workers, true));
        }
    }
}

#[test]
fn task_parallel_array_cancel_after_children_install_their_captures() {
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
        fn observed(value: Array<Payload>) @suspends -> Array<Payload> { entered(); parked(value) }
        fn active(value: Array<Payload>) @suspends -> Array<Payload> {
            let (a, b) = join { | observed(value) | observed([Payload.Items([Item { code: int8(9), text: "other", flag: false }])]) }
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
        vm.load_source(&source).unwrap();
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
                "children did not receive their array captures"
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
#[ignore = "exhaustive: array packet preparation, publication and result budget failures"]
fn exhaustive_task_parallel_array_budget_failures() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_source(SOURCE).unwrap();
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
