//! Iterator syntax shares loop planning while fixing standard protocol identity.
use super::support::*;
use super::{VmEngine, VmMode};
use crate::runtime::arena::LIVE;

const SOURCE: &str = include_str!("../../../../tests/fixtures/for_iterators.jk");
const EXPECTED: &[i32] = &[3, -1, 1, -2, 2, 99, -1, 1, -2, 2, -3, 3, 99, -1, 1, 99];

#[test]
fn for_into_iter_converts_once_and_preserves_generic_protocol_selection() {
    let source = include_str!("../../../../tests/fixtures/for_into_iter.jk");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(source, mode, false);
        assert_eq!(vm.call_int32_0("compute").unwrap(), 42);
        EVENTS.with(|events| assert_eq!(*events.borrow(), [100, 101, 101]));
        LIVE.with(|live| assert_eq!(live.get(), 0));
        assert!(vm.call_unit_0("conversion_failure").is_err());
        EVENTS.with(|events| assert_eq!(*events.borrow(), [100, 101, 101, 0]));
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

#[test]
fn for_into_iter_keeps_container_identity_in_imported_generic_functions() {
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "app/loops",
            r#"
        import joky/array.{type Array}
        import joky/map.{type Map}
        import joky/iter.{type IntoIterator}
        pub fn count<I: IntoIterator>(input: I) -> Int32 {
            var n = 0
            for index, _ in input { n += 1 + index - index }
            n
        }
        pub fn mutate(input: &mut Array<Int32>) {
            for value in input { *value += 10 }
        }
        pub fn sum<K, V: Copy>(input: &Map<K, V>) -> Int32 {
            var n = 0
            for index, _ in input { n += 1 + index - index }
            n
        }
    "#,
        )
        .unwrap();
    let source = r#"
        import app/loops
        import joky/map.{type Map}
        fn compute() -> Int32 {
            var a = [10, 10]
            loops.mutate(&mut a)
            var m = Map.new::<Int32, Int32>()
            m.insert(1, 1)
            var n = 0
            for v in &a { n += *v }
            n + loops.count(a) * loops.sum(&m)
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_source_with_modules(source, &modules).unwrap();
        assert_eq!(vm.call_int32_0("compute").unwrap(), 42);
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

#[test]
fn for_into_iter_rejects_fake_protocols_and_conflicting_borrows() {
    for source in [
        "import joky/iter.{range, type Range} struct Fake {} impl Fake { fn into_iter(self) -> Range<Int32> { range(0, 1) } } fn bad() { for _ in (Fake {}) {} }",
        "import joky/iter.{range, type Range} trait IntoIterator { type IntoIter; fn into_iter(self) -> Self.IntoIter } struct Fake {} impl IntoIterator for Fake { type IntoIter = Range<Int32>; fn into_iter(self) -> Range<Int32> { range(0, 1) } } fn bad() { for _ in (Fake {}) {} }",
        "import joky/iter.{range, type Range} struct Map {} impl Map { fn iter(&self) -> Range<Int32> { range(0, 1) } } fn bad() { let m = Map {}; for _ in &m {} }",
        "import joky/iter.{type IntoIterator} struct Fake {} impl IntoIterator for Fake { type IntoIter = Int32; fn into_iter(self) -> Int32 { 1 } }",
        "import joky/iter.{type IntoIterator, type Range, range} struct Fake {} impl IntoIterator for Fake { type IntoIter = Range<Int32>; fn into_iter(self) @suspends -> Range<Int32> { range(0, 1) } }",
        "fn bad<I>(input: I) { for _ in input {} }",
        "fn bad() { var a = [1, 2]; for v in &a { a.push(3); let n = *v; } }",
        "fn bad() { var a = [1, 2]; for v in &mut a { a.push(3); *v = 3; } }",
        "fn bad() { let a = [1, 2]; for v in &a { *v = 3; } }",
        "fn bad() { let a = [1, 2]; for _ in &mut a {} }",
        "fn bad() { var a = [1, 2]; let r = &mut a; for _ in r {}; r.len(); }",
        "import joky/map.{type Map} fn bad() { var m = Map.new::<Int32, Int32>(); m.insert(1, 2); for e in &mut m { *map.entry_key(e) = 3; } }",
    ] {
        let error = frontend_error(source, &crate::ModuleSources::new());
        assert!(error.span().is_some(), "{source}: {error}");
    }
}

fn pending_source() -> String {
    format!(
        r#"{SOURCE}
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn pending() @suspends -> Int32 {{
            for index, value in owners() {{
                expect(index == value.id - 1);
                delayed_value(record(value.id + 10))
                if value.id == 1 {{ continue }}
                break
            }}
            42
        }}
    "#
    )
}

#[test]
fn for_iterators_resume_cancel_and_fail_without_replaying_next() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(&pending_source(), mode, false);
        for cancel in [false, true] {
            READY.with(|ready| ready.set(false));
            EVENTS.with(|events| events.borrow_mut().clear());
            let mut task = vm.call_async("pending", &[]).unwrap();
            for _ in 0..3 {
                task.resume().unwrap();
            }
            assert!(!task.is_finished());
            EVENTS.with(|events| assert_eq!(*events.borrow(), [-1, 11, 11]));
            if !cancel {
                READY.with(|ready| ready.set(true));
                for _ in 0..20 {
                    if task.is_finished() {
                        break;
                    }
                    task.resume().unwrap();
                }
                assert_eq!(task.cps_result(), Some(42));
            }
            drop(task);
            let expected = if cancel {
                &[-1, 11, 11, 1, 99][..]
            } else {
                &[-1, 11, 11, 1, -2, 12, 12, 2, 99][..]
            };
            EVENTS.with(|events| assert_eq!(*events.borrow(), expected));
            LIVE.with(|live| assert_eq!(live.get(), 0));
        }
        EVENTS.with(|events| events.borrow_mut().clear());
        assert!(vm.call_unit_0("force_failure").is_err());
        EVENTS.with(|events| assert_eq!(*events.borrow(), [-1, 1, 99]));
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

fn cancellation(sweep: CancellationSweep) {
    let mut vm = parity_vm_with_prelude(&pending_source(), VmMode::Interpreter, false);
    assert_cancellation(
        &mut vm,
        "pending",
        10_000,
        sweep,
        || {
            READY.with(|ready| ready.set(true));
            EVENTS.with(|events| events.borrow_mut().clear());
        },
        || EVENTS.with(|events| events.borrow().clone()),
        || {
            LIVE.with(|live| assert_eq!(live.get(), 0));
            EVENTS.with(|events| {
                let events = events.borrow();
                for id in [1, 2, 99] {
                    assert!(
                        events.iter().filter(|&&event| event == id).count() <= 1,
                        "{events:?}"
                    );
                }
            });
        },
    );
}

#[test]
fn for_iterators_cancel_at_resource_boundaries() {
    cancellation(CancellationSweep::ResourceBoundaries);
}

#[test]
#[ignore = "exhaustive: cancel iterator and item owners at every instruction boundary"]
fn exhaustive_for_iterators_instruction_cancellation() {
    cancellation(CancellationSweep::EveryInstruction);
}

#[test]
fn for_iterators_execute_protocol_patterns_and_cleanup() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(SOURCE, suspends, Suspends::Named("compute"));
            let vm = parity_vm_with_prelude(&source, mode, false);
            assert_eq!(call_int32(&vm, suspends, "compute", &[]), 42);
            EVENTS.with(|events| assert_eq!(*events.borrow(), EXPECTED));
            LIVE.with(|live| assert_eq!(live.get(), 0));
        }
    }
}

