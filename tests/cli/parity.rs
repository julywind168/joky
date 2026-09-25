use super::*;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Output, Stdio};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How much of the AOT matrix the parity harness should run.
///
/// Release AOT uses Cranelift `opt_level=speed` and is the expensive part of
/// the suite: many parallel `joky build --release` processes pin every core.
/// Default `debug` keeps JIT plus one unoptimized native build. Set
/// `JOKY_TEST_AOT=full` for the complete matrix, or `off` to skip AOT.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AotCoverage {
    Off,
    Debug,
    Full,
}

fn aot_coverage() -> AotCoverage {
    match std::env::var("JOKY_TEST_AOT")
        .ok()
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        None | Some("debug") => AotCoverage::Debug,
        Some("full") => AotCoverage::Full,
        Some("off" | "0" | "none") => AotCoverage::Off,
        Some(other) => panic!("unknown JOKY_TEST_AOT={other:?}; expected off, debug, or full"),
    }
}

fn aot_jobs() -> usize {
    if let Ok(value) = std::env::var("JOKY_TEST_AOT_JOBS") {
        match value.trim().parse::<usize>() {
            Ok(count) if count > 0 => return count,
            _ => panic!("JOKY_TEST_AOT_JOBS must be a positive integer, got {value:?}"),
        }
    }
    match aot_coverage() {
        // `opt_level=speed` is single-thread CPU-heavy; keep one compile at a time.
        AotCoverage::Full => 1,
        AotCoverage::Debug => std::thread::available_parallelism()
            .map(|count| count.get().div_ceil(4).max(1))
            .unwrap_or(1),
        AotCoverage::Off => 1,
    }
}

fn compile_jobs() -> usize {
    if let Ok(value) = std::env::var("JOKY_TEST_COMPILE_JOBS") {
        match value.trim().parse::<usize>() {
            Ok(count) if count > 0 => return count,
            _ => panic!("JOKY_TEST_COMPILE_JOBS must be a positive integer, got {value:?}"),
        }
    }
    // Every compiler child gets two Cranelift workers. Limit all JIT and AOT
    // compiler processes together so parallel Rust tests do not oversubscribe.
    std::thread::available_parallelism()
        .map(|count| count.get().div_ceil(2).max(1))
        .unwrap_or(1)
}

struct JobSlots {
    remaining: Mutex<usize>,
    wake: Condvar,
}

static COMPILE_SLOTS: OnceLock<JobSlots> = OnceLock::new();
static AOT_SLOTS: OnceLock<JobSlots> = OnceLock::new();

struct JobPermit {
    slots: &'static JobSlots,
}

fn acquire_job(slots: &'static OnceLock<JobSlots>, jobs: usize) -> JobPermit {
    let slots = slots.get_or_init(|| JobSlots {
        remaining: Mutex::new(jobs),
        wake: Condvar::new(),
    });
    let mut remaining = slots.remaining.lock().unwrap();
    while *remaining == 0 {
        remaining = slots.wake.wait(remaining).unwrap();
    }
    *remaining -= 1;
    drop(remaining);
    JobPermit { slots }
}

fn acquire_compile() -> JobPermit {
    acquire_job(&COMPILE_SLOTS, compile_jobs())
}

fn acquire_aot() -> JobPermit {
    acquire_job(&AOT_SLOTS, aot_jobs())
}

impl Drop for JobPermit {
    fn drop(&mut self) {
        let mut remaining = self.slots.remaining.lock().unwrap();
        *remaining += 1;
        self.slots.wake.notify_one();
    }
}

pub(super) struct Package {
    pub root: PathBuf,
    name: String,
}

