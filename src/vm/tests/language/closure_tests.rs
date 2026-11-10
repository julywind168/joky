//! Closure behavior and rejected programs must agree across generated-code backends.
use super::{VmEngine, VmMode};

fn executes(source: &str, expected: i32) {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap_or_else(|error| {
            panic!(
                "{source}\n{error} at {:?}",
                error.span().and_then(|s| error
                    .sources()
                    .get(s.source_id())
                    .and_then(|f| s.slice(f.text())))
            )
        });
        assert_eq!(vm.call_int32_0("probe").unwrap(), expected, "{mode:?}");
    }
    crate::backends::aot::tests::assert_sync_int32(
        &crate::parser::parse(source).unwrap(),
        "probe",
        expected,
    );
}

#[track_caller]
fn rejects(source: &str) {
    super::support::frontend_error(source, &crate::ModuleSources::new());
}

#[test]
fn closure_storage_rejects_borrow_escape_copy_and_unsupported_storage() {
    for body in [
        "let n = 1; let f = || { n }; alloc(wrap(f))",
        "let n = 1; let r = &n; let f = move || { *r }; alloc(wrap(f))",
        "let p = alloc(1); let refs = [&*p]; let f = move || { refs.len() }; wrap(f)",
        "let n = 1; let f = || { n }; let g = move || { f() }; alloc(wrap(g))",
        "let f = move || { 1 }; let h = wrap(f); let other = h; let again = h",
        "let f = move || { 1 }; let h = wrap(f); let other = h.value; let again = h.value",
        "let f = move || { 1 }; let h = wrap(f); let r = &h; let other = h; consume_ref(r)",
        "let f = move || { 1 }; let t = (f, 1)",
        "let f = move || { 1 }; let a = [f]",
        "let f = move || { 1 }; let h = wrap(f); let a = [h]",
        "let f = move || { 1 }; let e = Some(f)",
    ] {
        let source = format!("struct Holder<T> {{ value: T }} fn wrap<T>(value: T) -> Holder<T> {{ Holder {{ value }} }} fn consume_ref<T>(value: &Holder<T>) {{}} fn probe() {{ {body}; {{}} }}");
        super::support::frontend_error(&source, &crate::ModuleSources::new());
    }
}

#[test]
fn closure_noncapturing_identity_and_move_rejections() {
    executes(
        "fn probe() -> Int32 { let n = 42; var f = || { n }; let g = f; f = g; f() }",
        42,
    );
    executes("import joky/string fn probe() -> Int32 { let n = \"owner\"; var f = move || { string.byte_length(&n) }; let g = f; f = g; if f() == 5 { 42 } else { 0 } }", 42);
    executes(
        "fn probe() -> Int32 { var f = || { 42 }; let g = f; f = g; g() }",
        42,
    );
    executes(
        "fn probe() -> Int32 { let f = || { 21 }; let p = &f; let g = f; p() + g() }",
        42,
    );
    executes(
        "fn probe() -> Int32 { var f = || { 42 }; let g = f; f = g; f() }",
        42,
    );
    rejects("fn probe() { let f = |x| { x + 1 } }");
    rejects("fn probe() { let f: fn(Int32) -> Bool = |x| { x + 1 } }");
    rejects("fn probe() { let f = || { break } }");
    rejects("fn probe() { while true { let f = || { continue } } }");
    rejects("fn probe() { let f = || { f() } }");
    rejects("fn probe() { let f = if true { || { 1 } } else { || { 1 } } }");
    executes(
        "fn probe() -> Int32 { let a = || { 21 }; let b = a; a() + b() }",
        42,
    );
    executes(
        "fn probe() -> Int32 { let a = || { 21 }; let b: fn() -> Int32 = a; a() + b() }",
        42,
    );
}

#[test]
fn closure_borrowed_captures_and_exclusive_updates() {
    executes("fn probe() -> Int32 { let offset = 10; let shift = |x: Int32| { x + offset }; shift(20) + shift(2) }", 42);
    executes("fn probe() -> Int32 { var count = 40; { var tick = || { count = count + 1 }; tick(); tick() }; count }", 42);
    executes("struct Point { x: Int32, y: Int32 } fn probe() -> Int32 { var p = Point { x: 10, y: 20 }; { var f = || { p.x = p.x + 1; p.y = p.y + 1 }; f(); f(); f(); f(); f(); f() }; p.x + p.y }", 42);
}

