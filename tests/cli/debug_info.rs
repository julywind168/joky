use super::*;
use gimli::{EndianSlice, RunTimeEndian};
use object::{Object, ObjectSection};
use sha2::Digest;

fn assert_source_debug_info(path: &Path) {
    let bytes = fs::read(path).unwrap();
    let object = object::File::parse(bytes.as_slice()).unwrap();
    let endian = if object.is_little_endian() {
        RunTimeEndian::Little
    } else {
        RunTimeEndian::Big
    };
    let sections = gimli::DwarfSections::load(|id| -> Result<Vec<u8>, gimli::Error> {
        Ok(object
            .section_by_name(id.name())
            .map(|section| section.data().unwrap().to_vec())
            .unwrap_or_default())
    })
    .unwrap();
    let dwarf = sections.borrow(|section| EndianSlice::new(section, endian));
    let mut units = dwarf.units();
    let mut found = false;
    while let Some(header) = units.next().unwrap() {
        let unit = dwarf.unit(header).unwrap();
        let mut entries = unit.entries();
        let root = entries.next_dfs().unwrap().unwrap();
        let Some(producer) = root.attr_value(gimli::DW_AT_producer) else {
            continue;
        };
        if !dwarf
            .attr_string(&unit, producer)
            .unwrap()
            .to_string_lossy()
            .starts_with("Joky ")
        {
            continue;
        }
        assert!(!found, "one Joky compilation unit");
        found = true;
        let mut unit_ranges = dwarf.die_ranges(&unit, root).unwrap();
        let unit_range = unit_ranges.next().unwrap().unwrap();
        let mut functions = Vec::new();
        while let Some(entry) = entries.next_dfs().unwrap() {
            if entry.tag() != gimli::DW_TAG_subprogram {
                continue;
            }
            let name = dwarf
                .attr_string(&unit, entry.attr_value(gimli::DW_AT_name).unwrap())
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let mut ranges = dwarf.die_ranges(&unit, entry).unwrap();
            let range = ranges.next().unwrap().unwrap();
            assert!(unit_range.begin <= range.begin && range.end <= unit_range.end);
            assert!(range.begin < range.end);
            functions.push((name, range));
        }
        functions.sort_by_key(|(_, range)| range.begin);
        assert!(
            functions
                .windows(2)
                .all(|pair| pair[0].1.end <= pair[1].1.begin),
            "function ranges must not overlap"
        );
        let mut generic_line = false;
        let mut resumed_line = false;
        let mut rows = unit.line_program.clone().unwrap().rows();
        while let Some((header, row)) = rows.next_row().unwrap() {
            if row.end_sequence() {
                continue;
            }
            let Some(line) = row.line() else { continue };
            let file = row.file(header).unwrap();
            let file = dwarf
                .attr_string(&unit, file.path_name())
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let function = functions
                .iter()
                .find(|(_, range)| range.begin <= row.address() && row.address() < range.end)
                .map(|(name, _)| name.as_str())
                .unwrap_or("");
            generic_line |=
                file.ends_with("util.jk") && line.get() == 2 && function.contains("copy");
            resumed_line |= file.ends_with("main.jk")
                && line.get() == 5
                && function.starts_with("delayed [resume");
        }
        assert!(
            generic_line,
            "generic body line points to its defining module"
        );
        assert!(resumed_line, "machine entry has the line after suspension");
    }
    assert!(found, "missing Joky compilation unit");
}

#[test]
fn build_debug_info_maps_generic_and_resumed_code() {
    let root = std::env::temp_dir().join(format!("joky-debug-info-{}", std::process::id()));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("joky.toml"), "name = \"debug-info\"\n").unwrap();
    fs::write(
        root.join("src/util.jk"),
        "pub fn copy(T: type, value: T) -> T {\n    println(7)\n    value\n}\n",
    )
    .unwrap();
    fs::write(root.join("src/main.jk"), "import util\neff time { @suspends fn sleep(duration: Duration) -> Unit }\nfn delayed() effects { time } {\n    time.sleep(1ms)\n    println(99)\n}\nfn main() effects { time } {\n    println(util.copy(42))\n    delayed()\n}\n").unwrap();
    let output = root.join("app with spaces.bin");
    let record_path = root.join("app with spaces.bin.build.json");
    let bundle = root.join("app with spaces.bin.dSYM");
    for release in [false, true] {
        let mut build = Command::new(env!("CARGO_BIN_EXE_joky"));
        build
            .current_dir(&root)
            .args(["build", if release { "--debug-info" } else { "-g" }, "-o"])
            .arg(&output);
        if release {
            build.arg("--release");
        }
        let result = build.output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
        assert_eq!(record["options"]["debug_info"], true);
        assert!(!output.with_extension("joky.o").exists());
        let debug_file = if cfg!(target_os = "macos") {
            let path = bundle
                .join("Contents/Resources/DWARF")
                .join(output.file_name().unwrap());
            let hash = hex::encode(sha2::Sha256::digest(fs::read(&path).unwrap()));
            assert_eq!(record["debug_artifact"]["sha256"], hash);
            path
        } else {
            output.clone()
        };
        assert_source_debug_info(&debug_file);
        let run = Command::new(&output)
            .current_dir(std::env::temp_dir())
            .output()
            .unwrap();
        assert!(run.status.success());
        assert_eq!(run.stdout, b"7\n42\n99\n");
    }
    // Conflicting flags must not overwrite a previously successful build.
    let previous_record = fs::read(&record_path).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(&root)
        .args(["build", "-g", "--strip", "-o"])
        .arg(&output)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("cannot be combined"));
    assert_eq!(fs::read(&record_path).unwrap(), previous_record);
    let result = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(&root)
        .args(["build", "--release", "-o"])
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!bundle.exists(), "non-debug rebuild removes stale symbols");
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
    assert_eq!(record["options"]["debug_info"], false);
    assert!(record.get("debug_artifact").is_none());
    fs::remove_dir_all(root).unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn failed_dsym_generation_invalidates_records_and_cleans_temporaries() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let root = std::env::temp_dir().join(format!("joky-debug-failure-{}", std::process::id()));
    fs::create_dir_all(root.join("tools")).unwrap();
    fs::write(root.join("main.jk"), "fn main() {}\n").unwrap();
    fs::write(root.join("app.build.json"), "old provenance").unwrap();
    fs::create_dir_all(root.join("app.dSYM")).unwrap();
    fs::write(root.join("app.dSYM/stale"), "old symbols").unwrap();
    symlink("/usr/bin/cc", root.join("tools/cc")).unwrap();
    let dsymutil = root.join("tools/dsymutil");
    fs::write(
        &dsymutil,
        "#!/bin/sh\nprintf 'injected dsym failure\\n' >&2\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&dsymutil, fs::Permissions::from_mode(0o755)).unwrap();
    let output = root.join("app");
    let result = Command::new(env!("CARGO_BIN_EXE_joky"))
        .args(["build", "-g", "--release"])
        .arg(root.join("main.jk"))
        .arg("-o")
        .arg(&output)
        .env("PATH", root.join("tools"))
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("injected dsym failure"),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!root.join("app.build.json").exists());
    assert!(!root.join("app.dSYM").exists());
    assert!(!output.with_extension("joky.o").exists());
    assert!(!output.with_extension("joky-launcher.c").exists());
    assert!(!fs::read_dir(&root).unwrap().any(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .ends_with(".tmp")));
    fs::remove_dir_all(root).unwrap();
}