impl Package {
    pub fn new(name: &str, modules: &[(&str, &str)]) -> Self {
        let root = std::env::temp_dir().join(format!("joky-parity-{name}-{}", std::process::id()));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("joky.toml"), format!("name = \"{name}\"\n")).unwrap();
        for (file, source) in modules {
            fs::write(root.join("src").join(file), source).unwrap();
        }
        Self {
            root,
            name: name.to_owned(),
        }
    }

    pub fn check(&self, stdout: &str, error: Option<&str>, instances: &[(&str, usize)]) {
        self.check_with(stdout, error, instances, |_, _| {});
    }

    pub fn check_cached(&self, stdout: &str, error: Option<&str>, instances: &[(&str, usize)]) {
        self.check_with_cache(stdout, error, instances, |_, _| {});
    }

    pub fn check_with(
        &self,
        stdout: &str,
        error: Option<&str>,
        instances: &[(&str, usize)],
        mut check: impl FnMut(&str, &Output),
    ) {
        self.check_modes_with_cache(false, instances, |mode, run| {
            assert_result(run, mode, stdout, error);
            check(mode, run);
        });
    }

    fn check_with_cache(
        &self,
        stdout: &str,
        error: Option<&str>,
        instances: &[(&str, usize)],
        mut check: impl FnMut(&str, &Output),
    ) {
        self.check_modes_with_cache(true, instances, |mode, run| {
            assert_result(run, mode, stdout, error);
            check(mode, run);
        });
    }

    pub(super) fn check_modes(
        &self,
        instances: &[(&str, usize)],
        mut check: impl FnMut(&str, &Output),
    ) {
        self.check_modes_with_cache(false, instances, &mut check);
    }

    fn check_modes_with_cache(
        &self,
        include_cached_jit: bool,
        instances: &[(&str, usize)],
        mut check: impl FnMut(&str, &Output),
    ) {
        let total_started = Instant::now();
        for cached in [false]
            .into_iter()
            .chain(include_cached_jit.then_some(true))
        {
            let mode = if cached { "cached JIT" } else { "cold JIT" };
            let _permit = acquire_compile();
            let started = Instant::now();
            let run = run_bounded(
                Command::new(env!("CARGO_BIN_EXE_joky"))
                    .current_dir(&self.root)
                    .env("JOKY_WORKER_COUNT", "2")
                    .env("JOKY_CODEGEN_JOBS", "2")
                    .env("JOKY_TEST_RESOURCE_REPORT", "1")
                    .args(["run", "--verbose"]),
            );
            self.report_timing(mode, started);
            let stderr = String::from_utf8_lossy(&run.stderr);
            if cached {
                assert!(!stderr.contains("[cache] compile"), "{mode}: {stderr}");
            } else {
                for (function, count) in instances {
                    assert_eq!(
                        stderr
                            .matches(&format!("compile generic {function} ["))
                            .count(),
                        *count,
                        "instance count for {function}: {stderr}"
                    );
                }
            }
            check(mode, &run);
        }
        self.run_aot_modes(&mut check);
        self.report_timing("total", total_started);
    }

    fn run_aot_modes(&self, check: &mut impl FnMut(&str, &Output)) {
        let profiles: &[(bool, &str, &str)] = match aot_coverage() {
            AotCoverage::Off => return,
            AotCoverage::Debug => &[(false, "debug AOT build", "debug AOT")],
            AotCoverage::Full => &[
                (false, "debug AOT build", "debug AOT"),
                (true, "release AOT build", "release AOT"),
            ],
        };
        let mut outputs = Vec::new();
        for &(release, build_phase, _) in profiles {
            let output = self.root.join(if release {
                "native-release"
            } else {
                "native-debug"
            });
            let mut build = Command::new(env!("CARGO_BIN_EXE_joky"));
            build
                .current_dir(&self.root)
                .env("JOKY_CODEGEN_JOBS", "2")
                .args(["build", "-o"])
                .arg(&output);
            if release {
                build.arg("--release");
            }
            let build = {
                let _aot_permit = acquire_aot();
                let _compile_permit = acquire_compile();
                let started = Instant::now();
                let build = build.output().unwrap();
                self.report_timing(build_phase, started);
                build
            };
            assert!(
                build.status.success(),
                "AOT build: {}",
                String::from_utf8_lossy(&build.stderr)
            );
            outputs.push(output);
        }
        // Keep only native artifacts and fixture data, then run from another cwd.
        fs::remove_dir_all(self.root.join("src")).unwrap();
        fs::remove_dir_all(self.root.join(".joky")).unwrap();
        fs::remove_file(self.root.join("joky.toml")).unwrap();
        for ((_, _, mode), output) in profiles.iter().zip(outputs) {
            let started = Instant::now();
            let run = run_bounded(
                Command::new(output)
                    .current_dir(std::env::temp_dir())
                    .env("JOKY_WORKER_COUNT", "2")
                    .env("JOKY_TEST_RESOURCE_REPORT", "1"),
            );
            self.report_timing(mode, started);
            check(mode, &run);
        }
    }

    fn report_timing(&self, phase: &str, started: Instant) {
        if std::env::var("JOKY_TEST_TIMINGS").as_deref() == Ok("1") {
            eprintln!(
                "[parity timing] {} | {phase} | {:.3}s",
                self.name,
                started.elapsed().as_secs_f64()
            );
        }
    }
}

pub(super) fn run_bounded(command: &mut Command) -> Output {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let read = |mut stream: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).unwrap();
            bytes
        })
    };
    let stdout = read(Box::new(stdout));
    let stderr = read(Box::new(stderr));
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            child.kill().unwrap();
            break child.wait().unwrap();
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let output = Output {
        status,
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
    };
    assert!(
        !timed_out,
        "program did not finish within 30s: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

impl Drop for Package {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub(super) fn assert_result(output: &Output, mode: &str, stdout: &str, error: Option<&str>) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(if error.is_some() { 1 } else { 0 }),
        "{mode}: {stderr}"
    );
    assert_eq!(output.stdout, stdout.as_bytes(), "{mode}: {stderr}");
    if let Some(error) = error {
        assert!(stderr.contains(error), "{mode}: {stderr}");
    }
    #[cfg(feature = "runtime-test-support")]
    for phase in ["startup", "drained"] {
        let expected = format!("[resources] {phase}: {:?}", [0usize; 18]);
        // Inherited child stderr can end without a newline. Match the complete
        // report suffix while still requiring exactly one all-zero snapshot.
        assert_eq!(
            stderr
                .lines()
                .filter(|line| line.ends_with(&expected))
                .count(),
            1,
            "{mode}: missing or nonzero {phase} resource snapshot: {stderr}"
        );
    }
    #[cfg(not(feature = "runtime-test-support"))]
    assert!(!stderr.contains("[resources]"), "{mode}: {stderr}");
}

pub(super) fn assert_package_parity(
    name: &str,
    modules: &[(&str, &str)],
    expected: &str,
    instances: &[(&str, usize)],
) {
    Package::new(name, modules).check(expected, None, instances);
}

#[test]
fn cown_pending_survives_cache_and_aot() {
    let package = Package::new(
        "cown-pending",
        &[("main.jk", include_str!("../fixtures/cown_pending.jk"))],
    );
    let legacy = run_bounded(
        Command::new(env!("CARGO_BIN_EXE_joky"))
            .current_dir(&package.root)
            .env("JOKY_TEST_RESOURCE_REPORT", "1")
            .args(["run", "--legacy"]),
    );
    assert_result(&legacy, "legacy", "8000\n", None);
    package.check_cached("8000\n", None, &[]);
}

#[test]
fn cown_abort_cleanup_survives_cache_and_aot() {
    let package = Package::new(
        "cown-abort",
        &[("main.jk", include_str!("../fixtures/cown_abort.jk"))],
    );
    let expected = "2\nleft payload\n3\nright payload\nnormal\n4\n4\nright payload\n4\n5\nnormal branch\n6\nright payload\n6\n";
    let legacy = run_bounded(
        Command::new(env!("CARGO_BIN_EXE_joky"))
            .current_dir(&package.root)
            .env("JOKY_TEST_RESOURCE_REPORT", "1")
            .args(["run", "--legacy"]),
    );
    assert_result(&legacy, "legacy", expected, None);
    package.check_cached(expected, None, &[]);
}