#[test]
fn for_iterators_reject_invalid_protocol_patterns_and_scope() {
    let mut vm = parity_vm(SOURCE, VmMode::Jit);
    for body in [
        "for value in 42 {}",
        "for value in range(0, 2) {}; value;",
        "for value in range(0, 2) { value = 4 }",
        "for (a, b) in range(0, 2) {}",
        "for Some(value) in range(0, 2) {}",
        "for 1 in range(0, 2) {}",
        "for (x, x) in (Pairs { remaining: 1 }) {}",
        "let source = owners(); for _ in source {}; source;",
        "for _ in range(0, 2) {}; break",
        "for _ in range(0, 2) {}; continue",
        "let result: Int32 = for _ in range(0, 2) {}",
        "for value in range(0, 2) { unknown() }",
    ] {
        assert!(
            vm.load_source(&format!("{SOURCE}\nfn bad() {{ {body} }}"))
                .is_err(),
            "{body}"
        );
    }
    for source in [
        "struct Fake {} impl Fake { fn next(&mut self) -> Option<Int32> { None } } fn bad() { for _ in (Fake {}) {} }",
        "trait Iterator { type Item; fn next(&mut self) -> Option<Self.Item> } struct Fake {} impl Iterator for Fake { type Item = Int32; fn next(&mut self) -> Option<Int32> { None } } fn bad() { for _ in (Fake {}) {} }",
        "import joky/iter.{type Iterator} struct Fake {} impl Iterator for Fake { type Item = Int32; fn next(&mut self) @suspends -> Option<Int32> { None } }",
        "fn bad<I>(source: I) { for _ in source {} }",
    ] {
        assert!(VmEngine::new().load_source(source).is_err(), "{source}");
    }
}

