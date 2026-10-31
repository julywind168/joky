//! Immutable startup input, explicitly supplied by CLI or embedding hosts.
use super::fs::FsError;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::PathBuf,
    sync::{Arc, OnceLock},
};

/// Owned arguments (excluding argv[0]), environment and caller working directory.
/// Snapshots are shared read-only across tasks; no process-global mutation is exposed.
#[derive(Clone)]
pub struct ProcessContext {
    arguments: Vec<OsString>,
    environment: BTreeMap<OsString, OsString>,
    directory: PathBuf,
}

pub(crate) enum Value {
    Count(u64),
    Text(String),
}

impl ProcessContext {
    /// Capture the environment and working directory before entering a project root.
    pub fn capture(arguments: Vec<OsString>) -> Result<Self, FsError> {
        Self::new(
            arguments,
            std::env::vars_os().collect(),
            std::env::current_dir()?,
        )
    }

    /// Supply an isolated snapshot for an embedded program. Total input is capped
    /// at 4 MiB and 65536 entries; invalid Unicode is preserved until queried.
    pub fn new(
        arguments: Vec<OsString>,
        environment: BTreeMap<OsString, OsString>,
        directory: PathBuf,
    ) -> Result<Self, FsError> {
        if !directory.is_absolute() {
            return Err(FsError::new(5, "Process directory must be absolute"));
        }
        let bytes = arguments
            .iter()
            .map(|s| s.len())
            .chain(
                environment
                    .iter()
                    .map(|(k, v)| k.len().saturating_add(v.len())),
            )
            .fold(directory.as_os_str().len(), usize::saturating_add);
        if bytes > 4 * 1024 * 1024 || arguments.len().saturating_add(environment.len()) > 65536 {
            return Err(FsError::new(8, "Process snapshot exceeds its input limit"));
        }
        Ok(Self {
            arguments,
            environment,
            directory,
        })
    }

    pub(crate) fn read(&self, kind: u64, index: u64, key: &str) -> Result<Value, FsError> {
        fn text(value: &std::ffi::OsStr) -> Result<Value, FsError> {
            value
                .to_str()
                .map(|s| Value::Text(s.into()))
                .ok_or_else(|| FsError::new(5, "Process value is not valid UTF-8"))
        }
        match kind {
            0 => Ok(Value::Count(self.arguments.len() as u64)),
            1 => self
                .arguments
                .get(usize::try_from(index).unwrap_or(usize::MAX))
                .ok_or_else(|| FsError::new(3, "Argument index is out of bounds"))
                .and_then(|s| text(s)),
            2 => {
                if key.is_empty() || key.contains(['=', '\0']) {
                    return Err(FsError::new(5, "Invalid environment variable name"));
                }
                self.environment
                    .get(std::ffi::OsStr::new(key))
                    .ok_or_else(|| FsError::new(3, "Environment variable is not set"))
                    .and_then(|s| text(s))
            }
            3 => text(self.directory.as_os_str()),
            4 if !key.contains('\0') && !key.is_empty() => {
                text(self.directory.join(key).as_os_str())
            }
            _ => Err(FsError::new(5, "Invalid process query")),
        }
    }
}

// Only generated standalone startup calls this. Embedded engines receive their
// own snapshot via FileService and never implicitly inherit this global value.
static STANDALONE: OnceLock<Arc<ProcessContext>> = OnceLock::new();
pub(crate) fn standalone() -> Option<Arc<ProcessContext>> {
    STANDALONE.get().cloned()
}

/// Capture standalone arguments and environment before application chdir.
/// # Safety
/// `argv` points to `argc` valid NUL-terminated C strings for the duration of this call.
#[no_mangle]
pub unsafe extern "C" fn joky_process_init_v1(
    argc: i32,
    argv: *const *const std::ffi::c_char,
) -> i32 {
    let result = std::panic::catch_unwind(|| {
        if !(0..=65536).contains(&argc) || (argc > 0 && argv.is_null()) {
            return Err(FsError::new(5, "Invalid process arguments"));
        }
        let mut arguments = Vec::with_capacity(argc as usize);
        for index in 0..argc as usize {
            let value = *argv.add(index);
            if value.is_null() {
                return Err(FsError::new(5, "Null process argument"));
            }
            let bytes = std::ffi::CStr::from_ptr(value).to_bytes();
            #[cfg(unix)]
            let argument = {
                use std::os::unix::ffi::OsStringExt;
                OsString::from_vec(bytes.to_vec())
            };
            #[cfg(not(unix))]
            let argument = OsString::from(
                std::str::from_utf8(bytes)
                    .map_err(|_| FsError::new(5, "Process argument is not valid UTF-8"))?,
            );
            arguments.push(argument);
        }
        STANDALONE
            .set(Arc::new(ProcessContext::capture(arguments)?))
            .map_err(|_| FsError::new(5, "Process context is already initialized"))
    })
    .unwrap_or_else(|_| Err(FsError::new(10, "Process initialization panicked")));
    match result {
        Ok(()) => 0,
        Err(error) => {
            super::error::record_error(crate::Error::new_with_code(
                crate::ErrorPhase::Runtime,
                crate::ErrorCode::HostError,
                error.message(),
            ));
            -1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn process_snapshot_preserves_missing_empty_and_invalid_unicode() {
        let mut environment = BTreeMap::from([(OsString::from("EMPTY"), OsString::new())]);
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            environment.insert("INVALID".into(), OsString::from_vec(vec![255]));
        }
        let context = ProcessContext::new(
            vec!["".into(), "two words".into()],
            environment,
            std::env::current_dir().unwrap(),
        )
        .unwrap();
        assert!(matches!(context.read(0, 0, ""), Ok(Value::Count(2))));
        assert!(matches!(context.read(1, 0, ""), Ok(Value::Text(value)) if value.is_empty()));
        assert!(matches!(context.read(2, 0, "EMPTY"), Ok(Value::Text(value)) if value.is_empty()));
        assert_eq!(context.read(2, 0, "MISSING").err().unwrap().code(), 3);
        assert_eq!(context.read(2, 0, "bad=name").err().unwrap().code(), 5);
        #[cfg(unix)]
        assert_eq!(context.read(2, 0, "INVALID").err().unwrap().code(), 5);
        assert_eq!(context.read(1, u64::MAX, "").err().unwrap().code(), 3);
        assert!(ProcessContext::new(vec![], BTreeMap::new(), "relative".into()).is_err());
    }
}
