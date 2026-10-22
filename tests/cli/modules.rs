//! User module loading, imports and prelude selection.
use super::*;

#[test]
#[cfg(unix)]
fn user_file_modules_run_and_build_from_the_entry_directory() {
    let work = Workspace::new();
    work.write(
        "project/app/value.jk",
        r#"
        pub enum Choice { Empty, Value(Int32) }
        pub type Number = Int32
        fn offset(n: Number) -> Number { n + 1 }
        pub fn make(n: Number) -> Choice { Value(offset(n)) }
        pub fn copy<T>(n: T) -> T { n }
    "#,
    );
    work.write(
        "project/app/left.jk",
        r#"
        import app/value as values.{type Choice, make}
        pub fn value() -> Choice { make(20) }
    "#,
    );
    work.write(
        "project/app/right.jk",
        r#"
        import app/value as values.{type Choice, Value, copy}
        pub fn value() -> Choice { Value(copy(21)) }
    "#,
    );
    work.write(
        "project/main.jk",
        r#"
        import app/left as l
        import app/right as r
        import app/value.{type Choice, Value}
        import joky/io.{println} fn report_int32(value: Int32) { println(&value) }
        fn main() -> Int32 {
            let x = match l.value() { Value(n) => n, Choice.Empty => 0 }
            let y = match r.value() { Value(n) => n, Choice.Empty => 0 }
            report_int32(x + y)
            x + y
        }
    "#,
    );
    for args in [
        vec!["run", "project/main.jk"],
        vec!["run", "--interpreter", "project/main.jk"],
    ] {
        let output = work.cli(&args);
        assert_eq!(
            output.status.code(),
            Some(42),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"42\n");
    }
    let output = work.cli(&["build", "project/main.jk", "-o", "program"]);
    success(&output);
    let output = Command::new(work.path().join("program")).output().unwrap();
    assert_eq!(output.status.code(), Some(42));
    assert_eq!(output.stdout, b"42\n");
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn user_file_module_constants_match_across_all_backends() {
    let work = Workspace::new();
    work.write(
        "project/app/config.jk",
        r#"
        pub const LIMIT: Int32 = 100
        const OFFSET: Int32 = 7
        pub const TOTAL: Int32 = LIMIT + OFFSET
    "#,
    );
    work.write(
        "project/app/derived.jk",
        r#"
        import app/config as cfg.{TOTAL}
        pub const SCALED: Int32 = TOTAL * 2
        pub fn scaled() -> Int32 { SCALED }
    "#,
    );
    work.write(
        "project/main.jk",
        r#"
        import app/config as cfg
        import app/derived as d
        import joky/io.{println} fn report_int32(value: Int32) { println(&value) }
        fn main() {
            report_int32(cfg.LIMIT + cfg.TOTAL + d.SCALED + d.scaled())
        }
    "#,
    );
    work.assert_all_backends("project/main.jk", &[], b"635\n", 0);
}

#[test]
fn user_file_runtime_trace_and_output_protection_cover_dependencies() {
    let work = Workspace::new();
    let dependency = "pub fn divide(n: Int32) -> Int32 { 42 / n }";
    work.write("app/math.jk", dependency);
    work.write(
        "main.jk",
        "import app/math fn main() -> Int32 { math.divide(0) }",
    );
    let native_error = work.cli(&["run", "main.jk"]);
    failure(&native_error, "app/math.jk:1");
    failure(&native_error, "42 / n");
    let output = work.cli(&["run", "--interpreter", "main.jk"]);
    failure(&output, "app/math.jk:1");
    failure(&output, "at app/math.divide");
    failure(&output, "at main");
    failure(
        &work.cli(&["build", "main.jk", "-o", "app/math.jk"]),
        "must not replace the source file",
    );
    assert_eq!(
        std::fs::read_to_string(work.path().join("app/math.jk")).unwrap(),
        dependency
    );
}

#[test]
#[cfg(unix)]
fn user_files_share_host_imports_including_aot() {
    let work = Workspace::new();
    work.write(
        "app/engine.jk",
        r#"
        import joky/vm as host
        pub fn value() -> Int32 {
            let h = host.Vm.interpreter()
            { let vm_text = "fn answer() -> Int32 {{ 42 }}"; host.load_source(h, &vm_text) }
            let result = { let vm_text = "answer"; host.call_int32_0(h, &vm_text) }
            host.close(h)
            result
        }
    "#,
    );
    work.write(
        "main.jk",
        "import app/engine import joky/vm fn main() -> Int32 { engine.value() }",
    );
    for args in [
        vec!["run", "main.jk"],
        vec!["run", "--interpreter", "main.jk"],
    ] {
        let output = work.cli(&args);
        assert_eq!(
            output.status.code(),
            Some(42),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    success(&work.cli(&["build", "main.jk", "-o", "program"]));
    let output = Command::new(work.path().join("program")).output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(42),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[cfg(unix)]
fn unused_dependency_failure_still_links_its_runtime() {
    // A private module function is emitted even when no export reaches it, so
    // its failing body must still select the native runtime library.
    let work = Workspace::new();
    work.write(
        "app/math.jk",
        r#"
        fn divide(n: Int32) -> Int32 { 42 / n }
        pub fn double(n: Int32) -> Int32 { n * 2 }
        pub fn fails(n: Int32) -> Int32 { divide(n) }
    "#,
    );
    work.write(
        "main.jk",
        "import app/math fn main() -> Int32 { math.double(21) }",
    );
    for args in [
        vec!["run", "main.jk"],
        vec!["run", "--interpreter", "main.jk"],
    ] {
        let output = work.cli(&args);
        assert_eq!(
            output.status.code(),
            Some(42),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    success(&work.cli(&["build", "main.jk", "-o", "program"]));
    let output = Command::new(work.path().join("program")).output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(42),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Reaching the failing body still names the dependency, not the entry file.
    work.write(
        "boom.jk",
        "import app/math.{fails} fn main() -> Int32 { fails(0) }",
    );
    failure(&work.cli(&["run", "boom.jk"]), "app/math.jk:2");
    failure(&work.cli(&["run", "boom.jk"]), "42 / n");
    success(&work.cli(&["build", "boom.jk", "-o", "boom"]));
    let output = Command::new(work.path().join("boom")).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Division by zero"));
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn prelude_module_aliases_and_opt_out_work_in_run_and_build() {
    let work = Workspace::new();
    for (file, source, no_prelude) in [
        ("prelude", "fn main() -> Int32 { match (42).to_u64() { Some(n) => int32(n), None => 0 } }".to_owned(), false),
        ("imports", format!("{}\nfn main() -> Int32 {{ check() }}", include_str!("../fixtures/imports.jk")), true),
        ("plain", "enum Option { Present(Int32) } fn main() -> Int32 { match Option.Present(42) { Present(n) => n } }".into(), true),
        ("explicit", "import joky/prelude fn main() -> Int32 { match Some(42) { Some(n) => n, None => 0 } }".into(), true),
    ] {
        let source_file = format!("{file}.jk");
        work.write(&source_file, &source);
        for interpreter in [false, true] {
            let mut args = vec!["run"];
            if interpreter { args.push("--interpreter"); }
            if no_prelude { args.push("--no-prelude"); }
            args.push(&source_file);
            let output = work.cli(&args);
            assert_eq!(output.status.code(), Some(42), "{}", String::from_utf8_lossy(&output.stderr));
        }
        let mut args = vec!["build"];
        if no_prelude { args.push("--no-prelude"); }
        args.push(&source_file);
        success(&work.cli(&args));
        let output = Command::new(work.path().join(file)).output().unwrap();
        assert_eq!(output.status.code(), Some(42), "{output:?}");
    }
    for command in ["run", "build"] {
        failure(
            &work.cli(&[command, "--no-prelude", "prelude.jk"]),
            "has no method",
        );
        failure(
            &work.cli(&[command, "--no-prelude", "--no-prelude", "plain.jk"]),
            "more than once",
        );
        failure(&work.cli(&[command, "plain.jk"]), "already defined");
    }
    // Native registration remains supported by JIT/AOT; ordinary aliased VM calls
    // also run in the interpreter.
    for register in [false, true] {
        let initialization = if register {
            "let name = \"next\"; register(h, &name, next); let code = \"extern fn next(n: Int32) -> Int32 fn answer() -> Int32 {{ next(41) }}\"; v.load_source(h, &code)"
        } else {
            "let code = \"fn answer() -> Int32 {{ 42 }}\"; v.load_source(h, &code)"
        };
        let source = format!("import joky/vm as v.{{type Vm as Engine, register_int32_1 as register, call_int32_0 as invoke, close}} fn next(n: Int32) -> Int32 {{ n + 1 }} fn main() -> Int32 {{ let vm = 7; let h: Engine = Engine.new(); {initialization}; let entry = \"answer\"; let answer = invoke(h, &entry); close(h); answer }}");
        work.write("host_alias.jk", &source);
        let output = work.cli(&["run", "host_alias.jk"]);
        assert_eq!(output.status.code(), Some(42), "{output:?}");
        if !register {
            let output = work.cli(&["run", "--interpreter", "host_alias.jk"]);
            assert_eq!(output.status.code(), Some(42), "{output:?}");
        }
        success(&work.cli(&["build", "host_alias.jk"]));
        let output = Command::new(work.path().join("host_alias"))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(42), "{output:?}");
    }
    work.assert_no_build_files();
}
