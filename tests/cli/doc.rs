//! Offline documentation generation, links, visibility and output ownership.
use super::*;
use std::fs;
use std::path::Path;

fn attributes<'a>(html: &'a str, name: &'a str) -> impl Iterator<Item = &'a str> {
    html.split(name)
        .skip(1)
        .map(|s| s.split('"').next().unwrap())
}

fn check_pages(path: &Path, count: &mut usize) {
    for entry in fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            check_pages(&path, count);
        } else if path.extension().is_some_and(|e| e == "html") {
            *count += 1;
            let html = fs::read_to_string(&path).unwrap();
            let ids: Vec<_> = attributes(&html, " id=\"").collect();
            let unique: std::collections::HashSet<_> = ids.iter().collect();
            assert_eq!(
                ids.len(),
                unique.len(),
                "duplicate anchors: {}",
                path.display()
            );
            for href in attributes(&html, " href=\"").chain(attributes(&html, " src=\"")) {
                if let Some(id) = href.strip_prefix('#') {
                    assert!(
                        id.is_empty() || ids.contains(&id),
                        "missing anchor {href}: {}",
                        path.display()
                    );
                } else if href.starts_with("https://") {
                    assert!(href.starts_with("https://github.com/julywind168/joky"));
                } else {
                    assert!(
                        path.parent().unwrap().join(href).is_file(),
                        "broken link {href}: {}",
                        path.display()
                    );
                }
            }
            assert!(!html.contains("__joky_"));
            assert!(!html.contains("joky/internal/"));
        }
    }
}

fn text_without_tags(html: &str) -> String {
    let mut tag = false;
    html.chars()
        .filter(|&c| {
            if c == '<' {
                tag = true;
                false
            } else if c == '>' {
                tag = false;
                false
            } else {
                !tag
            }
        })
        .collect()
}

