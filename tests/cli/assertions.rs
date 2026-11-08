//! Comparison assertion behavior and caller-facing diagnostics.
use super::*;

#[test]
#[cfg(unix)]
fn comparison_assertions_borrow_evaluate_once_and_render_only_on_failure() {
    let work = Workspace::new();
    work.write("assertions.jk", include_str!("../fixtures/assertions.jk"));
    work.assert_all_backends("assertions.jk", &["--no-prelude"], b"", 0);
}

#[test]
fn comparison_assertions_report_values_and_the_actual_call_site() {
    let work = Workspace::new();
    work.write(
        "assertions_test.jk",
        r#"import joky/task
import joky/eq.{type Eq}
import joky/show.{type Show}
import joky/string.{type StringBuilder}
#test
fn equal_values() {
    assert_eq(4, 4)
    assert_eq(4, 5)
}
#test
fn unequal_values() @suspends {
    task.yield_now()
    assert_ne("中\n\r\t\0\"\\", "中\n\r\t\0\"\\")
}
#test
fn empty_text() { assert_eq("", " ") }
struct Value { number: Int32 }
impl Value {
    fn eq(&self, other: &Self) -> Bool { false }
    fn to_text(&self) -> String { "wrong inherent method" }
}
impl Eq for Value {
    fn eq(&self, other: &Self) -> Bool { self.number == other.number }
}
impl Show for Value {
    fn show(&self, output: &mut StringBuilder) @noescape {
        assert(self.number != 3, "user display failed")
        output.push_str("custom ")
        Show.show(&self.number, output)
    }
}
#test
fn custom_values() {
    assert_eq(Value { number: 1 }, Value { number: 1 })
    assert_eq(Value { number: 1 }, Value { number: 2 })
}
#test
fn display_failure() { assert_eq(Value { number: 3 }, Value { number: 4 }) }
"#,
    );
    for backend in [vec![], vec!["--interpreter"]] {
        let mut args = vec!["test", "assertions_test.jk"];
        args.extend(backend);
        let output = work.cli(&args);
        assert_eq!(output.status.code(), Some(1));
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stdout.contains("0 passed; 5 failed"), "{stdout}\n{stderr}");
        for expected in [
            "assert_eq failed\n  actual: \"4\"\nexpected: \"5\"",
            "assert_ne failed (values must differ)",
            r#"actual: "中\n\r\t\u{00}\"\\""#,
            "actual: \"\"\nexpected: \" \"",
            "actual: \"custom 1\"\nexpected: \"custom 2\"",
            "user display failed",
            "assertions_test.jk:8:5",
            "assertions_test.jk:13:5",
            "assertions_test.jk:16:19",
            "assertions_test.jk:35:5",
            "assertions_test.jk:27:9",
        ] {
            assert!(stderr.contains(expected), "missing {expected:?}: {stderr}");
        }
        assert!(!stderr.contains("╭─[ joky/assertions.jk:"), "{stderr}");
        assert_eq!(
            stderr
                .matches("assert_eq failed\n  actual: \"4\"\nexpected: \"5\"")
                .count(),
            1,
            "{stderr}"
        );
    }
}

#[test]
#[cfg(unix)]
fn comparison_assertion_failures_keep_messages_and_call_sites_in_aot() {
    let work = Workspace::new();
    for (function, right) in [("assert_eq", "5"), ("assert_ne", "4")] {
        let source = format!("fn main() {{ {function}(4, {right}) }}");
        work.write("failure.jk", &source);
        success(&work.cli(&["build", "failure.jk", "-o", "failure"]));
        let output = Command::new(work.path().join("failure")).output().unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(&format!("{function} failed")), "{stderr}");
        assert!(stderr.contains("actual: \"4\""), "{stderr}");
        assert!(
            stderr.contains(&format!("expected: \"{right}\"")),
            "{stderr}"
        );
        assert!(stderr.contains("called at source"), "{stderr}");
        let start = source.find(function).unwrap();
        let end = source.rfind(')').unwrap() + 1;
        assert!(
            stderr.contains(&format!("bytes {start}..{end}")),
            "{stderr}"
        );
    }
}

#[test]
fn comparison_assertions_require_matching_types_and_both_traits() {
    let work = Workspace::new();
    for source in [
        "fn main() { assert_eq(1, true) }",
        "struct Missing { value: Int32 } fn main() { assert_eq(Missing { value: 1 }, Missing { value: 1 }) }",
        "fn main() { assert_ne(1) }",
        "import joky/eq.{type Eq} struct OnlyEq {} impl Eq for OnlyEq { fn eq(&self, other: &Self) -> Bool { true } } fn main() { assert_eq(OnlyEq {}, OnlyEq {}) }",
        "import joky/show.{type Show} import joky/string.{type StringBuilder} struct OnlyShow {} impl Show for OnlyShow { fn show(&self, output: &mut StringBuilder) @noescape {} } fn main() { assert_ne(OnlyShow {}, OnlyShow {}) }",
    ] {
        work.write("invalid.jk", source);
        let output = work.cli(&["check", "invalid.jk"]);
        assert!(!output.status.success(), "accepted {source}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalid.jk:"));
    }
}
