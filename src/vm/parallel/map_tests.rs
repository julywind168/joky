use super::*;
use crate::vm::{VmEngine, VmMode};

pub(crate) const SOURCE: &str = include_str!("../../../tests/fixtures/task_maps.jk");
pub(crate) const BUDGET: usize = 4 << 20;
fn scalar(value: i32) -> TaskValue {
    TaskValue::Scalar(InstanceValue::Int32(value))
}
fn key(ty: &Type, id: i32) -> TaskValue {
    TaskValue::Record {
        ty: ty.clone(),
        fields: vec![scalar(id), TaskValue::String("host key".into())],
    }
}
pub(crate) fn input(ty: &Type, populated: bool, set: bool) -> TaskValue {
    let element = ty.array_element_type().unwrap();
    let elements = if !populated {
        vec![]
    } else if set {
        [1, 2, 1].into_iter().map(|id| key(element, id)).collect()
    } else {
        let pair = element.tuple_element_types().unwrap();
        let text = pair[1].enum_variant("Text").unwrap().0;
        [(1, "first"), (2, "second"), (1, "replacement")]
            .into_iter()
            .map(|(id, value)| {
                TaskValue::Tuple(vec![
                    key(&pair[0], id),
                    TaskValue::Enum {
                        ty: pair[1].clone(),
                        variant: text,
                        payload: Some(Box::new(TaskValue::String(value.into()))),
                    },
                ])
            })
            .collect()
    };
    TaskValue::Array {
        ty: ty.clone(),
        elements,
    }
}
fn id(key: &TaskValue) -> i32 {
    let TaskValue::Record { fields, .. } = key else {
        panic!("expected Key")
    };
    let TaskValue::Scalar(InstanceValue::Int32(id)) = fields[0] else {
        panic!("expected id")
    };
    id
}
pub(crate) fn check_bundle(value: &TaskValue) {
    let TaskValue::Tuple(parts) = value else {
        panic!("expected Bundle")
    };
    let TaskValue::Map(map) = &parts[0] else {
        panic!("expected Map")
    };
    let TaskValue::Set(set) = &parts[1] else {
        panic!("expected Set")
    };
    assert_eq!(map.len(), 2);
    assert_eq!(set.len(), 2);
    assert!(!map.is_empty() && !set.is_empty());
    assert!(map.value_type().map_entry_types().is_some());
    assert!(set.value_type().set_element_type().is_some());
    let mut ids: Vec<_> = set.elements().iter().map(id).collect();
    ids.sort();
    assert_eq!(ids, [11, 14]);
    for (key, value) in map.entries() {
        let TaskValue::Enum {
            payload: Some(payload),
            ..
        } = value
        else {
            panic!("expected Payload")
        };
        match id(key) {
            1 => {
                let TaskValue::List { elements, .. } = &**payload else {
                    panic!("expected List")
                };
                assert_eq!(elements, &[TaskValue::String("replacement".into())]);
            }
            4 => {
                let TaskValue::Map(inner) = &**payload else {
                    panic!("expected nested Map")
                };
                assert_eq!(inner.keys(), &[scalar(9)]);
                assert_eq!(inner.values(), &[TaskValue::String("nested".into())]);
            }
            other => panic!("unexpected key {other}"),
        }
    }
}
pub(crate) fn check_factory(value: &TaskValue, populated: bool, set: bool) {
    if set {
        let TaskValue::Set(value) = value else {
            panic!("expected Set")
        };
        assert_eq!(value.len(), usize::from(populated) * 2);
        assert_eq!(value.is_empty(), !populated);
        if populated {
            let mut ids: Vec<_> = value.elements().iter().map(id).collect();
            ids.sort();
            assert_eq!(ids, [1, 2]);
        }
    } else {
        let TaskValue::Map(value) = value else {
            panic!("expected Map")
        };
        assert_eq!(value.len(), usize::from(populated) * 2);
        assert_eq!(value.is_empty(), !populated);
        for (key, value) in value.entries() {
            let TaskValue::Enum {
                payload: Some(payload),
                ..
            } = value
            else {
                panic!("expected Payload")
            };
            assert_eq!(
                **payload,
                TaskValue::String(
                    if id(key) == 1 {
                        "replacement"
                    } else {
                        "second"
                    }
                    .into()
                )
            );
        }
    }
}
fn vm(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    vm.set_prelude_enabled(false);
    vm.load_source(SOURCE).unwrap_or_else(|e| panic!("{e}"));
    vm
}
fn snapshot(vm: &VmEngine) -> TaskValue {
    super::tests::finish(vm.prepare_parallel_task("root", vec![], vec![]).unwrap())
        .unwrap()
        .into_value()
}

