//! joky/vm builtin calls inside user expressions.

use super::support::*;
use super::VmMode;

#[test]
fn conditional_builtin_calls_and_opaque_results_resolve_and_execute() {
    let source = r#"
        import joky/vm
        fn compute(x: Int32) -> Int32 {
            let engine = if x == 0 { vm.create_interpreter() } else { vm.create() }
            { let vm_text = "fn value() -> Int32 { 42 }" vm.load_source(engine, &vm_text) }
            let result = if { let vm_text = "value" vm.call_int32(engine, &vm_text) } == 42 && { let vm_text = "value" vm.call_int32(engine, &vm_text) } != 0 {
                { let vm_text = "value" vm.call_int32(engine, &vm_text) }
            } else { { let vm_text = "value" vm.call_int32(engine, &vm_text) } + 1 }
            vm.close(engine)
            result
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        for input in [0, 1] {
            assert_eq!(vm.call_int32_1("compute", input).unwrap(), 42);
        }
    }
}

#[test]
fn builtin_calls_inside_sequences_resolve_and_execute() {
    let source = r#"
        import joky/vm;
        fn compute() -> Int32 {
            let engine = vm.create_interpreter();
            let result = ({ let vm_text = "fn value() -> Int32 { 42 }" vm.load_source(engine, &vm_text) };
                          -{ let vm_text = "value" vm.call_int32(engine, &vm_text) });
            vm.close(engine);
            result
        };
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        assert_eq!(
            parity_vm(source, mode).call_int32_0("compute").unwrap(),
            -42
        );
    }
}

#[test]
fn builtin_calls_resolve_inside_mutable_initializers_assignments_and_loops() {
    let source = r#"
        import joky/vm
        fn compute() -> Int32 {
            var engine: vm.Handle = vm.create_interpreter()
            vm.close(engine)
            engine = vm.create_interpreter()
            var source = "fn value() -> Int32 { 7 }"
            var i = 0
            var total = 0
            while i < 2 {
                var status = vm.reload_source(engine, &source)
                total = total + { let vm_text = "value" vm.call_int32(engine, &vm_text) } + status
                source = "fn value() -> Int32 { 9 }"
                i = i + 1
            }
            while vm.error_code() != 0 { vm.close(engine) }
            vm.close(engine)
            total
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        assert_eq!(parity_vm(source, mode).call_int32_0("compute").unwrap(), 16);
    }
}