#[test]
fn doc_cli_generates_linked_offline_reference_and_regenerates() {
    let work = Workspace::new();
    // Unlike run/build, documentation needs neither a runtime archive nor cc.
    let output = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(work.path())
        .env("JOKY_RUNTIME", "/missing/runtime.a")
        .args(["doc", "--std"])
        .output()
        .unwrap();
    success(&output);
    assert!(output.stderr.is_empty());
    assert!(String::from_utf8_lossy(&output.stdout).contains("target/doc/std/index.html"));
    let root = work.path().join("target/doc/std");
    let mut count = 0;
    check_pages(&root, &mut count);
    assert!(count > 30, "expected the embedded standard inventory");
    let index = fs::read_to_string(root.join("index.html")).unwrap();
    assert!(index.contains("json/typed/index.html"));
    let testing = fs::read_to_string(root.join("testing/index.html")).unwrap();
    assert!(text_without_tags(&testing).contains("pub fn set_stdin(text: String) @suspends"));
    assert!(testing.contains("class=\"syntax-annotation\">suspends</span>"));
    assert!(!text_without_tags(&testing).contains("fn configure"));
    assert!(!text_without_tags(&testing).contains("fn error"));
    for module in ["log", "http", "time", "task", "string"] {
        let html = fs::read_to_string(root.join(module).join("index.html")).unwrap();
        let text = text_without_tags(&html);
        assert!(!text.contains("try_emit"), "{module}");
        assert!(!text.contains("fn equals("), "{module}");
        assert!(!text.contains("__sleep_until"), "{module}");
        if module == "time" {
            assert!(!text.contains("fn sleep_until("));
            for name in ["Duration", "Instant", "Timestamp"] {
                assert!(text.contains(&format!("pub struct {name}")), "{name}");
            }
            assert!(text.contains("pub fn from_nanos(value: UInt64) -&gt; Duration"));
            assert!(text.contains("pub fn now() -&gt; Instant"));
            assert!(text.contains("pub fn parse(text: &amp;String) -&gt; Option&lt;Timestamp&gt;"));
            assert!(!text.contains("fn scaled("));
            assert!(!text.contains("fn valid("));
        } else if module == "task" {
            assert!(text.contains("pub fn sleep_until(deadline: Instant) @suspends"));
        } else if module == "log" {
            assert!(text.contains("-&gt; Result&lt;Unit, LogError&gt;"));
            assert!(text.contains("-&gt; Result&lt;Emission, LogError&gt;"));
        }
    }
    for (module, hidden) in [
        ("assertions", vec!["fn quoted", "fn hex_digit"]),
        (
            "http",
            vec![
                "enum Wire",
                "enum ReadFailure",
                "fn read_request",
                "fn open(",
            ],
        ),
        ("tls", vec!["fn failure", "fn flush(", "const RECORD"]),
        (
            "postgres",
            vec!["struct SqlBuilder", "fn encode(", "fn rows("],
        ),
        ("float", vec!["fn float64_sqrt", "fn i8_from_valid_f64"]),
        ("fs", vec!["const IO_CHUNK_SIZE", "fn read_remaining"]),
    ] {
        let text =
            text_without_tags(&fs::read_to_string(root.join(module).join("index.html")).unwrap());
        for name in hidden {
            assert!(!text.contains(name), "{module}: {name}");
        }
    }
    let integer = fs::read_to_string(root.join("integer/index.html")).unwrap();
    assert!(!integer.contains("rune_from_valid_u32"));
    assert!(!integer.contains("parse_decimal"));
    let memory = fs::read_to_string(root.join("mem/index.html")).unwrap();
    assert!(text_without_tags(&memory).contains("fn replace&lt;T&gt;"));
    assert!(!memory.contains("#intrinsic"));
    let option = fs::read_to_string(root.join("option/index.html")).unwrap();
    assert!(option.contains("class=\"language-joky\""));
    assert!(option.contains("class=\"syntax-constant\">21</span>"));
    assert!(option.contains("class=\"syntax-function\">println</span>"));
    let style = fs::read_to_string(root.join("style.css")).unwrap();
    assert!(style.contains(".syntax-keyword"));
    assert!(style.contains("prefers-color-scheme: dark"));
    // Byte-for-byte deterministic output, including impl anchors.
    success(&work.cli(&["doc", "--std"]));
    assert_eq!(index, fs::read_to_string(root.join("index.html")).unwrap());
    assert_eq!(
        testing,
        fs::read_to_string(root.join("testing/index.html")).unwrap()
    );
    work.assert_no_build_files();
}

#[test]
fn doc_cli_validates_arguments_and_preserves_unowned_output() {
    let work = Workspace::new();
    for args in [
        vec!["doc", "--open", "--open"],
        vec!["doc", "--std", "--std"],
        vec!["doc", "--std", "--output"],
        vec!["doc", "--std", "input.jk"],
        vec!["doc", "--std", "-o", ""],
    ] {
        assert!(!work.cli(&args).status.success(), "{args:?}");
    }
    work.write("existing/index.html", "keep me");
    failure(&work.cli(&["doc", "--std", "-o", "existing"]), "not empty");
    assert_eq!(
        fs::read_to_string(work.path().join("existing/index.html")).unwrap(),
        "keep me"
    );
    assert!(!work.path().join("existing/.joky-doc").exists());
    success(&work.cli(&["doc", "--std", "--output", "custom docs/std"]));
    assert!(work.path().join("custom docs/std/index.html").is_file());
    success(&work.cli(&["doc", "--help"]));
}

#[cfg(unix)]
#[test]
fn doc_cli_refuses_symlink_output_files() {
    use std::os::unix::fs::symlink;
    let work = Workspace::new();
    work.write("outside", "untouched");
    success(&work.cli(&["doc", "--std", "-o", "docs"]));
    fs::remove_file(work.path().join("docs/style.css")).unwrap();
    symlink(
        work.path().join("outside"),
        work.path().join("docs/style.css"),
    )
    .unwrap();
    failure(&work.cli(&["doc", "--std", "-o", "docs"]), "symlink");
    assert_eq!(
        fs::read_to_string(work.path().join("outside")).unwrap(),
        "untouched"
    );
}