#[test]
fn for_iterators_bind_standard_names_and_interpolation_dependencies() {
    let source = r#"
        import joky/iter as it
        import joky/option as opt
        enum Option { Some, None }
        fn next() -> Bool { false }
        fn compute() -> Int32 {
            var sum = 0
            for value in it.range(0, 4) { sum += value }
            sum
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(source, mode, false);
        assert_eq!(vm.call_int32_0("compute").unwrap(), 6);
        let vm = parity_vm_with_prelude(
            r#"
            import joky/iter.{range}
            fn compute() -> Int32 {
                let text = "{for _ in range(0, 2) {}}"
                42
            }
        "#,
            mode,
            false,
        );
        assert_eq!(vm.call_int32_0("compute").unwrap(), 42);
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

const ADAPTERS: &str = include_str!("../../../../tests/fixtures/iterator_adapters.jk");

#[test]
fn iterator_adapters_are_lazy_stateful_fused_and_release_owned_items() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(ADAPTERS, mode, false);
        assert_eq!(vm.call_int32_0("compute").unwrap(), 42);
        assert_eq!(vm.call_int32_0("fields").unwrap(), 42);
        for (name, expected) in [
            (
                "lazy",
                vec![1, 2, 3, 4, 13, 14, 13, 14, 13, 20, 21, 30, 31, 32],
            ),
            ("cleanup", vec![1, 2, 3, 9]),
            ("zip_cleanup", vec![1, 3, 2]),
        ] {
            EVENTS.with(|events| events.borrow_mut().clear());
            vm.call_unit_0(name).unwrap();
            EVENTS.with(|events| assert_eq!(*events.borrow(), expected, "{mode:?} {name}"));
            LIVE.with(|live| assert_eq!(live.get(), 0));
        }
        EVENTS.with(|events| events.borrow_mut().clear());
        assert_eq!(vm.call_int32_0("early").unwrap(), 1);
        EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 2, 9]));
        for name in ["fail_map", "fail_filter"] {
            EVENTS.with(|events| events.borrow_mut().clear());
            let error = vm.call_unit_0(name).unwrap_err();
            assert!(error.to_string().contains("callback failure"), "{error}");
            EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 2, 9], "{mode:?} {name}"));
            LIVE.with(|live| assert_eq!(live.get(), 0));
        }
        EVENTS.with(|events| events.borrow_mut().clear());
        let mut task = vm.call_async("pending", &[]).unwrap();
        while !task.is_finished() {
            task.resume().unwrap();
        }
        assert_eq!(task.cps_result(), Some(42));
        drop(task);
        EVENTS.with(|events| assert_eq!(*events.borrow(), [14, 3, 1, 16, 4, 2]));
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