#[test]
fn cown_payload_drop_glue_survives_cache_and_aot() {
    let package = Package::new(
        "cown-payload-drop",
        &[("main.jk", include_str!("../fixtures/cown_payload_drop.jk"))],
    );
    let legacy = run_bounded(
        Command::new(env!("CARGO_BIN_EXE_joky"))
            .current_dir(&package.root)
            .env("JOKY_TEST_RESOURCE_REPORT", "1")
            .args(["run", "--legacy"]),
    );
    assert_result(&legacy, "legacy", "nested value\n", None);
    package.check_cached("nested value\n", None, &[]);
}

#[test]
fn cancelled_cown_payload_drop_glue_survives_cache_and_aot() {
    let package = Package::new(
        "cown-payload-cancel",
        &[(
            "main.jk",
            include_str!("../fixtures/cown_payload_cancel.jk"),
        )],
    );
    let legacy = run_bounded(
        Command::new(env!("CARGO_BIN_EXE_joky"))
            .current_dir(&package.root)
            .env("JOKY_TEST_RESOURCE_REPORT", "1")
            .args(["run", "--legacy"]),
    );
    assert_result(&legacy, "legacy", "winner\n", None);
    package.check_cached("winner\n", None, &[]);
}

#[test]
fn hash_keys_cross_modules_cache_and_aot() {
    Package::new("hash-keys", &[
        ("keys.jk", r#"
            struct Key { let id: Int32; let label: String }
            impl PartialEq for Key { fn equals(&self, other: &Self) -> Bool { self.id == other.id } }
            impl Eq for Key {}
            impl Hash for Key { fn hash(&self, state: &Hasher) -> Unit { Hash.hash(self.id, state) } }
            pub fn make(id: Int32, label: String) -> Key { Key(id: id, label: label) }
            pub fn build(n: Int32) -> Map(Key, String) {
                if n == 0 { Map.empty(Key, String) } else {
                    build(n - 1).insert(make(n, "stored" + " key"), "old" + " value")
                }
            }
            pub fn lookup(K: type + Hash + Eq, a: K, b: K) -> Bool {
                Map#{a => 42}.get(b).unwrap_or(0) == 42
            }
            pub fn pair_lookup(K: type, a: (K, Bytes), b: (K, Bytes)) -> Bool {
                Map#{a => 42}.get(b).unwrap_or(0) == 42
            }
            pub fn digest(K: type + Hash, value: &K) -> UInt64 {
                let state = Hasher()
                Hash.hash(value, state)
                state.finish()
            }
        "#),
        ("main.jk", r#"
            import keys
            import joky/mut_map
            import joky/mut_set
            import joky/time
            struct Local { let id: Int32 }
            impl PartialEq for Local { fn equals(&self, other: &Self) -> Bool { self.id == other.id } }
            impl Eq for Local {}
            impl Hash for Local { fn hash(&self, state: &Hasher) -> Unit {} }
            eff KeyReply {
                @resumable fn map() -> Map((Local, String), Int32)
                @resumable fn empty() -> MutMap(Unit, Unit)
            }
            class IndexCursor {
                var value: Int32 = 0
                fn advance(&self, step: Int32) { self.value = self.value + step }
                fn reset(&self) { self.value = 0 }
            }
            fn expect(value: Bool) { if !value { panic("Hash parity") } }
            fn verify(K: type + Hash + Eq, a: K, b: K) {
                let map = Map#{a => "first"}.insert(b, "second")
                expect(map.get(a).unwrap_or("") == "second")
                let one: UInt64 = 1
                expect(map.length() == one)
                expect(map.remove(a).is_empty())
                expect(Set#{a, b}.length() == one)
                let mm = MutMap#{a => 1}
                expect(mm.insert(b, 2).unwrap_or(0) == 1)
                expect(mm.get(a).unwrap_or(0) == 2)
                expect(mm.remove(b).unwrap_or(0) == 2)
                let ms = MutSet#{a, b}
                expect(ms.length() == one)
                expect(ms.contains(a))
                expect(ms.remove(b))
                expect(ms.is_empty())
            }
            fn verify_index() {
                let map = MutMap#{keys.make(-1, "seed") => "seed"}
                let collisions = MutMap(Local, Int32)()
                let set = MutSet(Local)()
                let cursor = IndexCursor()
                while cursor.value < 16 {
                    let i = cursor.value
                    expect(map.insert(keys.make(i, "original" + " label"), "old" + " value").is_none())
                    expect(collisions.insert(Local(id: i), i).is_none())
                    expect(set.add(Local(id: i)))
                    cursor.advance(1)
                }
                cursor.reset()
                while cursor.value < 16 {
                    let i = cursor.value
                    expect(map.get(keys.make(i, "query")).unwrap_or("") == "old value")
                    expect(collisions.get(Local(id: i)).unwrap_or(-1) == i)
                    expect(set.contains(Local(id: i)))
                    expect(map.remove(keys.make(i, "remove")).unwrap_or("") == "old value")
                    expect(collisions.remove(Local(id: i)).unwrap_or(-1) == i)
                    expect(set.remove(Local(id: i)))
                    expect(!set.contains(Local(id: i)))
                    cursor.advance(2)
                }
                cursor.reset()
                while cursor.value < 16 {
                    let i = cursor.value
                    expect(map.insert(keys.make(i, "reinsert"), "new value").is_none())
                    expect(collisions.insert(Local(id: i), i + 1).is_none())
                    expect(set.add(Local(id: i)))
                    cursor.advance(2)
                }
                cursor.reset()
                while cursor.value < 16 {
                    let i = cursor.value
                    expect(map.contains_key(keys.make(i, "check")))
                    expect(collisions.contains_key(Local(id: i)))
                    expect(set.contains(Local(id: i)))
                    cursor.advance(1)
                }
                expect(!collisions.contains_key(Local(id: 16)))
                let count: UInt64 = 16
                expect(collisions.length() == count)
                expect(set.length() == count)
            }
            fn collision_map(n: Int32) -> Map(Local, Int32) {
                if n == 0 { Map.empty(Local, Int32) } else {
                    collision_map(n - 1).insert(Local(id: n), n)
                }
            }
            fn collision_set(n: Int32) -> Set(Local) {
                if n == 0 { Set#{} } else { collision_set(n - 1).insert(Local(id: n), true) }
            }
            fn verify_persistent() {
                let old = keys.build(24)
                let changed = old.insert(keys.make(12, "renamed"), "new value")
                let removed = changed.remove(keys.make(1, "query"))
                let missing = old.remove(keys.make(999, "absent"))
                let count: UInt64 = 24
                expect(old.length() == count)
                expect(changed.length() == count)
                expect(missing.length() == count)
                let cursor = IndexCursor()
                cursor.advance(1)
                while cursor.value <= 24 {
                    let key = keys.make(cursor.value, "fresh query")
                    expect(old.get(key).unwrap_or("") == "old value")
                    if cursor.value == 12 { expect(changed.get(key).unwrap_or("") == "new value") }
                    else { expect(changed.get(key).unwrap_or("") == "old value") }
                    if cursor.value == 1 { expect(!removed.contains_key(key)) }
                    else { expect(removed.contains_key(key)) }
                    cursor.advance(1)
                }
                let m = collision_map(12)
                let m2 = m.remove(Local(id: 6)).insert(Local(id: 9), 99)
                expect(m.get(Local(id: 6)).unwrap_or(0) == 6)
                expect(m.get(Local(id: 9)).unwrap_or(0) == 9)
                expect(m2.get(Local(id: 6)).is_none())
                expect(m2.get(Local(id: 9)).unwrap_or(0) == 99)
                let s = collision_set(12)
                let s2 = s.remove(Local(id: 1)).insert(Local(id: 99), true)
                expect(s.contains_key(Local(id: 1)))
                expect(!s.contains_key(Local(id: 99)))
                expect(!s2.contains_key(Local(id: 1)))
                expect(s2.contains_key(Local(id: 99)))
                let branches = parallel {
                    | old.insert(keys.make(12, "left"), "left").get(keys.make(12, "query")).unwrap_or("")
                    | old.insert(keys.make(12, "right"), "right").get(keys.make(12, "query")).unwrap_or("")
                }
                expect(branches.0 == "left")
                expect(branches.1 == "right")
                expect(old.get(keys.make(12, "query")).unwrap_or("") == "old value")
            }
            fn main() effects { time } {
                let a = keys.make(7, "hello" + " world")
                let b = keys.make(7, "different")
                let carried = Map#{a => 9}
                let carried_mut = MutMap#{a => 11}
                let bytes = Bytes.from_string("pay" + "load")
                let tuple = (a, (bytes, 1s), ())
                let tuple_map = Map#{tuple => 12}
                let state = Hasher()
                Hash.hash(tuple, state)
                time.sleep(1ms)
                expect(carried.get(b).unwrap_or(0) == 9)
                expect(carried_mut.get(b).unwrap_or(0) == 11)
                let same = (b, (Bytes.from_string("payload"), 1000ms), ())
                expect(tuple_map.get(same).unwrap_or(0) == 12)
                expect(keys.digest(tuple) == keys.digest(same))
                expect(keys.pair_lookup((tuple, bytes), (same, Bytes.from_string("payload"))))
                verify(tuple, same)
                expect(carried.insert(b, 10).get(a).unwrap_or(0) == 10)
                let fresh = Hasher()
                Hash.hash(same, fresh)
                expect(state.finish() == fresh.finish())
                verify(a, b)
                expect(keys.lookup(a, b))
                let x = Local(id: 1)
                let y = Local(id: 2)
                verify(x, x)
                expect(keys.lookup(x, x))
                let m = Map#{x => 10, y => 20}
                expect(m.get(x).unwrap_or(0) == 10)
                expect(m.remove(x).get(y).unwrap_or(0) == 20)
                let h = Hasher()
                Hash.hash(a, h)
                let h2 = Hasher()
                Hash.hash(b, h2)
                expect(h.finish() == h2.finish())
                verify_index()
                verify_persistent()
                let reply = do { KeyReply.map() } with {
                    KeyReply.map() => resume(Map#{(Local(id: 1), "key") => 42})
                }
                expect(reply.get((Local(id: 1), "k" + "ey")).unwrap_or(0) == 42)
                let empty = do { KeyReply.empty() } with {
                    KeyReply.empty() => resume(MutMap#{})
                }
                expect(empty.insert((), ()).is_none())
                expect(empty.contains_key(()))
                println("hash-parity-ok")
            }
        "#),
    ])
    .check_cached("hash-parity-ok\n", None, &[]);
}

#[test]
fn equality_crosses_modules_cache_and_aot() {
    let package = Package::new(
        "equality",
        &[
            ("equality.jk", include_str!("../fixtures/partial_eq.jk")),
            (
                "main.jk",
                r#"
            import equality
            import joky/file
            fn verify_values() {
                equality.verify()
                if !equality.EQUAL_DURATION || !equality.EQUAL_UNIT { panic("imported equality constants") }
                let owned_a = equality.make_owned(7)
                let owned_b = equality.make_owned(7)
                if (owned_a != owned_b) || !PartialEq.equals(owned_a, owned_b) || !equality.equivalent(owned_a, owned_b) {
                    panic("imported borrowed equality")
                }
                let a = equality.make("hello", 1)
                let b = equality.make("hel" + "lo", 9)
                if (a != b) || !PartialEq.equals(a, b) || !equality.same(a, b) || !equality.equivalent(a, b) {
                    panic("imported equality")
                }
                if (FileMode.Read != FileMode.Read) || (SeekFrom.Start == SeekFrom.End) {
                    panic("standard enums")
                }
                if !equality.equivalent(FileMode.Read, FileMode.Read) { panic("enum Eq") }
            }
            fn expect(value: Bool) { if !value { panic("imported container equality") } }
            fn verify_containers() {
                let a = List#{Some(equality.make("hello", 1)), None}
                let b = List#{Some(equality.make("hel" + "lo", 9)), None}
                expect(a == b)
                expect(PartialEq.equals(a, b))
                expect(equality.same(a, b))
                expect(equality.equivalent(a, b))
                expect(equality.optional(Some(a), Some(b)))
                let trap = equality.make("trap", -1)
                expect((0, trap) != (1, trap))
                expect(List#{(0, trap), (1, trap)} != List#{(1, trap), (1, trap)})
                expect(Some(trap) != None)
                expect(None != Some(trap))
                expect(!(Some(trap) == None))
                expect(!(None == Some(trap)))
                expect(equality.same((Some(1.0), List#{2.0}), (Some(1.0), List#{2.0})))
                let nan = 0.0 / 0.0
                let values = List#{nan}
                expect(!equality.same(values, values))
                expect(a.reverse().reverse() == b)
                let expected_length: UInt64 = 2
                expect(a.length() == expected_length)
            }
            fn verify_owned() {
                let owned_a = Some(equality.make_owned(7))
                let owned_b = Some(equality.make_owned(7))
                expect(owned_a != None)
                expect(None != owned_a)
                expect(owned_a == owned_b)
                expect(equality.equivalent(owned_a, owned_b))
                expect((owned_a, "shared") == (owned_b, "shared"))
                let ok: Result(equality.OwnedValue, String) = Ok(equality.make_owned(8))
                expect(ok == Ok(equality.make_owned(8)))
                expect(ok != Err("error"))
                let err: Result(Int32, String) = Err("err" + "or")
                expect(err == Err("error"))
            }
            fn main() {
                verify_values()
                verify_containers()
                verify_owned()
                println("container-eq-ok")
            }
        "#,
            ),
        ],
    );
    let expected = "partial-eq-ok\ncontainer-eq-ok\n";
    let run = run_bounded(
        Command::new(env!("CARGO_BIN_EXE_joky"))
            .current_dir(&package.root)
            .env("JOKY_TEST_RESOURCE_REPORT", "1")
            .args(["run", "--no-cache"]),
    );
    assert_result(&run, "uncached JIT", expected, None);
    package.check_cached(expected, None, &[]);
}

#[test]
fn ordering_crosses_modules_cache_and_aot() {
    let package = Package::new(
        "ordering",
        &[
            ("ordering.jk", include_str!("../fixtures/ordering.jk")),
            (
                "bridge.jk",
                r#"
            import ordering
            pub fn result() -> Ordering { ordering.less() }
            pub fn optional() -> Option(Ordering) { Some(ordering.less()) }
        "#,
            ),
            (
                "main.jk",
                r#"
            import ordering
            import bridge
            fn expect(b: Bool) { if !b { panic("imported ordering") } }
            fn main() {
                ordering.verify()
                expect(ordering.STRING_LESS)
                expect(ordering.DURATION_LESS)
                expect(bridge.result() == Ordering.Less)
                expect(bridge.optional() == Some(Ordering.Less))
                let a = ordering.make(1)
                let b = ordering.make(2)
                expect(a > b)
                expect(Ord.compare(a, b) == Ordering.Greater)
                expect(PartialOrd.partial_compare(a, b) == Some(Ordering.Greater))
                expect(ordering.compare(a, b) == Ordering.Greater)
                expect(ordering.redundant(a, a))
                expect(ordering.compare("hello", "world") == Ordering.Less)
                expect(ordering.compare(Ordering.Equal, bridge.result()) == Ordering.Greater)
            }
        "#,
            ),
        ],
    );
    package.check_cached("ordering-ok\n", None, &[]);
}

#[test]
fn container_ordering_crosses_modules_cache_and_aot() {
    let package = Package::new(
        "container-ordering",
        &[
            (
                "ordering.jk",
                r#"
                struct Rank { let key: Int32; let text: String }
                impl PartialEq for Rank { fn equals(&self, other: &Self) -> Bool { self.key == other.key } }
                impl Eq for Rank {}
                impl PartialOrd for Rank {
                    fn partial_compare(&self, other: &Self) -> Option(Ordering) {
                        if (self.key == 99) || (other.key == 99) { panic("unneeded partial comparison") }
                        PartialOrd.partial_compare(other.key, self.key)
                    }
                }
                impl Ord for Rank {
                    fn compare(&self, other: &Self) -> Ordering {
                        if (self.key == 99) || (other.key == 99) { panic("unneeded total comparison") }
                        Ord.compare(other.key, self.key)
                    }
                }
                pub fn rank(key: Int32) -> Rank { Rank(key: key, text: "owned" + " field") }
                pub fn compare(T: type + Ord, left: &T, right: &T) -> Ordering { Ord.compare(left, right) }
                pub fn partial(T: type + PartialOrd, left: &T, right: &T) -> Option(Ordering) { PartialOrd.partial_compare(left, right) }
                "#,
            ),
            (
                "main.jk",
                include_str!("../fixtures/ordering_containers_parity.jk"),
            ),
        ],
    );
    package.check_cached("container-ordering-ok\n", None, &[]);
}

#[test]
fn sorting_crosses_modules_cache_and_aot() {
    let package = Package::new(
        "sorting",
        &[
            (
                "sorting.jk",
                r#"
                struct Entry { let key: Int32; let label: String; let payload: Option(String) }
                impl PartialEq for Entry { fn equals(&self, other: &Self) -> Bool { self.key == other.key } }
                impl Eq for Entry {}
                impl PartialOrd for Entry {
                    fn partial_compare(&self, other: &Self) -> Option(Ordering) { PartialOrd.partial_compare(self.key, other.key) }
                }
                impl Ord for Entry {
                    fn compare(&self, other: &Self) -> Ordering { Ord.compare(self.key, other.key) }
                }
                pub fn entry(key: Int32, label: String) -> Entry {
                    let payload: Option(String) = if key == 1 { Some(label + " payload") } else { None }
                    Entry(key: key, label: label, payload: payload)
                }
                pub fn sorted(T: type + Ord, xs: List(T)) -> List(T) { xs.sorted() }
                pub fn sort(T: type + Ord, xs: &MutList(T)) { xs.sort() }
            "#,
            ),
            (
                "main.jk",
                r#"
            import sorting
            import joky/list
            import joky/mut_list
            fn expect(b: Bool) { if !b { panic("imported sorting") } }
            fn main() {
                let input = List#{sorting.entry(2, "a"), sorting.entry(1, "b"), sorting.entry(2, "c")}
                let result = input.sorted()
                expect(result.head()!.label == "b")
                expect(result.tail()!.head()!.label == "a")
                expect(result.tail()!.tail()!.head()!.label == "c")
                expect(result.head()!.payload == Some("b payload"))
                expect(result.tail()!.head()!.payload.is_none())
                expect(sorting.sorted(input) == result)
                expect(list.sorted(input) == result)
                expect(input.head()!.label == "a")
                let xs = MutList#{sorting.entry(2, "a"), sorting.entry(1, "b"), sorting.entry(2, "c")}
                xs.sort()
                expect(xs.get(0)!.label == "b")
                sorting.sort(xs)
                expect(xs.get(1)!.label == "a")
                expect(xs.get(2)!.label == "c")
                expect(list.sorted(List#{3, 1, 2}) == List#{1, 2, 3})
                expect(List#{"z", "", "ab", "a"}.sorted() == List#{"", "a", "ab", "z"})
                let words = MutList#{"z", "a", "ab"}
                words.sort()
                expect(words.get(0)! == "a")
                expect(words.get(2)! == "z")
                println("sorting-ok")
            }
        "#,
            ),
        ],
    );
    package.check_cached("sorting-ok\n", None, &[]);
}

#[test]
fn standard_value_structs_cross_cache_and_aot() {
    let package = Package::new(
        "standard-value-structs",
        &[(
            "main.jk",
            r#"
            import joky/bytes
            import joky/map
            import joky/set
            import joky/string
            fn expect(value: Bool) { if !value { panic("value structs") } }
            fn main() {
                expect(bytes.empty().is_empty())
                let data = bytes.from_string("abc")
                let three: UInt64 = 3
                let first_byte: UInt8 = 97
                expect(bytes.length(data) == three)
                expect(data.get(0)! == first_byte)
                expect(data.slice(1, 2)!.to_string()! == "bc")
                expect(data.get(3).is_none())
                expect(data.slice(1, 3).is_none())
                expect(data.concat(Bytes.from_string("d")).to_string()! == "abcd")
                expect(data.to_string()! == "abc")
                let text = string.trim(" hello ")
                expect(string.to_upper(text) == "HELLO")
                expect(string.to_lower("HELLO") == text)
                expect(string.starts_with(text, "he"))
                expect(string.ends_with(text, "lo"))
                expect(string.contains(text, "ell"))
                expect(string.is_ascii(text))
                let five: UInt64 = 5
                expect(string.scalar_count(text) == five)
                expect(string.grapheme_count(text) == five)
                expect(text == "hello")
                expect(Debug.debug(text) == "\"hello\"")
                let original = Map(String, Int32)()
                let first = map.insert(original, "key", 1)
                let second = map.insert(first, "key", 2)
                expect(map.is_empty(original))
                expect(map.get(first, "key")! == 1)
                expect(map.get(second, "key")! == 2)
                expect(map.is_empty(map.remove(second, "key")))
                let empty = set.empty(Int32)
                let added = set.insert(empty, 1)
                expect(set.is_empty(empty))
                expect(set.contains(added, 1))
                expect(added.get(1)! == true)
                expect(set.is_empty(set.remove(added, 1)))
                println("value-structs-ok")
            }
        "#,
        )],
    );
    package.check_cached("value-structs-ok\n", None, &[]);
}

#[test]
fn from_string_crosses_modules_generics_cache_and_aot() {
    let package = Package::new(
        "from-string",
        &[
            (
                "parse.jk",
                r#"
            import joky/string
            pub fn parse(T: type + FromString, value: String) -> Result(T, String) { value.parse(T) }
            pub fn explicit(T: type + FromString, value: String) -> Result(T, String) { FromString.from_string(T, value) }
            pub fn check(T: type + FromString + PartialEq, text: String, expected: T, invalid: String) {
                let owned = text.concat("")
                if parse(T, owned)! != expected { panic("parse") }
                if explicit(T, owned)! != expected { panic("explicit parse") }
                match parse(T, invalid.concat("")) {
                    Ok(value) => panic("unexpected success")
                    Err(source) => if source != invalid { panic("lost input") }
                }
            }
            trait Factory { fn create(value: Int32) -> Self }
            trait Default { fn default() -> Self }
        "#,
            ),
            (
                "model.jk",
                r#"
            import parse
            struct Number { let value: Int32 }
            impl FromString for Number {
                fn from_string(value: &String) -> Result(Self, String) { Ok(Number(value: value.parse(Int32)?)) }
            }
            impl parse.Factory for Number {
                fn create(value: Int32) -> Self { Number(value: value) }
            }
            impl parse.Default for Number { fn default() -> Self { Number(value: 52) } }
            class Label { let value: String }
            impl FromString for Label {
                fn from_string(value: &String) -> Result(Self, String) { Ok(Label(value: value)) }
            }
            pub fn parse_number(value: String) -> Result(Number, String) { parse.parse(Number, value) }
        "#,
            ),
            (
                "main.jk",
                r#"
            import joky/string
            import parse
            import model
            struct Local { let value: Int32 }
            impl FromString for Local {
                fn from_string(value: &String) -> Result(Self, String) { Ok(Local(value: value.parse(Int32)? + 1)) }
            }
            fn expect(value: Bool) { if !value { panic("FromString") } }
            fn verify_custom() {
                expect("43".parse(model.Number)!.value == 43)
                expect(FromString.from_string(model.Number, value: "44")!.value == 44)
                expect(parse.parse(model.Number, "45")!.value == 45)
                expect(parse.explicit(model.Number, "46")!.value == 46)
                expect(model.parse_number("47")!.value == 47)
                expect(parse.parse(Local, "48")!.value == 49)
                expect(parse.explicit(Local, "49")!.value == 50)
                let label = parse.parse(model.Label, "label")!
                expect(label.value == "label")
                let other = FromString.from_string(model.Label, "other")!
                expect(other.value == "other")
                expect(parse.Factory.create(model.Number, 51).value == 51)
                expect(parse.Default.default(model.Number).value == 52)
            }
            fn verify_builtins() {
                parse.check(Int8, "-128", -128, "128")
                parse.check(Int16, "-32768", -32768, "32768")
                parse.check(Int32, "-2147483648", -2147483648, "2147483648")
                parse.check(Int64, "-9223372036854775808", -9223372036854775808, "9223372036854775808")
                parse.check(UInt8, "255", 255, "256")
                parse.check(UInt16, "65535", 65535, "65536")
                parse.check(UInt32, "4294967295", 4294967295, "4294967296")
                parse.check(UInt64, "18446744073709551615", 18446744073709551615, "18446744073709551616")
                parse.check(Float32, "1.0000000596046448", 1.00000011920928955078125, "invalid")
                parse.check(Float64, "6.25", 6.25, "invalid")
                parse.check(Bool, "false", false, "True")
                expect("true".parse(Bool)!)
            }
            fn main() {
                verify_custom()
                verify_builtins()
                println("from-string-ok")
            }
        "#,
            ),
        ],
    );
    package.check_cached("from-string-ok\n", None, &[]);
}

#[test]
fn sorting_comparator_panic_terminates_jit_and_aot() {
    let source = r#"
        struct Entry { let text: String }
        impl PartialEq for Entry { fn equals(&self, other: &Self) -> Bool { self.text == other.text } }
        impl Eq for Entry {}
        impl PartialOrd for Entry {
            fn partial_compare(&self, other: &Self) -> Option(Ordering) { PartialOrd.partial_compare(self.text, other.text) }
        }
        impl Ord for Entry {
            fn compare(&self, other: &Self) -> Ordering {
                if self.text == "panic" { panic("comparison failed") }
                Ord.compare(self.text, other.text)
            }
        }
    "#;
    for (kind, operation) in [("List", "sorted"), ("MutList", "sort")] {
        let main = format!("{source}\nfn main() {{ let xs = {kind}#{{Entry(text: \"z\"), Entry(text: \"y\"), Entry(text: \"panic\"), Entry(text: \"a\")}}; let _ = xs.{operation}(); () }}");
        let package = Package::new(&format!("sort-panic-{kind}"), &[("main.jk", &main)]);
        // Panic is an unrecoverable trap, not an unwinding runtime error.
        package.check_modes(&[], |mode, output| {
            assert!(!output.status.success(), "{mode}: {output:?}");
            assert!(output.stdout.is_empty(), "{mode}: {output:?}");
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("panic: comparison failed"),
                "{mode}: {output:?}"
            );
        });
    }
}

#[test]
fn cown_regions_survive_cache_and_aot() {
    let package = Package::new(
        "cown-regions",
        &[("main.jk", include_str!("../fixtures/cown_regions.jk"))],
    );
    let expected = "2\n2\n2\n3\ndrained\n";
    let legacy = run_bounded(
        Command::new(env!("CARGO_BIN_EXE_joky"))
            .current_dir(&package.root)
            .env("JOKY_TEST_RESOURCE_REPORT", "1")
            .args(["run", "--legacy"]),
    );
    assert_result(&legacy, "legacy", expected, None);
    package.check_cached(expected, None, &[]);
}

#[test]
fn cown_region_contracts_cross_modules_cache_and_aot() {
    Package::new(
        "cown-region-modules",
        &[
            (
                "nodes.jk",
                r#"
            class Node {
                var next: Option(Cown(Node)) = None
                var value: Int32 = 0
                fn link(other: Cown(Node)) { self.next = Some(other) }
                fn bump() { self.value = self.value + 1 }
            }
            pub fn make() -> Cown(Node) { Cown.new(Node()) }
            pub fn link(a: Cown(Node), b: Cown(Node)) {
                when (a) |state| { state.link(b) }
            }
        "#,
            ),
            (
                "main.jk",
                r#"
            import nodes
            fn main() {
                let outer = nodes.make()
                region {
                    let a = nodes.make()
                    let b = nodes.make()
                    nodes.link(a, b)
                    nodes.link(b, a)
                    when (outer) |state| { state.bump() }
                }
                println(when (outer) |state| { state.value })
            }
        "#,
            ),
        ],
    )
    .check_cached("1\n", None, &[]);
}

#[test]
fn cown_region_escape_is_rejected_across_imports() {
    let package = Package::new(
        "cown-region-escape",
        &[
            (
                "nodes.jk",
                r#"
            class Node { var next: Option(Cown(Node)) = None; fn link(other: Cown(Node)) { self.next = Some(other) } }
            pub fn make() -> Cown(Node) { Cown.new(Node()) }
            pub fn link(a: Cown(Node), b: Cown(Node)) { when (a) |state| { state.link(b) } }
        "#,
            ),
            (
                "main.jk",
                r#"
            import nodes
            fn main() { let outer = nodes.make(); region { nodes.link(outer, nodes.make()) } }
        "#,
            ),
        ],
    );
    let output = run_bounded(
        Command::new(env!("CARGO_BIN_EXE_joky"))
            .current_dir(&package.root)
            .args(["check"]),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("region"), "{stderr}");
}

#[test]
fn cown_region_contract_changes_invalidate_dependents() {
    let nodes = r#"
        class Node { var next: Option(Cown(Node)) = None; fn link(other: Cown(Node)) { self.next = Some(other) } }
        pub fn make() -> Cown(Node) { Cown.new(Node()) }
        pub fn touch(a: Cown(Node)) { () }
    "#;
    let package = Package::new("cown-region-contract-cache", &[
        ("nodes.jk", nodes),
        ("main.jk", "import nodes\nfn main() { let outer = nodes.make(); region { nodes.touch(outer) } }"),
    ]);
    let checked = run_bounded(
        Command::new(env!("CARGO_BIN_EXE_joky"))
            .current_dir(&package.root)
            .args(["check"]),
    );
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    // Same public signature, but touch now stores a caller-region allocation
    // into its parameter. A cached caller must be checked against that contract.
    fs::write(
        package.root.join("src/nodes.jk"),
        nodes.replace(
            "pub fn touch(a: Cown(Node)) { () }",
            "pub fn touch(a: Cown(Node)) { let b = make(); when (a) |s| { s.link(b) } }",
        ),
    )
    .unwrap();
    let checked = run_bounded(
        Command::new(env!("CARGO_BIN_EXE_joky"))
            .current_dir(&package.root)
            .args(["check"]),
    );
    let stderr = String::from_utf8_lossy(&checked.stderr);
    assert_eq!(checked.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("region"), "{stderr}");
}

#[test]
fn tcp_split_survives_cache_and_aot() {
    use std::io::Write;
    use std::sync::atomic::{AtomicBool, Ordering};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let server = std::thread::spawn(move || {
        while !stopped.load(Ordering::Acquire) {
            let (mut stream, _) = match listener.accept() {
                Ok(pair) => pair,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                Err(e) => panic!("accept: {e}"),
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut input = Vec::new();
            stream.read_to_end(&mut input).unwrap();
            assert_eq!(input, b"ping");
            stream.write_all(b"pong").unwrap();
        }
    });
    let source = format!(
        r#"
        import joky/socket/tcp
        import joky/time
        fn main() effects {{ tcp, time }} {{
            let stream = tcp.connect("127.0.0.1", {port})!
            let (reader, writer) = stream.split()!
            let responses = parallel {{
                | reader.read(4)!.to_string()!
                | {{ time.sleep(10ms); let _ = writer.write(Bytes.from_string("ping"))!; writer.close()!; "sent" }}
            }}
            println(responses.0)
        }}
    "#
    );
    let package = Package::new("tcp-split", &[("main.jk", &source)]);
    let legacy = run_bounded(
        Command::new(env!("CARGO_BIN_EXE_joky"))
            .current_dir(&package.root)
            .env("JOKY_TEST_RESOURCE_REPORT", "1")
            .env("JOKY_WORKER_COUNT", "1")
            .args(["run", "--legacy"]),
    );
    assert_result(&legacy, "legacy", "pong\n", None);
    package.check_cached("pong\n", None, &[]);
    stop.store(true, Ordering::Release);
    server.join().unwrap();
}

#[test]
fn when_until_survives_cache_and_aot() {
    let package = Package::new(
        "when-until",
        &[("main.jk", include_str!("../fixtures/cown_until.jk"))],
    );
    for workers in ["1", "4"] {
        let legacy = run_bounded(
            Command::new(env!("CARGO_BIN_EXE_joky"))
                .current_dir(&package.root)
                .env("JOKY_TEST_RESOURCE_REPORT", "1")
                .env("JOKY_WORKER_COUNT", workers)
                .args(["run", "--legacy"]),
        );
        assert_result(&legacy, "legacy", "until ok\n", None);
    }
    package.check_cached("until ok\n", None, &[]);
}

#[test]
fn tuple_bindings_survive_cache_and_aot() {
    let package = Package::new(
        "tuple-bindings",
        &[("main.jk", include_str!("../fixtures/tuple_bindings.jk"))],
    );
    let legacy = run_bounded(
        Command::new(env!("CARGO_BIN_EXE_joky"))
            .current_dir(&package.root)
            .env("JOKY_TEST_RESOURCE_REPORT", "1")
            .args(["run", "--legacy"]),
    );
    assert_result(&legacy, "legacy", "", None);
    package.check_cached("", None, &[]);
}

#[test]
fn imported_unit_variant_defaults_preserve_enum_identity_and_cache() {
    let util = r#"
enum Mode { Disabled, Enabled(payload: Bytes) }
struct Config { let mode: Mode = Mode.Disabled }
pub fn disabled(config: Config) -> Bool {
    match config.mode { Mode.Disabled => true; Mode.Enabled(payload) => { let _ = payload; false } }
}
"#;
    let main = r#"
import config
fn main() {
    println(config.disabled(config.Config()))
    let Mode: type = config.Mode
    println(config.disabled(config.Config(mode: Mode.Enabled(b"custom"))))
}
"#;
    Package::new(
        "unit-variant-defaults",
        &[("config.jk", util), ("main.jk", main)],
    )
    .check_cached("true\nfalse\n", None, &[]);
}
