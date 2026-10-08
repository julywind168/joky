use super::{InstanceLimits, InstanceStatus, InstanceValue as V, VmEngine, VmMode};
use crate::{runtime::arena::LIVE, ErrorCode};

const SOURCE: &str = include_str!("../../tests/fixtures/persistent_instances.jk");

fn vm(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    vm.load_source(SOURCE).unwrap();
    vm
}

#[test]
fn persistent_instances_keep_independent_plain_state_and_release_temporaries() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        let a = vm
            .create_instance("make", &[V::Int32(10), V::Float64(0.5)])
            .unwrap();
        let b = vm
            .create_instance("make", &[V::Int32(100), V::Float64(2.0)])
            .unwrap();
        let bytes = vm.instance_bytes();
        for i in 1..=100 {
            assert_eq!(
                vm.call_instance(a, "advance", &[V::Int32(1), V::Float64(0.25)])
                    .unwrap(),
                V::Int32(10 + i)
            );
            assert_eq!(vm.instance_bytes(), bytes);
            LIVE.with(|n| assert_eq!(n.get(), 2));
        }
        assert_eq!(vm.call_instance(a, "total", &[]).unwrap(), V::Float64(25.5));
        assert_eq!(vm.call_instance(b, "count", &[]).unwrap(), V::Int32(100));
        assert_eq!(vm.call_instance(b, "small", &[]).unwrap(), V::Int16(-7));
        assert_eq!(vm.call_instance(b, "enabled", &[]).unwrap(), V::Bool(true));
        assert_eq!(vm.call_instance(a, "reset", &[]).unwrap(), V::Unit);
        assert_eq!(vm.call_instance(a, "count", &[]).unwrap(), V::Int32(0));
        vm.close_instance(a).unwrap();
        assert!(vm.close_instance(a).is_err());
        assert!(vm.call_instance(a, "count", &[]).is_err());
        assert_eq!(vm.instance_count(), 1);
        drop(vm);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

#[test]
fn persistent_instances_validate_signatures_and_fail_without_publishing_results() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        for name in ["make_pointer", "count", "missing"] {
            assert!(vm.create_instance(name, &[]).is_err(), "{name}");
        }
        assert!(vm
            .create_instance("make", &[V::Int64(1), V::Float64(0.0)])
            .is_err());
        assert!(vm.create_instance("bad_make", &[V::Int32(0)]).is_err());
        assert_eq!(vm.instance_count(), 0);
        assert_eq!(vm.instance_bytes(), 0);
        let id = vm
            .create_instance("make", &[V::Int32(1), V::Float64(0.0)])
            .unwrap();
        for name in ["other", "escaping", "borrowed", "suspended", "missing"] {
            assert!(vm.call_instance(id, name, &[]).is_err(), "{name}");
            assert_eq!(vm.instance_status(id).unwrap(), InstanceStatus::Ready);
        }
        assert!(vm
            .call_instance(id, "advance", &[V::Int64(1), V::Float64(0.0)])
            .is_err());
        let foreign = VmEngine::with_mode(mode);
        assert!(foreign.close_instance(id).is_err());
        assert!(foreign.call_instance(id, "count", &[]).is_err());
        let failure = vm.call_instance(id, "fail", &[V::Int32(0)]).unwrap_err();
        assert_eq!(vm.instance_status(id).unwrap(), InstanceStatus::Failed);
        assert_eq!(
            vm.instance_error(id).unwrap().unwrap().code(),
            failure.code()
        );
        assert!(vm.call_instance(id, "count", &[]).is_err());
        assert_eq!(vm.instance_bytes(), 0);
        assert_eq!(vm.instance_count(), 1);
        vm.close_instance(id).unwrap();
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

#[test]
fn persistent_instances_preserve_nested_layouts_zero_sizes_and_scalar_bits() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        let id = vm.create_instance("nested", &[]).unwrap();
        assert_eq!(vm.call_instance(id, "letter", &[]).unwrap(), V::Rune('界'));
        assert_eq!(vm.call_instance(id, "byte", &[]).unwrap(), V::UInt8(255));
        assert_eq!(
            vm.call_instance(id, "choice", &[]).unwrap(),
            V::UInt16(65535)
        );
        let empty = vm.create_instance("empty", &[]).unwrap();
        let V::Float32(value) = vm
            .call_instance(empty, "empty_value", &[V::Unit, V::Float32(-0.0)])
            .unwrap()
        else {
            panic!("wrong result")
        };
        assert_eq!(value.to_bits(), (-0.0_f32).to_bits());
        drop(vm);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

