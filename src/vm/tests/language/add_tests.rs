use super::{support::*, VmMode};
use crate::runtime::arena::LIVE;
const SOURCE: &str = include_str!("../../../../tests/fixtures/add_operators.jk");
fn clean() {
    LIVE.with(|n| assert_eq!(n.get(), 0));
}
#[test]
fn add_operators_dispatch_and_transfer_owners_in_source_order() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(SOURCE, mode);
        assert_eq!(vm.call_int32_0("add_values").unwrap(), 42);
        let mut task = vm.call_async("add_pending", &[]).unwrap();
        for _ in 0..3 {
            task.resume().unwrap();
        }
        EVENTS.with(|v| assert_eq!(*v.borrow(), [120, 21]));
        READY.with(|v| v.set(true));
        while !task.is_finished() {
            task.resume().unwrap();
        }
        assert_eq!(task.cps_result(), Some(42));
        drop(task);
        EVENTS.with(|v| assert_eq!(*v.borrow(), [120, 21, 122, 300, 20, 22, 42]));
        clean();
        READY.with(|v| v.set(false));
        EVENTS.with(|v| v.borrow_mut().clear());
        let mut task = vm.call_async("add_failed_rhs", &[]).unwrap();
        READY.with(|v| v.set(true));
        assert!(task.resume().is_err());
        drop(task);
        EVENTS.with(|v| assert_eq!(*v.borrow(), [120, 21, 20]));
        clean();
        assert!(vm
            .call_int32_0("add_overflow")
            .unwrap_err()
            .to_string()
            .contains("Duration addition overflow"));
        clean();
    }
}
fn cancel(sweep: CancellationSweep) {
    let mut vm = parity_vm(SOURCE, VmMode::Interpreter);
    assert_cancellation(
        &mut vm,
        "add_pending",
        10000,
        sweep,
        || {
            READY.with(|v| v.set(true));
            EVENTS.with(|v| v.borrow_mut().clear());
        },
        || EVENTS.with(|v| v.borrow().clone()),
        clean,
    );
}
#[test]
fn add_operators_cancel_at_resource_boundaries() {
    cancel(CancellationSweep::ResourceBoundaries);
}
#[test]
#[ignore = "exhaustive: cancel consuming addition at every instruction"]
fn exhaustive_add_operators_cancellation() {
    cancel(CancellationSweep::EveryInstruction);
}
#[test]
fn add_operators_require_the_standard_trait_and_preserve_type_boundaries() {
    for source in [
        "struct N{n:Int32} impl N{fn add(self,r:N)->N{self}} fn f(a:N,b:N)->N{a+b}",
        "trait Add{fn add(self,r:Self)->Self} struct N{n:Int32} impl Add for N{fn add(self,r:N)->N{self}} fn f(a:N,b:N)->N{a+b}",
        "import joky/ops fn f<T>(a:T,b:T)->T{a+b}",
        "fn f(){let d=1s+1;}",
        "fn f(){let a=1s; let b=2s; let d=(&a)+(&b);}",
        "fn f(){var d=1s; d+=1s;}",
        "#state struct S{n:Int32} impl S{#every(1s+250ms) fn tick(&mut self){}}",
        "import joky/ops.{type Add} struct N{s:String} impl Add for N{fn add(self,r:N)->N{self}} fn f(a:N,b:N)->N{let c=a+b; a}",
    ] { frontend_error(source, &crate::ModuleSources::new()); }
    let vm = parity_vm_with_prelude(
        "fn f()->Int32{int32((1s+250ms).as_millis())}",
        VmMode::Interpreter,
        false,
    );
    assert_eq!(vm.call_int32_0("f").unwrap(), 1250);
}

#[test]
fn add_operators_keep_identity_across_import_aliases_and_hidden_trait_scopes() {
    let mut modules = crate::ModuleSources::new();
    modules.insert("counts", "import joky/ops.{type Add as Sum} pub struct Count {pub value:Int32} impl Sum for Count {fn add(self, other:Count)->Count {Count {value:self.value+other.value}}}").unwrap();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = super::VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_source_with_modules("import counts.{type Count} trait Add{fn add(self,other:Self)->Self} fn f()->Int32{(Count{value:20}+Count{value:22}).value}", &modules).unwrap();
        assert_eq!(vm.call_int32_0("f").unwrap(), 42);
    }
}

#[test]
fn add_operators_infer_long_numeric_and_duration_chains() {
    let numeric = (1..=32)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(" + ");
    let durations = (1..=32)
        .map(|n| format!("{n}ms"))
        .collect::<Vec<_>>()
        .join(" + ");
    let source = format!("fn numeric()->Int32{{{numeric}}} fn durations()->Int32{{int32(({durations}).as_millis())}}");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(&source, mode);
        assert_eq!(vm.call_int32_0("numeric").unwrap(), 528);
        assert_eq!(vm.call_int32_0("durations").unwrap(), 528);
    }
}
