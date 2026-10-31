use super::{VmEngine, VmMode};
use crate::{
    runtime::arena::{LIMIT, LIVE, MAX_BYTES},
    ErrorCode,
};

const SOURCE: &str = include_str!("../../../../tests/fixtures/arrays.jk");
const ENTRIES: &[&str] = &[
    "array_empty",
    "array_growth",
    "array_access",
    "array_moves",
    "array_branches",
    "array_reserve_existing",
];

const GENERIC_SOURCE: &str = include_str!("../../../../tests/fixtures/generic_arrays.jk");

#[test]
fn generic_arrays_preserve_consumer_nominal_types_through_imported_aliases() {
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "collections",
            r#"
        import joky/array.{type Array}
        pub type Batch<T> = Array<T>
        pub fn make<T>(value: T) -> Batch<T> {
            var values = array.new::<T>()
            array.push(&mut values, value)
            values
        }
        pub fn forward<T>(values: Batch<T>) -> Batch<T> { values }
        pub fn first<T: Copy>(values: &Batch<T>, fallback: T) -> T {
            array.get(values, 0).unwrap_or(fallback)
        }
    "#,
        )
        .unwrap();
    let source = r#"
        import collections as c.{type Batch}
        struct Point { flag: Bool, value: Int64 }
        fn check() -> Int32 {
            let point = Point { flag: true, value: 4294967297 }
            let values: Batch<Point> = c.make(point)
            let moved = c.forward(values)
            let result = c.first(&moved, Point { flag: false, value: 0 })
            if result.flag && result.value == 4294967297 { 42 } else { 0 }
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_source_with_modules(source, &modules).unwrap();
        assert_eq!(vm.call_int32_0("check").unwrap(), 42);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

#[test]
fn owning_arrays_cross_ordinary_suspending_calls_in_all_vm_modes() {
    let source = r#"
        import joky/array.{type Array}
        import joky/task

        fn make_values() -> Array<Int32> {
            var values = array.new::<Int32>()
            array.push(&mut values, 7)
            array.push(&mut values, 35)
            values
        }
        fn relay(values: Array<Int32>) @suspends -> Array<Int32> {
            task.yield_now()
            values
        }
        fn inspect(values: Array<Int32>) -> Int32 {
            let result = int32(array.len(&values)) + array.get(&values, 0).unwrap_or(0) + array.get(&values, 1).unwrap_or(0)
            values.release()
            result
        }
        fn run() @suspends -> Int32 {
            let values = make_values()
            let moved = relay(values)
            inspect(moved)
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        let mut task = vm.call_async("run", &[]).unwrap();
        assert!(!task.is_finished(), "{mode:?}");
        task.resume().unwrap();
        assert_eq!(task.cps_result(), Some(44), "{mode:?}");
        drop(task);
        LIVE.with(|live| assert_eq!(live.get(), 0, "{mode:?}"));
    }
}

#[test]
fn owning_arrays_cross_scope_and_race_in_all_vm_modes() {
    let source = r#"
        import joky/array.{type Array}
        import joky/task

        fn hold(values: Array<Int32>) @suspends {
            task.yield_now()
            values.release()
        }
        fn finish() @suspends {}

        fn scope_run() @suspends -> Int32 {
            let values = array.new::<Int32>()
            join { | hold(values) }
            42
        }
        fn race_run() @suspends -> Int32 {
            let values = array.new::<Int32>()
            race { | hold(values) | finish() }
            42
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        for entry in ["scope_run", "race_run"] {
            let mut task = vm.call_async(entry, &[]).unwrap();
            for _ in 0..8 {
                if task.is_finished() {
                    break;
                }
                task.resume().unwrap();
            }
            assert_eq!(task.cps_result(), Some(42), "{mode:?}: {entry}");
            drop(task);
            LIVE.with(|live| assert_eq!(live.get(), 0, "{mode:?}: {entry}"));
        }
    }
}

#[test]
fn generic_arrays_charge_padded_layouts_and_skip_zero_sized_payloads() {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            LIMIT.with(|n| n.set(MAX_BYTES));
        }
    }
    let prefix = "import joky/array struct Mixed { small: Bool, wide: Int64, tail: Bool } ";
    let record = std::mem::size_of::<(std::ptr::NonNull<u8>, std::alloc::Layout)>();
    let control = std::mem::size_of::<crate::runtime::buffer::Buffer>() + record;
    let mixed = std::alloc::Layout::new::<bool>()
        .extend(std::alloc::Layout::new::<i64>())
        .unwrap()
        .0
        .extend(std::alloc::Layout::new::<bool>())
        .unwrap()
        .0
        .pad_to_align();
    for (body, budget) in [
        ("let a = array.with_capacity::<Unit>(1024); if array.capacity(&a) == 1024 { 42 } else { 0 }", control),
        ("var a = array.with_capacity::<Mixed>(1); array.push(&mut a, Mixed { small: true, wide: 4294967297, tail: false }); if array.len(&a) == 1 { 42 } else { 0 }", control + mixed.size() + record),
    ] {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(&format!("{prefix} fn check() -> Int32 {{ {body} }}")).unwrap();
            let _restore = Restore;
            LIMIT.with(|n| n.set(budget - 1));
            assert_eq!(vm.call_int32_0("check").unwrap_err().code(), ErrorCode::ResourceLimit);
            LIVE.with(|n| assert_eq!(n.get(), 0));
            LIMIT.with(|n| n.set(budget));
            assert_eq!(vm.call_int32_0("check").unwrap(), 42);
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}

