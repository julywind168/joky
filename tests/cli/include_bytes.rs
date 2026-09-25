use super::parity::Package;
use super::*;
use sha2::{Digest, Sha256};

#[test]
fn include_bytes_preserves_binary_data_defaults_templates_and_suspension() {
    let package = Package::new("include-bytes-parity", &[]);
    fs::create_dir_all(package.root.join("src/assets")).unwrap();
    fs::write(
        package.root.join("src/assets/all.bin"),
        (0u8..=255).collect::<Vec<_>>(),
    )
    .unwrap();
    fs::write(package.root.join("src/empty.bin"), []).unwrap();
    fs::write(
        package.root.join("src/assets/resource.jk"),
        r#"
pub const CONTENT = include_bytes("all.bin")
struct DefaultValue { let data: Bytes = include_bytes("all.bin") }
pub fn generic(T: type, value: T) -> Bytes { let _ = value; include_bytes("all.bin") }
pub fn bytes() -> Bytes { include_bytes("all.bin") }
"#,
    )
    .unwrap();
    fs::write(
        package.root.join("src/main.jk"),
        r#"
import assets/resource
eff time { @suspends fn sleep(duration: Duration) -> Unit }
fn assert_bytes(data: Bytes) {
    if data.length() != 256u64 { panic("embedded length") }
    var index = 0u64
    while index < 256u64 {
        if data.get(index)! != (index as% UInt8) { panic("embedded content") }
        index += 1
    }
}
fn main() effects { time } {
    let before = include_bytes("assets/all.bin")
    time.sleep(1ms)
    assert_bytes(before)
    assert_bytes(include_bytes("assets/all.bin"))
    assert_bytes(resource.CONTENT)
    assert_bytes(resource.DefaultValue().data)
    assert_bytes(resource.generic(UInt8, 7u8))
    assert_bytes(resource.bytes())
    let captured = fn() -> Bytes { include_bytes("assets/all.bin") }
    assert_bytes(captured())
    if !include_bytes("empty.bin").is_empty() { panic("empty resource") }
    println("embedded {include_bytes(\"assets/all.bin\").length()}")
}
"#,
    )
    .unwrap();
    // Package removes src (including every resource) before running AOT binaries.
    package.check_cached("embedded 256\n", None, &[]);
}

fn checked(output: std::process::Output) -> std::process::Output {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn include_bytes_changes_invalidate_jit_and_aot_and_update_provenance() {
    let package = Package::new(
        "include-bytes-invalidate",
        &[
            (
                "data.jk",
                r#"
pub const CONTENT = include_bytes("value.bin")
struct DefaultValue { let data: Bytes = include_bytes("value.bin") }
pub fn generic(T: type, value: T) -> Bytes { let _ = value; include_bytes("value.bin") }
pub fn bytes() -> Bytes { include_bytes("value.bin") }
"#,
            ),
            (
                "main.jk",
                r#"
import data
fn main() {
    println(data.bytes().get(0)!)
    println(data.CONTENT.get(0)!)
    println(data.generic(Int32, 1).get(0)!)
    println(data.DefaultValue().data.get(0)!)
}
"#,
            ),
        ],
    );
    let resource = package.root.join("src/value.bin");
    let executable = package.root.join("native");
    for byte in [0u8, 255u8] {
        fs::write(&resource, [byte]).unwrap();
        let expected = format!("{byte}\n").repeat(4);
        for cached in [false, true] {
            let run = checked(
                Command::new(env!("CARGO_BIN_EXE_joky"))
                    .current_dir(&package.root)
                    .args(["run", "--verbose"])
                    .output()
                    .unwrap(),
            );
            assert_eq!(run.stdout, expected.as_bytes());
            if cached {
                assert!(!String::from_utf8_lossy(&run.stderr).contains("[cache] compile"));
            }
        }
        for cached in [false, true] {
            let build = checked(
                Command::new(env!("CARGO_BIN_EXE_joky"))
                    .current_dir(&package.root)
                    .args(["build", "--verbose", "-o"])
                    .arg(&executable)
                    .output()
                    .unwrap(),
            );
            let stderr = String::from_utf8_lossy(&build.stderr);
            if cached {
                assert!(stderr.contains("hit aot object"), "{stderr}");
            }
            let run = checked(Command::new(&executable).output().unwrap());
            assert_eq!(run.stdout, expected.as_bytes());
            let record: serde_json::Value =
                serde_json::from_slice(&fs::read(package.root.join("native.build.json")).unwrap())
                    .unwrap();
            let source = record["sources"]
                .as_array()
                .unwrap()
                .iter()
                .find(|source| {
                    Path::new(source["path"].as_str().unwrap()) == resource.canonicalize().unwrap()
                })
                .unwrap();
            assert_eq!(source["sha256"], hex::encode(Sha256::digest([byte])));
        }
    }
    fs::remove_file(&resource).unwrap();
    for mode in ["run", "check", "build"] {
        let result = Command::new(env!("CARGO_BIN_EXE_joky"))
            .current_dir(&package.root)
            .arg(mode)
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(
            String::from_utf8_lossy(&result.stderr).contains("cannot read include_bytes resource")
        );
    }
    let run = checked(Command::new(&executable).output().unwrap());
    assert_eq!(run.stdout, b"255\n255\n255\n255\n");
    fs::remove_dir_all(&package.root).unwrap();
}

#[test]
fn include_bytes_reports_source_located_resource_and_argument_errors() {
    for (index, (expression, message)) in [
        (
            r#"include_bytes("missing.bin")"#,
            "cannot read include_bytes resource",
        ),
        (r#"include_bytes(".")"#, "regular file"),
        (r#"include_bytes("/absolute.bin")"#, "relative path"),
        (r#"include_bytes("")"#, "relative path"),
        (r#"include_bytes("{1}.bin")"#, "non-interpolated"),
        (r#"include_bytes(b"file.bin")"#, "non-interpolated"),
        (r#"include_bytes(name)"#, "literal relative path"),
        (r#"include_bytes("a", "b")"#, "expected ')'"),
        (r#"include_bytes("large.bin")"#, "exceed 16 MiB"),
    ]
    .into_iter()
    .enumerate()
    {
        let source = format!("fn main() {{ let data = {expression}; () }}");
        let package = Package::new(
            &format!("include-bytes-error-{index}"),
            &[("main.jk", &source)],
        );
        if expression.contains("large.bin") {
            fs::File::create(package.root.join("src/large.bin"))
                .unwrap()
                .set_len(16 * 1024 * 1024 + 1)
                .unwrap();
        }
        let result = Command::new(env!("CARGO_BIN_EXE_joky"))
            .current_dir(&package.root)
            .arg("check")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(!result.status.success(), "{expression}");
        assert!(stderr.contains(message), "{expression}: {stderr}");
        assert!(stderr.contains("main.jk"), "{stderr}");
        fs::remove_dir_all(&package.root).unwrap();
    }
}
