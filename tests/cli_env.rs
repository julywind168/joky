use std::fs;
use std::process::Command;

fn source() -> &'static str {
    r#"import joky/env

fn fixture(name: String) -> Result(Option(String), String) { Ok(Some(name)) }
fn read_mock() -> Result(Option(String), String) effects { env } { env.get("mock") }

fn main() -> Result(Unit, String) effects { env } {
    let mock = do { read_mock() } with { env.get(name) => fixture(name) }
    if mock? != Some("mock") { panic("local handler") }
    if env.get("JOKY_ENV_MISSING")? != None { panic("missing variable") }
    match env.get("JOKY_ENV_EMPTY")? {
        Some(value) => println("empty=" + value)
        None => println("empty=missing")
    }
    let values = env.vars()?
    println("vars=" + values.get("JOKY_ENV_EMPTY").is_some().show())
    println("cwd=" + env.current_dir()?.is_empty().show())
    println("tmp=" + env.temp_dir()?.is_empty().show())
    let arguments = env.args()?
    println("args=" + arguments.length().show())
    for argument in arguments { println(argument) }
    Ok(())
}
"#
}

fn run_jit(path: &std::path::Path, legacy: bool) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_joky"));
    command.arg("run");
    if legacy {
        command.arg("--legacy");
    }
    command
        .arg(path)
        .arg("--")
        .arg("")
        .arg("你好")
        .arg("--verbose");
    command.env("JOKY_ENV_EMPTY", "");
    command.env_remove("JOKY_ENV_MISSING");
    command.output().unwrap()
}

fn assert_output(output: &std::process::Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("empty=\n"), "{text}");
    assert!(text.contains("vars=true\n"), "{text}");
    assert!(text.contains("cwd=false\n"), "{text}");
    assert!(text.contains("tmp=false\n"), "{text}");
    assert!(text.contains("args=3\n\n你好\n--verbose\n"), "{text}");
}

#[test]
fn env_args_and_context_match_across_modular_legacy_and_aot() {
    let root = std::env::temp_dir().join(format!("joky-env-cli-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let source_path = root.join("main.jk");
    let output_path = root.join("env-app");
    fs::write(&source_path, source()).unwrap();

    assert_output(&run_jit(&source_path, false));
    assert_output(&run_jit(&source_path, false));
    assert_output(&run_jit(&source_path, true));

    let project = root.join("project");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("joky.toml"), "name = \"env-test\"\n").unwrap();
    fs::write(project.join("src/main.jk"), source()).unwrap();
    let project_run = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(&project)
        .args(["run", "--", "", "你好", "--verbose"])
        .env("JOKY_ENV_EMPTY", "")
        .env_remove("JOKY_ENV_MISSING")
        .output()
        .unwrap();
    assert_output(&project_run);

    let build = Command::new(env!("CARGO_BIN_EXE_joky"))
        .args(["build", "--no-cache"])
        .arg(&source_path)
        .args(["-o"])
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let aot = Command::new(&output_path)
        .env("JOKY_ENV_EMPTY", "")
        .env_remove("JOKY_ENV_MISSING")
        .arg("")
        .arg("你好")
        .arg("--verbose")
        .output()
        .unwrap();
    assert_output(&aot);
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn env_args_reports_invalid_utf8_after_cli_passthrough() {
    use std::os::unix::ffi::OsStringExt;
    let root = std::env::temp_dir().join(format!("joky-env-invalid-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let path = root.join("main.jk");
    fs::write(&path, r#"import joky/env
fn main() effects { env } {
    match env.args() { Err(_) => println("bad args"), Ok(_) => panic("args accepted") }
    match env.get("JOKY_ENV_INVALID") { Err(_) => println("bad value"), Ok(_) => panic("value accepted") }
    match env.vars() { Err(_) => println("bad vars"), Ok(_) => panic("vars accepted") }
    match env.get("JOKY_ENV_VALID")! { Some(value) => println(value), None => panic("valid missing") }
}"#).unwrap();
    let output_path = root.join("env-invalid");
    let build = Command::new(env!("CARGO_BIN_EXE_joky"))
        .arg("build")
        .arg(&path)
        .arg("-o")
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    for mode in ["modular", "legacy", "aot"] {
        let mut command = if mode == "aot" {
            Command::new(&output_path)
        } else {
            let mut command = Command::new(env!("CARGO_BIN_EXE_joky"));
            command.arg("run");
            if mode == "legacy" {
                command.arg("--legacy");
            }
            command.arg(&path).arg("--");
            command
        };
        let invalid = std::ffi::OsString::from_vec(vec![0xff]);
        let output = command
            .arg(&invalid)
            .env("JOKY_ENV_INVALID", invalid)
            .env("JOKY_ENV_VALID", "valid")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            output.stdout, b"bad args\nbad value\nbad vars\nvalid\n",
            "{mode}"
        );
    }
    fs::remove_dir_all(root).unwrap();
}