#[test]
fn doc_cli_standard_library_examples_execute() {
    let work = Workspace::new();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/stdlib");
    let mut paths: Vec<_> = fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "jk"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty());
    let mut programs = Vec::new();
    let mut tests = Vec::new();
    for path in paths {
        let source = fs::read_to_string(&path).unwrap();
        let mut count = 0;
        for block in source.split("//! ```joky\n").skip(1) {
            let (example, _) = block.split_once("//! ```").expect("unclosed doc example");
            let code = example
                .lines()
                .map(|line| {
                    line.strip_prefix("//! ").unwrap_or_else(|| {
                        assert_eq!(line, "//!", "{}", path.display());
                        ""
                    })
                })
                .collect::<Vec<_>>()
                .join("\n")
                + "\n";
            let file = format!(
                "doc_{}_{count}.jk",
                path.file_stem().unwrap().to_str().unwrap()
            );
            work.write(&format!("examples/{file}"), &code);
            if code.contains("#test\n") {
                tests.push((file, code));
            } else {
                programs.push((file, code));
            }
            count += 1;
        }
        assert!(
            count > 0,
            "{} needs a runnable module example",
            path.display()
        );
    }
    success(&work.cli(&["fmt", "examples", "--check"]));
    // Compile the snippets as standalone programs, just as a reader copies them.
    // Importing them into a batch can change trait and associated-type resolution.
    for (file, code) in programs {
        work.write(&file, &code);
        let output = work.cli(&["run", &file]);
        success(&output);
        let interpreted = work.cli(&["run", &file, "--interpreter"]);
        success(&interpreted);
        assert_eq!(interpreted.stdout, output.stdout, "{file}: interpreter");
        #[cfg(unix)]
        {
            success(&work.cli(&["build", &file]));
            let native = std::process::Command::new(work.path().join(&file).with_extension(""))
                .current_dir(work.path())
                .output()
                .unwrap();
            success(&native);
            assert_eq!(native.stdout, output.stdout, "{file}: AOT");
        }
    }
    for (file, code) in tests {
        work.write(&file, &code);
        success(&work.cli(&["test", &file]));
        success(&work.cli(&["test", &file, "--interpreter"]));
    }
}

#[test]
fn doc_cli_generates_homepage_and_upgrades_standard_only_output() {
    let work = Workspace::new();
    success(&work.cli(&["doc", "--std"]));
    let root = work.path().join("target/doc");
    let standalone = fs::read_to_string(root.join("std/index.html")).unwrap();
    assert!(!standalone.contains("Joky 首页"));
    work.write("target/doc/preview.txt", "keep preview");
    let output = work.cli(&["doc"]);
    success(&output);
    assert_eq!(
        fs::read_to_string(root.join("preview.txt")).unwrap(),
        "keep preview"
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("target/doc/index.html"));
    let home = fs::read_to_string(root.join("index.html")).unwrap();
    let style = fs::read_to_string(root.join("style.css")).unwrap();
    assert!(!style.contains("@import"));
    assert!(!style.contains("https://"));
    assert_eq!(
        home,
        include_str!("../../sites/joky-home/public/index.html")
    );
    let mut count = 0;
    check_pages(&root, &mut count);
    assert!(count > 30);
    let nested = fs::read_to_string(root.join("std/json/typed/index.html")).unwrap();
    assert!(nested.contains("href=\"../../../index.html\">Joky 首页"));
    // Regenerating just std preserves the installed site's home navigation.
    success(&work.cli(&["doc", "--std"]));
    assert_eq!(
        nested,
        fs::read_to_string(root.join("std/json/typed/index.html")).unwrap()
    );
    work.write("target/doc/notes.txt", "keep");
    success(&work.cli(&["doc"]));
    assert_eq!(home, fs::read_to_string(root.join("index.html")).unwrap());
    assert_eq!(fs::read_to_string(root.join("notes.txt")).unwrap(), "keep");
    success(&work.cli(&["doc", "-o", "custom site"]));
    check_pages(&work.path().join("custom site"), &mut count);
    work.write("unowned/index.html", "keep");
    failure(&work.cli(&["doc", "-o", "unowned"]), "not empty");
    assert_eq!(
        fs::read_to_string(work.path().join("unowned/index.html")).unwrap(),
        "keep"
    );
    success(&work.cli(&["doc", "--std", "-o", "conflict/std"]));
    work.write("conflict/index.html", "handwritten home");
    failure(&work.cli(&["doc", "-o", "conflict"]), "not empty");
    assert_eq!(
        fs::read_to_string(work.path().join("conflict/index.html")).unwrap(),
        "handwritten home"
    );
    // Neither output layout may be mistaken for the other and overwritten.
    failure(
        &work.cli(&["doc", "--std", "-o", "custom site"]),
        "not empty",
    );
    failure(&work.cli(&["doc", "-o", "target/doc/std"]), "not empty");
}

