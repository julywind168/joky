use super::{VmEngine, VmMode};
use crate::{
    runtime::arena::{LIMIT, LIVE, MAX_BYTES},
    ErrorCode,
};

const SOURCE: &str = include_str!("../../tests/fixtures/arrays.jk");
const ENTRIES: &[&str] = &[
    "array_empty",
    "array_growth",
    "array_access",
    "array_moves",
    "array_branches",
    "array_reserve_existing",
];

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
        import joky/array as array.{type Array}
        pub type Numbers = Array<Int32>
        pub fn make() -> Numbers { var a = array.new() array.push(&mut a, 42) a }
        pub fn forward(a: Numbers) -> Numbers { a }
        pub fn read(a: &Numbers) -> Int32 { array.get(a, 0).unwrap_or(0) }
    "#,
        )
        .unwrap();
    let source = "import numbers as n.{type Numbers} fn check() -> Int32 { let a: Numbers = n.make() let moved = n.forward(a) n.read(&moved) }";
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_source_with_modules(source, &modules).unwrap();
        assert_eq!(vm.call_int32_0("check").unwrap(), 42);
        let invalid =
            "import numbers as n fn check() { let a = n.make() n.forward(a); n.read(&a); }";
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
fn arrays_reject_capability_escapes_and_unopened_features_in_all_backends() {
    let prefix = r#"
        import joky/array as array.{type Array}
        fn consume(a: Array<Int32>) {}
        fn view(a: &Array<Int32>) -> &Array<Int32> { a }
    "#;
    for source in [
        "fn bad() { let a = array.new() consume(a) consume(a) }",
        "fn pair(a: Array<Int32>, b: Array<Int32>) {} fn bad() { let a = array.new() pair(a, a) }",
        "fn bad() { var a = array.new() let r = view(&a) array.push(&mut a, 1) array.len(r); }",
        "fn bad() { let a = array.new() let r = &a consume(a) array.len(r); }",
        "fn bad() { var a = array.new() let r = &a a = array.new() array.len(r); }",
        "fn bad() { var a = array.new() array.push(&mut a, array.get(&a, 0).unwrap_or(0)) }",
        "fn bad() -> &Array<Int32> { let a = array.new() &a }",
        "fn bad(a: Array<Int32>) -> &Array<Int32> { &a }",
        "fn bad(a: &Array<Int32>) -> Array<Int32> { a }",
        "fn bad() { let a = array.new() let r = { let b = a &b } array.len(r); }",
        "fn bad(flag: Bool) { let a = array.new() if flag { consume(a) } consume(a) }",
        "fn bad() { let a = array.new() var i = 0 while i < 2 { consume(a) i = i + 1 } }",
        "type Wrong = Array<Int64> fn bad() {}",
        "fn bad<T>(a: Array<T>) {}",
        "struct Box { value: Array<Int32> } fn bad() {}",
        "struct Box { value: &Array<Int32> } fn bad() {}",
        "enum Box { Owned(Array<Int32>) } fn bad() {}",
        "fn bad() { let a = array.new() let pair = (a, 1) }",
        "fn bad() { let a = array.new() let pair = (&a, 1) }",
        "fn id<T>(value: T) -> T { value } fn bad() { let a = array.new() id(a); }",
        "fn id<T>(value: T) -> T { value } fn bad() { let a = array.new() id(&a); }",
        "fn bad() { let a = array.new() let value = Some(a) }",
        "fn bad() { let a = array.new() alloc(a); }",
        "fn bad() { let a = array.new() memory.read(&a); }",
        "fn bad() { var a = array.new() memory.write(&mut a, array.new()); }",
        "fn bad() { let a = array.new() share(a); }",
        "fn bad() @suspends { let a = array.new() }",
        "fn bad(a: &Array<Int32>) @suspends {}",
        "fn branch(a: Array<Int32>) {} fn bad() @suspends { scope { | branch(array.new()) } }",
        "fn branch(a: Array<Int32>) {} fn bad() @suspends { race { | branch(array.new()) } }",
        "extern fn host(a: Array<Int32>) fn bad() {}",
        "unsafe extern \"C\" fn host(a: &Array<Int32>) fn bad() {}",
        "fn bad() { array.allocate(1); }",
        "fn bad() { array.storage.allocate(1); }",
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
        let ast = crate::parser::parse(&source).unwrap();
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            let error = vm.load_module(&ast).expect_err(&source);
            assert!(error.span().is_some(), "{source}: {error:?}");
        }
        let mut compiler = crate::aot::AotCompiler::new("invalid-array").unwrap();
        assert!(compiler.compile_module(&ast).is_err(), "{source}");
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
        assert_eq!(peak, 15); // One initial empty block, then seven new block/payload pairs.
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
    // Pause at successive growth allocations, including partial migration.
    for checkpoint in [1, 3, 5, 7, 13, 15] {
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
                if LIVE.with(|n| n.get() >= checkpoint) {
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
fn arrays_sync_helpers_keep_storage_until_the_async_root_finishes() {
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
            vm.load_source(&format!("{SOURCE} extern fn pause() @suspends fn task() @suspends -> Int32 {{ let answer = array_growth() pause() answer }}")).unwrap();
            let mut task = vm.call_async("task", &[]).unwrap();
            assert!(!task.is_finished());
            LIVE.with(|n| assert_eq!(n.get(), 15));
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