#[test]
fn generic_arrays_preserve_scalar_aggregate_and_zero_sized_elements() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for prelude in [true, false] {
            let mut vm = VmEngine::with_mode(mode);
            vm.set_prelude_enabled(prelude);
            vm.load_source(GENERIC_SOURCE)
                .unwrap_or_else(|e| panic!("{e}"));
            for entry in [
                "generic_array_scalars",
                "generic_array_records",
                "generic_array_tuples",
                "generic_array_enums",
                "generic_array_zero_size",
            ] {
                assert_eq!(vm.call_int32_0(entry).unwrap(), 42, "{mode:?} {entry}");
                LIVE.with(|n| assert_eq!(n.get(), 0));
            }
            for entry in ["generic_array_failure", "generic_array_zero_failure"] {
                let error = vm.call_int32_0(entry).unwrap_err();
                assert_eq!(error.code(), ErrorCode::ResourceLimit);
                assert!(error.span().is_some());
                LIVE.with(|n| assert_eq!(n.get(), 0));
            }
        }
    }
}

#[test]
fn generic_arrays_reject_noncopy_elements_and_preserve_template_ownership() {
    let prefix = "import joky/array.{type Array}\n";
    for (body, message) in [
        ("fn bad(a: Array<&mut Int32>) {}", "Copy"),
        ("fn bad(a: Array<&Array<Int32>>) {}", "Copy"),
        ("fn make<T>(value: T) -> Array<T> { var a = array.new::<T>(); array.push(&mut a, value); a } fn bad() { let x = 42; make(&x); }", "local borrow"),
        ("fn bad<T>(a: Array<T>) { let b = a; array.len(&a); }", "moved"),
        ("fn bad<T>(a: Array<T>) -> &Array<T> { &a }", "borrowed input"),
        ("fn view<T>(a: &Array<T>) -> &Array<T> { a } fn bad<T>(a: &mut Array<T>, value: T) { let r = view(a); array.push(a, value); array.len(r); }", "borrow"),
        ("fn bad() { let a = array.new() }", "Cannot infer"),
        ("fn bad() { var a = array.new::<Int64>(); array.push::<Bool>(&mut a, true) }", "Conflicting types"),
    ] {
        let source = format!("{prefix}{body}");
        let error = super::support::frontend_error(&source, &crate::ModuleSources::new());
        assert!(error.message().contains(message), "{source}: {error}");
        assert!(error.span().is_some());
    }
}

#[test]
fn generic_arrays_complete_aggregate_writes_before_interpreter_slicing() {
    use crate::runtime::scheduler::Scheduler;
    for entry in [
        "generic_array_records",
        "generic_array_enums",
        "generic_array_zero_size",
    ] {
        let source = format!("{GENERIC_SOURCE} fn task() @suspends -> Int32 {{ {entry}() }}");
        for outcome in ["finish", "drop", "cancel"] {
            let mut vm = VmEngine::with_mode(VmMode::Interpreter);
            vm.load_source(&source).unwrap();
            vm.set_interpreter_limits(super::InterpreterLimits {
                poll_budget: Some(1),
                ..Default::default()
            })
            .unwrap();
            let mut task = vm.call_async("task", &[]).unwrap();
            for _ in 0..100_000 {
                if LIVE.with(|n| n.get() >= 3) {
                    break;
                }
                task.resume().unwrap();
            }
            assert!(!task.is_finished());
            assert!(LIVE.with(|n| n.get() >= 3));
            vm.load_source("fn replacement() -> Int32 { 7 }").unwrap();
            drop(vm);
            match outcome {
                "finish" => {
                    for _ in 0..100_000 {
                        if task.is_finished() {
                            break;
                        }
                        task.resume().unwrap();
                    }
                    assert_eq!(task.cps_result(), Some(42), "{entry}");
                }
                "drop" => drop(task),
                "cancel" => {
                    let mut scheduler = Scheduler::new();
                    let id = scheduler.suspend(task);
                    scheduler.cancel(id).unwrap();
                }
                _ => unreachable!(),
            }
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}

#[test]
fn arrays_execute_source_algorithms_and_move_owners_in_both_engines() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for prelude in [true, false] {
            let mut vm = VmEngine::with_mode(mode);
            vm.set_prelude_enabled(prelude);
            vm.load_source(SOURCE).unwrap();
            for name in ENTRIES {
                assert_eq!(vm.call_int32_0(name).unwrap(), 42, "{name}");
                LIVE.with(|n| assert_eq!(n.get(), 0));
            }
            assert!(vm.call_int32_0("array_make").is_err());
        }
    }
}

