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
            for href in attributes(&html, " href=\"") {
                if let Some(id) = href.strip_prefix('#') {
                    assert!(
                        ids.contains(&id),
                        "missing anchor {href}: {}",
                        path.display()
                    );
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
        vec!["doc"],
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
fn doc_cli_option_example_executes() {
    let work = Workspace::new();
    let source = include_str!("../../src/stdlib/option.jk");
    let example = source
        .split_once("//! ```joky\n")
        .unwrap()
        .1
        .split_once("//! ```")
        .unwrap()
        .0;
    let code = example
        .lines()
        .map(|line| line.strip_prefix("//! ").unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    work.write("doc_option.jk", &code);
    let output = work.cli(&["run", "doc_option.jk"]);
    success(&output);
    assert_eq!(output.stdout, b"42\n");
}
