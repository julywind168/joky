use std::fs;
use std::path::Path;
use std::process::Command;

#[path = "cli/check.rs"]
mod check;

#[path = "cli/aot.rs"]
mod aot;
#[path = "cli/cursors.rs"]
mod cursors;
#[path = "cli/dynamic.rs"]
mod dynamic;
#[path = "cli/echo.rs"]
mod echo;
#[path = "cli/local_var.rs"]
mod local_var;
#[path = "cli/parity.rs"]
mod parity;
#[path = "cli/path_process.rs"]
mod path_process;
#[path = "cli/providers.rs"]
mod providers;

#[path = "cli/build_record.rs"]
mod build_record;
#[path = "cli/debug_info.rs"]
mod debug_info;
#[path = "cli/target.rs"]
mod target;

fn assert_cli_run(relative: &str, stdout: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    let output = Command::new(env!("CARGO_BIN_EXE_joky"))
        .arg("run")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{relative}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, stdout.as_bytes(), "{relative}");
}

fn strip_ansi(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(character) = chars.next() {
        if character != '\u{1b}' {
            output.push(character);
            continue;
        }
        if chars.next() != Some('[') {
            continue;
        }
        for character in chars.by_ref() {
            if character.is_ascii_alphabetic() {
                break;
            }
        }
    }
    output
}

