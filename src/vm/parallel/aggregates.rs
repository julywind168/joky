use super::*;
use crate::vm::{VmEngine, VmMode};

pub(crate) const SOURCE: &str = include_str!("../../../tests/fixtures/task_aggregates.jk");

pub(crate) fn input(ty: &Type, variant: usize) -> TaskValue {
    let head = ty.record_field_type("head").unwrap();
    let (index, payload) = head
        .enum_variant(["Empty", "One", "Other"][variant])
        .unwrap();
    let payload = match variant {
        0 => None,
        1 => Some(Box::new(TaskValue::Record {
            ty: payload.unwrap().clone(),
            fields: vec![
                TaskValue::Scalar(InstanceValue::Int8(-7)),
                TaskValue::String("字段 跨线程".into()),
                TaskValue::Scalar(InstanceValue::UInt16(65530)),
            ],
        })),
        _ => Some(Box::new(TaskValue::Tuple(vec![
            TaskValue::String("variant two".into()),
            TaskValue::Scalar(InstanceValue::Int64(-1234567890123)),
        ]))),
    };
    TaskValue::Record {
        ty: ty.clone(),
        fields: vec![
            TaskValue::Enum {
                ty: head.clone(),
                variant: index,
                payload,
            },
            TaskValue::Tuple(vec![
                TaskValue::Scalar(InstanceValue::Bool(true)),
                TaskValue::String("second owner".into()),
            ]),
        ],
    }
}
pub(crate) fn expected(sig: &FunctionSignature, input: TaskValue) -> TaskValue {
    let types = sig.ret.tuple_element_types().unwrap();
    TaskValue::Tuple(vec![
        input,
        TaskValue::Enum {
            ty: types[1].clone(),
            variant: types[1].enum_variant("Some").unwrap().0,
            payload: Some(Box::new(TaskValue::String("optional".into()))),
        },
        TaskValue::Enum {
            ty: types[2].clone(),
            variant: types[2].enum_variant("Err").unwrap().0,
            payload: Some(Box::new(TaskValue::String("error payload".into()))),
        },
    ])
}

#[test]
fn task_parallel_aggregate_admission_and_frozen_nominal_identity() {
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
        let mut wrong = valid.clone();
        let TaskValue::Record { fields, .. } = &mut wrong else {
            unreachable!()
        };
        let TaskValue::Enum { variant, .. } = &mut fields[0] else {
            unreachable!()
        };
        *variant = usize::MAX;
        assert!(vm
            .prepare_parallel_task("identity", vec![wrong], vec![])
            .is_err());
        let mut wrong = valid.clone();
        let TaskValue::Record { fields, .. } = &mut wrong else {
            unreachable!()
        };
        fields[1] = TaskValue::Tuple(vec![
            TaskValue::Scalar(InstanceValue::Int8(1)),
            TaskValue::String("wrong scalar".into()),
        ]);
        assert!(vm
            .prepare_parallel_task("identity", vec![wrong], vec![])
            .is_err());
        let mut wrong = valid.clone();
        let TaskValue::Record { fields, .. } = &mut wrong else {
            unreachable!()
        };
        fields.pop();
        assert!(vm
            .prepare_parallel_task("identity", vec![wrong], vec![])
            .is_err());
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
fn task_parallel_aggregate_failure_cancellation_and_budget_boundaries() {
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
fn task_parallel_aggregate_cancel_after_children_install_their_captures() {
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
        fn observed(value: Batch<Choice>) @suspends -> Batch<Choice> { entered(); parked(value) }
        fn active(value: Batch<Choice>) @suspends -> Batch<Choice> {
            let (a, b) = join { observed(value), observed(Batch { head: Choice.Other(("other", int64(9))), tail: (false, "tail") }) }
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
                "children did not receive their aggregate captures"
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
#[ignore = "exhaustive: aggregate packet preparation, publication and result budget failures"]
fn exhaustive_task_parallel_aggregate_budget_failures() {
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
