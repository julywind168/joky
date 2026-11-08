//! Project roots, manifests, runtime files, and native bundle publication.
use super::*;

const CONFIG: &str = r#"
[project]
name = "demo"
entry = "src/bin/main.jk"
source-root = "src"
resources = ["assets", "src/shared.jk"]
"#;

fn project(work: &Workspace) {
    work.write("app/joky.toml", CONFIG);
    work.write("app/src/shared.jk", "pub fn answer() -> Int32 { 42 }");
    work.write(
        "app/src/bin/main.jk",
        r#"
        import shared
        import joky/fs
        fn main() @suspends {
            var file = fs.open("assets/message.txt", fs.OpenOptions.default())!
            println(file.read_text()!)
            println(shared.answer())
        }
    "#,
    );
    work.write("app/assets/message.txt", "project root");
    // A same-named caller resource must not win over the application's root.
    work.write("assets/message.txt", "wrong directory");
}

#[test]
fn project_run_resolves_nested_entries_and_resources_from_any_directory() {
    let work = Workspace::new();
    project(&work);
    for args in [
        vec!["run", "app"],
        vec!["run", "app/joky.toml", "--interpreter"],
    ] {
        let output = work.cli(&args);
        success(&output);
        assert_eq!(output.stdout, b"project root\n42\n");
    }
    let output = work
        .command()
        .args(["run", "--interpreter"])
        .current_dir(work.path().join("app/src/bin"))
        .output()
        .unwrap();
    success(&output);
    assert_eq!(output.stdout, b"project root\n42\n");
    // Explicit .jk inputs keep their entry-directory import semantics.
    failure(&work.cli(&["run", "app/src/bin/main.jk"]), "shared.jk");
}

#[test]
#[cfg(unix)]
fn project_build_relocates_resources_and_preserves_previous_output_on_failure() {
    let work = Workspace::new();
    project(&work);
    work.write(
        "app/joky.toml",
        &CONFIG.replace(
            "\"assets\"",
            "\"./assets\", \"assets/message.txt\", \"assets\"",
        ),
    );
    std::fs::create_dir(work.path().join("app/assets/empty")).unwrap();
    let built = work.cli(&["build", "app"]);
    success(&built);
    let stderr = String::from_utf8_lossy(&built.stderr);
    let lines: Vec<_> = stderr.lines().collect();
    assert_eq!(lines.len(), 3, "{stderr}");
    assert_eq!(lines[0].trim_start(), "Compiling app (AOT)");
    assert_eq!(lines[1].trim_start(), "Linking demo");
    assert!(lines[2].trim_start().starts_with("Finished "), "{stderr}");
    assert!(lines[2].contains("/app/dist (built in "), "{stderr}");
    assert!(!stderr.contains(".joky-build-"), "{stderr}");
    let dist = work.path().join("app/dist");
    let binary = dist.join("demo");
    assert!(dist.join("assets/empty").is_dir());
    assert!(!dist.join("src/bin/main.jk").exists());
    assert_eq!(
        std::fs::read(dist.join("src/shared.jk")).unwrap(),
        std::fs::read(work.path().join("app/src/shared.jk")).unwrap()
    );
    let previous = std::fs::read(&binary).unwrap();
    let failed = work
        .command()
        .args(["build", "app"])
        .env("JOKY_RUNTIME", work.path().join("missing-runtime.a"))
        .output()
        .unwrap();
    failure(&failed, "JOKY_RUNTIME does not name a runtime archive");
    assert!(!String::from_utf8_lossy(&failed.stderr).contains("Finished"));
    assert_eq!(std::fs::read(&binary).unwrap(), previous);
    assert!(std::fs::read_dir(work.path().join("app"))
        .unwrap()
        .all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".joky-build-")));
    work.write("app/src/shared.jk", "pub fn answer() -> Int32 { true }");
    failure(&work.cli(&["build", "app"]), "Return type");
    assert_eq!(std::fs::read(&binary).unwrap(), previous);
    assert_eq!(
        std::fs::read_to_string(dist.join("src/shared.jk")).unwrap(),
        "pub fn answer() -> Int32 { 42 }"
    );
    work.write("app/src/shared.jk", "pub fn answer() -> Int32 { 43 }");
    success(&work.cli(&["build", "app"]));
    let relocated = work.path().join("moved bundle");
    std::fs::rename(&dist, &relocated).unwrap();
    std::fs::remove_dir_all(work.path().join("app")).unwrap();
    std::os::unix::fs::symlink(relocated.join("demo"), work.path().join("launch")).unwrap();
    let output = Command::new(work.path().join("launch"))
        .current_dir(work.path())
        .env("PATH", "")
        .output()
        .unwrap();
    success(&output);
    assert_eq!(output.stdout, b"project root\n43\n");
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn project_native_drivers_cover_sync_and_parallel_entries() {
    let work = Workspace::new();
    work.write("app/joky.toml", "[project]\nname='demo'\nentry='main.jk'");
    for (source, flags, result) in [
        ("fn main() -> Int32 { 42 }", vec![], 42),
        (
            "import joky/task fn main() @suspends -> Int32 { task.yield_now(); 43 }",
            vec!["--task-workers", "2"],
            43,
        ),
    ] {
        work.write("app/main.jk", source);
        let mut args = vec!["build", "app", "-o", "bundle"];
        args.extend(flags);
        success(&work.cli(&args));
        let output = Command::new(work.path().join("bundle/demo"))
            .current_dir("/")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(result), "{output:?}");
    }
}

