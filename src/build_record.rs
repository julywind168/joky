//! Provenance for a completed native build, not a hermetic build recipe.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Serialize)]
pub(crate) struct FileRecord {
    path: PathBuf,
    sha256: String,
    size_bytes: u64,
}

impl FileRecord {
    pub(crate) fn read(path: &Path) -> io::Result<Self> {
        let path = path.canonicalize()?;
        let mut file = File::open(&path)?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        let mut size_bytes = 0;
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
            size_bytes += count as u64;
        }
        Ok(Self {
            path,
            sha256: hex::encode(hasher.finalize()),
            size_bytes,
        })
    }

    fn verify_unchanged(&self) -> io::Result<()> {
        if Self::read(&self.path)?.sha256 != self.sha256 {
            return Err(io::Error::other(format!(
                "build input changed during linking: {}",
                self.path.display()
            )));
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct CompilerRecord {
    version: &'static str,
    build_id: &'static str,
    executable: FileRecord,
}

#[derive(Serialize)]
struct Options {
    target: &'static str,
    pointer_width: u32,
    cpu: &'static str,
    opt_level: &'static str,
    runtime_profile: &'static str,
    strip: bool,
    debug_info: bool,
}

#[derive(Serialize)]
pub(crate) struct LinkerRecord {
    executable: FileRecord,
    version: String,
    command: Vec<String>,
    working_directory: PathBuf,
    environment: BTreeMap<String, String>,
}

impl LinkerRecord {
    pub(crate) fn capture(command: &Command) -> io::Result<Self> {
        let executable = FileRecord::read(Path::new(command.get_program()))?;
        let version = Command::new(command.get_program())
            .arg("--version")
            .output()?;
        if !version.status.success() {
            return Err(io::Error::other(format!(
                "linker version query failed: {}",
                String::from_utf8_lossy(&version.stderr)
            )));
        }
        let arguments = std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(|arg| {
                arg.to_str().map(str::to_owned).ok_or_else(|| {
                    io::Error::other("build record requires UTF-8 command arguments")
                })
            })
            .collect::<io::Result<Vec<_>>>()?;
        let mut environment = BTreeMap::new();
        for name in [
            "SDKROOT",
            "MACOSX_DEPLOYMENT_TARGET",
            "LIBRARY_PATH",
            "CPATH",
            "C_INCLUDE_PATH",
            "CPLUS_INCLUDE_PATH",
            "COMPILER_PATH",
            "DEVELOPER_DIR",
            "SOURCE_DATE_EPOCH",
        ] {
            if let Some(value) = std::env::var_os(name) {
                let value = value
                    .into_string()
                    .map_err(|_| io::Error::other(format!("{name} is not UTF-8")))?;
                environment.insert(name.into(), value);
            }
        }
        Ok(Self {
            executable,
            version: String::from_utf8(version.stdout)
                .map_err(io::Error::other)?
                .trim()
                .into(),
            command: arguments,
            working_directory: std::env::current_dir()?,
            environment,
        })
    }
}

#[derive(Serialize)]
pub(crate) struct BuildRecord {
    schema_version: u32,
    compiler: CompilerRecord,
    options: Options,
    pub(crate) sources: Vec<joky::AotSourceRecord>,
    runtime_abi_version: u32,
    runtime: FileRecord,
    linker: LinkerRecord,
    object_sha256: String,
    launcher_sha256: String,
    artifact: Option<FileRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) debug_artifact: Option<FileRecord>,
}

impl BuildRecord {
    pub(crate) fn new(
        build: joky::AotBuildOptions,
        strip: bool,
        runtime_profile: &'static str,
        runtime: FileRecord,
        linker: LinkerRecord,
        object: &[u8],
        launcher: &str,
    ) -> io::Result<Self> {
        Ok(Self {
            schema_version: 1,
            compiler: CompilerRecord {
                version: env!("CARGO_PKG_VERSION"),
                build_id: env!("JOKY_BUILD_ID"),
                executable: FileRecord::read(&std::env::current_exe()?)?,
            },
            options: Options {
                target: build.target.triple(),
                pointer_width: u32::from(build.target.pointer_width()),
                cpu: build.target.cpu(),
                opt_level: if build.release { "speed" } else { "none" },
                runtime_profile,
                strip,
                debug_info: build.debug_info,
            },
            sources: Vec::new(),
            runtime_abi_version: joky_runtime_abi::AOT_RUNTIME_ABI_VERSION,
            runtime,
            linker,
            object_sha256: hex::encode(Sha256::digest(object)),
            launcher_sha256: hex::encode(Sha256::digest(launcher.as_bytes())),
            artifact: None,
            debug_artifact: None,
        })
    }

    pub(crate) fn publish(mut self, output: &Path) -> io::Result<PathBuf> {
        self.runtime.verify_unchanged()?;
        self.linker.executable.verify_unchanged()?;
        self.artifact = Some(FileRecord::read(output)?);
        let path = record_path(output);
        let temporary = append_suffix(&path, &format!(".{}.tmp", std::process::id()));
        let bytes = serde_json::to_vec_pretty(&self)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        let result = (|| {
            file.write_all(&bytes)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, &path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result?;
        Ok(path)
    }
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

pub(crate) fn record_path(output: &Path) -> PathBuf {
    append_suffix(output, ".build.json")
}

pub(crate) fn invalidate(output: &Path) -> io::Result<()> {
    match fs::remove_file(record_path(output)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// Resolve once so version discovery and linking use the same driver. Retain
/// its invoked filename because cc/clang/gcc may interpret argv[0].
pub(crate) fn linker_path() -> io::Result<PathBuf> {
    let search = std::env::var_os("PATH").unwrap_or_default();
    for directory in std::env::split_paths(&search) {
        let path = std::path::absolute(directory.join("cc"))?;
        if path.is_file() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if fs::metadata(&path)?.permissions().mode() & 0o111 == 0 {
                    continue;
                }
            }
            return Ok(path);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "C linker 'cc' was not found in PATH",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_hash_uses_sha256_and_rejects_changed_input() {
        let path = std::env::temp_dir().join(format!("joky-record-hash-{}", std::process::id()));
        fs::write(&path, b"abc").unwrap();
        let record = FileRecord::read(&path).unwrap();
        assert_eq!(
            record.sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        record.verify_unchanged().unwrap();
        fs::write(&path, b"abd").unwrap();
        assert!(record
            .verify_unchanged()
            .unwrap_err()
            .to_string()
            .contains("changed during linking"));
        fs::remove_file(path).unwrap();
    }
}
