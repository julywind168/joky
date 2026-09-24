use super::*;
use serde_json::Value;
use sha2::{Digest, Sha256};

fn read_record(output: &Path) -> Value {
    let mut name = output.as_os_str().to_owned();
    name.push(".build.json");
    serde_json::from_slice(&fs::read(name).unwrap()).unwrap()
}

fn hash(path: &Path) -> String {
    hex::encode(Sha256::digest(fs::read(path).unwrap()))
}

fn assert_file(record: &Value, path: &Path) {
    assert_eq!(
        Path::new(record["path"].as_str().unwrap()),
        path.canonicalize().unwrap()
    );
    assert_eq!(record["sha256"], hash(path));
    assert_eq!(record["size_bytes"], fs::metadata(path).unwrap().len());
}

#[test]
fn build_record_tracks_inputs_options_and_final_artifact() {
    let root = std::env::temp_dir().join(format!("joky-build-record-{}", std::process::id()));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("joky.toml"), "name = \"records\"\n").unwrap();
    let entry = root.join("src/main.jk");
    let dependency = root.join("src/util.jk");
    fs::write(
        &entry,
        "import util\nfn main() { println(util.answer()) }\n",
    )
    .unwrap();
    fs::write(&dependency, "pub fn answer() -> Int32 { 42 }\n").unwrap();
    let output = root.join("dist with spaces/program.bin");
    let build = |flags: &[&str]| {
        let result = Command::new(env!("CARGO_BIN_EXE_joky"))
            .current_dir(&root)
            .args(["build", "-o"])
            .arg(&output)
            .args(flags)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(String::from_utf8_lossy(&result.stderr).contains("Record   '"));
        read_record(&output)
    };
    let first = build(&[]);
    assert_eq!(first["schema_version"], 1);
    assert_eq!(first["compiler"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(first["compiler"]["build_id"], env!("JOKY_BUILD_ID"));
    assert_file(
        &first["compiler"]["executable"],
        Path::new(env!("CARGO_BIN_EXE_joky")),
    );
    assert_eq!(first["options"]["target"], env!("JOKY_TARGET"));
    assert_eq!(first["options"]["opt_level"], "none");
    assert_eq!(first["options"]["strip"], false);
    assert_eq!(first["options"]["debug_info"], false);
    assert_eq!(first["options"]["pointer_width"], usize::BITS);
    assert_eq!(first["options"]["cpu"], "native");
    assert_eq!(
        first["runtime_abi_version"],
        joky_runtime_abi::AOT_RUNTIME_ABI_VERSION
    );
    assert_file(
        &first["runtime"],
        Path::new(first["runtime"]["path"].as_str().unwrap()),
    );
    assert_file(&first["artifact"], &output);
    let sources = first["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 2);
    assert!(sources
        .windows(2)
        .all(|pair| pair[0]["path"].as_str() < pair[1]["path"].as_str()));
    for path in [&entry, &dependency] {
        let source = sources
            .iter()
            .find(|source| {
                Path::new(source["path"].as_str().unwrap()) == path.canonicalize().unwrap()
            })
            .unwrap();
        assert_eq!(source["sha256"], hash(path));
    }
    let linker = &first["linker"];
    assert!(!linker["version"].as_str().unwrap().is_empty());
    assert_file(
        &linker["executable"],
        Path::new(linker["executable"]["path"].as_str().unwrap()),
    );
    assert_eq!(
        Path::new(linker["working_directory"].as_str().unwrap()),
        root.canonicalize().unwrap()
    );
    let command = linker["command"].as_array().unwrap();
    assert_eq!(
        command.last().unwrap().as_str().unwrap(),
        output.to_str().unwrap()
    );
    assert!(command.iter().any(|arg| arg == "-o"));
    assert!(!command.iter().any(|arg| arg == "-s"));
    for key in ["object_sha256", "launcher_sha256"] {
        assert_eq!(first[key].as_str().unwrap().len(), 64);
    }
    assert!(
        !output.with_extension("build.json").exists(),
        "append, do not replace the executable extension"
    );
    assert!(!output.with_extension("joky.o").exists());
    assert!(!output.with_extension("joky-launcher.c").exists());

    let repeated = build(&["--target", env!("JOKY_TARGET")]);
    for key in [
        "compiler",
        "sources",
        "runtime",
        "runtime_abi_version",
        "options",
        "linker",
        "launcher_sha256",
        "object_sha256",
    ] {
        assert_eq!(first[key], repeated[key], "stable provenance: {key}");
    }
    assert_file(&repeated["artifact"], &output);

    fs::write(&dependency, "pub fn answer() -> Int32 { 43 }\n").unwrap();
    let changed = build(&["--target", env!("JOKY_TARGET"), "--release", "--strip"]);
    assert_ne!(first["sources"], changed["sources"]);
    assert_eq!(changed["options"]["opt_level"], "speed");
    assert_eq!(changed["options"]["runtime_profile"], "release");
    assert_eq!(changed["options"]["strip"], true);
    assert!(changed["linker"]["command"]
        .as_array()
        .unwrap()
        .iter()
        .any(|arg| arg == "-s"));
    assert_file(&changed["artifact"], &output);
    let run = Command::new(&output).output().unwrap();
    assert!(run.status.success());
    assert_eq!(run.stdout, b"43\n");
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn failed_link_invalidates_the_previous_build_record() {
    use std::os::unix::fs::PermissionsExt;
    let root =
        std::env::temp_dir().join(format!("joky-record-link-failure-{}", std::process::id()));
    fs::create_dir_all(root.join("tools")).unwrap();
    let source = root.join("main.jk");
    let output = root.join("app");
    let record = root.join("app.build.json");
    fs::write(&source, "fn main() {}\n").unwrap();
    fs::write(&output, "old executable").unwrap();
    fs::write(&record, "old provenance").unwrap();
    let cc = root.join("tools/cc");
    fs::write(&cc, "#!/bin/sh\nif [ \"$1\" = '--version' ]; then printf 'test cc\\n'; exit 0; fi\nprintf 'injected linker failure\\n' >&2\nexit 1\n").unwrap();
    fs::set_permissions(&cc, fs::Permissions::from_mode(0o755)).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_joky"))
        .args(["build"])
        .arg(&source)
        .arg("-o")
        .arg(&output)
        .env("PATH", root.join("tools"))
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("injected linker failure"));
    assert!(!record.exists());
    assert!(!output.with_extension("joky.o").exists());
    assert!(!output.with_extension("joky-launcher.c").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn unwritable_build_record_does_not_report_success() {
    let root =
        std::env::temp_dir().join(format!("joky-record-write-failure-{}", std::process::id()));
    fs::create_dir_all(root.join("app.build.json")).unwrap();
    let source = root.join("main.jk");
    fs::write(&source, "fn main() {}\n").unwrap();
    let output = root.join("app");
    fs::write(&output, "previous executable").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_joky"))
        .arg("build")
        .arg(&source)
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("build record failed"));
    assert_eq!(fs::read(&output).unwrap(), b"previous executable");
    assert!(!output.with_extension("joky.o").exists());
    fs::remove_dir_all(root).unwrap();
}
