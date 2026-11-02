//! Local packages exercise the same resolved graph across CLI execution routes.
use super::*;

const APP: &str = "[project]\nname='app'\nentry='src/main.jk'\nsource-root='src'\n[dependencies]\nleft={path='../left'}\nright={path='../right'}\nagain={path='../left/.'}\n";
const LEFT: &str = "[package]\nname='left'\nversion='0.1.0'\nsource-root='src'\n[dependencies]\nbase={path='../shared'}\n";
const MAIN: &str = r#"
import left/api as left
import right/api as right
import joky/io.{println}
import joky/check.{assert}
fn main() { let value = left.make(); println(right.take(&value)) }
#test fn shared_identity() { let value = left.make(); assert(right.take(&value) == 42, "diamond identity") }
"#;

fn fixture(work: &Workspace) {
    work.write("app/joky.toml", APP);
    work.write("app/src/main.jk", MAIN);
    work.write("app/src/aliases_test.jk", r#"
import again/api as left
import right/api as right
import joky/check.{assert}
#test fn alias_identity() { let value = left.make(); assert(right.take(&value) == 42, "alias identity") }
"#);
    work.write("app/src/helper.jk", "pub fn number() -> Int32 { 9000 }");
    work.write("left/joky.toml", LEFT);
    work.write("left/src/helper.jk", "pub fn number() -> Int32 { 23 }");
    work.write(
        "left/src/api.jk",
        r#"
import base/model.{type Item}
import helper
import joky/check.{assert}
pub fn make() -> Item { Item { value: helper.number() } }
#test fn own_dependency() { let item = make(); assert(item.value == 23, "library test") }
"#,
    );
    work.write("right/joky.toml", "[package]\nname='right'\nsource-root='src'\n[dependencies]\nstorage={path='../shared/.'}\n");
    work.write("right/src/helper.jk", "pub fn number() -> Int32 { 19 }");
    work.write(
        "right/src/api.jk",
        r#"
import storage/model.{type Item, type Score}
import helper
pub fn take(item: &Item) -> Int32 { item.score() + helper.number() }
"#,
    );
    work.write(
        "shared/joky.toml",
        "[package]\nname='shared'\nversion='0.1.0'\nsource-root='src'\n",
    );
    work.write(
        "shared/src/model.jk",
        r#"
import joky/check.{assert}
pub struct Item { pub value: Int32 }
pub trait Score { fn score(&self) -> Int32 }
impl Score for Item { fn score(&self) -> Int32 { self.value } }
#test fn dependency_tests_are_not_inherited() { assert(false, "must run only in shared package") }
"#,
    );
}

#[test]
fn package_diamonds_and_scoped_modules_execute_on_three_backends() {
    let work = Workspace::new();
    fixture(&work);
    success(&work.cli(&["check", "app", "--no-prelude"]));
    for flags in [
        vec!["run", "app", "--no-prelude"],
        vec!["run", "app", "--no-prelude", "--interpreter"],
    ] {
        let output = work.cli(&flags);
        success(&output);
        assert_eq!(output.stdout, b"42\n");
    }
    success(&work.cli(&["build", "app", "--no-prelude"]));
    let moved = work.path().join("bundle");
    std::fs::rename(work.path().join("app/dist"), &moved).unwrap();
    for package in ["app", "left", "right", "shared"] {
        std::fs::remove_dir_all(work.path().join(package)).unwrap();
    }
    let output = Command::new(moved.join("app"))
        .current_dir(work.path())
        .env("PATH", "")
        .output()
        .unwrap();
    success(&output);
    assert_eq!(output.stdout, b"42\n");
}

#[test]
fn package_libraries_check_and_test_without_entries_or_inherited_tests() {
    let work = Workspace::new();
    fixture(&work);
    success(&work.cli(&["check", "left", "--no-prelude"]));
    for args in [
        vec!["test", "app", "--no-prelude"],
        vec!["test", "left", "--no-prelude", "--interpreter"],
    ] {
        let output = work.cli(&args);
        success(&output);
        let count = if args[1] == "app" { 2 } else { 1 };
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(&format!("{count} passed; 0 failed"))
        );
    }
    failure(
        &work.cli(&["run", "left"]),
        "Library packages have no executable entry",
    );
    failure(
        &work.cli(&["build", "left"]),
        "Library packages have no executable entry",
    );
    assert!(!work.path().join("left/dist").exists());
    // Application checking follows its entry; library checking covers every module.
    work.write("left/src/unused.jk", "pub fn wrong() -> Int32 { true }");
    success(&work.cli(&["check", "app"]));
    let output = work.cli(&["check", "left"]);
    failure(&output, "Return type");
    assert!(String::from_utf8_lossy(&output.stderr).contains("left/src/unused.jk"));
    // Explicit files do not silently discover or adopt a nearby package manifest.
    failure(&work.cli(&["check", "left/src/api.jk"]), "base/model.jk");
}

