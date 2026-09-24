//! End-to-end throughput/latency benchmarks for the suspend/resume machinery.
//!
//! These are `#[ignore]`d so normal test runs stay fast. Run them with:
//!
//! ```text
//! cargo test --lib --release bench_suspend_resume -- --ignored --nocapture
//! ```
//!
//! Each scenario times whole-program runs. Because every run also pays the
//! fixed JIT compilation cost, the harness measures two workload sizes and
//! estimates the per-suspend cost from their difference. The programs drive
//! the suspend loop through recursion: one level per iteration covers both a
//! suspension and a Pending call handoff, which is the shape phase 3
//! optimized for.
//!
//! Workload sizes are TOTAL suspends, split evenly across the branches.
//! Each program checks its completed operation count. A four-way run has
//! one parallel group for the entire workload, not one join per suspend.
//!
//! We warm up both sizes, alternate their measurement order, and report the
//! median and range of signed paired differences. A non-positive median is
//! inconclusive, never an unsigned Duration subtraction or infinite rate.
//! Differencing only estimates fixed compilation overhead; compilation and
//! scheduler noise remain, so these numbers are not a regression gate.
//!
//! The old 8d90b93 baseline was 7.45us/suspend for one branch and 18.35us
//! for four branches: ~0.41x aggregate throughput, not 1.6x. The earlier
//! interpretation incorrectly multiplied the already-normalized rate by four.
//! It also attributed the gap to joins without profiling evidence.
//! Historical measurements and reproduction details: docs/archive/reports/phase5-0.md.

use crate::mir::passes::MirPassManager;
use crate::mir::MirProgram;
use joky_runtime::host::{
    testing::{complete_suspend_with_payload, register_suspend_provider},
    RuntimeScope,
};
use std::ffi::c_void;
use std::time::{Duration, Instant};

const WARMUP_RUNS: usize = 1;
const MEASURED_RUNS: usize = 5;

fn build_mir(source: &str) -> MirProgram {
    let program = crate::syntax::parse_program(source).expect("bench source should parse");
    let types = crate::sema::check_program(&program).expect("bench source should type check");
    let core = crate::hir::CoreProgram::lower(program, types).expect("bench HIR lowering");
    let mut mir = MirProgram::lower(&core).expect("bench MIR lowering");
    MirPassManager::default_pipeline()
        .run(&mut mir)
        .expect("bench MIR passes");
    mir
}

unsafe extern "C" fn inline_complete_provider(
    handle: *mut c_void,
    operation: u64,
    _arguments: *const u8,
    _arguments_size: usize,
    _result: *mut u8,
    _result_size: usize,
) -> u8 {
    // Complete inside the start hook: the resume is enqueued immediately and
    // a worker dispatches the machine entry without any timer involvement.
    // This is the fastest legitimate suspend/resume round trip.
    unsafe { complete_suspend_with_payload(handle, operation, std::ptr::null(), 0) }
}

/// One whole-program run: compiles (JIT) and executes `iterations` suspends.
fn run_bench_program(
    scenario: &str,
    run: usize,
    source: &str,
    provider_operation: Option<u64>,
) -> Duration {
    let started = Instant::now();
    eprintln!("[bench] {scenario} run{run} start");
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    let mut backend = crate::codegen::CraneliftBackend::new().expect("bench JIT backend");
    let mir = build_mir(source);
    let provider = provider_operation.map(|operation| unsafe {
        register_suspend_provider(
            &scope,
            operation,
            inline_complete_provider as *mut c_void,
            std::ptr::null_mut(),
        )
        .expect("benchmark provider registration")
    });
    backend
        .compile_and_run_program(&mir, &scope)
        .expect("bench program should run");
    drop(provider);
    scope.close_and_wait();
    let elapsed = started.elapsed();
    eprintln!("[bench] {scenario} run{run} done in {elapsed:?}");
    elapsed
}

/// Estimate seconds per total suspend from signed paired workload differences.
fn measure(name: &str, source: fn(usize) -> String, n: usize) -> Option<f64> {
    let provider_operation = provider_operation_for(name);
    for run in 0..WARMUP_RUNS {
        run_bench_program(name, run, &source(n), provider_operation);
        run_bench_program(name, run, &source(2 * n), provider_operation);
    }
    let mut samples = Vec::with_capacity(MEASURED_RUNS);
    for run in 0..MEASURED_RUNS {
        let mut elapsed = [Duration::ZERO; 2];
        let order = if run % 2 == 0 { [0, 1] } else { [1, 0] };
        for index in order {
            elapsed[index] = run_bench_program(
                name,
                WARMUP_RUNS + 2 * run + index,
                &source(n * (index + 1)),
                provider_operation,
            );
        }
        samples.push((elapsed[1].as_secs_f64() - elapsed[0].as_secs_f64()) / n as f64);
    }
    samples.sort_by(f64::total_cmp);
    let median = samples[MEASURED_RUNS / 2];
    eprintln!(
        "[bench] {name}: paired ns/suspend min={:.0} median={:.0} max={:.0}",
        samples[0] * 1e9,
        median * 1e9,
        samples[MEASURED_RUNS - 1] * 1e9
    );
    if samples[0] <= 0.0 {
        eprintln!("[bench] {name}: non-positive sample; timing noise overlaps the workload delta");
    }
    (median > 0.0).then_some(median)
}