#[test]
fn persistent_instances_pin_old_code_and_bound_registry_storage() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = vm(mode);
        vm.set_instance_limits(InstanceLimits {
            max_instances: 2,
            max_bytes: 4096,
            ..Default::default()
        })
        .unwrap();
        let old = vm
            .create_instance("make", &[V::Int32(10), V::Float64(0.0)])
            .unwrap();
        let old_type = vm.instance_type(old).unwrap();
        assert!(vm.load_source("fn broken(").is_err());
        vm.load_source(&SOURCE.replace("state.count + amount", "state.count + amount + 1000"))
            .unwrap();
        let new = vm
            .create_instance("make", &[V::Int32(10), V::Float64(0.0)])
            .unwrap();
        assert_ne!(old_type, vm.instance_type(new).unwrap());
        assert_eq!(
            vm.call_instance(old, "advance", &[V::Int32(1), V::Float64(0.0)])
                .unwrap(),
            V::Int32(11)
        );
        assert_eq!(
            vm.call_instance(new, "advance", &[V::Int32(1), V::Float64(0.0)])
                .unwrap(),
            V::Int32(1011)
        );
        assert_eq!(
            vm.create_instance("empty", &[]).unwrap_err().code(),
            ErrorCode::ResourceLimit
        );
        vm.close_instance(old).unwrap();
        vm.close_instance(new).unwrap();
        for _ in 0..200 {
            let next = vm.create_instance("empty", &[]).unwrap();
            assert_ne!(next, old);
            vm.close_instance(next).unwrap();
            assert_eq!(vm.instance_bytes(), 0);
        }
        vm.set_instance_limits(InstanceLimits {
            max_instances: 1,
            max_bytes: 1,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            vm.create_instance("empty", &[]).unwrap_err().code(),
            ErrorCode::ResourceLimit
        );
        assert_eq!(vm.instance_bytes(), 0);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

#[test]
fn persistent_instances_reject_reentry_and_defer_close_until_receiver_returns() {
    use super::InstanceId;
    use crate::{FunctionSignature, Type};
    use std::{cell::RefCell, rc::Rc};
    type Context = (Rc<VmEngine>, InstanceId, InstanceId, bool);
    thread_local! {
        static CONTEXT: RefCell<Option<Context>> = const { RefCell::new(None) };
    }
    extern "C" fn callback() -> i32 {
        let (vm, active, other, close) = CONTEXT.with(|c| c.borrow().as_ref().unwrap().clone());
        // Never panic across the host ABI; encode every observation in the result.
        if vm.instance_status(active).ok() != Some(InstanceStatus::Busy)
            || vm.call_instance(active, "count", &[]).is_ok()
            || vm.call_instance(other, "count", &[]).ok() != Some(V::Int32(7))
        {
            return -1;
        }
        if close {
            let before = vm.instance_bytes();
            if vm.close_instance(active).is_err()
                || vm.instance_bytes() != before
                || vm.call_instance(active, "count", &[]).is_ok()
            {
                return -2;
            }
        }
        42
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_fn(
                "callback",
                FunctionSignature::new(vec![], Type::Int32),
                callback as *const u8,
            )
            .unwrap();
        }
        vm.load_source(&format!("{SOURCE}\nextern fn callback() -> Int32\nfn invoke(state: &mut State) @noescape -> Int32 {{ let n = callback() state.count = n state.count }}")).unwrap();
        let vm = Rc::new(vm);
        let a = vm
            .create_instance("make", &[V::Int32(0), V::Float64(0.0)])
            .unwrap();
        let b = vm
            .create_instance("make", &[V::Int32(7), V::Float64(0.0)])
            .unwrap();
        CONTEXT.with(|c| *c.borrow_mut() = Some((vm.clone(), a, b, false)));
        assert_eq!(vm.call_instance(a, "invoke", &[]).unwrap(), V::Int32(42));
        CONTEXT.with(|c| c.borrow_mut().as_mut().unwrap().3 = true);
        assert!(vm
            .call_instance(a, "invoke", &[])
            .unwrap_err()
            .message()
            .contains("closed during"));
        assert_eq!(vm.instance_count(), 1);
        LIVE.with(|n| assert_eq!(n.get(), 1));
        CONTEXT.with(|c| *c.borrow_mut() = None);
        drop(vm);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

#[test]
fn persistent_instances_release_state_on_allocation_and_instruction_failure() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        crate::runtime::arena::LIMIT.with(|limit| {
            let previous = limit.replace(0);
            let result = vm.create_instance("empty", &[]);
            limit.set(previous);
            assert_eq!(result.unwrap_err().code(), ErrorCode::ResourceLimit);
        });
        let id = vm
            .create_instance("make", &[V::Int32(0), V::Float64(0.0)])
            .unwrap();
        crate::runtime::arena::LIMIT.with(|limit| {
            let previous = limit.replace(0);
            let result = vm.call_instance(id, "advance", &[V::Int32(1), V::Float64(0.0)]);
            limit.set(previous);
            assert_eq!(result.unwrap_err().code(), ErrorCode::ResourceLimit);
        });
        assert_eq!(vm.instance_status(id).unwrap(), InstanceStatus::Failed);
        assert_eq!(vm.instance_bytes(), 0);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
    let mut vm = vm(VmMode::Interpreter);
    let id = vm
        .create_instance("make", &[V::Int32(0), V::Float64(0.0)])
        .unwrap();
    vm.set_interpreter_limits(super::InterpreterLimits {
        instruction_budget: Some(0),
        ..Default::default()
    })
    .unwrap();
    assert!(vm
        .create_instance("make", &[V::Int32(0), V::Float64(0.0)])
        .is_err());
    assert_eq!(vm.instance_count(), 1);
    assert!(vm
        .call_instance(id, "advance", &[V::Int32(1), V::Float64(0.0)])
        .is_err());
    assert_eq!(vm.instance_status(id).unwrap(), InstanceStatus::Failed);
    assert_eq!(vm.instance_bytes(), 0);
    LIVE.with(|n| assert_eq!(n.get(), 0));
}

#[test]
fn persistent_instances_reject_hidden_references_and_keep_checked_entry_private() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = vm(mode);
        // Ordinary scalar calls cannot acquire the receiver pointer.
        assert!(vm.function_signature("advance").is_none());
        assert!(vm.call_int32_0("count").is_err());
        vm.load_source("struct Hidden<T> {} fn hidden() -> Hidden<&Int32> { Hidden::<&Int32> {} }")
            .unwrap();
        assert!(vm.create_instance("hidden", &[]).is_err());
        let examples = [
            "struct Hidden<T> {} struct Outer { hidden: Hidden<&Int32> } fn make() -> Outer { Outer { hidden: Hidden::<&Int32> {} } }",
            "struct Hidden<T> {} enum Choice { Empty, Item(Hidden<&Int32>) } struct Outer { choice: Choice } fn make() -> Outer { Outer { choice: Choice.Empty } }",
            "struct State { value: &Int32 } fn make() -> State { State { value: (&*(alloc(42))) } }",
            "struct Inner { value: Ptr } struct Outer { inner: (Inner, Int32) } fn make() -> Outer { Outer { inner: (Inner { value: Ptr.null() }, 42) } }",
        ];
        for source in examples {
            vm.load_source(source).unwrap();
            assert!(vm.create_instance("make", &[]).is_err());
        }
        let error = vm
            .load_source("struct State { n: Int32 } fn escape(s: &State) @noescape -> &State { s }")
            .unwrap_err();
        assert!(error.message().contains("noescape"));
    }
}