#[test]
fn iterator_adapters_reject_borrow_escape_consuming_callbacks_and_suspension() {
    for source in [
        "struct Mixed { n: Int32, items: String } fn bad() -> &Int32 { let m = Mixed { n: 1, items: \"a\" }; &m.n }",
        "struct Mixed { n: Int32, items: String } fn bad() { var m = Mixed { n: 1, items: \"a\" }; let r = &mut m.n; let x = m.items; *r += 1 }",
        "struct Mixed { n: Int32, items: String } fn bad(m: &Mixed) { let r = &mut m.n; }",

        "fn bad() { let n = 1; let values = iter.map(0..2, |x| { x + n }); }",
        "fn bad() { let text = \"owned\"; let values = iter.map(0..2, move |x| { text }); }",
        "fn bad() { let values = iter.map([\"owned\"], move |x| { x }); for _ in values {} for _ in values {} }",
        "fn bad() { let a = [1, 2]; let values = iter.take(&a, 1); }",
        "fn bad() { for _ in iter.filter(0..3, move |x| { alloc(x); true }) {} }",
        "fn escape(x: &Int32) -> Bool { alloc(x); true } fn bad() { for _ in iter.filter(0..3, escape) {} }",
        "fn bad() { let a = [1]; let r = &a; let values = iter.map(0..2, move |x| { r.len(); x }); }",
        "fn fail() @suspends -> Int32 { 1 } fn bad() { let values = iter.map(0..2, fail); }",
    ] {
        let source = format!("import joky/iter\nimport joky/task\n{source}");
        let error = frontend_error(&source, &crate::ModuleSources::new());
        assert!(error.span().is_some(), "{source}: {error}");
    }
}

fn adapter_cancellation(sweep: CancellationSweep) {
    let mut vm = parity_vm_with_prelude(ADAPTERS, VmMode::Interpreter, false);
    // Include synchronous closure frames reached from a suspending entry.
    for entry in ["pending", "sync_pending"] {
        assert_cancellation(
            &mut vm,
            entry,
            20_000,
            sweep,
            || EVENTS.with(|events| events.borrow_mut().clear()),
            || EVENTS.with(|events| events.borrow().clone()),
            || {
                LIVE.with(|live| assert_eq!(live.get(), 0));
                EVENTS.with(|events| {
                    let events = events.borrow();
                    for id in [1, 2, 3, 4, 9] {
                        assert!(
                            events.iter().filter(|&&event| event == id).count() <= 1,
                            "{events:?}"
                        );
                    }
                });
            },
        );
    }
}

#[test]
fn iterator_adapters_cancel_at_resource_boundaries() {
    adapter_cancellation(CancellationSweep::ResourceBoundaries);
}

#[test]
#[ignore = "exhaustive: cancel at every adapter and callback instruction boundary"]
fn exhaustive_iterator_adapters_instruction_cancellation() {
    adapter_cancellation(CancellationSweep::EveryInstruction);
}

const SHARED_ADAPTERS: &str = include_str!("../../../../tests/fixtures/shared_combinators.jk");

#[test]
fn shared_combinators_preserve_sources_generic_composition_and_cleanup() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_source(SHARED_ADAPTERS)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(vm.call_int32_0("compute").unwrap(), 42);
        LIVE.with(|n| assert_eq!(n.get(), 0));
        assert!(vm
            .call_unit_0("failed")
            .unwrap_err()
            .message()
            .contains("shared predicate failed"));
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

#[test]
fn shared_combinators_reject_storage_escape_conflicts_and_suspending_callbacks() {
    for body in [
        "fn bad() { var a = [\"a\"]; let cursor = a.iter_shared(); a.push(\"b\"); for _ in cursor {} }",
        "fn bad() { let a = [\"a\"]; let cursor = a.iter_shared(); let copy = cursor; a.release(); for _ in copy {} }",
        "fn bad() { var a = [\"a\"]; let (_, value) = iter.step_shared(a.iter_shared()); a.clear(); value.is_some(); }",
        "fn bad() -> array.SharedIter<String> { let a = [\"a\"]; a.iter_shared() }",
        "fn bad(a: Array<String>) -> array.SharedIter<String> { a.iter_shared() }",
        "fn bad() { let a = [\"a\"]; let stored = [a.iter_shared()]; }",
        "fn bad() { let a = [\"a\"]; alloc(a.iter_shared()); }",
        "fn store<T: Copy>(value: T) { let saved = [value]; } fn bad() { let a = [\"a\"]; store(a.iter_shared()); }",
        "fn store<T: Copy>(value: T) { alloc(value); } fn bad() { let a = [\"a\"]; store(a.iter_shared()); }",
        "fn bad() { var a = [\"a\"]; let optional = Some(a.borrow_mut_ref(0)); }",
        "fn leak(value: &Int32) -> Bool { alloc(value); true } fn bad() { let a = [1]; for _ in iter.filter_shared(a.iter_shared(), leak) {} }",
        "fn delayed(value: &String) @suspends @noescape -> Bool { true } fn bad() { let a = [\"a\"]; for _ in iter.filter_shared(a.iter_shared(), delayed) {} }",
        "fn bad() @suspends { let a = [\"a\"]; for _ in a.iter_shared() { task.yield_now() } }",
    ] {
        let source = format!("import joky/array.{{type Array}} import joky/iter import joky/task {body}");
        let error = frontend_error(&source, &crate::ModuleSources::new());
        assert!(matches!(error.code(), crate::ErrorCode::TypeMismatch | crate::ErrorCode::InvalidAssignment | crate::ErrorCode::InvalidCall), "{body}: {error}");
    }
}