#[test]
fn closure_borrowed_capture_conflicts_and_escape_rejections() {
    rejects("fn probe() { var n = 1; let f = || { n }; n = 2; f() }");
    rejects("fn probe() { let n = 1; let f = || { n = 2 } }");
    rejects("fn probe() { var n = 1; let f = || { n = 2 }; f() }");
    rejects("fn probe() { var n = 1; var f = || { n = 2 }; let g = || { n }; f(); g() }");
    rejects("fn probe() { let text = \"owned\"; let f = || -> String { text } }");
    rejects("fn probe() { let n = 1; let f = || -> &Int32 { &n } }");
    rejects("fn probe() { let n = 1; let f = || { n }; let p: fn() -> Int32 = f }");
    rejects("fn probe() { var n = 0; let f = || { n += 1 }; let pair = (f, 1) }");
    rejects("fn bad<F: FnMut() -> Int32>(f: &mut F) { let pair = (f, 1) } fn probe() {}");
    rejects("fn probe() { var n = 0; var f = |x: Int32| { n = n + x; n }; f({ f(1); 2 }) }");
    rejects("fn probe() { let value = \"once\"; let f = move |x: Int32| { let owned = value; x }; f({ f(1); 2 }) }");
}

#[test]
fn closure_move_snapshots_and_consumes_owning_captures() {
    executes(
        "fn probe() -> Int32 { var n = 40; let f = move || { n + 2 }; n = 0; f() }",
        42,
    );
    executes("import joky/string fn probe() -> Int32 { let value = \"hello\"; let take = move || -> String { value }; let result = take(); if string.byte_length(&result) == 5 { 42 } else { 0 } }", 42);
    executes("import joky/string fn probe() -> Int32 { let value = \"hello\"; let f = move || { string.byte_length(&value) }; if f() + f() == 10 { 42 } else { 0 } }", 42);
    rejects(
        "fn probe() { let value = \"owned\"; let take = move || -> String { value }; take(); take() }",
    );
    rejects(
        "fn probe() { let value = \"owned\"; let f = move || { println(value) }; println(value) }",
    );
}

#[test]
fn closure_replaces_owners_without_consuming_the_environment() {
    executes("import joky/string fn probe() -> Int32 { var value = \"a\"; var update = move || { value = \"hello\"; string.byte_length(&value) }; if update() + update() == 10 { 42 } else { 0 } }", 42);
    executes("import joky/string fn probe() -> Int32 { var value = \"a\"; { var update = || { value = \"hello\" }; update(); update() }; if string.byte_length(&value) == 5 { 42 } else { 0 } }", 42);
}

