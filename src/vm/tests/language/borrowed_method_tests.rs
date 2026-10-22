use super::{VmEngine, VmMode};
use crate::{ErrorCode, FunctionSignature, Type};

const SOURCE: &str = include_str!("../../../../tests/fixtures/borrowed_methods.jk");
const ENTRIES: &[&str] = &[
    "borrowed_method_locals",
    "borrowed_method_fields",
    "borrowed_method_control",
    "borrowed_method_layouts",
    "borrowed_method_patterns",
    "borrowed_method_shared_content",
];

#[test]
fn borrowed_receivers_execute_generics_fields_and_layouts_in_both_vm_modes() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for prelude in [true, false] {
            let mut vm = VmEngine::with_mode(mode);
            vm.set_prelude_enabled(prelude);
            let exports = vm.load_source(SOURCE).unwrap();
            assert!(!exports.iter().any(|name| name.contains('.')));
            for entry in ENTRIES {
                assert_eq!(vm.call_int32_0(entry).unwrap(), 42, "{mode:?}: {entry}");
                crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
            }
            assert_eq!(
                vm.call_async("borrowed_method_value_async", &[])
                    .unwrap()
                    .cps_result(),
                Some(42)
            );
        }
    }
}

#[test]
fn borrowed_receivers_reject_invalid_syntax() {
    for source in [
        "fn f(&self) {}",
        "extern fn f(&mut self)",
        "struct P {} impl P { fn f(n: Int32, &self) {} }",
        "struct P {} impl P { fn f(&self: &P) {} }",
        "struct P {} impl P { fn f(&mut other) {} }",
        "struct P {} impl P { fn f(&self, self) {} }",
    ] {
        assert!(crate::parser::parse(source).is_err(), "{source}");
    }
    let module = crate::parser::parse(
        "struct P {} impl P { fn a(&self) {} fn b(&mut self) {} fn c(self) {} }",
    )
    .unwrap();
    let methods = &module.implementations[0].methods;
    assert!(!methods[0].params[0].ty.reference_type().unwrap().mutable);
    assert!(methods[1].params[0].ty.reference_type().unwrap().mutable);
    assert!(methods[2].params[0].ty.reference_type().is_none());
}

#[test]
fn borrowed_receivers_reject_conflicts_escapes_and_unsupported_places() {
    for (body, message) in [
        ("fn bad() { let c = Counter { value: 1 }; c.add(1) }", "var binding"),
        ("fn bad() { var c = Counter { value: 1 }; let view = c.view(); c.add(1); view.read(); }", "borrow"),
        ("fn bad() { var c = Counter { value: 1 }; let view = c.exclusive(); c.read(); view.add(1) }", "borrow"),
        ("fn bad() { var c = Counter { value: 1 }; c.add(c.read()) }", "borrow"),
        ("fn bad() { var c = Counter { value: 1 }; c.add({ c.value = 2; 1 }) }", "borrow"),
        ("fn bad() { var c = Counter { value: 1 }; let view = c.view(); c = Counter { value: 2 }; view.read(); }", "borrow"),
        ("fn bad() -> &Counter { let c = Counter { value: 1 }; c.view() }", "local"),
        ("fn bad() { let view = { let c = Counter { value: 1 }; c.view() }; view.read(); }", "local"),
        ("fn bad() { let c = (&*(alloc(Counter { value: 1 }))); c.add(1) }", "Argument type mismatch"),
        ("fn bad() { var c = Counter { value: 1 }; let p = &mut c; let moved = p; p.add(1); moved.read(); }", "moved"),
        ("fn bad() { Counter { value: 1 }.add(1); }", "temporary borrowing"),
        ("fn bad() { let view = Counter { value: 1 }.view(); view.read(); }", "Implicit borrowing"),
        ("fn bad() { let view = &Counter { value: 1 }; view.read(); }", "temporary borrowing"),
        ("fn bad() { let c = Counter { value: 1 }; Counter.view(c); }", "Implicit borrowing"),
        ("fn bad() { Counter.add(alloc(Counter { value: 0 }), true) }", "Argument type mismatch"),
        ("fn bad() { let c = alloc(ValueCounter { value: 1 }); c.read(); }", "by-value method"),
        ("impl Counter { fn bad(&self) { self.value = 0 } }", "shared reference"),
        ("impl Counter { fn bad(&self) -> &mut Self { self } }", "Return type mismatch"),
        ("struct Saved { view: &Counter } impl Counter { fn keep(&self) { alloc(Saved { view: self }); } } fn bad() { let c = Counter { value: 1 }; c.keep() }", "retained"),
        ("struct Holder { item: Counter } fn bad() { let p = alloc(Holder { item: Counter { value: 1 } }); let view = p.item.view(); view.read(); }", "Implicit borrowing"),
        ("struct View { value: &Counter } impl View { fn leak(&self) -> &Counter { self.value } } fn bad() { let c: &Counter = alloc(Counter { value: 1 }); let view = View { value: c }; view.leak(); }", "Implicit borrowing"),
        ("fn bad<T>(value: T) { value.read(); }", "no method"),
    ] {
        let source = format!("{SOURCE} {body}");
        let error = super::support::frontend_error(&source, &crate::ModuleSources::new());
        assert!(error.message().contains(message), "{error}\n{source}");
        assert!(error.span().is_some());
    }
}