#[test]
fn task_parallel_map_snapshot_identity_lifetimes_and_forgery() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = vm(mode);
        let value = snapshot(&vm);
        check_bundle(&value);
        let TaskValue::Tuple(parts) = &value else {
            unreachable!()
        };
        let TaskValue::Map(map) = &parts[0] else {
            unreachable!()
        };
        let TaskValue::Set(set) = &parts[1] else {
            unreachable!()
        };
        // Even an otherwise correctly typed private representation must not be
        // accepted as an ordinary record from the safe host API.
        for forged in [
            TaskValue::Tuple(vec![
                TaskValue::Record {
                    ty: map.ty.clone(),
                    fields: map.fields.clone(),
                },
                parts[1].clone(),
            ]),
            TaskValue::Tuple(vec![
                parts[0].clone(),
                TaskValue::Record {
                    ty: set.ty.clone(),
                    fields: set.fields.clone(),
                },
            ]),
        ] {
            assert!(vm
                .prepare_parallel_task("roundtrip", vec![forged], vec![])
                .is_err());
        }
        for stage in 0..3 {
            let budget = TaskBudget::new(BUDGET);
            let prepared = vm
                .prepare_parallel_task("roundtrip", vec![value.clone()], vec![budget.clone()])
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
            assert_eq!(budget.used(), 0, "{mode:?} stage={stage}");
        }
        let budget = TaskBudget::new(BUDGET);
        let prepared = vm
            .prepare_parallel_task("roundtrip", vec![value.clone()], vec![budget.clone()])
            .unwrap();
        vm.load_source(SOURCE).unwrap();
        assert!(vm
            .prepare_parallel_task("roundtrip", vec![value.clone()], vec![])
            .is_err());
        drop(vm);
        let output = std::thread::spawn(move || super::tests::finish(prepared).unwrap())
            .join()
            .unwrap();
        assert_eq!(output.value(), &value);
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn task_parallel_map_snapshot_validation_rejects_damaged_index_metadata() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        let value = snapshot(&vm);
        for field in [2, 3, 4, 5] {
            // Test-only access breaks the seal to exercise the output validator.
            // Public callers can neither construct nor mutate these fields.
            let mut damaged = value.clone();
            let TaskValue::Tuple(parts) = &mut damaged else {
                unreachable!()
            };
            let TaskValue::Map(map) = &mut parts[0] else {
                unreachable!()
            };
            if field == 5 {
                map.fields[5] = TaskValue::Scalar(InstanceValue::UInt64(99));
            } else {
                let TaskValue::Array { elements, .. } = &mut map.fields[field] else {
                    unreachable!()
                };
                if field == 2 {
                    elements.pop();
                } else {
                    elements[0] =
                        TaskValue::Scalar(InstanceValue::UInt64(if field == 3 { 0 } else { 999 }));
                }
            }
            let budget = TaskBudget::new(BUDGET);
            let result = vm
                .prepare_parallel_task("roundtrip", vec![damaged], vec![budget.clone()])
                .and_then(super::tests::finish);
            assert!(result.is_err(), "{mode:?} field={field}");
            drop(result);
            assert_eq!(budget.used(), 0);
        }
    }
}

fn attempt(vm: &VmEngine, bytes: usize, workers: usize) -> bool {
    let budget = TaskBudget::new(bytes);
    let pool = crate::runtime::parallel::ParallelExecutor::new(workers, 64).unwrap();
    let outcome = vm
        .prepare_parallel_task("root", vec![], vec![budget.clone()])
        .and_then(|prepared| prepared.submit(&pool))
        .and_then(|task| {
            task.wait()?;
            task.take_result()
        });
    if let Err(error) = &outcome {
        assert_eq!(error.code(), ErrorCode::ResourceLimit, "{error}");
    }
    let success = outcome.is_ok();
    drop((outcome, pool));
    assert_eq!(budget.used(), 0, "bytes={bytes}");
    success
}
#[test]
fn task_parallel_map_budget_boundaries() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        for workers in [1, 3] {
            assert!(!attempt(&vm, 512, workers));
            assert!(attempt(&vm, BUDGET, workers));
        }
    }
}
#[test]
#[ignore = "exhaustive: Map/Set preparation, publication and snapshot budget failures"]
fn exhaustive_task_parallel_map_budget_failures() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        assert!(
            (0..=BUDGET)
                .step_by(256)
                .any(|bytes| attempt(&vm, bytes, 2)),
            "{mode:?}"
        );
    }
}

#[test]
fn task_parallel_map_cancel_after_children_install_their_captures() {
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
        fn observed(value: Bundle) @suspends -> Bundle { entered(); parked(value) }
        fn active(value: Bundle) @suspends -> Bundle {
            let (a, b) = join { observed(value), observed((make(), make_set())) }
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
        for workers in [1, 3] {
            let (count, wake) = ENTERED.get_or_init(|| (Mutex::new(0), Condvar::new()));
            *count.lock().unwrap() = 0;
            let pool = crate::runtime::parallel::ParallelExecutor::new(workers, 32).unwrap();
            let budget = TaskBudget::new(BUDGET);
            let task = vm
                .prepare_parallel_task("active", vec![snapshot(&vm)], vec![budget.clone()])
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
                "children did not receive their map captures"
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