thread_local! {
    static EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) };
}
extern "C" fn record(value: i32) {
    EVENTS.with(|e| e.borrow_mut().push(value));
}
const OWNERS: &str = include_str!("../../../../tests/fixtures/closures.jk");
fn owner_vm(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    unsafe {
        vm.register_fn(
            "closure_event",
            crate::FunctionSignature::new(vec![crate::Type::Int32], crate::Type::Unit),
            record as *const u8,
        )
        .unwrap();
    }
    vm.load_source(OWNERS)
        .unwrap_or_else(|error| panic!("{error}"));
    vm
}
#[test]
fn closure_owning_cleanup_success_skip_consume_and_failure() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = owner_vm(mode);
        for (name, expected, error) in [
            ("stored_work", vec![-1, -2], None),
            ("stored_unused", vec![-1], None),
            ("stored_replace", vec![-1, -2], None),
            (
                "stored_failure",
                vec![-1, -2],
                Some("Stored closure failure"),
            ),
            ("unused", vec![-1, -2], None),
            ("lazy_skipped", vec![-1, -2], None),
            ("lazy_consumed", vec![-1], None),
            ("lazy_cancellation_work", vec![-1, -2], None),
            ("lazy_error_consumed", vec![-1], None),
            ("query_owned", vec![-1, -2], None),
            ("query_temporary", vec![-1, 100], None),
            ("lazy_failure", vec![-1, -2], Some("Closure failure")),
            ("consume", vec![-1], None),
            ("repeated", vec![-1], None),
            ("skipped", vec![-1], None),
            ("consumed_helper", vec![-1, -2], None),
            ("failure", vec![-1, -2], Some("Closure failure")),
            ("cancellation_work", vec![-1, -2], None),
            ("consume_last", vec![-1, -2, -3], None),
            ("argument_order", vec![100, 200, 300, 400], None),
            (
                "consuming_argument_failure",
                vec![-1],
                Some("Closure failure"),
            ),
            (
                "consuming_failure",
                vec![-1, -2, -3],
                Some("Closure failure"),
            ),
            (
                "failure_before_cleanup_failure",
                vec![-4, -5, -1],
                Some("Closure failure"),
            ),
        ] {
            EVENTS.with(|e| e.borrow_mut().clear());
            let result = vm.call_int32_0(name);
            if let Some(error) = error {
                assert_eq!(result.unwrap_err().message(), error);
            } else {
                assert_eq!(result.unwrap(), 42, "{mode:?} {name}");
            }
            EVENTS.with(|e| assert_eq!(*e.borrow(), expected, "{mode:?} {name}"));
            crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}