#[test]
fn borrowed_receivers_preserve_definition_scope_visibility_and_consumer_types() {
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "cells",
            r#"
        pub struct Cell<T> { pub value: T }
        fn keep<T>(value: T) -> T { value }
        impl<T: Copy> Cell<T> {
            fn hidden(&self) -> T { keep(self.value) }
            pub fn read(&self) -> T { self.hidden() }
            pub fn set(&mut self, value: T) { self.value = keep(value) }
            pub fn view(&self) -> &Self { self }
            pub fn pair<U>(&self, other: U) -> (T, U) { (self.read(), keep(other)) }
        }
    "#,
        )
        .unwrap();
    modules
        .insert(
            "left",
            "import cells.{type Cell} pub fn make() -> Cell<Int64> { Cell { value: 20 } }",
        )
        .unwrap();
    modules
        .insert(
            "right",
            "import cells.{type Cell} pub fn read(c: &Cell<Int64>) -> Int64 { c.read() }",
        )
        .unwrap();
    let source = r#"
        import cells as cells.{type Cell as Saved}
        import left
        import right
        struct Cell<T> { wrong: T }
        struct Payload { value: Int64, valid: Bool }
        fn keep<T>(value: T) -> Bool { false }
        fn check() -> Int32 {
            var c = left.make()
            c.set(42)
            let view = c.view()
            let (value, payload) = cells.Cell::<Int64>.pair::<Payload>(view, Payload { value: 4294967297, valid: true })
            let (again, valid) = view.pair(payload.valid)
            let explicit = Saved.read::<Int64>(&c)
            if value == 42 && again == explicit && right.read(view) == explicit && valid && payload.value == 4294967297 { 42 } else { 0 }
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_source_with_modules(source, &modules).unwrap();
        assert_eq!(vm.call_int32_0("check").unwrap(), 42);
        for bad in [
            "import cells fn bad() { let c = cells.Cell { value: 42 }; c.hidden(); }",
            "import cells struct Cell<T> { value: T } fn bad() { let c = Cell { value: 42 }; cells.Cell.read(&c); }",
        ] {
            assert!(vm.load_source_with_modules(bad, &modules).is_err());
            assert_eq!(vm.call_int32_0("check").unwrap(), 42);
        }
    }
    let ast = crate::TypeRegistry::default()
        .resolve_with_modules(&crate::parser::parse(source).unwrap(), &modules)
        .unwrap();
    crate::aot::AotCompiler::new("imported-borrowed-methods")
        .unwrap()
        .compile_module(&ast)
        .unwrap();
}