#[test]
fn arrays_fail_with_embedded_source_locations_and_release_storage() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(SOURCE).unwrap();
        for name in [
            "array_capacity_overflow",
            "array_reserve_overflow",
            "array_budget_failure",
        ] {
            let error = vm.call_int32_0(name).unwrap_err();
            assert_eq!(error.code(), ErrorCode::ResourceLimit);
            let span = error.span().unwrap();
            let source = error.sources().get(span.source_id()).unwrap();
            assert_eq!(source.name(), "joky/internal/buffer.jk");
            assert!(span.slice(source.text()).unwrap().contains("fn allocate"));
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
        assert_eq!(vm.call_int32_0("array_empty").unwrap(), 42);
    }
}

#[test]
fn arrays_reserve_with_existing_capacity_never_allocates_again() {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            LIMIT.with(|n| n.set(MAX_BYTES));
        }
    }
    let record = std::mem::size_of::<(std::ptr::NonNull<u8>, std::alloc::Layout)>();
    let limit = std::mem::size_of::<crate::runtime::buffer::Buffer>() + 16 * 4 + 2 * record;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(SOURCE).unwrap();
        let _restore = Restore;
        LIMIT.with(|n| n.set(limit));
        assert_eq!(vm.call_int32_0("array_reserve_existing").unwrap(), 42);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

#[test]
fn arrays_preserve_ownership_through_source_module_aliases() {
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "numbers",
            r#"
        import joky/array.{type Array}
        pub type Numbers = Array<Int32>
        pub fn make() -> Numbers { var a = array.new::<Int32>(); array.push(&mut a, 42); a }
        pub fn forward(a: Numbers) -> Numbers { a }
        pub fn read(a: &Numbers) -> Int32 { array.get(a, 0).unwrap_or(0) }
    "#,
        )
        .unwrap();
    let source = "import numbers as n.{type Numbers} fn check() -> Int32 { let a: Numbers = n.make(); let moved = n.forward(a); n.read(&moved) }";
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_source_with_modules(source, &modules).unwrap();
        assert_eq!(vm.call_int32_0("check").unwrap(), 42);
        let invalid =
            "import numbers as n fn check() { let a = n.make(); n.forward(a); n.read(&a); }";
        assert!(vm
            .load_source_with_modules(invalid, &modules)
            .unwrap_err()
            .message()
            .contains("moved"));
        assert_eq!(vm.call_int32_0("check").unwrap(), 42);
    }
    assert!(modules.insert("joky/array", "fn fake() {}").is_err());
    modules
        .insert_file(
            "pretend",
            crate::SourceFile::new(
                "joky/array.jk",
                "import joky/internal/buffer as b pub fn get() -> UInt64 { b.capacity_limit() }",
            ),
        )
        .unwrap();
    let mut vm = VmEngine::new();
    assert_eq!(
        vm.load_source_with_modules("import pretend fn check() {}", &modules)
            .unwrap_err()
            .code(),
        ErrorCode::InvalidImport
    );
}

#[test]
fn arrays_reject_capability_escapes_and_invalid_storage() {
    let prefix = r#"
        import joky/array.{type Array}
        fn consume(a: Array<Int32>) {}
        fn view(a: &Array<Int32>) -> &Array<Int32> { a }
    "#;
    for source in [
        "fn bad() { let a = array.new::<Int32>(); consume(a); consume(a) }",
        "fn pair(a: Array<Int32>, b: Array<Int32>) {} fn bad() { let a = array.new::<Int32>(); pair(a, a) }",
        "fn bad() { var a = array.new::<Int32>(); let r = view(&a); array.push(&mut a, 1); array.len(r); }",
        "fn bad() { let a = array.new::<Int32>(); let r = &a; consume(a); array.len(r); }",
        "fn bad() { var a = array.new::<Int32>(); let r = &a; a = array.new::<Int32>(); array.len(r); }",
        "fn bad() { var a = array.new::<Int32>(); array.push(&mut a, array.get(&a, 0).unwrap_or(0)) }",
        "fn bad() -> &Array<Int32> { let a = array.new::<Int32>() &a }",
        "fn bad(a: Array<Int32>) -> &Array<Int32> { &a }",
        "fn bad(a: &Array<Int32>) -> Array<Int32> { a }",
        "fn bad() { let a = array.new::<Int32>(); let r = { let b = a &b }; array.len(r); }",
        "fn bad(flag: Bool) { let a = array.new::<Int32>(); if flag { consume(a) }; consume(a) }",
        "fn bad() { let a = array.new::<Int32>(); var i = 0; while i < 2 { consume(a); i = i + 1 } }",
        "type Wrong = Array<&String> fn bad() {}",
        "struct Box { value: Array<Int32>, extra: &Int32 } fn bad() {}",
        "struct Box { value: &Array<Int32> } fn bad() { let a = array.new::<Int32>(); alloc(Box { value: &a }); }",
        "fn bad() { let a = array.new::<Int32>(); let pair = (a, 1); array.len(&a); }",
        "fn bad() { let a = array.new::<Int32>(); let pair = (&a, 1); alloc(pair); }",
        "fn id<T>(value: T) -> T { value } fn bad() { let a = array.new::<Int32>(); let r = id(&a); consume(a); r.len(); }",
        "fn bad() { let a = array.new::<Int32>(); alloc(a); }",
        "fn bad() { let a = array.new::<Int32>() ; (*(&a)); }",
        "fn bad() { var a = array.new::<Int32>(); let r = &a; (*(&mut a) = array.new::<Int32>()); array.len(r); }",
        "fn bad() { let a = array.new::<Int32>() ; (&*a); }",
        "fn bad(a: &Array<&Int32>) @suspends {}",
        "fn branch_task(a: &Array<&Int32>) @suspends {} fn bad() @suspends { let a = array.new::<&Int32>(); join { | branch_task(&a) } }",
        "fn branch_task(a: &Array<&Int32>) @suspends {} fn bad() @suspends { let a = array.new::<&Int32>(); race { | branch_task(&a) } }",
        "fn branch_task(a: Array<&Int32>) @suspends {} fn bad() @suspends { let a = array.new::<&Int32>(); join { | branch_task(a) } }",
        "fn branch_task(a: Array<&Int32>) @suspends {} fn bad() @suspends { let a = array.new::<&Int32>(); race { | branch_task(a) } }",
        "extern fn host(a: Array<Int32>) fn bad() {}",
        "unsafe extern \"C\" fn host(a: &Array<Int32>) fn bad() {}",
        "fn bad() { array.allocate(1); }",
        "fn bad() { array.buffer.allocate(1); }",
        "import joky/internal/buffer as b fn bad() { b.allocate(1); }",
        "import joky/internal/buffer.{capacity_limit} fn bad() { capacity_limit(); }",
        "fn __joky_buffer_allocate() {}",
    ] {
        let source = if let Some((import, body)) = source
            .strip_prefix("import ")
            .and_then(|s| s.split_once(" fn "))
        {
            format!("import {import}\n{prefix}\nfn {body}")
        } else {
            format!("{prefix}\n{source}")
        };
        let error = super::support::frontend_error(&source, &crate::ModuleSources::new());
        assert!(error.span().is_some(), "{source}: {error}");
    }
}

