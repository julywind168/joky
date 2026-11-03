//! Explicit test inputs and output capture, separate from production process snapshots.
use super::{worker::Native, FileHandle, FileSystem, FsError};
use crate::runtime::process::{ProcessContext, Value};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
};

pub(super) struct Inputs(Mutex<State>);
struct State {
    output: [CapturedOutput; 2],
    arguments: Vec<OsString>,
    environment: BTreeMap<OsString, OsString>,
    directory: PathBuf,
    snapshot: Option<ProcessContext>,
    stdin: Vec<u8>,
    offset: usize,
    reader: bool,
}
impl Inputs {
    pub(super) fn new(directory: PathBuf) -> Self {
        Self(Mutex::new(State {
            output: Default::default(),
            arguments: Vec::new(),
            environment: BTreeMap::new(),
            directory,
            snapshot: None,
            stdin: Vec::new(),
            offset: 0,
            reader: false,
        }))
    }
    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|error| error.into_inner())
    }
    pub(super) fn write_output(
        &self,
        stderr: bool,
        text: &str,
        emit: impl FnOnce() -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        // Serialize the tee and snapshot together, including parallel writers.
        let mut state = self.lock();
        if let Err(error) = emit() {
            // write_all can fail after partial progress. Never publish a
            // supposedly complete snapshot after such a failure.
            state.output[usize::from(stderr)].failed = true;
            return Err(error);
        }
        state.output[usize::from(stderr)].append(text);
        Ok(())
    }
    pub(super) fn read(&self, kind: u64, index: u64, key: &str) -> Result<Value, FsError> {
        let mut state = self.lock();
        state.freeze()?.read(kind, index, key)
    }
    pub(super) fn configure(
        &self,
        kind: u64,
        key: &str,
        value: &str,
    ) -> Result<Option<String>, FsError> {
        let mut state = self.lock();
        if matches!(kind, 5 | 6) {
            return state.output[(kind - 5) as usize].snapshot().map(Some);
        }
        // Inspecting the test directory is setup, not an application input read.
        if kind == 4 {
            return state
                .directory
                .to_str()
                .map(|s| Some(s.to_owned()))
                .ok_or_else(|| FsError::new(5, "Test directory is not valid UTF-8"));
        }
        if state.snapshot.is_some() {
            return Err(FsError::new(
                6,
                "Test inputs are frozen after the first process or stdin read",
            ));
        }
        if value.len() > 4 * 1024 * 1024 || key.len() > 4 * 1024 * 1024 {
            return Err(FsError::new(8, "Test input exceeds 4 MiB"));
        }
        match kind {
            0 => {
                // Each argument ends in NUL; the empty packet represents zero arguments.
                let mut args = Vec::new();
                if !value.is_empty() {
                    let text = value
                        .strip_suffix('\0')
                        .ok_or_else(|| FsError::new(5, "Invalid test argument packet"))?;
                    for arg in text.split('\0') {
                        if args.len() == 65536 {
                            return Err(FsError::new(8, "Too many test arguments"));
                        }
                        args.push(OsString::from(arg));
                    }
                }
                ProcessContext::new(
                    args.clone(),
                    state.environment.clone(),
                    state.directory.clone(),
                )?;
                state.arguments = args;
            }
            1 | 2 => {
                if key.is_empty() || key.contains(['=', '\0']) || value.contains('\0') {
                    return Err(FsError::new(5, "Invalid test environment name or value"));
                }
                let mut environment = state.environment.clone();
                if kind == 1 {
                    environment.insert(key.into(), value.into());
                } else {
                    environment.remove(std::ffi::OsStr::new(key));
                }
                ProcessContext::new(
                    state.arguments.clone(),
                    environment.clone(),
                    state.directory.clone(),
                )?;
                state.environment = environment;
            }
            3 => {
                if value.len() > 1024 * 1024 {
                    return Err(FsError::new(8, "Test stdin exceeds 1 MiB"));
                }
                state.stdin = value.as_bytes().to_vec();
                state.offset = 0;
            }
            _ => return Err(FsError::new(5, "Unknown test input operation")),
        }
        Ok(None)
    }
}

