//! Preserve macOS debug information before temporary object files are deleted.

use std::io;
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
fn bundle_path(output: &Path) -> PathBuf {
    let mut path = output.as_os_str().to_owned();
    path.push(".dSYM");
    PathBuf::from(path)
}

pub(crate) fn invalidate(output: &Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    match std::fs::remove_dir_all(bundle_path(output)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        result => result?,
    }
    let _ = output;
    Ok(())
}

pub(crate) fn publish(output: &Path) -> io::Result<Option<PathBuf>> {
    #[cfg(target_os = "macos")]
    {
        let bundle = bundle_path(output);
        let mut temporary = bundle.as_os_str().to_owned();
        temporary.push(format!(".{}.tmp", std::process::id()));
        let temporary = PathBuf::from(temporary);
        std::fs::create_dir(&temporary)?;
        let result = (|| {
            let result = std::process::Command::new("dsymutil")
                .arg(output)
                .arg("-o")
                .arg(&temporary)
                .output()?;
            if !result.status.success() {
                return Err(io::Error::other(
                    String::from_utf8_lossy(&result.stderr).into_owned(),
                ));
            }
            let file = Path::new("Contents/Resources/DWARF").join(
                output
                    .file_name()
                    .ok_or_else(|| io::Error::other("output has no filename"))?,
            );
            if !temporary.join(&file).is_file() {
                return Err(io::Error::other("dsymutil produced no DWARF file"));
            }
            std::fs::rename(&temporary, &bundle)?;
            Ok(Some(bundle.join(file)))
        })();
        if result.is_err() {
            let _ = std::fs::remove_dir_all(&temporary);
        }
        result
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = output;
        Ok(None)
    }
}