#[test]
fn package_imports_enforce_direct_dependencies_and_private_members() {
    let work = Workspace::new();
    fixture(&work);
    for source in [
        "import storage/model fn main() {}",
        "import shared/model fn main() {}",
        "import __joky_package_shared/model fn main() {}",
    ] {
        work.write("app/src/main.jk", source);
        failure(&work.cli(&["check", "app"]), "Cannot access");
    }
    work.write(
        "app/src/main.jk",
        "import left/api as first import again/api as second fn main() {}",
    );
    failure(&work.cli(&["check", "app"]), "Duplicate import");
    work.write(
        "left/src/model.jk",
        "pub struct Item { pub value: Int32 } pub fn make() -> Item { Item { value: 42 } }",
    );
    work.write(
        "right/src/model.jk",
        "pub struct Item { pub value: Int32 } pub fn take(item: Item) -> Int32 { item.value }",
    );
    work.write("app/src/main.jk", "import left/model as left import right/model as right fn main() -> Int32 { right.take(left.make()) }");
    failure(&work.cli(&["check", "app"]), "type_mismatch");
    work.write("app/src/main.jk", MAIN);
    work.write(
        "left/src/api.jk",
        "import private_app pub fn make() -> Int32 { private_app.value() }",
    );
    work.write("app/src/private_app.jk", "pub fn value() -> Int32 { 42 }");
    let output = work.cli(&["check", "app"]);
    failure(&output, "left/src/private_app.jk");
    assert!(String::from_utf8_lossy(&output.stderr).contains("left/src/api.jk"));
    work.write("left/src/api.jk", "fn secret() -> Int32 { 42 }");
    work.write(
        "app/src/main.jk",
        "import left/api fn main() -> Int32 { api.secret() }",
    );
    failure(&work.cli(&["check", "app"]), "secret");
}

#[test]
fn package_manifests_reject_conflicts_cycles_and_unsupported_sources() {
    let work = Workspace::new();
    fixture(&work);
    for (manifest, message) in [
        (
            APP.replace("path='../left'", "git='https://example.invalid/left'"),
            "path",
        ),
        (
            APP.replace("path='../left'", "path='/tmp/left'"),
            "relative",
        ),
        (APP.replace("left={", "joky={"), "reserved dependency alias"),
        (APP.replace("left={", "'bad/name'={"), "dependency alias"),
        (
            APP.replace("path='../left'", "path='../missing'"),
            "missing/joky.toml",
        ),
    ] {
        work.write("app/joky.toml", &manifest);
        failure(&work.cli(&["check", "app"]), message);
    }
    work.write("app/joky.toml", APP);
    for (manifest, message) in [
        (
            LEFT.replace("[package]", "[project]")
                .replace("name='left'", "name='left'\nentry='src/api.jk'")
                .replace("version='0.1.0'\n", ""),
            "must declare [package]",
        ),
        (
            LEFT.replace("source-root='src'", "source-root='src'\nentry='src/api.jk'"),
            "Unknown project manifest field: entry",
        ),
        (
            LEFT.replace("source-root='src'", "source-root='src'\nresources=['src']"),
            "Unknown project manifest field: resources",
        ),
        (
            LEFT.replace("path='../shared'", "path='../left'"),
            "Cyclic package dependency",
        ),
        (
            LEFT.replace("name='left'", "name='shared'"),
            "Conflicting sources for package",
        ),
    ] {
        work.write("left/joky.toml", &manifest);
        failure(&work.cli(&["check", "app"]), message);
    }
    work.write("left/joky.toml", LEFT);
    work.write("app/src/left.jk", "pub fn unrelated() {}");
    failure(
        &work.cli(&["check", "app"]),
        "conflicts with a local module",
    );
    std::fs::remove_file(work.path().join("app/src/left.jk")).unwrap();
    work.write(
        "right/joky.toml",
        "[package]\nname='shared'\nversion='0.1.0'\nsource-root='src'",
    );
    failure(
        &work.cli(&["check", "app"]),
        "multiple versions are not supported",
    );
}

#[test]
#[cfg(unix)]
fn package_paths_bound_sources_and_protect_dependency_build_inputs() {
    use std::os::unix::fs::symlink;
    let work = Workspace::new();
    fixture(&work);
    // Even an unused dependency directory cannot be replaced by build output.
    work.write(
        "unused/joky.toml",
        "[package]\nname='unused'\nsource-root='src'",
    );
    work.write("unused/src/value.jk", "pub fn value() {}");
    work.write(
        "app/joky.toml",
        &format!("{APP}unused={{path='../unused'}}\n"),
    );
    failure(
        &work.cli(&["build", "app", "-o", "unused"]),
        "overlaps a dependency package",
    );
    work.write("app/joky.toml", APP);
    failure(
        &work.cli(&["build", "app", "-o", "shared"]),
        "overlaps a dependency package",
    );
    assert!(work.path().join("shared/joky.toml").is_file());
    symlink(work.path().join("shared"), work.path().join("shared-link")).unwrap();
    work.write("right/joky.toml", "[package]\nname='right'\nsource-root='src'\n[dependencies]\nstorage={path='../shared-link'}");
    success(&work.cli(&["check", "app"]));
    symlink(
        work.path().join("app/src/helper.jk"),
        work.path().join("left/src/outside.jk"),
    )
    .unwrap();
    work.write(
        "left/src/api.jk",
        "import outside pub fn make() -> Int32 { outside.number() }",
    );
    failure(
        &work.cli(&["check", "app"]),
        "outside its package source root",
    );
    work.write(
        "left/joky.toml",
        &LEFT.replace("source-root='src'", "source-root='../app/src'"),
    );
    failure(
        &work.cli(&["check", "app"]),
        "relative to the project directory",
    );
    work.write(
        "left/joky.toml",
        &LEFT.replace("source-root='src'", "source-root='.'"),
    );
    work.write(
        "left/nested/joky.toml",
        "[package]\nname='nested'\nsource-root='src'",
    );
    work.write("left/nested/src/value.jk", "pub fn value() {}");
    work.write(
        "app/joky.toml",
        &format!("{APP}nested={{path='../left/nested'}}\n"),
    );
    failure(&work.cli(&["check", "app"]), "Package source roots overlap");
}