#[test]
fn persistent_instances_roundtrip_every_scalar_kind_without_numeric_casts() {
    let values = [
        V::Unit,
        V::Bool(true),
        V::Rune('界'),
        V::Int8(i8::MIN),
        V::Int16(i16::MIN),
        V::Int32(i32::MIN),
        V::Int64(i64::MIN),
        V::UInt8(u8::MAX),
        V::UInt16(u16::MAX),
        V::UInt32(u32::MAX),
        V::UInt64(u64::MAX),
        V::Float32(-0.0),
        V::Float64(f64::MIN_POSITIVE),
        V::Float64(f64::NAN),
    ];
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for value in values {
            let ty = value.value_type();
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(&format!("struct State {{ value: {ty} }} fn make(value: {ty}) -> State {{ State {{ value: value }} }} fn get(state: &State) @noescape -> {ty} {{ state.value }}")).unwrap();
            let id = vm.create_instance("make", &[value]).unwrap();
            let result = vm.call_instance(id, "get", &[]).unwrap();
            match value {
                V::Float64(v) if v.is_nan() => {
                    assert!(matches!(result, V::Float64(r) if r.is_nan()))
                }
                _ => assert_eq!(result.bits(), value.bits(), "{mode:?}: {ty}"),
            }
            vm.close_instance(id).unwrap();
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}

#[test]
fn persistent_instance_metadata_is_checked_and_does_not_execute_source() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        let signature = vm.instance_function_signature("advance").unwrap();
        assert_eq!(signature.params.len(), 3);
        assert_eq!(signature.ret, crate::Type::Int32);
        for name in [
            "escaping",
            "suspended",
            "borrowed",
            "make_pointer",
            "missing",
        ] {
            assert!(vm.instance_function_signature(name).is_none());
        }
        assert!(vm.instance_function_signature("bad_make").is_some());
        assert!(vm.instance_function_signature("make_text").is_some());
        assert_eq!(vm.instance_count(), 0);
        assert_eq!(vm.instance_bytes(), 0);
    }
}

#[path = "instance_owner_tests.rs"]
mod owners;
