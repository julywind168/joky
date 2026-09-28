//! Steady-state while-loop benchmark. Run with `cargo bench --bench while`.
//! Compilation, linking, loading, and correctness checks are outside timing.
#[path = "support/lua.rs"]
mod lua;

use joky::{aot::AotCompiler, parser, vm::VmEngine};
use libloading::Library;
use std::{
    error::Error,
    fmt::Write as _,
    fs,
    hint::black_box,
    io,
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

type BenchResult<T> = Result<T, Box<dyn Error>>;
type Entry = unsafe extern "C" fn(i32, i32) -> i32;
const SOURCE: &str = include_str!("programs/while.jk");
const FUNCTIONS: [&str; 3] = ["while_sum", "while_recurrence", "while_sequence"];
const SEED: i32 = 7;
const MAX_BATCH: usize = 10_000_000;

struct Config {
    iterations: Vec<i32>,
    samples: usize,
    sample_time: Duration,
    output: PathBuf,
    check_only: bool,
    lua: bool,
}

impl Config {
    fn parse() -> BenchResult<Option<Self>> {
        let mut config = Self {
            iterations: vec![0, 1_000, 100_000],
            samples: 15,
            sample_time: Duration::from_millis(30),
            output: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/benchmarks/while"),
            check_only: false,
            lua: false,
        };
        let mut benchmark = false;
        let mut output_set = false;
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                // Cargo passes these to harness-free benchmark executables.
                "--bench" => benchmark = true,
                "--check" | "--test" => config.check_only = true,
                "--lua" => config.lua = true,
                "--help" | "-h" => {
                    println!("Usage: cargo bench --bench while -- [OPTIONS]\n\
                        --iterations N,N  Loop sizes; zero-iteration baseline is always included (default: 1000,100000)\n\
                        --samples N       Samples per backend and workload, 3..1000 (default: 15)\n\
                        --sample-ms N     Target batch time in milliseconds, 1..10000 (default: 30)\n\
                        --output DIR      Save metadata, source, raw samples, and summaries\n\
                        --check           Check all backends without timing (also used by cargo test)\n\
                        --lua             Include installed Lua (native integer and per-body mask32 variants)\n\
                                          Requires pkg-config and Lua 5.3+ with 64-bit integers; LUA_PKG_CONFIG selects package\n\
                        Requires macOS/Linux and cc; CC may specify a compiler executable.");
                    return Ok(None);
                }
                "--iterations" => {
                    config.iterations = vec![0];
                    for part in next_arg(&mut args, &arg)?.split(',') {
                        let value: i32 = part.parse()?;
                        if !(0..=100_000_000).contains(&value) {
                            return Err(invalid("Iterations must be between 0 and 100000000"));
                        }
                        config.iterations.push(value);
                    }
                    config.iterations.sort_unstable();
                    config.iterations.dedup();
                }
                "--samples" => {
                    config.samples = next_arg(&mut args, &arg)?.parse()?;
                    if !(3..=1000).contains(&config.samples) {
                        return Err(invalid("Samples must be between 3 and 1000"));
                    }
                }
                "--sample-ms" => {
                    let value: u64 = next_arg(&mut args, &arg)?.parse()?;
                    if !(1..=10_000).contains(&value) {
                        return Err(invalid("Sample time must be between 1 and 10000 ms"));
                    }
                    config.sample_time = Duration::from_millis(value);
                }
                "--output" => {
                    config.output = next_arg(&mut args, &arg)?.into();
                    output_set = true;
                }
                _ => return Err(invalid(format!("Unknown argument: {arg}"))),
            }
        }
        // `cargo test --bench while` (including --release) invokes this binary
        // without --bench. Keep ordinary test runs fast and free of timings.
        config.check_only |= !benchmark;
        if config.lua && !output_set {
            config.output.set_file_name("while-lua");
        }
        Ok(Some(config))
    }
}

fn next_arg(args: &mut impl Iterator<Item = String>, flag: &str) -> BenchResult<String> {
    args.next()
        .ok_or_else(|| invalid(format!("Missing value for {flag}")))
}