#[test]
#[ignore = "exhaustive: cancel every shared adapter instruction"]
fn exhaustive_shared_combinators_instruction_cancellation() {
    let mut vm = VmEngine::new_interpreter();
    vm.load_source(&format!(
        "{SHARED_ADAPTERS}\nfn sweep() @suspends -> Int32 {{ compute() }}"
    ))
    .unwrap_or_else(|e| panic!("{e}"));
    assert_cancellation(
        &mut vm,
        "sweep",
        20_000,
        CancellationSweep::EveryInstruction,
        || {},
        || LIVE.with(|n| n.get()),
        || LIVE.with(|n| assert_eq!(n.get(), 0)),
    );
}

const SHARED_PIPELINE: &str = include_str!("../../../../tests/fixtures/shared_pipeline.jk");

#[test]
fn shared_pipeline_chains_map_plain_results_and_preserve_lazy_borrowing() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SHARED_PIPELINE, mode, false);
        assert_eq!(vm.call_int32_0("compute").unwrap(), 42);
        EVENTS.with(|events| events.borrow_mut().clear());
        assert_eq!(vm.call_int32_0("lazy").unwrap(), 42);
        EVENTS.with(|events| assert_eq!(*events.borrow(), [5, 105, 0, 3, 103, 105]));
        EVENTS.with(|events| events.borrow_mut().clear());
        assert_eq!(vm.call_int32_0("early").unwrap(), 1);
        EVENTS.with(|events| assert_eq!(*events.borrow(), [101]));
        assert!(vm
            .call_unit_0("failed")
            .unwrap_err()
            .message()
            .contains("pipeline callback failed"));
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

#[test]
fn shared_pipeline_rejects_nonplain_results_retaining_callbacks_and_owner_invalidation() {
    for source in [
        "fn f(value: &String) @noescape -> String { \"new\" } fn bad() { let a = [\"a\"]; for _ in iter.shared(&a).map(f) {} }",
        "fn f(value: &String) -> &String { value } fn bad() { let a = [\"a\"]; for _ in iter.shared(&a).map(f) {} }",
        "fn f(value: &Int32) @noescape -> &Int32 { alloc(1) } fn bad() { let a = [1]; for _ in iter.shared(&a).map(f) {} }",
        "fn f(value: &String) -> Bool { alloc(value); true } fn bad() { let a = [\"a\"]; for _ in iter.shared(&a).filter(f) {} }",
        "fn f(value: &Int32) @suspends -> Int32 { *value } fn bad() { let a = [1]; for _ in iter.shared(&a).map(f) {} }",
        "fn bad() { var a = [1]; let p = iter.shared(&a); a.push(2); for _ in p {} }",
        "fn bad() { let a = [1]; let p = iter.shared(&a); a.release(); for _ in p {} }",
        "fn bad() { let a = [1]; alloc(iter.shared(&a)); }",
        "fn bad() { let a = [1]; let saved = [iter.shared(&a)]; }",
        "fn bad() { let p = { let a = [1]; iter.shared(&a) }; for _ in p {} }",
    ] {
        frontend_error(&format!("import joky/array.{{type Array}} import joky/iter {source}"), &crate::ModuleSources::new());
    }
}

const EXCLUSIVE_PIPELINE: &str = include_str!("../../../../tests/fixtures/exclusive_pipeline.jk");

const CAPTURED_PIPELINE: &str = include_str!("../../../../tests/fixtures/captured_pipeline.jk");

