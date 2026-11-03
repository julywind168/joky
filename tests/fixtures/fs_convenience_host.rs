use joky::runtime::fs::{FileHandle, FileLimits, FileService, FileSystem, FsError};
use std::sync::{Arc, Mutex};
#[derive(Default)]
pub struct Host {
    completed: Mutex<Vec<String>>,
}
struct File {
    host: Arc<Host>,
    path: String,
    bytes: Vec<u8>,
    position: usize,
    calls: usize,
}
struct Backend(Arc<Host>);
fn large() -> Vec<u8> {
    (0..1048583).map(|i| (i % 256) as u8).collect()
}
impl FileSystem for Backend {
    fn open(&self, path: &str, _: u64) -> Result<Box<dyn FileHandle>, FsError> {
        let bytes = match path {
            "binary" => vec![0, 255, 42],
            "short_read" => "你好🙂\0\r\n".as_bytes().to_vec(),
            "cross_block" => [vec![b'a'; 65535], "你好".as_bytes().to_vec()].concat(),
            "invalid" => vec![b'x', 255],
            "truncated" => vec![b'x', 0xe2, 0x82],
            "read_error" => vec![255],
            "large" => large(),
            _ => vec![],
        };
        Ok(Box::new(File {
            host: self.0.clone(),
            path: path.into(),
            bytes,
            position: 0,
            calls: 0,
        }))
    }
}
impl FileHandle for File {
    fn read(&mut self, bytes: &mut [u8]) -> Result<usize, FsError> {
        assert!((1..=65536).contains(&bytes.len()));
        self.calls += 1;
        if self.path == "read_error" && self.calls == 2 {
            return Err(FsError::new(10, "Injected read failure"));
        }
        let maximum = if self.path == "short_read" {
            1
        } else {
            bytes.len()
        };
        let size = maximum.min(self.bytes.len().saturating_sub(self.position));
        bytes[..size].copy_from_slice(&self.bytes[self.position..self.position + size]);
        self.position += size;
        Ok(size)
    }
    fn write(&mut self, bytes: &[u8]) -> Result<usize, FsError> {
        assert!(bytes.len() <= 65536);
        self.calls += 1;
        if self.calls == 2 {
            match self.path.as_str() {
                "write_zero" => return Ok(0),
                "write_busy" => return Err(FsError::new(6, "Injected busy result")),
                "write_error" => {
                    self.bytes.push(bytes[0]);
                    self.position += 1;
                    return Err(FsError::new(10, "Injected failure after external progress"));
                }
                _ => {}
            }
        }
        let size = if (self.path == "write_partial" && self.calls == 1)
            || matches!(
                self.path.as_str(),
                "write_short" | "write_zero" | "write_busy" | "write_error"
            ) {
            bytes.len().min(3)
        } else {
            bytes.len()
        };
        self.bytes.extend_from_slice(&bytes[..size]);
        self.position += size;
        Ok(size)
    }
    fn seek(&mut self, origin: u32, offset: u64) -> Result<u64, FsError> {
        self.position = match origin {
            0 => offset as usize,
            1 => (self.position as i64 + offset as i64) as usize,
            _ => unreachable!(),
        };
        Ok(self.position as u64)
    }
    fn sync_all(&mut self) -> Result<(), FsError> {
        panic!("Convenience writes must not sync")
    }
    fn close(self: Box<Self>) -> Result<(), FsError> {
        match self.path.as_str() {
            "write_short" => {
                assert_eq!(self.bytes, "你好🙂\0\r\n".as_bytes());
                assert_eq!(self.calls, 5);
            }
            "write_partial" => {
                assert_eq!(self.bytes, [0, 255, 42, 0, 1, 2]);
                assert_eq!(self.calls, 2);
            }
            "write_large" => {
                assert_eq!(self.bytes, large());
                assert_eq!(self.calls, 17);
            }
            "write_empty" => {
                assert!(self.bytes.is_empty());
                assert_eq!(self.calls, 0);
            }
            "write_zero" | "write_busy" => {
                assert_eq!(self.bytes, "你".as_bytes());
                assert_eq!(self.calls, 2);
            }
            "write_error" => {
                assert_eq!(self.bytes, &"你好".as_bytes()[..4]);
                assert_eq!(self.calls, 2);
            }
            "large" => assert_eq!(self.calls, 18),
            _ => {}
        }
        self.host.completed.lock().unwrap().push(self.path.clone());
        Ok(())
    }
}
pub fn suite(run: impl FnOnce(FileService) -> u64) {
    let host = Arc::new(Host::default());
    let service = FileService::new(Arc::new(Backend(host.clone())), FileLimits::default()).unwrap();
    assert_eq!(run(service.clone()), 42);
    service.shutdown();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !service.is_quiescent() {
        assert!(
            std::time::Instant::now() < deadline,
            "file service did not drain"
        );
        std::thread::yield_now();
    }
    assert_eq!(service.usage(), Some((0, 0, 0)));
    assert_eq!(host.completed.lock().unwrap().len(), 26);
}