fn invalid(message: impl Into<String>) -> Box<dyn Error> {
    io::Error::other(message.into()).into()
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> BenchResult<Self> {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path = std::env::temp_dir().join(format!("joky-while-{}-{nonce}", std::process::id()));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

enum Backend {
    Aot {
        entries: [Entry; 3],
        // Keep code loaded until every function pointer is no longer used.
        _library: Library,
    },
    Jit(VmEngine),
    Interpreter(VmEngine),
    Lua {
        vm: Rc<lua::Lua>,
        masked: bool,
    },
}

impl Backend {
    fn name(&self) -> &'static str {
        match self {
            Self::Aot { .. } => "AOT",
            Self::Jit(_) => "JIT",
            Self::Interpreter(_) => "Interpreter",
            Self::Lua { masked: false, .. } => "Lua-native",
            Self::Lua { masked: true, .. } => "Lua-mask32",
        }
    }

    fn call(&self, kernel: usize, n: i32, seed: i32) -> BenchResult<i32> {
        match self {
            Self::Aot { entries, .. } => {
                // SAFETY: All exports come from the source above, have the exact
                // two-Int32 ABI, and are kept alive by this backend's library.
                Ok(unsafe { entries[kernel](n, seed) })
            }
            Self::Jit(vm) | Self::Interpreter(vm) => {
                Ok(vm.call_int32_2(FUNCTIONS[kernel], n, seed)?)
            }
            Self::Lua { vm, masked } => vm.call(kernel + if *masked { 3 } else { 0 }, n, seed),
        }
    }
}

fn load_backends(directory: &Path, cc: &std::ffi::OsStr) -> BenchResult<Vec<Backend>> {
    if !cfg!(any(target_os = "macos", target_os = "linux")) {
        return Err(invalid(
            "The AOT benchmark currently supports macOS and Linux",
        ));
    }
    let object = directory.join("while.o");
    let library_path = directory.join(if cfg!(target_os = "macos") {
        "while.dylib"
    } else {
        "while.so"
    });
    let ast = parser::parse(SOURCE)?;
    let mut compiler = AotCompiler::new("while_benchmark")?;
    compiler.compile_module(&ast)?;
    compiler.write_object(&object)?;
    let linked = Command::new(cc)
        .arg(if cfg!(target_os = "macos") {
            "-dynamiclib"
        } else {
            "-shared"
        })
        .arg(&object)
        .arg("-o")
        .arg(&library_path)
        .output()?;
    if !linked.status.success() {
        return Err(invalid(format!(
            "AOT linking failed: {}",
            String::from_utf8_lossy(&linked.stderr)
        )));
    }
    // SAFETY: This is our freshly generated, trusted native object, with no
    // constructors or imported host functions. Its owner stays in Backend.
    let library = unsafe { Library::new(&library_path)? };
    let mut entries = Vec::new();
    for name in FUNCTIONS {
        // SAFETY: The source fixes every export's ABI to (i32, i32) -> i32.
        entries.push(unsafe { *library.get::<Entry>(name.as_bytes())? });
    }
    let mut jit = VmEngine::new();
    jit.load_source(SOURCE)?;
    let mut interpreter = VmEngine::new_interpreter();
    interpreter.load_source(SOURCE)?;
    Ok(vec![
        Backend::Aot {
            entries: entries.try_into().unwrap(),
            _library: library,
        },
        Backend::Jit(jit),
        Backend::Interpreter(interpreter),
    ])
}

fn reference(kernel: usize, n: i32, seed: i32) -> i32 {
    let mut value = seed;
    for i in 0..n {
        value = if kernel == 1 {
            value
                .wrapping_mul(1_664_525)
                .wrapping_add(1_013_904_223)
                .wrapping_add(i)
        } else {
            value.wrapping_add(i)
        };
    }
    value
}

fn check(backends: &[Backend], iterations: &[i32]) -> BenchResult<()> {
    let mut sizes = vec![-1, 0, 1, 2, 17, 1000];
    sizes.extend(iterations);
    sizes.sort_unstable();
    sizes.dedup();
    for (kernel, name) in FUNCTIONS.iter().enumerate() {
        for &n in &sizes {
            for seed in [0, SEED, -17, i32::MIN, i32::MAX] {
                let expected = reference(kernel, n, seed);
                for backend in backends {
                    let actual = backend.call(kernel, n, seed)?;
                    if actual != expected {
                        return Err(invalid(format!(
                            "{} {name}({n}, {seed}): expected {expected}, got {actual}",
                            backend.name(),
                        )));
                    }
                }
            }
        }
    }
    Ok(())
}

fn measure(
    backend: &Backend,
    kernel: usize,
    n: i32,
    calls: usize,
    expected: i32,
) -> BenchResult<Duration> {
    let mut checksum = 0_i32;
    let started = Instant::now();
    for _ in 0..calls {
        let result = backend.call(black_box(kernel), black_box(n), black_box(SEED))?;
        checksum = checksum.wrapping_add(black_box(result));
    }
    let elapsed = started.elapsed();
    // Check after stopping the timer. Every measured batch consumes its results.
    if checksum != expected.wrapping_mul(calls as i32) {
        return Err(invalid(
            "Measured batch checksum differs from the reference",
        ));
    }
    black_box(checksum);
    Ok(elapsed)
}

fn calibrate(
    backend: &Backend,
    kernel: usize,
    n: i32,
    expected: i32,
    target: Duration,
) -> BenchResult<usize> {
    let mut calls = 1;
    loop {
        let elapsed = measure(backend, kernel, n, calls, expected)?;
        if elapsed >= target / 4 || calls == MAX_BATCH {
            let scale = target.as_secs_f64() / elapsed.as_secs_f64().max(1e-9);
            return Ok(((calls as f64 * scale).ceil() as usize).clamp(1, MAX_BATCH));
        }
        calls = (calls * 2).min(MAX_BATCH);
    }
}

fn percentile(sorted: &[f64], percent: usize) -> f64 {
    sorted[(sorted.len() * percent).div_ceil(100).saturating_sub(1)]
}

fn metadata(config: &Config, cc: &std::ffi::OsStr) -> String {
    fn command(program: &std::ffi::OsStr, args: &[&str]) -> String {
        Command::new(program)
            .args(args)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .unwrap_or_else(|| "unavailable".into())
    }
    let cpu = if cfg!(target_os = "macos") {
        command("sysctl".as_ref(), &["-n", "machdep.cpu.brand_string"])
    } else {
        fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|text| {
                text.lines()
                    .find(|line| line.starts_with("model name"))
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "unavailable".into())
    };
    let rust = command("rustc".as_ref(), &["--version"]);
    let compiler = command(cc, &["--version"])
        .lines()
        .next()
        .unwrap_or("unavailable")
        .to_owned();
    let default_opt_level =
        cranelift_codegen::settings::Flags::new(cranelift_codegen::settings::builder()).opt_level();
    format!("timestamp_utc={}\nrevision={}\nworking_tree_dirty={}\nos={}\narch={}\ncpu={}\nrust={}\ncc={}\ndebug_assertions={}\niterations={:?}\nsamples={}\nsample_target_ms={}\nseed={}\ncodegen=current backend defaults, no benchmark-specific overrides\ncranelift_default_opt_level={}\ncall_boundary=AOT native symbol; JIT and Interpreter checked VmEngine API\n",
        command("date".as_ref(), &["-u", "+%Y-%m-%dT%H:%M:%SZ"]),
        command("git".as_ref(), &["rev-parse", "HEAD"]),
        !command("git".as_ref(), &["status", "--porcelain"]).is_empty(),
        command("uname".as_ref(), &["-sr"]), std::env::consts::ARCH, cpu, rust, compiler,
        cfg!(debug_assertions), config.iterations, config.samples, config.sample_time.as_millis(), SEED, default_opt_level)
}

fn run() -> BenchResult<()> {
    let Some(config) = Config::parse()? else {
        return Ok(());
    };
    if cfg!(debug_assertions) && !config.check_only {
        return Err(invalid(
            "Timing requires an optimized build; use cargo bench --bench while",
        ));
    }
    let cc = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let scratch = Scratch::new()?;
    let mut backends = load_backends(&scratch.0, &cc)?;
    let lua_metadata = if config.lua {
        let vm = Rc::new(lua::Lua::load(&scratch.0, &cc)?);
        let metadata = vm.metadata.clone();
        backends.push(Backend::Lua {
            vm: vm.clone(),
            masked: false,
        });
        backends.push(Backend::Lua { vm, masked: true });
        metadata
    } else {
        String::new()
    };
    check(&backends, &config.iterations)?;
    println!(
        "Correctness: all {} backends/variants match wrapping Int32 references.",
        backends.len()
    );
    if config.check_only {
        println!("Check-only run; no timing or performance comparison was performed.");
        return Ok(());
    }
    fs::create_dir_all(&config.output)?;
    let metadata = metadata(&config, &cc) + &lua_metadata;
    fs::write(config.output.join("metadata.txt"), &metadata)?;
    fs::write(config.output.join("while.jk"), SOURCE)?;
    fs::write(config.output.join("runner.rs"), include_str!("while.rs"))?;
    if config.lua {
        fs::write(config.output.join("while.lua"), lua::SOURCE)?;
        fs::write(config.output.join("lua_bridge.c"), lua::BRIDGE)?;
        fs::write(config.output.join("lua.rs"), include_str!("support/lua.rs"))?;
    }
    fs::write(
        config.output.join("Cargo.lock"),
        include_str!("../Cargo.lock"),
    )?;
    let mut raw =
        String::from("kernel,iterations,backend,sample,calls,elapsed_ns,ns_per_call,expected\n");
    let mut summary = String::from("kernel,iterations,backend,calls_per_sample,samples,min_ns,median_ns,p95_ns,max_ns,ns_per_iteration,relative_to_aot\n");
    let mut report = format!(
        "# While benchmark\n\n```text\n{metadata}```\n\n\
        Steady-state execution; excludes compilation/linking/loading. Includes the call boundary.\n\
        AOT uses resolved native symbols; JIT/Interpreter use checked VmEngine calls.\n\
        N=0 measures call/dispatch overhead; ns/iteration is median/N without subtraction.\n\
        Batches adapt per backend, after calibration and one untimed warm-up batch.\n\
        Backend order rotates each sample. P95 is nearest-rank over batch means, not individual-call tail latency.\n\
        Relative to AOT means backend median/AOT median for the same kernel and N; greater than 1 is slower.\n\
        No confidence intervals or automated performance thresholds are provided.\n\
        Ratios describe this machine/run, not a language-wide performance guarantee.\n\n\
        "
    );
    if config.lua {
        report.push_str("Lua uses cached functions and protected calls in the same process; no process startup is timed.\n\
            Lua-native uses 64-bit integers, then normalizes the result to signed Int32.\n\
            Lua-mask32 additionally masks the accumulator after each loop body (one extra operation).\n\
            Both match these kernels' Int32 outputs: only addition/multiplication use the accumulator,\n\
            and loop control never observes it. This equivalence does not extend to arbitrary programs.\n\
            Lua's sequence kernel uses increment/conditional break, since it has no assignment expression.\n\
            N=0 includes Lua's final normalization; call boundaries are not identical across backends.\n\n");
    }
    report.push_str("| Kernel | N | Backend | Calls/sample | Median ns/call | P95 ns/call | ns/iteration | vs AOT |\n\
        |---|---:|---|---:|---:|---:|---:|---:|\n");
    // Clear prior results before measuring, so an interrupted run cannot pair
    // newly written metadata with samples left over from an older run.
    fs::write(config.output.join("samples.csv"), &raw)?;
    fs::write(config.output.join("summary.csv"), &summary)?;
    fs::write(config.output.join("report.md"), &report)?;
    println!("Steady-state only; AOT uses a native symbol, JIT/Interpreter use checked VM calls.");
    println!(
        "{:<18} {:>8} {:<12} {:>14} {:>14} {:>12} {:>10}",
        "kernel", "N", "backend", "median ns", "p95 ns", "ns/iter", "vs AOT"
    );
    for (kernel, name) in FUNCTIONS.iter().enumerate() {
        for &n in &config.iterations {
            let expected = reference(kernel, n, SEED);
            let mut batches = vec![0; backends.len()];
            let mut timings = vec![Vec::new(); backends.len()];
            for (index, backend) in backends.iter().enumerate() {
                batches[index] = calibrate(backend, kernel, n, expected, config.sample_time)?;
                measure(backend, kernel, n, batches[index], expected)?;
            }
            for sample in 0..config.samples {
                for offset in 0..backends.len() {
                    let index = (sample + offset) % backends.len();
                    let backend = &backends[index];
                    let calls = batches[index];
                    let elapsed = measure(backend, kernel, n, calls, expected)?;
                    let per_call = elapsed.as_nanos() as f64 / calls as f64;
                    timings[index].push(per_call);
                    writeln!(
                        raw,
                        "{name},{n},{},{sample},{calls},{},{per_call:.6},{expected}",
                        backend.name(),
                        elapsed.as_nanos()
                    )?;
                }
            }
            for values in &mut timings {
                values.sort_by(f64::total_cmp);
            }
            let baseline = median(&timings[0]);
            for (index, backend) in backends.iter().enumerate() {
                let values = &timings[index];
                let med = median(values);
                let p95 = percentile(values, 95);
                let per_iter = if n == 0 {
                    String::new()
                } else {
                    format!("{:.6}", med / n as f64)
                };
                let ratio = med / baseline;
                writeln!(
                    summary,
                    "{name},{n},{},{},{},{:.6},{med:.6},{p95:.6},{:.6},{per_iter},{ratio:.6}",
                    backend.name(),
                    batches[index],
                    config.samples,
                    values[0],
                    values[values.len() - 1]
                )?;
                writeln!(
                    report,
                    "| {name} | {n} | {} | {} | {med:.3} | {p95:.3} | {per_iter} | {ratio:.3}x |",
                    backend.name(),
                    batches[index]
                )?;
                println!(
                    "{name:<18} {n:>8} {:<12} {med:>14.3} {p95:>14.3} {per_iter:>12} {ratio:>9.2}x",
                    backend.name()
                );
            }
            // Preserve completed groups if a later case is interrupted.
            fs::write(config.output.join("samples.csv"), &raw)?;
            fs::write(config.output.join("summary.csv"), &summary)?;
            fs::write(config.output.join("report.md"), &report)?;
        }
    }
    println!("Results: {}", config.output.display());
    Ok(())
}

fn median(sorted: &[f64]) -> f64 {
    let middle = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    } else {
        sorted[middle]
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}