fn cancellation(sweep: super::support::CancellationSweep) {
    let mut vm = owner_vm(VmMode::Interpreter);
    for entry in ["closure_task", "lazy_closure_task", "stored_task"] {
        super::support::assert_cancellation(
            &mut vm,
            entry,
            100_000,
            sweep,
            || EVENTS.with(|e| e.borrow_mut().clear()),
            || EVENTS.with(|e| e.borrow().clone()),
            || {
                crate::runtime::arena::LIVE.with(|live| {
                    assert_eq!(live.get(), 0);
                });
                crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
            },
        );
    }
}
#[test]
fn closure_interpreter_cancellation_at_resource_boundaries() {
    cancellation(super::support::CancellationSweep::ResourceBoundaries);
}
#[test]
#[ignore = "exhaustive: cancel at every closure instruction boundary"]
fn exhaustive_closure_interpreter_cancellation_every_instruction() {
    cancellation(super::support::CancellationSweep::EveryInstruction);
}
#[test]
fn closure_interpreter_keeps_old_entries_alive_across_reload() {
    let mut vm = owner_vm(VmMode::Interpreter);
    vm.set_interpreter_limits(super::InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    EVENTS.with(|e| e.borrow_mut().clear());
    let mut task = vm.call_async("closure_task", &[]).unwrap();
    while !EVENTS.with(|e| e.borrow().contains(&-2)) {
        task.resume().unwrap();
    }
    vm.load_source("fn replacement() -> Int32 { 7 }").unwrap();
    drop(vm);
    for _ in 0..100_000 {
        if task.is_finished() {
            break;
        }
        task.resume().unwrap();
    }
    assert_eq!(task.cps_result(), Some(42));
    drop(task);
    EVENTS.with(|e| assert_eq!(*e.borrow(), [-1, -2]));
    crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
}

#[test]
fn closure_fixed_seed_model_preserves_snapshots_and_repeated_updates() {
    let mut source =
        String::from("fn apply<F: FnOnce(Int32) -> Int32>(f: F, x: Int32) -> Int32 { f(x) }\n");
    for (case, seed) in [1, 7, 17, 31, 73, 101, 509, 1009].into_iter().enumerate() {
        let mut state = seed;
        let mut expected = 0;
        for i in 0..64 {
            let delta = (i * 17 + seed) % 13 - 6;
            let snapshot = state;
            state += delta;
            expected += state + snapshot + delta;
        }
        source.push_str(&format!("fn model{case}() -> Int32 {{ var state = {seed}; var sum = 0; var i = 0; while i < 64 {{ let delta = (i * 17 + {seed}) - ((i * 17 + {seed}) / 13) * 13 - 6; let snapshot = move |x: Int32| {{ state + x }}; {{ var update = |x: Int32| {{ state = state + x; state }}; sum = sum + update(delta) }}; sum = sum + apply(snapshot, delta); i = i + 1 }}; if sum == {expected} {{ 42 }} else {{ 0 }} }}\n"));
    }
    source.push_str("fn probe() -> Int32 { if ");
    source.push_str(
        &(0..8)
            .map(|i| format!("model{i}() == 42"))
            .collect::<Vec<_>>()
            .join(" && "),
    );
    source.push_str(" { 42 } else { 0 } }");
    executes(&source, 42);
}

#[test]
fn closure_limits_and_reserved_capability_names_are_checked() {
    let mut source = String::new();
    for i in 0..2049 {
        source.push_str(&format!("fn probe_{i}() {{ let f = || {{ 1 }} }}"));
    }
    let error = super::support::frontend_error(&source, &crate::ModuleSources::new());
    assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
    assert_eq!(error.message(), "Generated function limit 4096 exceeded (0 generic instances + 4096 closure entries, requesting 2 more)");
    rejects("struct Fn { n: Int32 } fn probe() {}");
    rejects("trait FnMut { fn call(&self) } fn probe() {}");
    rejects("type FnOnce = Int32 fn probe() {}");
    rejects("import joky/option.{type Option as Fn} fn probe() {}");
    rejects("fn bad<Fn>(x: Fn) {} fn probe() {}");
    rejects("fn bad<F: Fn() -> Int32 + Fn(Bool) -> Int32>(f: F) {} fn probe() {}");
    rejects("fn bad<F: Fn() @suspends -> Int32>(f: F) {} fn probe() {}");
    rejects("fn probe() { let f = || { 1 }; let duplicate = (*(&f)) }");
    rejects("struct Box<T> { value: T } fn make<T>(x: T) -> Box<T> { Box { value: x } } fn probe() { let f = || { 1 }; make(f) }");
}

#[test]
fn closure_entries_survive_native_and_interpreted_task_reload() {
    let source = "extern fn delayed_value(x: Int32) @suspends -> Int32 fn helper() -> Int32 { let n = 20; let f = || { n + 1 }; f() } fn old_task() @suspends -> Int32 { let first = helper(); delayed_value(0); first + helper() }";
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = super::support::parity_vm(source, mode);
        let mut task = vm.call_async("old_task", &[]).unwrap();
        assert!(!task.is_finished());
        vm.load_source("fn replacement() -> Int32 { 7 }").unwrap();
        drop(vm);
        super::support::READY.with(|r| r.set(true));
        for _ in 0..1000 {
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        assert_eq!(task.cps_result(), Some(42));
    }
}

#[test]
fn closure_instruction_budget_failure_releases_initialized_resources() {
    let mut vm = owner_vm(VmMode::Interpreter);
    for budget in [0, 1, 3, 10, 31, 63, 127, 255, 511, 1023] {
        vm.set_interpreter_limits(super::InterpreterLimits {
            instruction_budget: Some(budget),
            ..Default::default()
        })
        .unwrap();
        EVENTS.with(|e| e.borrow_mut().clear());
        let result = vm.call_int32_0("cancellation_work");
        if let Ok(value) = result {
            assert_eq!(value, 42);
        }
        crate::runtime::arena::LIVE.with(|live| {
            assert_eq!(live.get(), 0);
        });
        crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0, "budget {budget}"));
    }
}

#[test]
fn generic_functions_and_closures_above_the_old_limit_execute_in_all_backends() {
    let mut source = String::new();
    for i in 0..300 {
        source.push_str(&format!("fn id_{i}<T>(x: T) -> T {{ x }}"));
    }
    for i in 0..130 {
        source.push_str(&format!(
            "fn closure_{i}() -> Int32 {{ let f = || {{ 1 }}; f() }}"
        ));
    }
    source.push_str("fn probe() -> Int32 { var total = 0");
    for i in 0..300 {
        source.push_str(&format!("; total = total + id_{i}(1)"));
    }
    for i in 0..130 {
        source.push_str(&format!("; total = total + closure_{i}()"));
    }
    source.push_str("; total }");
    executes(&source, 430);
}
