use super::{VmEngine, VmMode};
use crate::{runtime::arena::LIVE, FunctionSignature, Type};
#[path = "../../tests/fixtures/map_stress_oracle.rs"]
mod oracle;
thread_local! {
    static EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) };
    static REMAINING: std::cell::Cell<i32> = const { std::cell::Cell::new(0) };
    static CHECKPOINTS: std::cell::Cell<i32> = const { std::cell::Cell::new(0) };
}
extern "C" fn record(n: i32) {
    EVENTS.with(|e| e.borrow_mut().push(n));
    REMAINING.with(|left| {
        if left.get() > 0 {
            left.set(left.get() - 1);
            if left.get() == 0 {
                CHECKPOINTS.with(|count| count.set(count.get() + 1));
            }
        } else if n == i32::MIN {
            left.set(4);
        }
    });
}
fn configured(mode: VmMode, source: &str) -> VmEngine {
    let registry = std::rc::Rc::new(crate::modules::ModuleRegistry::default());
    registry
        .types()
        .standard
        .set(crate::core::imports::map_test_library())
        .unwrap();
    let mut vm = VmEngine::with_mode_and_registry(mode, registry);
    unsafe {
        vm.register_fn(
            "print_int",
            FunctionSignature::new(vec![Type::Int32], Type::Unit),
            record as *const u8,
        )
        .unwrap();
    }
    vm.load_source_with_modules(source, &crate::ModuleSources::new())
        .unwrap_or_else(|e| {
            panic!(
                "{e} {:?}",
                e.span().and_then(|s| e
                    .sources()
                    .get(s.source_id())
                    .map(|f| (f.name(), s.slice(f.text()))))
            )
        });
    vm
}
// One 256-step cycle covers growth, dense deletion, rehash, random operations and clear.
#[test]
fn map_model_checks_each_state_and_owner_through_one_complete_cycle() {
    check_model(256);
}

#[test]
#[ignore = "exhaustive: long Map model matrix; run by the stress check in Release"]
fn exhaustive_map_stress_checks_each_state_and_owner_against_an_independent_model() {
    check_model(oracle::STEPS);
}

fn check_model(steps: i32) {
    let mut source = oracle::SOURCE.to_owned();
    for distribution in 0..3 {
        for (index, seed) in oracle::SEEDS.into_iter().enumerate() {
            source.push_str(&format!(
                "\nfn probe_{distribution}_{index}() -> Int32 {{ stress({seed}, {distribution}, {steps}) }}"
            ));
        }
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        // All cases share the same Key/Value specializations; compile them once per backend.
        let vm = configured(mode, &source);
        for distribution in 0..3 {
            for (index, seed) in oracle::SEEDS.into_iter().enumerate() {
                reset();
                let entry = format!("probe_{distribution}_{index}");
                assert_eq!(vm.call_int32_0(&entry).unwrap(), 42, "{mode:?} {entry}");
                EVENTS.with(|e| oracle::verify(&e.borrow(), seed, steps, true));
                LIVE.with(|n| assert_eq!(n.get(), 0));
            }
        }
    }
}

#[test]
fn map_stress_reuses_a_bounded_budget_across_calls() {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            crate::runtime::arena::LIMIT.with(|l| l.set(crate::runtime::arena::MAX_BYTES));
        }
    }
    let _restore = Restore;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode, &oracle::source(oracle::SEEDS[0], 2, 256, true));
        crate::runtime::arena::LIMIT.with(|l| l.set(512 * 1024));
        for _ in 0..3 {
            reset();
            assert_eq!(vm.call_int32_0("probe").unwrap(), 42);
            EVENTS.with(|e| oracle::verify(&e.borrow(), oracle::SEEDS[0], 256, true));
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}