/// Looks up the runtime operation id for the inline provider scenario.
fn provider_operation_for(name: &str) -> Option<u64> {
    if name != "provider_inline" && name != "provider_inline_parallel_4" {
        return None;
    }
    let mir = build_mir(&tick_scenario(1, 1));
    let effect = mir.types().effects().by_name("Tick").expect("Tick effect");
    let operation = mir
        .types()
        .effects()
        .operation_by_name(effect, "tick")
        .expect("tick operation");
    Some((operation.effect.0 as u64) << 32 | operation.operation as u64)
}

fn tick_scenario(iterations: usize, branches: usize) -> String {
    assert!(branches > 0 && iterations.is_multiple_of(branches));
    let total = (0..branches)
        .map(|index| format!("results.{index}"))
        .collect::<Vec<_>>()
        .join(" + ");
    let arms: String = (0..branches)
        .map(|_| format!("            | work({})\n", iterations / branches))
        .collect();
    format!(
        "eff Tick {{ @suspends fn tick() -> Unit }}\n\
         fn work(n: Int32) -> Int32 effects {{ Tick }} {{\n\
             if n == 0 {{ 0 }} else {{\n\
                 Tick.tick()\n\
                 1 + work(n - 1)\n\
             }}\n\
         }}\n\
         fn main() effects {{ Tick }} {{\n\
             let results = parallel {{\n{arms}            }}\n\
             if {total} != {iterations} {{ panic(\"wrong suspend count\") }} else {{ }}\n\
         }}"
    )
}

fn timer_scenario(iterations: usize, branches: usize, duration: &str) -> String {
    assert!(branches > 0 && iterations.is_multiple_of(branches));
    let total = (0..branches)
        .map(|index| format!("results.{index}"))
        .collect::<Vec<_>>()
        .join(" + ");
    let arms: String = (0..branches)
        .map(|_| format!("            | work({})\n", iterations / branches))
        .collect();
    format!(
        "eff time {{ @suspends fn sleep(duration: Duration) -> Unit }}\n\
         fn work(n: Int32) -> Int32 effects {{ time }} {{\n\
             if n == 0 {{ 0 }} else {{\n\
                 time.sleep({duration})\n\
                 1 + work(n - 1)\n\
             }}\n\
         }}\n\
         fn main() effects {{ time }} {{\n\
             let results = parallel {{\n{arms}            }}\n\
             if {total} != {iterations} {{ panic(\"wrong suspend count\") }} else {{ }}\n\
         }}"
    )
}

fn report(name: &str, n: usize, per_suspend: f64, ops_per_second: f64) {
    eprintln!(
        "{:<28} n={:<8} {:>10.0} ns/suspend {:>12.0} ops/s",
        name,
        n,
        per_suspend * 1e9,
        ops_per_second
    );
}

#[test]
#[ignore = "benchmark: cargo test --lib --release bench_suspend_resume -- --ignored --nocapture"]
fn bench_suspend_resume_paths() {
    eprintln!(
        "=== suspend/resume benchmark ({MEASURED_RUNS} paired samples, estimated JIT difference) ==="
    );
    // This workload intentionally retains recursive Pending call handoffs;
    // it measures that activation shape as well as provider suspension.
    type Scenario = (&'static str, fn(usize) -> String, usize);
    let scenarios: &[Scenario] = &[
        ("provider_inline", |n| tick_scenario(n, 1), 5_000),
        ("timer_1ms", |n| timer_scenario(n, 1, "1ms"), 500),
        ("timer_0ms", |n| timer_scenario(n, 1, "0ms"), 5_000),
        ("provider_inline_parallel_4", |n| tick_scenario(n, 4), 5_000),
    ];
    for (name, source, n) in scenarios {
        if let Some(per_suspend) = measure(name, *source, *n) {
            report(name, *n, per_suspend, 1.0 / per_suspend);
        } else {
            eprintln!("{name}: inconclusive; increase workload or reduce measurement noise");
        }
    }
}