#[test]
fn build_cli_writes_native_object() {
    let root = std::env::temp_dir().join(format!("joky-cli-build-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let source = root.join("hello.jk");
    let output = root.join("hello");
    fs::write(&source, "fn main() { println(\"hello\") }\n").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_joky"))
        .arg("build")
        .args(["--target", env!("JOKY_TARGET")])
        .arg(&source)
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let status = String::from_utf8_lossy(&result.stderr);
    assert!(status.contains(&format!("Building '{}'", source.display())));
    let abi_suffix = format!("' (AOT ABI v{})", joky_runtime_abi::AOT_RUNTIME_ABI_VERSION);
    let runtime = status
        .lines()
        .find_map(|line| {
            line.strip_prefix("Runtime  '")
                .and_then(|line| line.strip_suffix(&abi_suffix))
        })
        .expect("build status should report the runtime archive and ABI");
    assert!(Path::new(runtime).is_absolute());
    assert!(runtime.ends_with(".a"));
    let finished = status
        .lines()
        .find(|line| line.starts_with(&format!("Finished '{}'", output.display())))
        .expect("build status should report the output");
    assert!(finished.contains(") in "));
    assert!(fs::metadata(&output).unwrap().len() > 64);
    let run = Command::new(&output).output().unwrap();
    assert!(run.status.success());
    assert_eq!(String::from_utf8_lossy(&run.stdout), "hello\n");
}

#[test]
fn build_release_and_strip_control_aot_artifact() {
    let root = std::env::temp_dir().join(format!("joky-cli-build-flags-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let source = root.join("main.jk");
    fs::write(&source, "fn main() { println(\"release and strip\") }\n").unwrap();
    let debug = root.join("debug");
    let release = root.join("release");
    let stripped = root.join("stripped");
    for (args, output) in [
        (vec!["build"], &debug),
        (vec!["build", "--release"], &release),
        (vec!["build", "--release", "--strip"], &stripped),
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_joky"))
            .args(args)
            .arg(&source)
            .args(["-o"])
            .arg(output)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            Command::new(output).output().unwrap().stdout,
            b"release and strip\n"
        );
    }
    // Page alignment can make differently optimized builds coincide in size;
    // compare contents so the profile difference is actually asserted.
    let debug_bytes = fs::read(&debug).unwrap();
    let release_bytes = fs::read(&release).unwrap();
    assert_ne!(debug_bytes, release_bytes);
    assert!(fs::metadata(&stripped).unwrap().len() < fs::metadata(&release).unwrap().len());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn build_package_links_imports_and_uses_default_output() {
    let root = std::env::temp_dir().join(format!("joky-cli-package-build-{}", std::process::id()));
    let package = root.join("hello");
    fs::create_dir_all(package.join("src")).unwrap();
    fs::write(package.join("joky.toml"), "name = \"hello\"\n").unwrap();
    fs::write(
        package.join("src/util.jk"),
        "pub fn greeting() -> String { \"from import\" }\n",
    )
    .unwrap();
    fs::write(
        package.join("src/main.jk"),
        "import util\nfn main() { println(util.greeting()) }\n",
    )
    .unwrap();

    let build = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(&package)
        .arg("build")
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let executable = package.join(".joky/bin/hello");
    assert!(executable.is_file());
    let run = Command::new(&executable).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(run.stdout, b"from import\n");
    let custom = package.join("dist/hello");
    let build = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(&package)
        .args(["build", "-o"])
        .arg(&custom)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    assert_eq!(
        Command::new(&custom).output().unwrap().stdout,
        b"from import\n"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn build_uses_explicit_runtime_archive_without_falling_back() {
    let root = std::env::temp_dir().join(format!("joky-cli-archive-build-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let source = root.join("main.jk");
    fs::write(&source, "fn main() {}\n").unwrap();
    let missing = root.join("missing-runtime.a");
    let build = Command::new(env!("CARGO_BIN_EXE_joky"))
        .args(["build"])
        .arg(&source)
        .arg("-o")
        .arg(root.join("app"))
        .env("JOKY_RUNTIME_ARCHIVE", &missing)
        .output()
        .unwrap();
    assert!(!build.status.success());
    assert!(String::from_utf8_lossy(&build.stderr).contains("missing-runtime.a"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn modular_cli_cache_is_persistent_observable_and_optional() {
    let root = std::env::temp_dir().join(format!("joky-cli-disk-cache-{}", std::process::id()));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("joky.toml"), "name = \"cache-test\"\n").unwrap();
    fs::write(
        root.join("src/util.jk"),
        "pub const ANSWER = 42\npub fn copy(T: type, value: T) -> T { value }",
    )
    .unwrap();
    fs::write(
        root.join("src/main.jk"),
        "import util\nfn main() { println(util.copy(util.ANSWER)) }",
    )
    .unwrap();
    let run = |flags: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_joky"))
            .current_dir(&root)
            .arg("run")
            .args(flags)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        (
            String::from_utf8(output.stdout).unwrap(),
            String::from_utf8(output.stderr).unwrap(),
        )
    };
    let (stdout, stderr) = run(&["--no-cache", "--verbose"]);
    assert_eq!(stdout, "42\n");
    assert!(stderr.contains("cache disabled"));
    assert!(!root.join(".joky").exists());
    let (stdout, stderr) = run(&["--verbose"]);
    assert_eq!(stdout, "42\n");
    assert!(stderr.contains(".joky/cache"));
    assert!(stderr.contains("compile generic copy"));
    let artifacts = fs::read_dir(root.join(".joky/cache"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jmir"))
        .collect::<Vec<_>>();
    assert_eq!(artifacts.len(), 3, "two modules and one generic instance");
    let (_, stderr) = run(&["--verbose"]);
    assert!(stderr.contains("hit generic copy"));
    assert!(!stderr.contains("[cache] compile"));
    fs::write(
        root.join("src/util.jk"),
        "pub const ANSWER = 43\npub fn copy(T: type, value: T) -> T { value }",
    )
    .unwrap();
    let (stdout, stderr) = run(&["--verbose"]);
    assert_eq!(stdout, "43\n");
    assert!(stderr.contains("[cache] compile"));
    for entry in fs::read_dir(root.join(".joky/cache")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|ext| ext == "jmir") {
            fs::write(path, b"truncated").unwrap();
        }
    }
    let (stdout, stderr) = run(&["--verbose"]);
    assert_eq!(stdout, "43\n");
    assert!(stderr.contains("invalid or incompatible artifact"));
    let (stdout, stderr) = run(&["--legacy", "--verbose"]);
    assert_eq!(stdout, "43\n");
    assert!(stderr.contains("legacy whole-program pipeline"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn aot_build_reuses_and_feeds_jit_module_cache() {
    let root = std::env::temp_dir().join(format!("joky-cli-shared-mir-{}", std::process::id()));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("joky.toml"), "name = \"shared-mir\"\n").unwrap();
    fs::write(
        root.join("src/util.jk"),
        "pub fn copy(T: type, value: T) -> T { value }",
    )
    .unwrap();
    fs::write(
        root.join("src/main.jk"),
        "import util\nfn main() { println(util.copy(42)) }",
    )
    .unwrap();
    let run = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_joky"))
            .current_dir(&root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stderr).unwrap()
    };
    let stderr = run(&["run", "--verbose"]);
    assert!(stderr.contains("compile generic copy"));
    let stderr = run(&["build", "-o", "from-jit", "--verbose"]);
    assert!(stderr.contains(".joky/cache"));
    assert!(stderr.contains("hit generic copy"));
    assert!(stderr.contains("emit aot object"));
    assert!(!stderr.contains("[cache] compile"));
    let stderr = run(&["build", "-o", "from-jit-again", "--verbose"]);
    assert!(stderr.contains("hit aot object"));
    assert!(!stderr.contains("emit aot object"));
    assert!(!stderr.contains("[cache] compile"));
    fs::remove_dir_all(root.join(".joky/cache")).unwrap();
    let stderr = run(&["build", "-o", "from-aot", "--verbose"]);
    assert!(stderr.contains("compile generic copy"));
    let stderr = run(&["run", "--verbose"]);
    assert!(stderr.contains("hit generic copy"));
    assert!(!stderr.contains("[cache] compile"));
    let stderr = run(&["build", "-o", "uncached", "--no-cache", "--verbose"]);
    assert!(stderr.contains("cache disabled"));
    assert!(stderr.contains("[cache] compile"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn modular_cli_reports_dependency_source_and_does_not_fall_back() {
    let root = std::env::temp_dir().join(format!("joky-cli-module-error-{}", std::process::id()));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/util.jk"),
        "pub fn broken() -> Int32 { missing_name }",
    )
    .unwrap();
    fs::write(
        root.join("src/main.jk"),
        "import util\nfn main() { println(util.broken()) }",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(&root)
        .args(["run", "src/main.jk", "--verbose"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = strip_ansi(&String::from_utf8_lossy(&output.stderr));
    assert!(stderr.contains("util.jk"));
    assert!(stderr.contains("missing_name"));
    assert!(!stderr.contains("legacy"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn modular_cli_renders_dependency_parse_errors_with_source_context() {
    let root = std::env::temp_dir().join(format!(
        "joky-cli-module-parse-error-{}",
        std::process::id()
    ));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/util.jk"),
        "pub fn broken() {\n    .nope()\n}\n",
    )
    .unwrap();
    fs::write(
        root.join("src/main.jk"),
        "import util\nfn main() { util.broken() }\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(&root)
        .args(["run", "src/main.jk"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = strip_ansi(&String::from_utf8_lossy(&output.stderr));
    assert!(stderr.contains("Error: syntax error"));
    assert!(stderr.contains("expected an expression"));
    assert!(stderr.contains("util.jk:2"));
    assert!(stderr.contains(".nope()"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn run_executes_a_joky_source_file() {
    let output = Command::new(env!("CARGO_BIN_EXE_joky"))
        .args(["run", "examples/basics/hello.jk"])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "hello world\n");
    assert!(output.stderr.is_empty());
}

#[test]
fn run_trait_examples_and_fixtures() {
    for (source, expected) in [
        (
            "examples/traits/traits.jk",
            "point\npoint\ncounter\nPoint\n",
        ),
        ("examples/traits/trait_associated_types.jk", "42\nhello\n"),
        (
            "tests/fixtures/trait_methods.jk",
            "true\nfalse\npoint\n42\n",
        ),
        (
            "tests/fixtures/trait_associated_types.jk",
            "9223372036854775807\nhello\n",
        ),
    ] {
        assert_cli_run(source, expected);
    }
}

#[test]
fn run_legacy_pipeline_executes_the_trait_example() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/traits/traits.jk");
    let output = Command::new(env!("CARGO_BIN_EXE_joky"))
        .args(["run", "--legacy"])
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"point\npoint\ncounter\nPoint\n");
}

#[test]
fn qualified_trait_methods_have_jit_cache_and_aot_parity() {
    parity::Package::new(
        "qualified-trait-methods",
        &[(
            "main.jk",
            include_str!("fixtures/trait_qualified_methods.jk"),
        )],
    )
    .check_cached(
        "42\nsecond\n42\nsecond\n42\n43\n42\n7\nsecond\n7\nsecond\n",
        None,
        &[],
    );
}

#[test]
fn run_trait_bound_rejects_missing_impl() {
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/trait_missing_impl.jk"
    );
    let output = Command::new(env!("CARGO_BIN_EXE_joky"))
        .args(["run", "--no-cache", fixture])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = strip_ansi(&String::from_utf8_lossy(&output.stderr));
    assert!(
        stderr.contains("type 'struct' does not implement trait 'Describe'"),
        "{stderr}"
    );
}

#[test]
fn run_erases_type_functions_and_preserves_wide_generic_payloads() {
    assert_cli_run(
        "tests/fixtures/type_functions.jk",
        "42\n9223372036854775807\nok\n18446744073709551615\n",
    );
}

#[test]
fn run_evaluates_type_function_annotations() {
    assert_cli_run(
        "tests/fixtures/type_annotations_calls.jk",
        "1\n42\n7\ntuple\n9\n0\n0\n",
    );
}

#[test]
fn run_print_writes_without_a_newline() {
    let path = std::env::temp_dir().join(format!("joky-print-{}.jk", std::process::id()));
    fs::write(&path, "fn main() { print(\"hello\"); print(\" world\") }\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_joky"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    fs::remove_file(&path).unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "hello world");
    assert!(output.stderr.is_empty());
}

#[test]
fn run_executes_the_effect_example() {
    assert_cli_run("examples/effects/handlers.jk", "42\n");
}

#[test]
fn run_executes_a_single_cown_when_lease() {
    assert_cli_run("tests/fixtures/cown_when.jk", "1\n");
}

#[test]
fn run_supports_an_implicit_cown_lease_binding() {
    assert_cli_run("tests/fixtures/cown_when_implicit.jk", "7\n");
}

#[test]
fn run_propagates_a_typed_task_abort_to_the_parent_handler() {
    assert_cli_run("tests/fixtures/task_abort_effect.jk", "child failed\n42\n");
}

#[test]
fn run_executes_closures_with_heap_captures() {
    assert_cli_run("tests/fixtures/closure_captures.jk", "12\nhi\nhi\n1\n1\n");
}

#[test]
fn run_executes_escaping_closures() {
    assert_cli_run("tests/fixtures/escaping_closures.jk", "42\n13\n10\n21\n");
}

#[test]
fn run_executes_labeled_and_field_function_value_calls() {
    assert_cli_run("tests/fixtures/function_value_calls.jk", "13\n11\n");
}

#[test]
fn run_reports_an_unhandled_normal_effect() {
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/unhandled_effect.jk"
    );
    let output = Command::new(env!("CARGO_BIN_EXE_joky"))
        .args(["run", fixture])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = strip_ansi(&String::from_utf8(output.stderr).unwrap());
    assert!(stderr.contains("unhandled effect operation"));
}

#[test]
fn run_executes_expression_sequences() {
    assert_cli_run("tests/fixtures/expression_sequence.jk", "first\nsecond\n");
}

#[test]
fn run_short_circuits_logical_operators() {
    assert_cli_run(
        "tests/fixtures/logical_operators.jk",
        "true\nfalse\ntrue\nfalse\ntrue\n",
    );
}

#[test]
fn run_executes_raw_and_multiline_string_literals() {
    assert_cli_run(
        "examples/basics/string.jk",
        concat!(
            "Hello, Joky\n",
            "\\d+\\.\\d+\n",
            "{\"name\": \"joky\"}\n",
            "SELECT *\n  FROM users\n WHERE name = 'Joky'\n",
            "path ~ '\\d+'\n",
            "8\n",
            "\\d+{x}\n",
        ),
    );
}

#[test]
fn run_executes_mutable_lists() {
    assert_cli_run("tests/fixtures/mut_list.jk", "2\n1\n2\n42\n4\n");
}

#[test]
fn run_executes_byte_string_literals() {
    assert_cli_run(
        "tests/fixtures/byte_strings.jk",
        concat!(
            "13\n",
            "chunked copy\n",
            "\n",
            "1\n",
            "6\n",
            "\\d+{x}\n",
            "true\n",
            "false\n",
            "true\n",
            "Bytes[0x41, 0x0a, 0x42]\n",
            "0\n",
            "xy\n",
        ),
    );
}

#[test]
fn run_executes_mutable_lists_of_strings() {
    assert_cli_run("tests/fixtures/mut_list_strings.jk", "one\ntwo\nupdated\n");
}

#[test]
fn run_executes_mutable_maps() {
    assert_cli_run(
        "tests/fixtures/mut_map.jk",
        "true\ntrue\n1\n2\ntrue\n2\ntrue\n4\n",
    );
}

#[test]
fn run_executes_mutable_sets() {
    assert_cli_run(
        "tests/fixtures/mut_set.jk",
        "true\ntrue\nfalse\ntrue\ntrue\nfalse\n0\n4\n",
    );
}

#[test]
fn run_executes_mutable_collection_literals() {
    assert_cli_run(
        "tests/fixtures/mut_collection_literals.jk",
        "3\n2\n2\n3\n2\ntrue\n",
    );
}

#[test]
fn run_accepts_newline_separated_expressions_without_semicolons() {
    assert_cli_run("tests/fixtures/no_semicolons.jk", "hello\n world\n");
}

#[test]
fn run_compiles_if_expressions() {
    assert_cli_run("tests/fixtures/if_expression.jk", "yes\n");
}

#[test]
fn run_executes_while_loops() {
    assert_cli_run("tests/fixtures/while_loop.jk", "tick\n");
}

#[test]
fn run_executes_loop_break_values() {
    assert_cli_run("tests/fixtures/loop_break.jk", "done\n");
}

#[test]
fn run_breaks_from_while_loops() {
    assert_cli_run("tests/fixtures/while_break.jk", "once\n");
}

#[test]
fn run_executes_user_defined_functions() {
    assert_cli_run("tests/fixtures/user_function.jk", "custom function ok\n");
}

#[test]
fn run_executes_user_functions_with_all_value_kinds() {
    assert_cli_run(
        "tests/fixtures/user_function_types.jk",
        "string function ok\nunit function ok\n",
    );
}

#[test]
fn run_executes_tuple_values() {
    assert_cli_run("tests/fixtures/tuple.jk", "tuple ok\n");
}

#[test]
fn run_executes_struct_values() {
    assert_cli_run("tests/fixtures/struct.jk", "struct ok\n");
}

#[test]
fn run_executes_class_methods() {
    assert_cli_run("tests/fixtures/class.jk", "class ok\n");
}

#[test]
fn run_executes_enum_adt_matches() {
    assert_cli_run("tests/fixtures/enum.jk", "enum ok\n");
}

#[test]
fn run_executes_cfg_lowered_match_arms() {
    assert_cli_run("tests/fixtures/match_cfg.jk", "match cfg ok\n");
}

#[test]
fn run_executes_class_reference_and_recursive_method_semantics() {
    assert_cli_run("tests/fixtures/class_semantics.jk", "class semantics ok\n");
}

#[test]
fn run_uses_the_latest_binding() {
    assert_cli_run("tests/fixtures/rebinding.jk", "second\n");
}

#[test]
fn run_accepts_binding_type_annotations() {
    assert_cli_run("tests/fixtures/type_annotations.jk", "hello\nupdated\n");
}

#[test]
fn run_compiles_all_numeric_types() {
    assert_cli_run("tests/fixtures/numeric_types.jk", "numeric types ok\n");
}

#[test]
fn run_renders_compiler_diagnostics_with_source_context() {
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/invalid_numeric_literal.jk"
    );
    let output = Command::new(env!("CARGO_BIN_EXE_joky"))
        .args(["run", fixture])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = strip_ansi(&String::from_utf8(output.stderr).unwrap());
    assert!(stderr.contains("Error: semantic error"));
    assert!(stderr.contains("integer literal is out of range for Int8"));
    assert!(stderr.contains("invalid_numeric_literal.jk:2"));
    assert!(stderr.contains("let value: Int8 = 128;"));
}

#[test]
fn run_renders_interpolation_diagnostics_at_the_embedded_expression() {
    let root = std::env::temp_dir().join(format!("joky-cli-interpolation-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let source = root.join("main.jk");
    fs::write(&source, "fn main() {\n    println(\"hello {()}\")\n}\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_joky"))
        .args(["run"])
        .arg(&source)
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = strip_ansi(&String::from_utf8(output.stderr).unwrap());
    assert!(
        stderr.contains("2 │     println(\"hello {()}\")"),
        "{stderr}"
    );
    assert!(stderr.contains("type 'Unit' does not implement trait 'Show'"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn invoking_without_arguments_reports_usage() {
    let output = Command::new(env!("CARGO_BIN_EXE_joky")).output().unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("usage: joky new <NAME> | joky run [FILE]"));
}

#[test]
fn direct_expressions_are_not_a_cli_command() {
    let output = Command::new(env!("CARGO_BIN_EXE_joky"))
        .arg("1 + 2")
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("usage: joky new <NAME> | joky run [FILE]"));
}

#[test]
fn new_creates_a_package_that_runs_from_its_root() {
    let parent = std::env::temp_dir().join(format!("joky-cli-package-{}", std::process::id()));
    let _ = fs::remove_dir_all(&parent);
    fs::create_dir_all(&parent).unwrap();

    let created = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(&parent)
        .args(["new", "foo"])
        .output()
        .unwrap();
    assert!(created.status.success());
    assert!(parent.join("foo/joky.toml").is_file());
    assert!(parent.join("foo/src/main.jk").is_file());
    fs::write(
        parent.join("foo/src/user.jk"),
        "pub fn greeting() -> String { \"hello from module\" }\n",
    )
    .unwrap();
    fs::write(
        parent.join("foo/src/main.jk"),
        "import user\nfn main() { println(user.greeting()) }\n",
    )
    .unwrap();

    let run = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(parent.join("foo"))
        .arg("run")
        .output()
        .unwrap();
    assert!(run.status.success());
    assert_eq!(
        String::from_utf8(run.stdout).unwrap(),
        "hello from module\n"
    );
    assert!(run.stderr.is_empty());

    fs::remove_dir_all(parent).unwrap();
}

#[test]
fn package_can_import_public_constants_with_module_qualification() {
    let parent = std::env::temp_dir().join(format!("joky-cli-constants-{}", std::process::id()));
    let _ = fs::remove_dir_all(&parent);
    fs::create_dir_all(parent.join("foo/src")).unwrap();
    fs::write(parent.join("foo/joky.toml"), "name = \"foo\"\n").unwrap();
    fs::write(
        parent.join("foo/src/config.jk"),
        "pub const MAX = 40\n\
         pub const LABEL = \"config\"\n\
         pub fn with_offset() -> Int32 { MAX + 2 }\n",
    )
    .unwrap();
    fs::write(parent.join("foo/src/limits.jk"), "pub const MAX = 2\n").unwrap();
    fs::write(
        parent.join("foo/src/main.jk"),
        "import config\n\
         import limits\n\
         fn main() { println(config.LABEL); println(config.MAX + limits.MAX); println(config.with_offset()) }\n",
    )
    .unwrap();

    let run = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(parent.join("foo"))
        .arg("run")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8(run.stdout).unwrap(), "config\n42\n42\n");
    assert!(run.stderr.is_empty());
    fs::remove_dir_all(parent).unwrap();
}

#[test]
fn package_can_import_builtin_string_module() {
    let parent = std::env::temp_dir().join(format!("joky-cli-std-{}", std::process::id()));
    let _ = fs::remove_dir_all(&parent);
    fs::create_dir_all(parent.join("foo/src")).unwrap();
    fs::write(parent.join("foo/joky.toml"), "name = \"foo\"\n").unwrap();
    fs::write(
        parent.join("foo/src/main.jk"),
        "import joky/string\nfn main() { println(if \"hello\" |> string.starts_with(prefix: \"he\") \"std ok\" else \"std bad\") }\n",
    )
    .unwrap();

    let run = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(parent.join("foo"))
        .arg("run")
        .output()
        .unwrap();
    assert!(run.status.success());
    assert_eq!(String::from_utf8(run.stdout).unwrap(), "std ok\n");
    assert!(run.stderr.is_empty());
    fs::remove_dir_all(parent).unwrap();
}

#[test]
fn package_can_import_builtin_time_module() {
    let parent = std::env::temp_dir().join(format!("joky-cli-time-{}", std::process::id()));
    let _ = fs::remove_dir_all(&parent);
    fs::create_dir_all(parent.join("foo/src")).unwrap();
    fs::write(parent.join("foo/joky.toml"), "name = \"foo\"\n").unwrap();
    fs::write(
        parent.join("foo/src/main.jk"),
        "import joky/time\nfn main() effects { time } { time.sleep(1ms); println(\"time ok\") }\n",
    )
    .unwrap();

    let run = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(parent.join("foo"))
        .arg("run")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8(run.stdout).unwrap(), "time ok\n");
    assert!(run.stderr.is_empty());
    fs::remove_dir_all(parent).unwrap();
}

#[test]
fn package_can_import_builtin_sqlite_module() {
    let parent = std::env::temp_dir().join(format!("joky-cli-sqlite-{}", std::process::id()));
    let _ = fs::remove_dir_all(&parent);
    fs::create_dir_all(parent.join("foo/src")).unwrap();
    fs::write(parent.join("foo/joky.toml"), "name = \"foo\"\n").unwrap();
    fs::write(
        parent.join("foo/src/main.jk"),
        "import joky/sqlite
fn main() effects { sqlite } {
    let db = sqlite.open(\":memory:\")!
    let create = db.prepare(\"create table users (name text, age integer)\")!
    let _ = create.execute()!
    let insert = db.prepare(\"insert into users values (?, ?)\")!
    let insert = insert.bind_text(1, \"Alice\")!
    let insert = insert.bind_i64(2, 42)!
    println(insert.execute()!)
    let query = db.prepare(\"select name from users where age = ?\")!
    let query = query.bind_i64(1, 42)!
    for result in query.query()! {
        let row = result!
        match row.text(0)! {
            None => panic(\"no name\")
            Some(name) => println(name)
        }
    }
    db.close()!
}
",
    )
    .unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(parent.join("foo"))
        .arg("run")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8(run.stdout).unwrap(), "1\nAlice\n");
    assert!(run.stderr.is_empty());
    fs::remove_dir_all(parent).unwrap();
}

#[test]
fn main_result_error_exits_unsuccessfully() {
    let parent = std::env::temp_dir().join(format!("joky-cli-main-result-{}", std::process::id()));
    let _ = fs::remove_dir_all(&parent);
    fs::create_dir_all(parent.join("src")).unwrap();
    fs::write(parent.join("joky.toml"), "name = \"main-result\"\n").unwrap();
    fs::write(
        parent.join("src/main.jk"),
        "fn main() -> Result(Unit, String) { Err(\"entry failed\") }\n",
    )
    .unwrap();

    let run = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(&parent)
        .arg("run")
        .output()
        .unwrap();

    assert!(!run.status.success());
    assert!(run.stdout.is_empty());
    let stderr = strip_ansi(&String::from_utf8(run.stderr).unwrap());
    assert!(stderr.contains("entry failed"));
    assert_eq!(stderr.matches("entry failed").count(), 1);
    fs::remove_dir_all(parent).unwrap();
}

#[test]
fn sqlite_error_propagates_from_pending_main_result() {
    let parent =
        std::env::temp_dir().join(format!("joky-cli-sqlite-result-{}", std::process::id()));
    let _ = fs::remove_dir_all(&parent);
    fs::create_dir_all(parent.join("src")).unwrap();
    fs::write(parent.join("joky.toml"), "name = \"sqlite-result\"\n").unwrap();
    fs::write(
        parent.join("src/main.jk"),
        "import joky/sqlite\n\
         fn main() -> Result(Unit, String) effects { sqlite } {\n\
             let db = sqlite.open(\":memory:\")?\n\
             let _ = db.prepare(\"invalid sql\")?\n\
             Ok(())\n\
         }\n",
    )
    .unwrap();

    let run = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(&parent)
        .arg("run")
        .output()
        .unwrap();

    assert!(!run.status.success());
    assert!(run.stdout.is_empty());
    let stderr = strip_ansi(&String::from_utf8(run.stderr).unwrap());
    assert!(stderr.contains("syntax error"), "{stderr}");
    fs::remove_dir_all(parent).unwrap();
}

#[test]
fn package_can_import_builtin_tcp_module() {
    let parent = std::env::temp_dir().join(format!("joky-cli-tcp-{}", std::process::id()));
    let _ = fs::remove_dir_all(&parent);
    fs::create_dir_all(parent.join("foo/src")).unwrap();
    fs::write(parent.join("foo/joky.toml"), "name = \"foo\"\n").unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    fs::write(
        parent.join("foo/src/main.jk"),
        format!(
            "import joky/socket/tcp\nfn main() effects {{ tcp }} {{ match tcp.listen(host: \"127.0.0.1\", port: {port}) {{ Ok(_) => println(\"tcp bad\"), Err(_) => println(\"tcp ok\") }} }}\n"
        ),
    )
    .unwrap();

    let run = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(parent.join("foo"))
        .arg("run")
        .output()
        .unwrap();
    assert!(run.status.success());
    assert_eq!(String::from_utf8(run.stdout).unwrap(), "tcp ok\n");
    assert!(run.stderr.is_empty());
    drop(listener);
    fs::remove_dir_all(parent).unwrap();
}

#[cfg(unix)]
#[test]
fn package_can_import_builtin_unix_module() {
    let parent = std::env::temp_dir().join(format!("joky-cli-unix-{}", std::process::id()));
    let _ = fs::remove_dir_all(&parent);
    fs::create_dir_all(parent.join("foo/src")).unwrap();
    fs::write(parent.join("foo/joky.toml"), "name = \"foo\"\n").unwrap();
    fs::write(
        parent.join("foo/src/main.jk"),
        "import joky/socket/unix\nfn main() effects { unix } { println(\"unix module\") }\n",
    )
    .unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(parent.join("foo"))
        .args(["run"])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8(run.stdout).unwrap(), "unix module\n");
    assert!(run.stderr.is_empty());
    fs::remove_dir_all(parent).unwrap();
}

#[cfg(unix)]
#[test]
fn builtin_unix_stream_connect_round_trip_uses_instance_methods() {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::thread;
    use std::time::{Duration, Instant};

    let path = std::env::temp_dir().join(format!("joky-unix-client-{}.sock", std::process::id()));
    let _ = fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let server_thread = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "Unix client did not connect");
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("Unix server accept: {error}"),
            }
        };
        // On macOS the accepted stream can inherit the listener's
        // nonblocking flag; a read timeout does not turn that into a blocking
        // read, so normalize the accepted peer before reading the request.
        stream
            .set_nonblocking(false)
            .expect("Unix server stream should be blocking");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = [0_u8; 4];
        stream.read_exact(&mut request).expect("Unix server read");
        assert_eq!(&request, b"ping");
        stream.write_all(b"pong").expect("Unix server write");
    });

    let parent = std::env::temp_dir().join(format!("joky-cli-unix-client-{}", std::process::id()));
    let _ = fs::remove_dir_all(&parent);
    fs::create_dir_all(parent.join("foo/src")).unwrap();
    fs::write(parent.join("foo/joky.toml"), "name = \"foo\"\n").unwrap();
    let path = path.to_str().unwrap().to_owned();
    let source = r#"
import joky/socket/unix
import joky/bytes

fn main() effects { unix } {
    match unix.connect(path: "SOCKET_PATH") {
        Ok(stream) => match stream.write(Bytes.from_string("ping")) {
            Ok(_) => match stream.read(64) {
                Ok(data) => match data.to_string() {
                    Some(text) => match stream.close() {
                        Ok(_) => println(text)
                        Err(error) => panic(error)
                    }
                    None => panic("Unix peer returned invalid UTF-8")
                }
                Err(error) => panic(error)
            }
            Err(error) => panic(error)
        }
        Err(error) => panic(error)
    }
}
"#
    .replace("SOCKET_PATH", &path);
    fs::write(parent.join("foo/src/main.jk"), source).unwrap();

    let run = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(parent.join("foo"))
        .args(["run"])
        .output()
        .unwrap();
    let server_result = server_thread.join();
    assert!(
        run.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    server_result.expect("Unix server thread should finish");
    assert_eq!(String::from_utf8(run.stdout).unwrap(), "pong\n");
    assert!(run.stderr.is_empty());
    fs::remove_dir_all(parent).unwrap();
    fs::remove_file(path).unwrap();
}

#[cfg(unix)]
#[test]
fn builtin_unix_stream_listener_accept_round_trip_uses_instance_methods() {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::thread;
    use std::time::{Duration, Instant};

    let path = std::env::temp_dir().join(format!("joky-unix-server-{}.sock", std::process::id()));
    let _ = fs::remove_file(&path);
    let client_path = path.clone();
    let client_thread = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            match UnixStream::connect(&client_path) {
                Ok(stream) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    assert!(Instant::now() < deadline, "Unix listener did not start");
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("Unix client connect: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(b"ping").expect("Unix client write");
        let mut response = [0_u8; 4];
        stream.read_exact(&mut response).expect("Unix client read");
        assert_eq!(&response, b"ping");
    });

    let parent = std::env::temp_dir().join(format!("joky-cli-unix-server-{}", std::process::id()));
    let _ = fs::remove_dir_all(&parent);
    fs::create_dir_all(parent.join("foo/src")).unwrap();
    fs::write(parent.join("foo/joky.toml"), "name = \"foo\"\n").unwrap();
    let path = path.to_str().unwrap().to_owned();
    let source = r#"
import joky/socket/unix

fn main() effects { unix } {
    match unix.listen(path: "SOCKET_PATH") {
        Ok(listener) => match listener.accept() {
            Ok(stream) => match stream.read(64) {
                Ok(request) => match stream.write(request) {
                    Ok(_) => match stream.close() {
                        Ok(_) => println("unix ok")
                        Err(error) => panic(error)
                    }
                    Err(error) => panic(error)
                }
                Err(error) => panic(error)
            }
            Err(error) => panic(error)
        }
        Err(error) => panic(error)
    }
}
"#
    .replace("SOCKET_PATH", &path);
    fs::write(parent.join("foo/src/main.jk"), source).unwrap();

    let run = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(parent.join("foo"))
        .args(["run"])
        .output()
        .unwrap();
    let client_result = client_thread.join();
    assert!(
        run.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    client_result.expect("Unix client thread should finish");
    assert_eq!(String::from_utf8(run.stdout).unwrap(), "unix ok\n");
    assert!(run.stderr.is_empty());
    assert!(!std::path::Path::new(&path).exists());
    fs::remove_dir_all(parent).unwrap();
}

#[cfg(unix)]
#[test]
fn builtin_unix_datagram_send_recv_from_uses_instance_methods() {
    use std::os::unix::net::UnixDatagram;
    use std::thread;
    use std::time::Duration;

    let peer_path =
        std::env::temp_dir().join(format!("joky-unix-dgram-peer-{}.sock", std::process::id()));
    let socket_path =
        std::env::temp_dir().join(format!("joky-unix-dgram-joky-{}.sock", std::process::id()));
    let _ = fs::remove_file(&peer_path);
    let _ = fs::remove_file(&socket_path);
    let peer = UnixDatagram::bind(&peer_path).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let peer_path_for_thread = peer_path.clone();
    let peer_thread = thread::spawn(move || {
        let mut buffer = [0_u8; 64];
        let (length, source) = peer
            .recv_from(&mut buffer)
            .expect("Unix datagram peer recv");
        assert_eq!(&buffer[..length], b"ping");
        peer.send_to_addr(b"pong", &source)
            .expect("Unix datagram peer send");
        fs::remove_file(peer_path_for_thread).unwrap();
    });

    let parent = std::env::temp_dir().join(format!("joky-cli-unix-dgram-{}", std::process::id()));
    let _ = fs::remove_dir_all(&parent);
    fs::create_dir_all(parent.join("foo/src")).unwrap();
    fs::write(parent.join("foo/joky.toml"), "name = \"foo\"\n").unwrap();
    let peer_path = peer_path.to_str().unwrap().to_owned();
    let socket_path = socket_path.to_str().unwrap().to_owned();
    let source = r#"
import joky/socket/unix
import joky/bytes

fn main() effects { unix_dgram } {
    match unix_dgram.bind(path: "SOCKET_PATH") {
        Ok(socket) => match socket.send_to(value: Bytes.from_string("ping"), path: "PEER_PATH") {
            Ok(_) => match socket.recv_from(max_bytes: 64) {
                Ok(result) => match result.0.to_string() {
                    Some(text) => match socket.close() {
                        Ok(_) => println(text.concat("|").concat(result.1))
                        Err(error) => panic(error)
                    }
                    None => panic("Unix datagram peer returned invalid UTF-8")
                }
                Err(error) => panic(error)
            }
            Err(error) => panic(error)
        }
        Err(error) => panic(error)
    }
}
"#
    .replace("SOCKET_PATH", &socket_path)
    .replace("PEER_PATH", &peer_path);
    fs::write(parent.join("foo/src/main.jk"), source).unwrap();

    let run = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(parent.join("foo"))
        .args(["run"])
        .output()
        .unwrap();
    let peer_result = peer_thread.join();
    assert!(
        run.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    peer_result.expect("Unix datagram peer thread should finish");
    assert_eq!(
        String::from_utf8(run.stdout).unwrap(),
        format!("pong|{peer_path}\n")
    );
    assert!(run.stderr.is_empty());
    assert!(!std::path::Path::new(&socket_path).exists());
    fs::remove_dir_all(parent).unwrap();
}

#[test]
fn package_can_import_builtin_udp_module() {
    let parent = std::env::temp_dir().join(format!("joky-cli-udp-{}", std::process::id()));
    let _ = fs::remove_dir_all(&parent);
    fs::create_dir_all(parent.join("foo/src")).unwrap();
    fs::write(parent.join("foo/joky.toml"), "name = \"foo\"\n").unwrap();
    fs::write(
        parent.join("foo/src/main.jk"),
        "import joky/socket/udp\nfn main() effects { udp } { println(\"udp module\") }\n",
    )
    .unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(parent.join("foo"))
        .args(["run"])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8(run.stdout).unwrap(), "udp module\n");
    fs::remove_dir_all(parent).unwrap();
}

#[test]
fn builtin_udp_round_trip_uses_instance_methods() {
    use std::net::UdpSocket;
    use std::thread;

    let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = peer.local_addr().unwrap().port();
    peer.set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let peer_thread = thread::spawn(move || {
        let mut buf = [0_u8; 64];
        let (length, source) = peer.recv_from(&mut buf).expect("peer recv");
        assert_eq!(&buf[..length], b"ping");
        peer.send_to(b"pong", source).unwrap();
    });

    let parent =
        std::env::temp_dir().join(format!("joky-cli-udp-roundtrip-{}", std::process::id()));
    let _ = fs::remove_dir_all(&parent);
    fs::create_dir_all(parent.join("foo/src")).unwrap();
    fs::write(parent.join("foo/joky.toml"), "name = \"foo\"\n").unwrap();
    fs::write(
        parent.join("foo/src/main.jk"),
        format!(
            "import joky/socket/udp\nimport joky/bytes\nfn main() effects {{ udp }} {{\n    match udp.bind(host: \"127.0.0.1\", port: 0) {{\n        Ok(socket) => match socket.send_to(value: Bytes.from_string(\"ping\"), host: \"127.0.0.1\", port: {port}) {{\n            Ok(_) => match socket.recv_from(max_bytes: 64) {{\n                Ok(_) => println(\"udp ok\")\n                Err(error) => panic(error)\n            }}\n            Err(error) => panic(error)\n        }}\n        Err(error) => panic(error)\n    }}\n}}\n"
        ),
    )
    .unwrap();

    let run = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(parent.join("foo"))
        .args(["run"])
        .output()
        .unwrap();
    peer_thread.join().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(run.stderr.is_empty());
    fs::remove_dir_all(parent).unwrap();
}

#[test]
fn builtin_udp_connected_send_recv_uses_instance_methods() {
    use std::net::UdpSocket;
    use std::thread;

    let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = peer.local_addr().unwrap().port();
    peer.set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let peer_thread = thread::spawn(move || {
        let mut buf = [0_u8; 64];
        let (length, source) = peer.recv_from(&mut buf).expect("peer recv");
        assert_eq!(&buf[..length], b"ping");
        peer.send_to(b"pong", source).unwrap();
    });

    let parent =
        std::env::temp_dir().join(format!("joky-cli-udp-connected-{}", std::process::id()));
    let _ = fs::remove_dir_all(&parent);
    fs::create_dir_all(parent.join("foo/src")).unwrap();
    fs::write(parent.join("foo/joky.toml"), "name = \"foo\"\n").unwrap();
    fs::write(
        parent.join("foo/src/main.jk"),
        format!(
            "import joky/socket/udp\nimport joky/bytes\nfn main() effects {{ udp }} {{\n    match udp.bind(host: \"127.0.0.1\", port: 0) {{\n        Ok(socket) => match socket.connect(host: \"127.0.0.1\", port: {port}) {{\n            Ok(_) => match socket.send(Bytes.from_string(\"ping\")) {{\n                Ok(_) => match socket.recv(64) {{\n                    Ok(data) => match data.to_string() {{\n                        Some(text) => println(text)\n                        None => panic(\"invalid utf8\")\n                    }}\n                    Err(error) => panic(error)\n                }}\n                Err(error) => panic(error)\n            }}\n            Err(error) => panic(error)\n        }}\n        Err(error) => panic(error)\n    }}\n}}\n"
        ),
    )
    .unwrap();

    let run = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(parent.join("foo"))
        .args(["run"])
        .output()
        .unwrap();
    peer_thread.join().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8(run.stdout).unwrap(), "pong\n");
    assert!(run.stderr.is_empty());
    fs::remove_dir_all(parent).unwrap();
}

#[test]
fn builtin_udp_keeps_multiple_socket_handles_isolated() {
    use std::net::UdpSocket;
    use std::thread;

    let first_peer = UdpSocket::bind("127.0.0.1:0").unwrap();
    let first_port = first_peer.local_addr().unwrap().port();
    first_peer
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let first_thread = thread::spawn(move || {
        let mut buf = [0_u8; 64];
        let (length, source) = first_peer.recv_from(&mut buf).expect("first peer recv");
        assert_eq!(&buf[..length], b"first ping");
        first_peer.send_to(b"first pong", source).unwrap();
    });

    let second_peer = UdpSocket::bind("127.0.0.1:0").unwrap();
    let second_port = second_peer.local_addr().unwrap().port();
    second_peer
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let second_thread = thread::spawn(move || {
        let mut buf = [0_u8; 64];
        let (length, source) = second_peer.recv_from(&mut buf).expect("second peer recv");
        assert_eq!(&buf[..length], b"second ping");
        second_peer.send_to(b"second pong", source).unwrap();
    });

    let parent = std::env::temp_dir().join(format!("joky-cli-udp-multiple-{}", std::process::id()));
    let _ = fs::remove_dir_all(&parent);
    fs::create_dir_all(parent.join("foo/src")).unwrap();
    fs::write(parent.join("foo/joky.toml"), "name = \"foo\"\n").unwrap();
    let source = r#"
import joky/socket/udp
import joky/bytes

fn main() effects { udp } {
    match udp.bind(host: "127.0.0.1", port: 0) {
        Ok(first) => match udp.bind(host: "127.0.0.1", port: 0) {
            Ok(second) => match second.connect(host: "127.0.0.1", port: SECOND_PORT) {
                Ok(_) => match first.connect(host: "127.0.0.1", port: FIRST_PORT) {
                    Ok(_) => match second.send(Bytes.from_string("second ping")) {
                        Ok(_) => match first.send(Bytes.from_string("first ping")) {
                            Ok(_) => match second.recv(64) {
                                Ok(second_data) => match first.recv(64) {
                                    Ok(first_data) => match first_data.to_string() {
                                        Some(first_text) => match second_data.to_string() {
                                            Some(second_text) => println(first_text.concat("|").concat(second_text))
                                            None => panic("second peer returned invalid UTF-8")
                                        }
                                        None => panic("first peer returned invalid UTF-8")
                                    }
                                    Err(error) => panic(error)
                                }
                                Err(error) => panic(error)
                            }
                            Err(error) => panic(error)
                        }
                        Err(error) => panic(error)
                    }
                    Err(error) => panic(error)
                }
                Err(error) => panic(error)
            }
            Err(error) => panic(error)
        }
        Err(error) => panic(error)
    }
}
"#
    .replace("FIRST_PORT", &first_port.to_string())
    .replace("SECOND_PORT", &second_port.to_string());
    fs::write(parent.join("foo/src/main.jk"), source).unwrap();

    let run = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(parent.join("foo"))
        .args(["run"])
        .output()
        .unwrap();
    let first_result = first_thread.join();
    let second_result = second_thread.join();
    assert!(
        run.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    first_result.expect("first peer thread should finish");
    second_result.expect("second peer thread should finish");
    assert_eq!(
        String::from_utf8(run.stdout).unwrap(),
        "first pong|second pong\n"
    );
    assert!(run.stderr.is_empty());
    fs::remove_dir_all(parent).unwrap();
}

#[test]
fn package_can_import_generic_list_module() {
    let parent = std::env::temp_dir().join(format!("joky-cli-list-{}", std::process::id()));
    let _ = fs::remove_dir_all(&parent);
    fs::create_dir_all(parent.join("foo/src")).unwrap();
    fs::write(parent.join("foo/joky.toml"), "name = \"foo\"\n").unwrap();
    fs::write(
        parent.join("foo/src/main.jk"),
        "import joky/list\nfn main() {\n\
             let values = List(1, 2, 3)\n\
             let reversed = list.reverse(value: values)\n\
             let first = list.head(value: reversed).unwrap_or(0)\n\
             let tail = list.tail(value: values).unwrap_or(List(Int32)())\n\
             let second = list.head(value: tail).unwrap_or(0)\n\
             let extended = list.push_front(value: values, item: 0)\n\
             let front = list.head(value: extended).unwrap_or(99)\n\
             let expected_length: UInt64 = 3\n\
             let length_ok = list.length(value: values) == expected_length\n\
             let empty = list.is_empty(value: List(Int32)())\n\
             println(if first == 3 { if second == 2 { if front == 0 { if length_ok { if empty \"list ok\" else \"list bad\" } else \"list bad\" } else \"list bad\" } else \"list bad\" } else \"list bad\")\n\
         }\n",
    )
    .unwrap();

    let run = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(parent.join("foo"))
        .arg("run")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8(run.stdout).unwrap(), "list ok\n");
    assert!(run.stderr.is_empty());
    fs::remove_dir_all(parent).unwrap();
}

#[test]
fn package_can_import_generic_map_module() {
    let parent = std::env::temp_dir().join(format!("joky-cli-map-{}", std::process::id()));
    let _ = fs::remove_dir_all(&parent);
    fs::create_dir_all(parent.join("foo/src")).unwrap();
    fs::write(parent.join("foo/joky.toml"), "name = \"foo\"\n").unwrap();
    fs::write(
        parent.join("foo/src/main.jk"),
        "import joky/map\nfn main() {\n\
             let empty = Map(String, Int32)()\n\
             let first = map.insert(value: empty, key: \"alice\", item: 41)\n\
             let second = map.insert(value: first, key: \"alice\", item: 42)\n\
             let original = map.get(value: first, key: \"alice\").unwrap_or(0)\n\
             let updated = map.get(value: second, key: \"alice\").unwrap_or(0)\n\
             let present = map.contains_key(value: second, key: \"alice\")\n\
             let removed = map.remove(value: second, key: \"alice\")\n\
             let absent = map.is_empty(value: removed)\n\
             println(if original == 41 { if updated == 42 { if present { if absent \"map ok\" else \"map bad\" } else \"map bad\" } else \"map bad\" } else \"map bad\")\n\
         }\n",
    )
    .unwrap();

    let run = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(parent.join("foo"))
        .arg("run")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8(run.stdout).unwrap(), "map ok\n");
    assert!(run.stderr.is_empty());
    fs::remove_dir_all(parent).unwrap();
}

#[test]
fn package_can_import_generic_set_module() {
    let parent = std::env::temp_dir().join(format!("joky-cli-set-{}", std::process::id()));
    let _ = fs::remove_dir_all(&parent);
    fs::create_dir_all(parent.join("foo/src")).unwrap();
    fs::write(parent.join("foo/joky.toml"), "name = \"foo\"\n").unwrap();
    fs::write(
        parent.join("foo/src/main.jk"),
        "import joky/set\nfn main() {\n\
             let empty = Set(Int32)()\n\
             let first = set.insert(value: empty, item: 1)\n\
             let second = set.insert(value: first, item: 2)\n\
             let present = set.contains(value: second, item: 1)\n\
             let unchanged = set.is_empty(value: empty)\n\
             let removed = set.remove(value: second, item: 1)\n\
             let count = set.length(value: removed)\n\
             let expected: UInt64 = 1\n\
             println(if present { if unchanged { if count == expected { \"set ok\" } else { \"set bad\" } } else { \"set bad\" } } else { \"set bad\" })\n\
         }\n",
    )
    .unwrap();

    let run = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(parent.join("foo"))
        .arg("run")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8(run.stdout).unwrap(), "set ok\n");
    assert!(run.stderr.is_empty());
    fs::remove_dir_all(parent).unwrap();
}

#[test]
fn list_values_round_trip_through_user_functions() {
    let source =
        std::env::temp_dir().join(format!("joky-cli-list-function-{}.jk", std::process::id()));
    fs::write(
        &source,
        "fn reverse_i32(value: List(Int32)) -> List(Int32) { value.reverse() }\n\
         fn main() {\n\
             let reversed = reverse_i32(value: List(1, 2, 3))\n\
             println(if reversed.head().unwrap_or(0) == 3 \"three\" else \"zero\")\n\
         }",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_joky"))
        .args(["run", source.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "three\n");
    assert!(output.stderr.is_empty());
    fs::remove_file(source).unwrap();
}

#[test]
fn generic_list_module_supports_strings() {
    let parent = std::env::temp_dir().join(format!("joky-cli-string-list-{}", std::process::id()));
    let _ = fs::remove_dir_all(&parent);
    fs::create_dir_all(parent.join("foo/src")).unwrap();
    fs::write(parent.join("foo/joky.toml"), "name = \"foo\"\n").unwrap();
    fs::write(
        parent.join("foo/src/main.jk"),
        "import joky/list\nfn main() {\n\
             let values = list.reverse(value: List(\"first\", \"second\"))\n\
             println(list.head(value: values).unwrap_or(\"missing\"))\n\
         }\n",
    )
    .unwrap();

    let run = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(parent.join("foo"))
        .arg("run")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8(run.stdout).unwrap(), "second\n");
    assert!(run.stderr.is_empty());
    fs::remove_dir_all(parent).unwrap();
}

#[test]
fn package_reports_module_dependency_cycles() {
    let parent = std::env::temp_dir().join(format!("joky-cli-cycle-{}", std::process::id()));
    let _ = fs::remove_dir_all(&parent);
    fs::create_dir_all(parent.join("foo/src")).unwrap();
    fs::write(parent.join("foo/joky.toml"), "name = \"foo\"\n").unwrap();
    fs::write(parent.join("foo/src/main.jk"), "import a\nfn main() {}\n").unwrap();
    fs::write(parent.join("foo/src/a.jk"), "import b\npub fn a() {}\n").unwrap();
    fs::write(parent.join("foo/src/b.jk"), "import a\npub fn b() {}\n").unwrap();

    let run = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(parent.join("foo"))
        .arg("run")
        .output()
        .unwrap();
    assert!(!run.status.success());
    assert!(String::from_utf8(run.stderr)
        .unwrap()
        .contains("module dependency cycle"));
    fs::remove_dir_all(parent).unwrap();
}

#[test]
fn run_keeps_values_through_nested_suspending_calls() {
    // Three suspend levels: the task body publishes pending and resumes on a
    // machine entry; the nested suspends block their own frames and keep the
    // real results flowing back through the call chain.
    assert_cli_run("tests/fixtures/nested_suspends.jk", "42\n");
}

#[test]
fn contended_cown_progresses_under_different_worker_counts() {
    // Starvation observation across scheduling configurations: the same
    // contention program must complete under a single worker, a small pool,
    // and the default pool. The runtime reads JOKY_WORKER_COUNT once at
    // scheduler start, so each configuration runs in its own process.
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/cown_contention.jk"
    );
    for worker_count in ["1", "2", "4"] {
        let output = parity::run_bounded(
            Command::new(env!("CARGO_BIN_EXE_joky"))
                .args(["run", fixture])
                .env("JOKY_WORKER_COUNT", worker_count),
        );
        assert!(
            output.status.success(),
            "worker_count={worker_count}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "8\n10\n",
            "worker_count={worker_count}"
        );
    }
}

/// Bound deadlock regressions and reap their processes rather than hanging CI.
fn task_wait_fixture(fixture: &str, expected: &str) {
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    for workers in ["1", "2", "4"] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_joky"))
            .args(["run", fixture])
            .env("JOKY_WORKER_COUNT", workers)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "{fixture}, workers={workers}: timed out; {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{fixture}, workers={workers}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            expected,
            "{fixture}, workers={workers}"
        );
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn nested_task_waits_release_workers() {
    task_wait_fixture("tests/fixtures/nested_task_waits.jk", "8\n");
}

#[test]
fn deep_task_waits_do_not_grow_the_native_stack() {
    task_wait_fixture("tests/fixtures/deep_task_waits.jk", "1\n");
}

#[test]
fn cancellation_drains_nested_task_waits() {
    task_wait_fixture("tests/fixtures/cancel_nested_task_waits.jk", "42\n");
}

#[test]
fn indirect_calls_share_the_task_wait_abi() {
    task_wait_fixture("tests/fixtures/indirect_task_waits.jk", "42\n42\n");
}

#[test]
fn task_waits_restore_and_close_handler_scopes() {
    task_wait_fixture("tests/fixtures/task_wait_handlers.jk", "23\n");
}

#[test]
fn nested_task_waits_propagate_failures() {
    task_wait_fixture("tests/fixtures/task_wait_failure.jk", "7\n");
}

#[test]
fn branch_scope_exits_release_workers() {
    task_wait_fixture("tests/fixtures/branch_task_waits.jk", "42\n");
}

#[test]
fn bounded_for_orders_results_and_drains_with_few_workers() {
    task_wait_fixture("tests/fixtures/bounded_for.jk", "a\nc\n11\n12\nwinner\n");
}

#[test]
fn builtin_file_copy_works_with_few_workers() {
    task_wait_fixture("examples/io/file_copy.jk", "chunked file copy\n\n");
    assert_eq!(
        fs::read("target/file-copy-input.bin").unwrap(),
        fs::read("target/file-copy-output.bin").unwrap()
    );
}

#[test]
fn bounded_for_dynamic_zero_exits_with_a_diagnostic() {
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let mut child = Command::new(env!("CARGO_BIN_EXE_joky"))
        .args(["run", "tests/fixtures/bounded_for_zero.jk"])
        .env("JOKY_WORKER_COUNT", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "invalid limit did not drain: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty(), "invalid limit executed user code");
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("parallel limit must be greater than zero"));
}