#[test]
fn project_manifest_rejects_unknown_invalid_and_escaping_paths() {
    let work = Workspace::new();
    work.write("app/main.jk", "fn main() {}");
    work.write("outside.jk", "fn main() {}");
    work.write("app/src/unused.jk", "fn unused() {}");
    for (fields, message) in [
        ("name='../demo'\nentry='main.jk'", "Project name"),
        ("name=''\nentry='main.jk'", "nonempty string"),
        ("name='demo'\nentry='../outside.jk'", "relative"),
        ("name='demo'\nentry='/outside.jk'", "relative"),
        ("name='demo'\nentry='.'", "relative"),
        ("name='demo'\nentry=''", "nonempty string"),
    ] {
        work.write("app/joky.toml", &format!("[project]\n{fields}"));
        failure(&work.cli(&["build", "app"]), message);
        assert!(!work.path().join("app/dist").exists());
    }
    for (prefix, fields, message) in [
        ("[project", "", "Invalid"),
        ("version=2", "", "version"),
        ("unknown=true", "", "Unknown"),
        ("", "source-root='src'", "outside source-root"),
        ("", "source-root='missing'", "Cannot access"),
        ("", "resoruces=[]", "Unknown"),
        ("", "resources=[42]", "array of paths"),
        ("", "resources='assets'", "array of paths"),
        ("", "resources=['../outside.jk']", "relative"),
        ("", "resources=['/outside.jk']", "relative"),
        ("", "resources=['']", "relative"),
        ("", "resources=['.']", "relative"),
        ("", "resources=['./demo']", "conflicts"),
    ] {
        work.write(
            "app/joky.toml",
            &format!("{prefix}\n[project]\nname='demo'\nentry='main.jk'\n{fields}"),
        );
        failure(&work.cli(&["build", "app"]), message);
        assert!(!work.path().join("app/dist").exists());
    }
}

#[test]
#[cfg(unix)]
fn project_rejects_resource_symlinks_and_protects_unmanaged_outputs() {
    let work = Workspace::new();
    project(&work);
    std::os::unix::fs::symlink(work.path().join("assets"), work.path().join("app/alias")).unwrap();
    work.write(
        "app/joky.toml",
        &CONFIG.replace("\"assets\"", "\"alias/message.txt\""),
    );
    failure(&work.cli(&["build", "app"]), "symlinks");
    work.write("app/joky.toml", CONFIG);
    work.write("existing/keep.txt", "do not change");
    failure(
        &work.cli(&["build", "app", "-o", "existing"]),
        "not a bundle",
    );
    failure(
        &work.cli(&["build", "app", "-o", "app/assets/output"]),
        "overlaps runtime resources",
    );
    assert_eq!(
        std::fs::read_to_string(work.path().join("existing/keep.txt")).unwrap(),
        "do not change"
    );
    assert!(!work.path().join("app/dist").exists());
    work.write("app/DEMO", "resource");
    if work.path().join("app/demo").exists() {
        work.write(
            "app/joky.toml",
            &CONFIG.replace("[\"assets\", \"src/shared.jk\"]", "[\"DEMO\"]"),
        );
        failure(&work.cli(&["build", "app"]), "resource conflicts");
    }
}