#[test]
fn captured_pipeline_borrows_callbacks_and_preserves_copy_state() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(CAPTURED_PIPELINE, mode, false);
        assert_eq!(vm.call_int32_0("compute").unwrap(), 42);
        EVENTS.with(|events| events.borrow_mut().clear());
        assert_eq!(vm.call_int32_0("lazy").unwrap(), 42);
        EVENTS.with(|events| assert_eq!(*events.borrow(), [55, 80, 180, 65, 165]));
        assert_eq!(vm.call_int32_0("early").unwrap(), 42);
        assert!(vm
            .call_unit_0("failed")
            .unwrap_err()
            .message()
            .contains("captured callback failed"));
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

#[test]
fn captured_pipeline_rejects_mutation_escape_storage_and_retaining_callbacks() {
    for source in [
        "fn bad() { let a = [1]; var limit = 0; let p = iter.shared(&a).filter(|v| { *v > limit }); limit = 2; for _ in p {} }",
        "fn bad() { var a = [1]; let limit = 0; let p = iter.shared(&a).filter(|v| { *v > limit }); a.push(2); for _ in p {} }",
        "fn bad() { let a = [1]; let p = { let limit = 0; iter.shared(&a).filter(|v| { *v > limit }) }; for _ in p {} }",
        "fn bad() { let a = [1]; let limit = 0; alloc(iter.shared(&a).filter(|v| { *v > limit })); }",
        "fn bad() { let a = [1]; let limit = 0; let saved = [iter.shared(&a).filter(|v| { *v > limit })]; }",
        "fn bad() { let a = [1]; var calls = 0; for _ in iter.shared(&a).filter(|v| { calls += 1; *v > calls }) {} }",
        "fn bad() { let a = [1]; let limit = 0; for _ in iter.shared(&a).filter(move |v| { *v > limit }) {} }",
        "fn bad() { let a = [1]; let limit = 0; for _ in iter.shared(&a).map(|v| { if *v > limit { v } else { v } }) {} }",
        "fn bad() @suspends { let a = [1]; let limit = 0; for _ in iter.shared(&a).filter(|v| { *v > limit }) {} }",
        "fn bad() { let n = 1; let f = || { n }; let saved = Some(f); }",
        "fn require<F: Fn(&Int32) @noescape -> Bool>(f: F) {} fn bad<F: Fn(&Int32) -> Bool>(f: F) { require(f) }",
    ] {
        let source = format!("import joky/array.{{type Array}} import joky/iter {source}");
        frontend_error(&source, &crate::ModuleSources::new());
    }
    for retention in ["alloc(Saved { value: v })", "retain(v)"] {
        let source = format!("import joky/array.{{type Array}} import joky/iter struct Saved {{ value: &Int32 }} fn retain(v: &Int32) {{ alloc(Saved {{ value: v }}); }} fn bad() {{ let a = [1]; let limit = 0; for _ in iter.shared(&a).filter(|v| {{ {retention}; *v > limit }}) {{}} }}");
        let error = frontend_error(&source, &crate::ModuleSources::new());
        assert!(error.message().contains("@noescape"), "{error}");
    }
    let source = "fn apply<F: Fn(&Int32) @noescape -> &Int32>(f: F, n: &Int32) -> &Int32 { f(n) } fn bad() { let n = 1; apply(|v| { v }, &n); }";
    let error = frontend_error(source, &crate::ModuleSources::new());
    assert!(error.message().contains("@noescape"), "{error}");
}

#[test]
fn exclusive_pipeline_chains_transfer_cursors_and_end_each_element_loan() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(EXCLUSIVE_PIPELINE, mode, false);
        EVENTS.with(|events| events.borrow_mut().clear());
        assert_eq!(vm.call_int32_0("compute").unwrap(), 42);
        EVENTS.with(|events| assert!(events.borrow().is_empty()));
        assert_eq!(vm.call_int32_0("lazy").unwrap(), 42);
        EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 0, 2, 1]));
        for entry in ["manual", "nested", "early"] {
            assert_eq!(vm.call_int32_0(entry).unwrap(), 42);
        }
        assert!(vm
            .call_unit_0("failed")
            .unwrap_err()
            .message()
            .contains("exclusive predicate failed"));
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