#[cfg(unix)]
#[test]
fn doc_cli_opens_selected_index_and_reports_launcher_failure() {
    use std::os::unix::fs::PermissionsExt;
    let work = Workspace::new();
    let launcher = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let script = format!("bin/{launcher}");
    work.write(
        &script,
        "#!/bin/sh\nprintf '%s' \"$1\" > \"$DOC_OPEN_CAPTURE\"\nexit \"$DOC_OPEN_STATUS\"\n",
    );
    fs::set_permissions(work.path().join(script), fs::Permissions::from_mode(0o755)).unwrap();
    let capture = work.path().join("opened");
    let run = |args: &[&str], status: &str| {
        Command::new(env!("CARGO_BIN_EXE_joky"))
            .current_dir(work.path())
            .env("PATH", work.path().join("bin"))
            .env("DOC_OPEN_CAPTURE", &capture)
            .env("DOC_OPEN_STATUS", status)
            .args(args)
            .output()
            .unwrap()
    };
    for (args, index) in [
        (
            vec!["doc", "--open", "-o", "site with spaces"],
            "site with spaces/index.html",
        ),
        (vec!["doc", "--std", "--open"], "target/doc/std/index.html"),
    ] {
        success(&run(&args, "0"));
        assert_eq!(
            fs::read_to_string(&capture).unwrap(),
            fs::canonicalize(work.path().join(index))
                .unwrap()
                .to_str()
                .unwrap()
        );
    }
    let failed = run(&["doc", "--open"], "1");
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stdout).contains("Generated target/doc/index.html"));
    assert!(String::from_utf8_lossy(&failed.stderr).contains("failed to open"));
    assert!(work.path().join("target/doc/index.html").is_file());
}

#[cfg(unix)]
#[test]
fn doc_cli_refuses_symlink_home_assets_and_standard_directory() {
    use std::os::unix::fs::symlink;
    let work = Workspace::new();
    success(&work.cli(&["doc"]));
    work.write("outside", "untouched");
    fs::remove_file(work.path().join("target/doc/main.js")).unwrap();
    symlink(
        work.path().join("outside"),
        work.path().join("target/doc/main.js"),
    )
    .unwrap();
    failure(&work.cli(&["doc"]), "symlink");
    assert_eq!(
        fs::read_to_string(work.path().join("outside")).unwrap(),
        "untouched"
    );
    fs::create_dir(work.path().join("linked")).unwrap();
    symlink(
        work.path().join("target/doc/std"),
        work.path().join("linked/std"),
    )
    .unwrap();
    failure(&work.cli(&["doc", "-o", "linked"]), "symlink");
}
