//! Build-time source identity. Git is optional and is never needed by the CLI.
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

// Only inputs that can affect the toolchain belong to source-state. Documentation
// changes do not make the compiler binary's source state modified.
pub const SOURCE_INPUTS: &[&str] = &[
    "src",
    "tools",
    "include",
    "sites",
    "build_support",
    "build.rs",
    "Cargo.toml",
    "Cargo.lock",
];

pub struct Source {
    pub commit: String,
    pub state: &'static str,
    pub watch: Vec<PathBuf>,
}

fn git(root: &Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).ok())
        .flatten()
}

pub fn source(root: &Path) -> Source {
    let mut result = Source {
        commit: "unknown".into(),
        state: "unknown",
        watch: Vec::new(),
    };
    // A source archive unpacked inside another repository must not inherit that
    // unrelated repository's revision.
    let same_root = git(root, &["rev-parse", "--show-toplevel"])
        .and_then(|path| Path::new(path.trim()).canonicalize().ok())
        .zip(root.canonicalize().ok())
        .is_some_and(|(git, source)| git == source);
    if !same_root {
        return result;
    }
    if let Some(commit) = git(root, &["rev-parse", "--verify", "HEAD"]) {
        result.commit = commit.trim().to_owned();
    }
    let mut arguments = vec!["status", "--porcelain", "--untracked-files=normal", "--"];
    arguments.extend_from_slice(SOURCE_INPUTS);
    if let Some(status) = git(root, &arguments) {
        result.state = if status.trim().is_empty() {
            "clean"
        } else {
            "modified"
        };
    }
    // --git-path handles both ordinary clones and linked worktrees. Watch refs
    // as well as HEAD so a new commit refreshes metadata without a source edit.
    for name in ["HEAD", "index", "packed-refs", "refs"] {
        if let Some(path) = git(root, &["rev-parse", "--git-path", name]) {
            let path = root.join(path.trim());
            if path.exists() {
                result.watch.push(path);
            }
        }
    }
    result
}