#[derive(Default)]
struct CapturedOutput {
    text: String,
    overflow: bool,
    failed: bool,
}
impl CapturedOutput {
    const LIMIT: usize = 1024 * 1024;

    fn append(&mut self, text: &str) {
        if self.overflow || self.failed {
            return;
        }
        if text.len() > Self::LIMIT - self.text.len() {
            self.overflow = true;
            self.text.clear();
        } else {
            self.text.push_str(text);
        }
    }

    fn snapshot(&self) -> Result<String, FsError> {
        if self.failed {
            Err(FsError::new(
                10,
                "Test output is incomplete after a write failure",
            ))
        } else if self.overflow {
            Err(FsError::new(8, "Test output exceeds 1 MiB"))
        } else {
            Ok(self.text.clone())
        }
    }
}
impl State {
    fn freeze(&mut self) -> Result<&ProcessContext, FsError> {
        if self.snapshot.is_none() {
            self.snapshot = Some(ProcessContext::new(
                std::mem::take(&mut self.arguments),
                std::mem::take(&mut self.environment),
                self.directory.clone(),
            )?);
        }
        Ok(self.snapshot.as_ref().unwrap())
    }
}

pub(super) struct Backend {
    pub native: Native,
    pub inputs: Arc<Inputs>,
}
impl FileSystem for Backend {
    fn open(&self, path: &str, flags: u64) -> Result<Box<dyn FileHandle>, FsError> {
        self.native.open(path, flags)
    }
    fn rename(&self, from: &str, to: &str) -> Result<(), FsError> {
        self.native.rename(from, to)
    }
    fn remove_file(&self, path: &str) -> Result<(), FsError> {
        self.native.remove_file(path)
    }
    fn read_dir(&self, path: &str) -> Result<super::DirectoryNames<'_>, FsError> {
        self.native.read_dir(path)
    }
    fn stdin(&self) -> Result<Box<dyn FileHandle>, FsError> {
        let mut state = self.inputs.lock();
        state.freeze()?;
        if state.reader {
            return Err(FsError::new(6, "Standard input already has a reader"));
        }
        state.reader = true;
        Ok(Box::new(Input(self.inputs.clone())))
    }
}
struct Input(Arc<Inputs>);
impl Drop for Input {
    fn drop(&mut self) {
        self.0.lock().reader = false;
    }
}
impl FileHandle for Input {
    fn read(&mut self, bytes: &mut [u8]) -> Result<usize, FsError> {
        let mut state = self.0.lock();
        let count = bytes.len().min(state.stdin.len() - state.offset);
        bytes[..count].copy_from_slice(&state.stdin[state.offset..state.offset + count]);
        state.offset += count;
        Ok(count)
    }
    fn write(&mut self, _: &[u8]) -> Result<usize, FsError> {
        Err(FsError::new(1, "Standard input is read-only"))
    }
    fn seek(&mut self, _: u32, _: u64) -> Result<u64, FsError> {
        Err(FsError::new(1, "Standard input cannot seek"))
    }
    fn sync_all(&mut self) -> Result<(), FsError> {
        Ok(())
    }
    fn close(self: Box<Self>) -> Result<(), FsError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_output_bounds_snapshots_and_write_failures_are_independent() {
        let inputs = Inputs::new(std::env::current_dir().unwrap());
        inputs.write_output(false, "中\0🙂", || Ok(())).unwrap();
        let first = inputs.configure(5, "", "").unwrap().unwrap();
        inputs.configure(3, "", "still configurable").unwrap();
        inputs.write_output(false, "\n", || Ok(())).unwrap();
        assert_eq!(first, "中\0🙂");
        assert_eq!(inputs.configure(5, "", "").unwrap().unwrap(), "中\0🙂\n");
        inputs.read(0, 0, "").unwrap();
        inputs
            .write_output(true, &"x".repeat(CapturedOutput::LIMIT), || Ok(()))
            .unwrap();
        assert_eq!(
            inputs.configure(6, "", "").unwrap().unwrap().len(),
            CapturedOutput::LIMIT
        );
        inputs.write_output(true, "", || Ok(())).unwrap();
        inputs.write_output(true, "🙂", || Ok(())).unwrap();
        for _ in 0..2 {
            assert_eq!(inputs.configure(6, "", "").unwrap_err().code(), 8);
        }
        assert_eq!(inputs.configure(5, "", "").unwrap().unwrap(), "中\0🙂\n");
        assert!(inputs
            .write_output(false, "lost", || Err(std::io::ErrorKind::BrokenPipe.into()))
            .is_err());
        assert_eq!(inputs.configure(5, "", "").unwrap_err().code(), 10);
    }

