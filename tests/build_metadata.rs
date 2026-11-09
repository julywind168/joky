//! Source identity must survive archives and Git worktrees without runtime Git.
#[path = "support/scratch.rs"]
mod scratch;
#[path = "../build_support/version.rs"]
mod version;

use scratch::ScratchDir;
use std::{fs, path::Path, process::Command};

fn git(root: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=Joky Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-C",
        ])
        .arg(root)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn metadata_tracks_commits_and_source_changes_in_clones_and_worktrees() {
    let work = ScratchDir::new("build-metadata");
    let root = work.path();
    git(root, &["init", "-q"]);
    fs::create_dir(root.join("src")).unwrap();
    fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "Initial source"]);
    let first = version::source(root);
    assert_eq!(first.commit.len(), 40);
    assert_eq!(first.state, "clean");
    assert!(!first.watch.is_empty());
    assert!(first.watch.iter().all(|path| path.exists()));
    fs::write(root.join("README.md"), "Documentation only").unwrap();
    assert_eq!(version::source(root).state, "clean");
    fs::write(
        root.join("src/main.rs"),
        "fn main() { println!(\"changed\"); }\n",
    )
    .unwrap();
    assert_eq!(version::source(root).state, "modified");
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "Update source"]);
    let next = version::source(root);
    assert_ne!(next.commit, first.commit);
    assert_eq!(next.state, "clean");
    let linked = ScratchDir::new("build-worktree");
    git(
        root,
        &[
            "worktree",
            "add",
            "--detach",
            linked.path().to_str().unwrap(),
            "HEAD",
        ],
    );
    let metadata = version::source(linked.path());
    assert_eq!(metadata.commit, next.commit);
    assert_eq!(metadata.state, "clean");
    assert!(!metadata.watch.is_empty());
    assert!(metadata.watch.iter().all(|path| path.exists()));
}

#[test]
fn source_archives_do_not_inherit_a_parent_repository_revision() {
    let work = ScratchDir::new("build-archive");
    let metadata = version::source(work.path());
    assert_eq!(metadata.commit, "unknown");
    assert_eq!(metadata.state, "unknown");
    assert!(metadata.watch.is_empty());
    git(work.path(), &["init", "-q"]);
    fs::write(work.path().join("README.md"), "Outer repository").unwrap();
    git(work.path(), &["add", "."]);
    git(work.path(), &["commit", "-qm", "Outer commit"]);
    let archive = work.path().join("archive");
    fs::create_dir(&archive).unwrap();
    let metadata = version::source(&archive);
    assert_eq!(metadata.commit, "unknown");
    assert_eq!(metadata.state, "unknown");
    assert!(metadata.watch.is_empty());
}
