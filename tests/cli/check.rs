use super::*;

struct Package(std::path::PathBuf);
impl Package {
    fn new(name: &str, source: &str) -> Self {
        let root = std::env::temp_dir().join(format!("joky-check-{name}-{}", std::process::id()));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("joky.toml"), "name = \"check-test\"\n").unwrap();
        fs::write(root.join("src/main.jk"), source).unwrap();
        Self(root)
    }
    fn command(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_joky"))
            .current_dir(&self.0)
            .args(args)
            .output()
            .unwrap()
    }
    fn check(&self, args: &[&str]) -> String {
        let result = self.command(args);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(result.stdout.is_empty(), "check must not run the program");
        String::from_utf8(result.stderr).unwrap()
    }
}
impl Drop for Package {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn check_accepts_projects_files_and_libraries_without_execution() {
    let package = Package::new(
        "inputs",
        "fn main() { println(\"must not run\"); panic(\"must not run\") }",
    );
    for args in [
        vec!["check", "--no-cache"],
        vec!["check", ".", "--no-cache"],
        vec!["check", "src/main.jk", "--no-cache"],
    ] {
        assert!(package.check(&args).contains("Checked"));
    }
    fs::write(
        package.0.join("src/library.jk"),
        "pub fn answer() -> Int32 { 42 }",
    )
    .unwrap();
    package.check(&["check", "src/library.jk", "--no-cache"]);
    assert!(!package.0.join(".joky").exists());
}

#[test]
fn check_shares_generic_cache_with_run_and_invalidates_dependencies() {
    let package = Package::new(
        "cache",
        "import util\nfn main() { println(util.copy(util.ANSWER)) }",
    );
    let util = package.0.join("src/util.jk");
    fs::write(
        &util,
        "pub const ANSWER = 42\npub fn copy(T: type, value: T) -> T { value }",
    )
    .unwrap();
    assert!(package
        .check(&["check", "-v"])
        .contains("compile generic copy"));
    let warm = package.check(&["check", "-v"]);
    assert!(warm.contains("hit generic copy"));
    assert!(!warm.contains("[cache] compile"));
    assert!(!package.0.join(".joky/bin").exists());
    let run = package.command(&["run", "-v"]);
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(run.stdout, b"42\n");
    assert!(!String::from_utf8_lossy(&run.stderr).contains("[cache] compile"));
    fs::write(
        &util,
        "pub const ANSWER = 43\npub fn copy(T: type, value: T) -> T { value }",
    )
    .unwrap();
    assert!(package.check(&["check", "-v"]).contains("[cache] compile"));
    fs::write(
        &util,
        "pub const ANSWER = 43\npub fn copy(T: type, value: T) -> T { missing }",
    )
    .unwrap();
    let failed = package.command(&["check"]);
    assert!(!failed.status.success());
    let stderr = strip_ansi(&String::from_utf8_lossy(&failed.stderr));
    assert!(
        stderr.contains("util.jk") && stderr.contains("missing"),
        "{stderr}"
    );
}

#[test]
fn check_reports_parse_type_ownership_effect_and_hidden_suspension_errors() {
    let cases = [
        ("eff time { @suspends fn sleep(duration: Duration) -> Unit } class Gauge { let base: Int32 = 40; fn bump() -> Int32 effects { time } { time.sleep(1ms); self.base + 2 } } fn main() effects { time } { let results = parallel {\n| Gauge().bump() }; println(results.0) }", "cannot carry borrowed local"),
        ("fn main( {", "syntax error"),
        ("fn main() { let n: Int32 = true }", "expected Int32, found Bool"),
        ("class C { fn touch() {} } fn main() { let first = C(); let second = first; first.touch() }", "after move"),
        ("eff Ask { fn get() -> Int32 } fn value() -> Int32 { Ask.get() } fn main() {}", "effect"),
        ("fn main() { var n = 1; let read = move fn() -> Int32 { let result = parallel {\n| 1\n| 2\n}; n } }", "mutable captures cannot suspend"),
    ];
    let package = Package::new("errors", "");
    for (source, expected) in cases {
        fs::write(package.0.join("src/main.jk"), source).unwrap();
        let result = package.command(&["check", "--no-cache"]);
        assert!(!result.status.success(), "accepted {source}");
        let stderr = strip_ansi(&String::from_utf8_lossy(&result.stderr));
        assert!(stderr.contains(expected), "expected {expected}: {stderr}");
        assert!(stderr.contains("main.jk"), "{stderr}");
    }
}

#[test]
fn check_frontend_session_resets_diagnostics_and_reports_cache_work() {
    let package = Package::new(
        "service",
        "import util\nfn main() { println(util.value()) }",
    );
    let util = package.0.join("src/util.jk");
    fs::write(&util, "pub fn value() -> Int32 { false }").unwrap();
    let graph =
        || joky::module::ModuleGraph::load(&package.0.join("src/main.jk"), &package.0).unwrap();
    let mut frontend = joky::Frontend::new();
    assert!(frontend.check(&graph(), None).is_err());
    assert_eq!(
        frontend.take_diagnostic_source().unwrap().0,
        util.canonicalize().unwrap()
    );
    assert!(frontend.check(&graph(), None).is_err()); // Leave this diagnostic undrained.
    fs::write(&util, "pub fn value() -> Int32 { 42 }").unwrap();
    let cache = package.0.join("cache");
    let report = frontend.check(&graph(), Some(&cache)).unwrap();
    assert_eq!(report.source_modules, 2);
    assert_eq!(report.generic_instances, 0);
    assert_eq!(report.compilations, 2);
    assert!(frontend.take_diagnostic_source().is_none());
    let report = frontend.check(&graph(), Some(&cache)).unwrap();
    assert_eq!(report.cache_hits, 2);
    assert_eq!(report.compilations, 0);
}

#[test]
fn check_validates_cli_arguments_and_project_manifest() {
    let package = Package::new("arguments", "fn main() {}");
    for args in [
        vec!["check", "--legacy"],
        vec!["check", "--unknown"],
        vec!["check", "a", "b"],
    ] {
        assert!(!package.command(&args).status.success());
    }
    fs::remove_file(package.0.join("joky.toml")).unwrap();
    assert!(String::from_utf8_lossy(&package.command(&["check"]).stderr).contains("no joky.toml"));
}

#[test]
fn check_does_not_load_foreign_libraries() {
    let package = Package::new(
        "foreign",
        r#"
        @extern(c, "joky_check_missing_library", "missing_symbol")
        fn missing() -> Int32;
        fn main() { println(missing()) }
    "#,
    );
    package.check(&["check", "--no-cache"]);
}

#[test]
fn check_reports_generic_ownership_errors_in_the_definition_module() {
    let package = Package::new(
        "generic-error",
        "import util\nclass Token {}\nfn main() { let value = util.bad(Token()); () }",
    );
    fs::write(
        package.0.join("src/util.jk"),
        "pub fn bad(T: type, value: T) -> T { let moved = value; value }",
    )
    .unwrap();
    let output = package.command(&["check", "--no-cache"]);
    assert!(!output.status.success());
    let stderr = strip_ansi(&String::from_utf8_lossy(&output.stderr));
    assert!(
        stderr.contains("util.jk") && stderr.contains("after move"),
        "{stderr}"
    );
}

#[test]
fn check_json_is_a_stable_machine_readable_result() {
    let package = Package::new("json", "fn main() {}");
    let output = package.command(&["check", "--json", "--no-cache"]);
    assert!(output.status.success());
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value["ok"].as_bool().unwrap());
    assert_eq!(value["report"]["source_modules"], 1);
    assert!(value["diagnostics"].as_array().unwrap().is_empty());