    #[test]
    fn test_input_limits_preserve_the_previous_configuration() {
        let inputs = Arc::new(Inputs::new(std::env::current_dir().unwrap()));
        inputs.configure(0, "", "original\0").unwrap();
        inputs.configure(1, "VALUE", "original").unwrap();
        inputs.configure(3, "", "original").unwrap();
        for (kind, key, value, code) in [
            (0, "", "x".repeat(4 * 1024 * 1024 + 1), 8),
            (0, "", "\0".repeat(65537), 8),
            (0, "", "missing terminator".into(), 5),
            (1, "VALUE", "x".repeat(4 * 1024 * 1024), 8),
            (1, "", "bad".into(), 5),
            (1, "VALUE", "bad\0value".into(), 5),
            (3, "", "x".repeat(1024 * 1024 + 1), 8),
            (99, "", "".into(), 5),
        ] {
            assert_eq!(
                inputs.configure(kind, key, &value).unwrap_err().code(),
                code
            );
        }
        assert!(matches!(inputs.read(1, 0, ""), Ok(Value::Text(s)) if s == "original"));
        assert!(matches!(inputs.read(2, 0, "VALUE"), Ok(Value::Text(s)) if s == "original"));
        assert_eq!(inputs.lock().stdin, b"original");
        assert_eq!(inputs.configure(1, "VALUE", "late").unwrap_err().code(), 6);
    }

    #[test]
    fn test_context_can_be_replaced_with_an_immutable_process_snapshot() {
        let directory = std::env::current_dir().unwrap();
        let service = super::super::FileService::testing(&directory, Default::default()).unwrap();
        let snapshot = service.clone().with_process_context(
            ProcessContext::new(
                vec![],
                BTreeMap::from([("VALUE".into(), "snapshot".into())]),
                directory,
            )
            .unwrap(),
        );
        service.test_input(1, "VALUE", "test").unwrap();
        assert!(matches!(service.process_read(2, 0, "VALUE"), Ok(Value::Text(s)) if s == "test"));
        assert!(
            matches!(snapshot.process_read(2, 0, "VALUE"), Ok(Value::Text(s)) if s == "snapshot")
        );
        assert_eq!(
            snapshot
                .test_input(1, "VALUE", "ignored")
                .unwrap_err()
                .code(),
            1
        );
    }

    #[test]
    fn test_stdin_is_bounded_exclusive_and_does_not_rewind_after_drop() {
        let inputs = Arc::new(Inputs::new(std::env::current_dir().unwrap()));
        inputs.configure(0, "", "discard\0").unwrap();
        inputs.configure(0, "", "").unwrap();
        inputs.configure(3, "", &"x".repeat(1024 * 1024)).unwrap();
        let backend = Backend {
            native: Native(std::env::current_dir().unwrap(), false),
            inputs: inputs.clone(),
        };
        let mut first = backend.stdin().unwrap();
        assert!(matches!(inputs.read(0, 0, ""), Ok(Value::Count(0))));
        assert_eq!(backend.stdin().err().unwrap().code(), 6);
        assert_eq!(first.write(b"x").unwrap_err().code(), 1);
        assert_eq!(first.seek(0, 0).unwrap_err().code(), 1);
        let mut bytes = [0; 3];
        assert_eq!(first.read(&mut bytes).unwrap(), 3);
        drop(first);
        let mut second = backend.stdin().unwrap();
        let mut bytes = vec![0; 1024 * 1024];
        assert_eq!(second.read(&mut bytes).unwrap(), 1024 * 1024 - 3);
        assert_eq!(second.read(&mut bytes).unwrap(), 0);
        second.close().unwrap();
        assert!(!inputs.lock().reader);
    }
}