#[test]
fn map_stress_pending_owners_survive_reload_and_clean_up_on_cancel() {
    for cut in [1, 64, 120, 128, 256, 300] {
        for finish in [false, true] {
            // Cancellation covers every phase; completing the same deterministic
            // trace after each cut adds no new model or ownership states.
            if finish && cut != 300 {
                continue;
            }
            let mut vm = configured(
                VmMode::Interpreter,
                // The model test checks private indices. Here the public views
                // and owner ledger cover cancellation and retained code instead.
                &oracle::source(oracle::SEEDS[1], 1, 320, false),
            );
            vm.set_interpreter_limits(super::InterpreterLimits {
                poll_budget: Some(1),
                ..Default::default()
            })
            .unwrap();
            reset();
            let mut task = vm.call_async("task", &[]).unwrap();
            while CHECKPOINTS.with(|n| n.get()) < cut {
                assert!(!task.is_finished());
                task.resume().unwrap();
            }
            vm.load_source("fn replacement() -> Int32 { 7 }").unwrap();
            drop(vm);
            if finish {
                while !task.is_finished() {
                    task.resume().unwrap();
                }
                assert_eq!(task.cps_result(), Some(42));
            }
            drop(task);
            EVENTS.with(|e| oracle::verify(&e.borrow(), oracle::SEEDS[1], 320, finish));
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}

#[test]
#[ignore = "opt-in Map arena peak measurement; writes a report, no timing assertions"]
fn map_memory_profile() {
    use crate::runtime::arena::{LIMIT, MAX_BYTES};
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            LIMIT.with(|l| l.set(MAX_BYTES));
        }
    }
    let _restore = Restore;
    let directory = std::env::var_os("JOKY_MAP_MEMORY_OUTPUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target/benchmarks/map-memory")
        });
    std::fs::create_dir_all(&directory).unwrap();
    let mut metadata = String::new();
    for (name, program, args) in [
        ("revision", "git", vec!["rev-parse", "HEAD"]),
        ("working_tree", "git", vec!["status", "--porcelain"]),
        ("rust", "rustc", vec!["--version"]),
        ("os", "uname", vec!["-sr"]),
        ("timestamp_utc", "date", vec!["-u", "+%Y-%m-%dT%H:%M:%SZ"]),
    ] {
        let output = std::process::Command::new(program)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success());
        metadata.push_str(&format!(
            "{name}={}\n",
            String::from_utf8(output.stdout).unwrap().trim()
        ));
    }
    metadata.push_str(&format!("debug_assertions={}\n", cfg!(debug_assertions)));
    let mut hash = std::process::Command::new(if cfg!(target_os = "macos") {
        "shasum"
    } else {
        "sha256sum"
    });
    if cfg!(target_os = "macos") {
        hash.args(["-a", "256"]);
    }
    let output = hash.arg(std::env::current_exe().unwrap()).output().unwrap();
    assert!(output.status.success());
    metadata.push_str(&format!(
        "runner_sha256={}\n",
        String::from_utf8(output.stdout).unwrap().trim()
    ));
    std::fs::write(directory.join("metadata.txt"), metadata).unwrap();
    let source = include_str!("../../benches/programs/map.jk");
    let mut csv = String::from("backend,kernel,n,minimum_successful_arena_budget_bytes\n");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        let mut cases = Vec::new();
        for kernel in ["insert_uniform", "insert_lowbits", "insert_constant"] {
            for n in [0, 32, 128] {
                cases.push((kernel, n));
            }
        }
        cases.extend([
            ("insert_uniform", 1024),
            ("lookup_uniform", 128),
            ("churn_uniform", 128),
            ("iterate_uniform", 128),
        ]);
        for (kernel, n) in cases {
            let run = |budget| {
                LIMIT.with(|limit| limit.set(budget));
                let result = vm.call_int32_2(kernel, n, 7);
                LIMIT.with(|limit| limit.set(MAX_BYTES));
                LIVE.with(|live| assert_eq!(live.get(), 0));
                match result {
                    Ok(value) => {
                        let sum: i32 = (0..n).map(|i| i + 7).sum();
                        let expected = if kernel.starts_with("insert") {
                            n
                        } else if kernel.starts_with("churn") {
                            sum * 4 + n * 6
                        } else {
                            sum * 8
                        };
                        assert_eq!(value, expected);
                        true
                    }
                    Err(e) => {
                        assert_eq!(e.code(), crate::ErrorCode::ResourceLimit, "{e}");
                        false
                    }
                }
            };
            let mut low = 0;
            let mut high = 1024;
            while !run(high) {
                low = high;
                high *= 2;
                assert!(high <= MAX_BYTES);
            }
            while high - low > 1 {
                let mid = low + (high - low) / 2;
                if run(mid) {
                    high = mid;
                } else {
                    low = mid;
                }
            }
            assert!(run(high) && !run(high - 1));
            csv.push_str(&format!("{mode:?},{kernel},{n},{high}\n"));
            println!("{mode:?} {kernel} N={n}: {high} bytes");
            std::fs::write(directory.join("memory.csv"), &csv).unwrap();
        }
    }
    std::fs::write(directory.join("map.jk"), source).unwrap();
    std::fs::write(
        directory.join("memory-runner.rs"),
        include_str!("map_stress_tests.rs"),
    )
    .unwrap();
}

fn reset() {
    EVENTS.with(|e| e.borrow_mut().clear());
    REMAINING.with(|n| n.set(0));
    CHECKPOINTS.with(|n| n.set(0));
}

#[test]
#[cfg(unix)]
fn map_stress_native_checks_tombstones_and_bidirectional_indices() {
    let registry = crate::TypeRegistry::default();
    registry
        .standard
        .set(crate::core::imports::map_test_library())
        .unwrap();
    let source = oracle::source(oracle::SEEDS[0], 2, 320, true);
    let ast = registry
        .resolve_with_modules(
            &crate::parser::parse(&source).unwrap(),
            &crate::ModuleSources::new(),
        )
        .unwrap();
    crate::backends::aot::tests::assert_sync_int32(&ast, "probe", 42);
}