    fs::write(
        package.0.join("src/main.jk"),
        "fn main() { let n: Int32 = true }",
    )
    .unwrap();
    let output = package.command(&["check", "--json", "--no-cache"]);
    assert!(!output.status.success());
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(!value["ok"].as_bool().unwrap());
    assert_eq!(value["diagnostics"][0]["stage"], "semantic");
    assert!(value["diagnostics"][0]["path"]
        .as_str()
        .unwrap()
        .ends_with("main.jk"));
    assert!(value["diagnostics"][0]["span"]["start"].is_number());
}

#[test]
fn check_json_collects_independent_module_errors_without_cascades() {
    let package = Package::new("multi-error", "import bad_a\nimport bad_b\nfn main() {}");
    fs::write(
        package.0.join("src/bad_a.jk"),
        "pub fn first() -> Int32 { true }",
    )
    .unwrap();
    fs::write(
        package.0.join("src/bad_b.jk"),
        "pub fn second() -> Int32 { false }",
    )
    .unwrap();
    let output = package.command(&["check", "--json", "--no-cache"]);
    assert!(!output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(!value["ok"].as_bool().unwrap());
    let diagnostics = value["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 2);
    let paths = diagnostics
        .iter()
        .map(|diagnostic| diagnostic["path"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!(paths.iter().any(|path| path.ends_with("bad_a.jk")));
    assert!(paths.iter().any(|path| path.ends_with("bad_b.jk")));
    assert!(diagnostics
        .iter()
        .all(|diagnostic| diagnostic["stage"] == "semantic"));
}

#[test]
fn frontend_checks_unsaved_sources_without_writing_them() {
    let root = std::env::temp_dir().join(format!("joky-check-sources-{}", std::process::id()));
    fs::create_dir_all(root.join("src")).unwrap();
    let entry = root.join("src/main.jk");
    let dependency = root.join("src/util.jk");
    let sources = [
        joky::SourceFile::new(&entry, "import util\nfn main() { println(util.answer()) }"),
        joky::SourceFile::new(&dependency, "pub fn answer() -> Int32 { 42 }"),
    ];
    let mut frontend = joky::Frontend::new();
    let report = frontend
        .check_sources(&entry, &root, &sources, None)
        .unwrap();
    assert_eq!(report.source_modules, 2);
    assert!(!entry.exists());
    assert!(!dependency.exists());

    let invalid = [
        joky::SourceFile::new(&entry, "import util\nfn main() { util.missing() }"),
        joky::SourceFile::new(&dependency, "pub fn answer() -> Int32 { 42 }"),
    ];
    assert!(frontend
        .check_sources(&entry, &root, &invalid, None)
        .is_err());
    let diagnostic = frontend.take_diagnostic().unwrap();
    assert_eq!(diagnostic.stage, joky::Stage::Semantic);
    assert!(diagnostic
        .path
        .as_ref()
        .is_some_and(|path| path.ends_with("main.jk")));
    assert!(diagnostic.span.is_some());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn frontend_reuses_incremental_graph_and_invalidates_changed_source() {
    let root = std::env::temp_dir().join(format!("joky-check-incremental-{}", std::process::id()));
    fs::create_dir_all(root.join("src")).unwrap();
    let entry = root.join("src/main.jk");
    let dependency = root.join("src/util.jk");
    let sources = [
        joky::SourceFile::new(&entry, "import util\nfn main() { println(util.answer()) }"),
        joky::SourceFile::new(&dependency, "pub fn answer() -> Int32 { 42 }"),
    ];
    let cache = root.join("cache");
    let mut frontend = joky::Frontend::new();

    let first = frontend
        .check_sources(&entry, &root, &sources, Some(&cache))
        .unwrap();
    assert_eq!(first.graph_cache_hits, 0);
    assert_eq!(first.graph_cache_misses, 2);
    assert_eq!(first.compilations, 2);

    let second = frontend
        .check_sources(&entry, &root, &sources, Some(&cache))
        .unwrap();
    assert_eq!(second.graph_cache_hits, 2);
    assert_eq!(second.graph_cache_misses, 0);
    assert_eq!(second.cache_hits, 2);
    assert_eq!(second.compilations, 0);

    let changed = [
        sources[0].clone(),
        joky::SourceFile::new(&dependency, "pub fn answer() -> Int32 { 43 }"),
    ];
    let third = frontend
        .check_sources(&entry, &root, &changed, Some(&cache))
        .unwrap();
    assert_eq!(third.graph_cache_hits, 1);
    assert_eq!(third.graph_cache_misses, 1);
    assert_eq!(third.cache_hits, 1);
    assert_eq!(third.compilations, 1);

    let abi_changed = [
        sources[0].clone(),
        joky::SourceFile::new(&dependency, "pub fn answer() -> Int64 { 43 }"),
    ];
    let fourth = frontend
        .check_sources(&entry, &root, &abi_changed, Some(&cache))
        .unwrap();
    assert_eq!(fourth.graph_cache_hits, 1);
    assert_eq!(fourth.graph_cache_misses, 1);
    assert_eq!(fourth.cache_hits, 0);
    assert_eq!(fourth.compilations, 2);

    frontend.clear_incremental_cache();
    let fifth = frontend
        .check_sources(&entry, &root, &changed, Some(&cache))
        .unwrap();
    assert_eq!(fifth.graph_cache_hits, 0);
    assert_eq!(fifth.graph_cache_misses, 2);
    assert_eq!(fifth.cache_hits, 2);
    assert_eq!(fifth.compilations, 0);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn tls_upgrade_requires_ownership_and_effects() {
    for (name, source, expected) in [
        (
            "tls-moved-tcp",
            r#"
import joky/socket/tcp
import joky/socket/tls
fn main() -> Result(Unit, String) effects { tcp, tls } {
    let tcp = tcp.connect("127.0.0.1", 5432)?
    let secure = tls.upgrade(tcp, "localhost", Bytes())?
    let _ = tcp.read(1)?
    secure.close()?
    Ok(())
}
"#,
            "moved",
        ),
        (
            "tls-missing-effect",
            r#"
import joky/socket/tcp
import joky/socket/tls
fn main() -> Result(Unit, String) effects { tcp } {
    let secure = tls.upgrade(tcp.connect("127.0.0.1", 5432)?, "localhost", Bytes())?
    let _ = secure
    Ok(())
}
"#,
            "tls",
        ),
        (
            "tls-borrowed-close",
            r#"
import joky/socket/tls
fn close(stream: &TlsStream) -> Result(Unit, String) effects { tls } { stream.close() }
fn main() {}
"#,
            "borrow",
        ),
    ] {
        let package = Package::new(name, source);
        let output = package.command(&["check", "--no-cache"]);
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{name} unexpectedly accepted");
        assert!(error.contains(expected), "{name}: {error}");
    }
}