#[test]
fn arrays_interpreter_slices_do_not_replay_growth_and_survive_reload() {
    use crate::runtime::{scheduler::Scheduler, suspension::ResumeError};
    let source = format!("{SOURCE} fn task() @suspends -> Int32 {{ array_growth() }}");
    for slice in [1, 3, 17] {
        let mut vm = VmEngine::with_mode(VmMode::Interpreter);
        vm.load_source(&source).unwrap();
        vm.set_interpreter_limits(super::InterpreterLimits {
            poll_budget: Some(slice),
            ..Default::default()
        })
        .unwrap();
        let mut task = vm.call_async("task", &[]).unwrap();
        let mut peak = 0;
        for _ in 0..100_000 {
            LIVE.with(|n| peak = peak.max(n.get()));
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        assert_eq!(task.cps_result(), Some(42));
        assert_eq!(peak, 4); // Current and replacement header/payload pairs during migration.
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
    // Pause before/after first publication and during a later migration.
    for checkpoint in [1, 2, 3, 4] {
        for outcome in ["finish", "drop", "cancel"] {
            let mut vm = VmEngine::with_mode(VmMode::Interpreter);
            vm.load_source(&source).unwrap();
            vm.set_interpreter_limits(super::InterpreterLimits {
                poll_budget: Some(1),
                ..Default::default()
            })
            .unwrap();
            let mut task = vm.call_async("task", &[]).unwrap();
            for _ in 0..100_000 {
                if LIVE.with(|n| n.get() == checkpoint) {
                    break;
                }
                task.resume().unwrap();
            }
            assert!(!task.is_finished());
            assert_eq!(LIVE.with(|n| n.get()), checkpoint);
            for _ in 0..10 {
                task.resume().unwrap();
            }
            assert!(vm.load_source("fn invalid(").is_err());
            vm.load_source("fn replacement() -> Int32 { 99 }").unwrap();
            drop(vm);
            match outcome {
                "finish" => {
                    for _ in 0..100_000 {
                        if task.is_finished() {
                            break;
                        }
                        task.resume().unwrap();
                    }
                    assert_eq!(task.cps_result(), Some(42));
                }
                "drop" => drop(task),
                "cancel" => {
                    let mut scheduler = Scheduler::new();
                    let id = scheduler.suspend(task);
                    scheduler.cancel(id).unwrap();
                }
                _ => unreachable!(),
            }
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
    for budget in [1, 20, 100, 500, 2000] {
        let mut vm = VmEngine::with_mode(VmMode::Interpreter);
        vm.load_source(&source).unwrap();
        vm.set_interpreter_limits(super::InterpreterLimits {
            instruction_budget: Some(budget),
            poll_budget: Some(1),
            ..Default::default()
        })
        .unwrap();
        let mut task = match vm.call_async("task", &[]) {
            Ok(task) => task,
            Err(error) => {
                assert_eq!(error.code(), ErrorCode::ResourceLimit);
                LIVE.with(|n| assert_eq!(n.get(), 0));
                continue;
            }
        };
        let mut failed = false;
        for _ in 0..=budget {
            if let Err(ResumeError::Failed(error)) = task.resume() {
                assert_eq!(error.code(), ErrorCode::ResourceLimit);
                failed = true;
                break;
            }
        }
        assert!(failed);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

#[test]
fn arrays_sync_helpers_release_storage_before_the_async_root_finishes() {
    use crate::runtime::cps::CpsFrame;
    unsafe extern "C" fn pause(frame: *mut CpsFrame) -> i32 {
        if (*frame).state == 0 {
            (*frame).state = 1;
            1
        } else {
            0
        }
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for finish in [false, true] {
            let mut vm = VmEngine::with_mode(mode);
            unsafe {
                vm.register_suspending_fn(
                    "pause",
                    crate::FunctionSignature::new(vec![], crate::Type::Unit),
                    pause,
                )
                .unwrap();
            }
            vm.load_source(&format!("{SOURCE} extern fn pause() @suspends fn task() @suspends -> Int32 {{ let answer = array_growth(); pause(); answer }}")).unwrap();
            let mut task = vm.call_async("task", &[]).unwrap();
            assert!(!task.is_finished());
            LIVE.with(|n| assert_eq!(n.get(), 0)); // The synchronous owner has left its scope.
            vm.load_source("fn replacement() -> Int32 { 99 }").unwrap();
            drop(vm);
            if finish {
                task.resume().unwrap();
                assert_eq!(task.cps_result(), Some(42));
                LIVE.with(|n| assert_eq!(n.get(), 0));
            }
            drop(task);
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}

const REFERENCE_SOURCE: &str = include_str!("../../../../tests/fixtures/reference_arrays.jk");
const REFERENCE_ENTRIES: &[&str] = &[
    "reference_array_growth",
    "reference_array_owner_end",
    "reference_array_records",
    "reference_array_tuples",
    "reference_array_enums",
    "reference_array_graphs",
    "reference_array_numeric_contract",
    "reference_array_returned",
];

#[test]
fn reference_arrays_copy_referents_independently_of_control_blocks() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for prelude in [true, false] {
            let mut vm = VmEngine::with_mode(mode);
            vm.set_prelude_enabled(prelude);
            vm.load_source(REFERENCE_SOURCE)
                .unwrap_or_else(|e| panic!("{mode:?}: {e} {:?}", e.span()));
            for entry in REFERENCE_ENTRIES {
                assert_eq!(vm.call_int32_0(entry).unwrap(), 42, "{mode:?}: {entry}");
                LIVE.with(|n| assert_eq!(n.get(), 0));
            }
        }
    }
}

#[test]
fn reference_arrays_reject_local_storage_and_retain_shared_loans() {
    let prefix = r#"
        import joky/array.{type Array}
        struct Item { value: &Int32 }
        enum Choice { Empty, Item(&Int32) }
        fn replace<T>(a: &mut Array<T>, p: T) { array.set(a, 0, p); }
        fn forward<T>(a: Array<T>) -> Array<T> { a }
        fn make<T>(p: T) -> Array<T> { var a = array.new::<T>(); array.push(&mut a, p); a }
        fn first<T: Copy>(a: &Array<T>) -> Option<T> { array.get(a, 0) }
        fn copy<T: Copy>(a: &mut Array<T>, b: &Array<T>) { match array.get(b, 0) { None => {}, Some(p) => replace(a, p) } }
        fn consume<T>(a: Array<T>, p: T) { var b = a; replace(&mut b, p) }
    "#;
    for (body, message) in [
        ("let x = 42; var a = array.new::<&Int32>(); array.push(&mut a, &x)", "local borrow"),
        ("let x = 42; var a = array.with_capacity::<&Int32>(1); array.set(&mut a, 0, &x);", "local borrow"),
        ("let x = 42; var a = array.new::<&Int32>(); replace(&mut a, &x)", "local borrow"),
        ("let x = 42; let a = array.new::<&Int32>(); consume(a, &x)", "local borrow"),
        ("let x = 42; var a = array.new::<Item>(); array.push(&mut a, Item { value: &x })", "local borrow"),
        ("let x = 42; var a = array.new::<Choice>(); array.push(&mut a, Choice.Item(&x))", "local borrow"),
        ("let x = 42; var a = array.new::<(Bool, &Int32)>(); array.push(&mut a, (true, &x))", "local borrow"),
        ("let x = 42; let a = make(&x); let b = forward(a)", "local borrow"),
        ("let p = alloc(42); var a = array.new::<&Int32>(); array.push(&mut a, (&*p)) ; (*(p) = 0)", "retained"),
        ("let p = alloc(42); var a = array.new::<&Int32>(); replace(&mut a, (&*p)) ; (*(p) = 0)", "retained"),
        ("let p = alloc(42); var a = array.new::<&Int32>(); array.set(&mut a, 0, (&*p)); (*(p) = 0)", "retained"),
        ("let p = alloc(42); var a = array.new::<&Int32>(); array.push(&mut a, (&*p)); array.pop(&mut a); (*(p) = 0)", "retained"),
        ("let p = alloc(42); let q = (&*(alloc(7))); var a = array.new::<&Int32>(); array.push(&mut a, (&*p)); array.set(&mut a, 0, q); (*(p) = 0)", "retained"),
        ("let p = alloc(42); { var a = array.new::<&Int32>(); array.push(&mut a, (&*p)) } ; (*(p) = 0)", "retained"),
        ("let p = alloc(42); let a = make((&*p)); let b = forward(a); first(&b); (*(p) = 0)", "retained"),
        ("let p = alloc(42); let a = make((&*p)); var b = array.new::<&Int32>(); copy(&mut b, &a) ; (*(p) = 0)", "retained"),
        ("let p = alloc(42); let a = make(Item { value: (&*p) }); first(&a); (*(p) = 0)", "retained"),
    ] {
        let source = format!("{prefix} fn bad() {{ {body} }}");
        let error = super::support::frontend_error(&source, &crate::ModuleSources::new());
        assert!(error.message().contains(message), "{source}: {error}");
        assert!(error.span().is_some());
    }
}

#[test]
fn reference_arrays_cross_module_aliases_preserve_storage_contracts() {
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "collections",
            r#"
        import joky/array.{type Array}
        pub type Batch<T> = Array<T>
        pub fn make<T>(p: T) -> Batch<T> { var a = array.new::<T>(); array.push(&mut a, p); a }
        pub fn forward<T>(a: Batch<T>) -> Batch<T> { a }
        pub fn first<T: Copy>(a: &Batch<T>) -> Option<T> { array.get(a, 0) }
        pub fn replace<T>(a: &mut Batch<T>, p: T) { array.set(a, 0, p); }
    "#,
        )
        .unwrap();
    let source = r#"
        import collections as c.{type Batch}
        struct Point { x: Int32 }
        struct Holder<T: Copy> { value: &T }
        fn check() -> Int32 {
            let point = (&*(alloc(Point { x: 42 })))
            let copied = {
                var a: Batch<Holder<Point>> = c.forward(c.make(Holder { value: point }))
                let value = c.first(&a)
                c.replace(&mut a, Holder { value: point })
                value
            }
            match copied { None => 0, Some(holder) => holder.value.x }
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        for _ in 0..2 {
            vm.load_source_with_modules(source, &modules).unwrap();
            assert_eq!(vm.call_int32_0("check").unwrap(), 42);
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
        for bad in [
            "let x = 42; var a = c.make((&*(alloc(0)))); c.replace(&mut a, &x)",
            "let p = alloc(42); let a = c.forward(c.make((&*p))); c.first(&a); (*(p) = 0)",
            "let p = alloc(42); var a = c.make((&*(alloc(0)))); c.replace(&mut a, (&*p)) ; (*(p) = 0)",
        ] {
            let bad = format!("import collections as c fn bad() {{ {bad} }}");
            assert!(
                vm.load_source_with_modules(&bad, &modules).is_err(),
                "{bad}"
            );
            assert_eq!(vm.call_int32_0("check").unwrap(), 42);
            let ast = crate::TypeRegistry::default()
                .resolve_with_modules(&crate::parser::parse(&bad).unwrap(), &modules)
                .unwrap();
            let mut compiler =
                crate::aot::AotCompiler::new("invalid-imported-reference-array").unwrap();
            assert!(compiler.compile_module(&ast).is_err(), "{bad}");
        }
    }
    let ast = crate::TypeRegistry::default()
        .resolve_with_modules(&crate::parser::parse(source).unwrap(), &modules)
        .unwrap();
    crate::aot::AotCompiler::new("imported-reference-array")
        .unwrap()
        .compile_module(&ast)
        .unwrap();
}

#[test]
fn reference_arrays_survive_interpreter_slices_reload_and_cancel() {
    use crate::runtime::scheduler::Scheduler;
    for entry in [
        "reference_array_growth",
        "reference_array_records",
        "reference_array_enums",
        "reference_array_graphs",
    ] {
        let source = format!("{REFERENCE_SOURCE} fn task() @suspends -> Int32 {{ {entry}() }}");
        for outcome in ["finish", "drop", "cancel"] {
            let mut vm = VmEngine::with_mode(VmMode::Interpreter);
            vm.load_source(&source).unwrap();
            vm.set_interpreter_limits(super::InterpreterLimits {
                poll_budget: Some(1),
                ..Default::default()
            })
            .unwrap();
            let mut task = vm.call_async("task", &[]).unwrap();
            for _ in 0..100_000 {
                if LIVE.with(|n| n.get() >= 5) {
                    break;
                }
                task.resume().unwrap();
            }
            assert!(!task.is_finished());
            assert!(LIVE.with(|n| n.get() >= 5));
            vm.load_source("fn replacement() -> Int32 { 7 }").unwrap();
            drop(vm);
            match outcome {
                "finish" => {
                    for _ in 0..100_000 {
                        if task.is_finished() {
                            break;
                        }
                        task.resume().unwrap();
                    }
                    assert_eq!(task.cps_result(), Some(42), "{entry}");
                }
                "drop" => drop(task),
                "cancel" => {
                    let mut scheduler = Scheduler::new();
                    let id = scheduler.suspend(task);
                    scheduler.cancel(id).unwrap();
                }
                _ => unreachable!(),
            }
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}

#[test]
fn reference_arrays_copied_elements_live_through_async_root_suspension() {
    use crate::runtime::cps::CpsFrame;
    unsafe extern "C" fn pause(frame: *mut CpsFrame) -> i32 {
        if (*frame).state == 0 {
            (*frame).state = 1;
            1
        } else {
            0
        }
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for finish in [false, true] {
            let mut vm = VmEngine::with_mode(mode);
            unsafe {
                vm.register_suspending_fn(
                    "pause",
                    crate::FunctionSignature::new(vec![], crate::Type::Unit),
                    pause,
                )
                .unwrap();
            }
            vm.load_source(&format!("{REFERENCE_SOURCE} extern fn pause() @suspends fn task() @suspends -> Int32 {{ let value = refs_return(); pause() ; (*value) }}")).unwrap();
            let mut task = vm.call_async("task", &[]).unwrap();
            assert!(!task.is_finished());
            LIVE.with(|n| assert!(n.get() > 0));
            vm.load_source("fn replacement() -> Int32 { 7 }").unwrap();
            drop(vm);
            if finish {
                task.resume().unwrap();
                assert_eq!(task.cps_result(), Some(42));
            }
            drop(task);
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}

const OWNED_SOURCE: &str = include_str!("../../../../tests/fixtures/owned_arrays.jk");

#[test]
fn owning_array_methods_preserve_moves_views_and_element_provenance() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for prelude in [true, false] {
            let mut vm = VmEngine::with_mode(mode);
            vm.set_prelude_enabled(prelude);
            vm.load_source(OWNED_SOURCE).unwrap();
            assert_eq!(vm.call_int32_0("methods_answer").unwrap(), 42, "{mode:?}");
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}

#[test]
fn owning_array_methods_reject_aliases_private_access_and_unsupported_storage() {
    for (body, message) in [
        ("fn bad() { let a = Array.new::<Int32>(); let b = a; a.len(); }", "moved"),
        ("fn bad() { var b = Batch.new::<Int32>(); let r = b.view(); b.add(1); r.len(); }", "borrow"),
        ("fn bad() { let b = Batch.new::<Int32>(); let a = b.values; b.get(0); }", "moved"),
        ("fn bad() { let b = Batch.new::<Int32>(); let a = b.values; let c = b.values }", "moved"),
        ("fn bad() { let b = Batch.new::<Int32>(); b.values.push(1) }", "var binding"),
        ("fn bad() { let a = Array.new::<Int32>(); a.storage; }", "private"),
        ("fn bad() { var a = Array.new::<Int32>(); let x = &mut a.storage }", "private"),
        ("fn bad(a: &mut Array<Int32>) { let x = &mut a.storage }", "private"),
        ("fn bad(a: Array<Int32>) -> Array<Int32> { Array { storage: a } }", "private"),
        ("fn bad(b: &Batch<Int32>) -> Array<Int32> { b.values }", "borrowed wrapper"),
        ("fn bad(b: &mut Batch<Int32>) { b.values = Array.new::<Int32>() }", "borrowed wrapper"),
        ("fn bad(b: Batch<Int32>) -> &Array<Int32> { &b.values }", "borrowed input"),
        ("type Hidden = Array<&Later> struct Later { value: Array<Int32> } fn bad() {}", "Copy"),
        ("type View = &Later struct Later { value: Array<Int32> } extern fn host(value: View) fn bad() {}", "host ABI"),
        ("fn bad() { let a = Array.new::<&Batch<Int32>>() }", "Copy"),
        ("struct Extra { value: Batch<Int32>, reference: &Int32 } fn bad() {}", "reference-free Copy"),
        ("fn bad() { var b = Batch.new::<&Int32>(); let p = alloc(42); b.add((&*p)) ; (*(p) = 0) }", "retained"),
        ("fn bad() { var b = Batch.new::<&Int32>(); let p = 42; b.add(&p) }", "local borrow"),
        ("fn bad() { let b = Batch.new::<Int32>(); alloc(b); }", "Copy"),
        ("fn bad() { let b = Batch.new::<Int32>() ; (*(&b)); }", "non-Copy"),
        ("extern fn host(b: &Batch<Int32>) fn bad() {}", "host ABI"),
    ] {
        let source = format!("{OWNED_SOURCE}\n{body}");
        let error = super::support::frontend_error(&source, &crate::ModuleSources::new());
        assert!(error.message().contains(message), "{source}: {error}");
        assert!(error.span().is_some());
    }
}

#[test]
fn owning_array_methods_cleanup_after_internal_pause_reload_and_cancel() {
    use crate::runtime::scheduler::Scheduler;
    for (source, entry) in [
        (OWNED_SOURCE, "methods_answer"),
        (MIXED_SOURCE, "mixed_all"),
    ] {
        for outcome in ["finish", "drop", "cancel"] {
            let mut vm = VmEngine::with_mode(VmMode::Interpreter);
            vm.load_source(&format!(
                "{source} fn task() @suspends -> Int32 {{ {entry}() }}"
            ))
            .unwrap();
            vm.set_interpreter_limits(super::InterpreterLimits {
                poll_budget: Some(1),
                ..Default::default()
            })
            .unwrap();
            let mut task = vm.call_async("task", &[]).unwrap();
            for _ in 0..100_000 {
                if LIVE.with(|n| n.get() >= 2) || task.is_finished() {
                    break;
                }
                task.resume().unwrap();
            }
            assert!(!task.is_finished());
            LIVE.with(|n| assert!(n.get() >= 2));
            vm.load_source("fn replacement() -> Int32 { 7 }").unwrap();
            drop(vm);
            match outcome {
                "finish" => {
                    for _ in 0..100_000 {
                        if task.is_finished() {
                            break;
                        }
                        task.resume().unwrap();
                    }
                    assert_eq!(task.cps_result(), Some(42));
                }
                "drop" => drop(task),
                "cancel" => {
                    let mut scheduler = Scheduler::new();
                    let id = scheduler.suspend(task);
                    scheduler.cancel(id).unwrap();
                }
                _ => unreachable!(),
            }
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}

const MIXED_SOURCE: &str = include_str!("../../../../tests/fixtures/mixed_owners.jk");

#[test]
fn mixed_owners_preserve_metadata_and_nested_moves() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for prelude in [true, false] {
            let mut vm = VmEngine::with_mode(mode);
            vm.set_prelude_enabled(prelude);
            vm.load_source(MIXED_SOURCE).unwrap();
            assert_eq!(vm.call_int32_0("mixed_all").unwrap(), 42, "{mode:?}");
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}

#[test]
fn mixed_owners_reject_partial_moves_and_borrow_conflicts() {
    for (body, message) in [
        ("fn bad() -> UInt64 { let b = Batch.new::<Int32>(); let a = b.values; b.version }", "moved"),
        ("fn bad() { var b = Batch.new::<Int32>(); let a = b.values; b.version = 3 }", "moved"),
        ("fn bad() { var b = Batch.new::<Int32>(); b.release(); b.sorted = true }", "moved"),
        ("fn bad() { var b = Batch.new::<Int32>(); let r = b.view(); b.version = 1; r.len(); }", "borrow"),
        ("fn bad() { var b = Batch.new::<Int32>(); let r = b.edit(); let n = b.version; r.len(); }", "borrow"),
        ("fn bad() { var b = Batch.new::<Int32>(); let r = b.view(); let a = b.values; r.len(); }", "borrow"),
        ("fn bad() { var b = Batch.new::<Int32>(); let r = b.view(); b = Batch.new::<Int32>(); r.len(); }", "borrow"),
        ("fn bad() { var b = Batch.new::<Int32>(); b.version = { b.release(); 4 } }", "borrow"),
        ("fn bad() { var b = Batch.new::<Int32>(); b.values = { b.release(); Array.new::<Int32>() } }", "moved"),
        ("fn bad() { var b = Batch.new::<Int32>(); let a = b.values; b.values = Array.new::<Int32>() }", "moved"),
        ("fn bad(flag: Bool) -> UInt64 { let b = Batch.new::<Int32>(); if flag { b.release() }; b.version }", "moved"),
        ("fn bad() { let b = Batch.new::<Int32>(); var i = 0; while i < 2 { b.release(); i = i + 1 } }", "moved"),
        ("fn bad() { let b = Batch.new::<Int32>(); b.version = 1 }", "immutable"),
        ("fn bad(b: &Batch<Int32>) { b.version = 1 }", "shared"),
        ("fn bad(b: &Batch<Int32>) -> Array<Int32> { b.values }", "borrowed wrapper"),
        ("fn bad(b: &mut Batch<Int32>) { b.values = Array.new::<Int32>() }", "borrowed wrapper"),
        ("fn bad() -> &Array<Int32> { let b = Batch.new::<Int32>(); &b.values }", "borrowed input"),
        ("fn bad() { var b = Batch.new::<Int32>(); let r = &b.version; b.version = 1; let n = *r }", "borrow"),
        ("fn bad() { let b = Batch.new::<Int32>(); alloc(b); }", "Copy"),
        ("fn bad() { let b = Batch.new::<Int32>() ; (*(&b)); }", "non-Copy"),
        ("fn bad() { var b = Batch.new::<Int32>(); let r = &b; (*(&mut b) = Batch.new::<Int32>()); r.values.len(); }", "borrow"),
        ("extern fn host(b: &Batch<Int32>) fn bad() {}", "host ABI"),
        ("struct Invalid { values: Array<Int32>, tag: &Int32 } fn bad() {}", "reference-free Copy"),
        ("struct Invalid<T> { values: Array<T>, tag: &Int32 } fn bad() {}", "reference-free Copy"),
        ("fn bad() { let g = GenericMeta { meta: (&*(alloc(0))), values: Array.new::<Int32>() } }", "reference-free Copy"),
        ("struct Invalid { tag: &Int32, owner: Later } struct Later { values: Array<Int32> } fn bad() {}", "reference-free Copy"),
        ("fn bad() { let w = Wrapper { batch: Batch.new::<Int32>() }; let a = w.batch.values; w.batch.version; }", "moved"),
        ("fn bad() { var w = Wrapper { batch: Batch.new::<Int32>() }; let r = w.batch.view(); w.batch.version = 0; r.len(); }", "borrow"),
        ("fn bad() { let b = Batch.new::<Int32>(); let r = { let c = b; &c.values }; r.len(); }", "borrow"),

        ("fn bad() { var b = Batch.new::<&Int32>(); let p = 42; b.add(&p) }", "local borrow"),
        ("fn bad() { var b = Batch.new::<&Int32>(); let p = alloc(42); b.add((&*p)) ; (*(p) = 0) }", "retained"),
    ] {
        let source = format!("{MIXED_SOURCE}\n{body}");
        let error = super::support::frontend_error(&source, &crate::ModuleSources::new());
        assert!(error.message().contains(message), "{source}: {error}");
        assert!(error.span().is_some());
    }
}