thread_local! {
    static READY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) };
}
extern "C" fn record(value: i32) -> i32 {
    EVENTS.with(|events| events.borrow_mut().push(value));
    value
}
unsafe extern "C" fn delay(frame: *mut crate::runtime::cps::CpsFrame) -> i32 {
    let frame = &mut *frame;
    if frame.state == 0 {
        record(-(frame.slots[0] as i32));
        frame.state = 1;
    }
    if !READY.with(std::cell::Cell::get) {
        return 1;
    }
    frame.result = frame.slots[0];
    0
}

#[test]
fn borrowed_receivers_preserve_evaluation_order_pending_errors_and_cancellation() {
    use crate::runtime::{arena::LIVE, scheduler::Scheduler, suspension::ResumeError};
    let source = format!(
        r#"
        {SOURCE}
        extern fn record(value: Int32) -> Int32
        extern fn delay(value: Int32) @suspends -> Int32
        impl Counter {{
            fn wait(&mut self, value: Int32) @suspends {{ self.add(delay(value)) }}
            fn fail(&self) @suspends -> Int32 {{ self.value / delay(0) }}
        }}
        fn receiver(value: &mut Counter) -> &mut Counter {{ record(1); value }}
        fn pending() @suspends -> Int32 {{
            let c = alloc(Counter {{ value: 20 }})
            receiver(c).wait(record(22))
            c.read()
        }}
        fn failure() @suspends -> Int32 {{ alloc(Counter {{ value: 42 }}).fail() }}
        fn branches() @suspends -> Int32 {{
            let left = alloc(Counter {{ value: 0 }})
            let right = alloc(Counter {{ value: 0 }})
            parallel {{ | left.wait(20) | right.wait(22) }}
            left.read() + right.read()
        }}
    "#
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for outcome in ["finish", "drop", "cancel", "failure", "branches"] {
            READY.with(|ready| ready.set(false));
            EVENTS.with(|events| events.borrow_mut().clear());
            let mut vm = VmEngine::with_mode(mode);
            unsafe {
                vm.register_fn(
                    "record",
                    FunctionSignature::new(vec![Type::Int32], Type::Int32),
                    record as *const u8,
                )
                .unwrap();
                vm.register_suspending_fn(
                    "delay",
                    FunctionSignature::new(vec![Type::Int32], Type::Int32),
                    delay,
                )
                .unwrap();
            }
            vm.load_source(&source).unwrap();
            let entry = match outcome {
                "failure" => "failure",
                "branches" => "branches",
                _ => "pending",
            };
            let mut task = vm.call_async(entry, &[]).unwrap();
            assert!(!task.is_finished());
            let initial = EVENTS.with(|events| events.borrow().clone());
            if entry == "pending" {
                assert_eq!(initial, [1, 22, -22]);
            }
            for _ in 0..3 {
                task.resume().unwrap();
            }
            EVENTS.with(|events| assert_eq!(*events.borrow(), initial));
            LIVE.with(|live| assert_eq!(live.get(), if entry == "branches" { 2 } else { 1 }));
            vm.load_source("fn replacement() -> Int32 { 7 }").unwrap();
            drop(vm);
            match outcome {
                "drop" => drop(task),
                "cancel" => {
                    let mut scheduler = Scheduler::new();
                    let id = scheduler.suspend(task);
                    scheduler.cancel(id).unwrap();
                }
                "failure" => {
                    READY.with(|ready| ready.set(true));
                    let ResumeError::Failed(error) = task.resume().unwrap_err() else {
                        panic!()
                    };
                    assert_eq!(error.code(), ErrorCode::RuntimeError);
                    assert!(error.span().is_some());
                }
                _ => {
                    READY.with(|ready| ready.set(true));
                    for _ in 0..100 {
                        if task.is_finished() {
                            break;
                        }
                        task.resume().unwrap();
                    }
                    assert_eq!(task.cps_result(), Some(42));
                }
            }
            LIVE.with(|live| assert_eq!(live.get(), 0));
        }
    }
}
