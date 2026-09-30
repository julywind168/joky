//! Locate the Cargo-built static runtime shared by native test subprocesses.
use std::path::PathBuf;

pub fn runtime_archive() -> PathBuf {
    // Cargo emits the current library's static archive beside the test binary,
    // including on a clean `cargo test` without a preceding `cargo build`.
    // Artifacts stay fixed during this test process. Avoid rescanning a large
    // deps directory for every CLI subprocess and every concurrent test.
    static ARCHIVE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ARCHIVE
        .get_or_init(|| {
            let deps = std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .to_owned();
            std::fs::read_dir(deps)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| {
                    path.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("libjoky-")
                        && path.extension().is_some_and(|ext| ext == "a")
                })
                .max_by_key(|path| path.metadata().unwrap().modified().unwrap())
                .expect("Cargo must build the runtime static archive")
        })
        .clone()
}