#[test]
fn exclusive_pipeline_rejects_duplicate_loans_storage_escape_and_retaining_callbacks() {
    for body in [
        "fn bad() { var a = [1]; let p = iter.exclusive(&mut a); let q = p; for _ in p {}; for _ in q {} }",
        "fn copy<T: Copy>(value: T) -> T { value } fn bad() { var a = [1]; let p = copy(iter.exclusive(&mut a)); for _ in p {} }",
        "fn bad() { var a = [1]; let p = iter.exclusive(&mut a); let q = iter.exclusive(&mut a); for _ in p {}; for _ in q {} }",
        "fn bad() { var a = [1]; let p = iter.exclusive(&mut a); a.push(2); for _ in p {} }",
        "fn bad() { var a = [1]; let p = iter.exclusive(&mut a); a.release(); for _ in p {} }",
        "fn bad() { var a = [1]; let p = iter.exclusive(&mut a); for _ in &a {}; for _ in p {} }",
        "fn bad() { var a = [1]; alloc(iter.exclusive(&mut a)); }",
        "fn bad() { var a = [1]; let saved = [iter.exclusive(&mut a)]; }",
        "fn bad() { var a = [1]; let saved = Some(iter.exclusive(&mut a)); }",
        "fn bad() { var a = [1]; var p = iter.exclusive(&mut a); let reference = &mut p; }",
        "fn bad() { let p = { var a = [1]; iter.exclusive(&mut a) }; for _ in p {} }",
        "fn bad() { var a = [1]; var (cursor, plan) = iter.exclusive(&mut a).into_parts(); iter.advance_exclusive(&mut plan, cursor); let element = iter.current_mut(cursor); iter.advance_exclusive(&mut plan, cursor); *element += 1; }",
        "fn bad() { var a = [1]; for value in iter.exclusive(&mut a) { for other in iter.exclusive(&mut a) { *value += *other } } }",
        "fn keep(value: &Int32) -> Bool { alloc(value); true } fn bad() { var a = [1]; for _ in iter.exclusive(&mut a).filter(keep) {} }",
        "fn keep(value: &Int32) @suspends -> Bool { true } fn bad() { var a = [1]; for _ in iter.exclusive(&mut a).filter(keep) {} }",
        "fn bad() { var a = [1]; let offset = 0; for _ in iter.exclusive(&mut a).filter(|n| { *n > offset }) {} }",
        "fn bad() { var a = [1]; let p = iter.exclusive(&mut a); let pair = (p, p); }",
        "fn bad() @suspends { var a = [1]; for _ in iter.exclusive(&mut a) {} }",
        "struct Mixed { cursor: &mut iter.Iter<Int32>, reference: &Int32 } fn bad() {}",
        "fn bad() { var a = [1]; let n = 1; let mixed = (a.iter_mut(), &n); }",
        "enum Saved { Cursor(&mut iter.Iter<Int32>) } fn bad() {}",
        "struct Fake { value: &mut Int32 } fn bad() {}",
        "fn bad() { var a = [\"a\"]; let saved = Some(a.borrow_mut_ref(0)); }",
    ] {
        let error = frontend_error(&format!("import joky/array.{{type Array}} import joky/iter {body}"), &crate::ModuleSources::new());
        assert!(matches!(error.code(), crate::ErrorCode::InvalidAssignment | crate::ErrorCode::TypeMismatch | crate::ErrorCode::InvalidCall), "{body}: {error}");
    }
}

#[test]
#[ignore = "exhaustive: cancel every shared/exclusive pipeline and callback instruction"]
fn exhaustive_pipeline_instruction_cancellation() {
    for source in [SHARED_PIPELINE, EXCLUSIVE_PIPELINE, CAPTURED_PIPELINE] {
        let entries = ["compute", "lazy", "early"];
        let wrappers = entries
            .iter()
            .map(|entry| format!("fn sweep_{entry}() @suspends -> Int32 {{ {entry}(); 42 }}"))
            .collect::<Vec<_>>()
            .join("\n");
        let source = format!("{source}\n{wrappers}");
        let mut vm = parity_vm_with_prelude(&source, VmMode::Interpreter, false);
        for entry in entries {
            assert_cancellation(
                &mut vm,
                &format!("sweep_{entry}"),
                20_000,
                CancellationSweep::EveryInstruction,
                || EVENTS.with(|events| events.borrow_mut().clear()),
                || LIVE.with(|live| live.get()),
                || LIVE.with(|live| assert_eq!(live.get(), 0)),
            );
        }
    }
}
