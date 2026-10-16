//! Optional engine FileAccess service. Engine objects remain on one worker.
use godot::{
    classes::{file_access::ModeFlags, FileAccess},
    global::Error as GError,
    prelude::*,
};
use joky::runtime::fs::{FileHandle, FileLimits, FileService, FileSystem, FsError};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    sync::Arc,
};
thread_local! {
    static HANDLES: RefCell<HashMap<u64,Gd<FileAccess>>> = RefCell::new(HashMap::new());
    static NEXT: Cell<u64> = const {Cell::new(1)};
    static SERVICE: RefCell<Option<FileService>> = const {RefCell::new(None)};
    static RETIRED: RefCell<Vec<FileService>> = const {RefCell::new(Vec::new())};
}
struct EngineFiles;
struct Handle {
    id: u64,
    readable: bool,
    writable: bool,
}
fn error(e: GError) -> FsError {
    FsError::new(
        match e {
            GError::ERR_FILE_NOT_FOUND | GError::ERR_FILE_BAD_PATH => 3,
            GError::ERR_FILE_NO_PERMISSION | GError::ERR_UNAUTHORIZED => 2,
            _ => 10,
        },
        format!("Godot FileAccess: {e:?}"),
    )
}
fn unsupported() -> FsError {
    FsError::new(
        1,
        "Godot FileAccess cannot preserve these open or durability semantics",
    )
}
impl FileSystem for EngineFiles {
    fn open(&self, path: &str, flags: u64) -> Result<Box<dyn FileHandle>, FsError> {
        if !path.starts_with("res://") && !path.starts_with("user://") {
            return Err(FsError::new(5, "Godot paths require res:// or user://"));
        }
        // FileAccess offers neither atomic exclusive create nor O_APPEND. Do not
        // substitute existence checks or seek-to-end for these guarantees.
        let mode = match flags {
            1 => ModeFlags::READ,
            2 | 3 => ModeFlags::READ_WRITE,
            42 | 43 => ModeFlags::WRITE_READ,
            _ => return Err(unsupported()),
        };
        if path.starts_with("res://") && flags != 1 {
            return Err(FsError::new(2, "Godot resources are read-only"));
        }
        let file =
            FileAccess::open(path, mode).ok_or_else(|| error(FileAccess::get_open_error()))?;
        let id = NEXT.with(|n| {
            let id = n.get();
            n.set(id.checked_add(1).expect("file identity exhausted"));
            id
        });
        HANDLES.with(|h| h.borrow_mut().insert(id, file));
        Ok(Box::new(Handle {
            id,
            readable: flags & 1 != 0,
            writable: flags & 2 != 0,
        }))
    }
}
impl Handle {
    fn access<T>(
        &self,
        f: impl FnOnce(&mut Gd<FileAccess>) -> Result<T, FsError>,
    ) -> Result<T, FsError> {
        HANDLES.with(|h| {
            f(h.borrow_mut()
                .get_mut(&self.id)
                .expect("file belongs to this worker"))
        })
    }
}
impl FileHandle for Handle {
    fn read(&mut self, bytes: &mut [u8]) -> Result<usize, FsError> {
        if !self.readable {
            return Err(FsError::new(2, "File is not open for reading"));
        }
        self.access(|f| {
            let data = f.get_buffer(bytes.len() as i64);
            let status = f.get_error();
            if status != GError::OK && status != GError::ERR_FILE_EOF {
                return Err(error(status));
            }
            let n = data.len();
            bytes[..n].copy_from_slice(data.as_slice());
            Ok(n)
        })
    }
    fn write(&mut self, bytes: &[u8]) -> Result<usize, FsError> {
        if !self.writable {
            return Err(FsError::new(2, "File is not open for writing"));
        }
        self.access(|f| {
            if f.store_buffer(&PackedByteArray::from(bytes)) {
                Ok(bytes.len())
            } else {
                Err(error(f.get_error()))
            }
        })
    }
    fn seek(&mut self, origin: u32, offset: u64) -> Result<u64, FsError> {
        self.access(|f| {
            let position = match origin {
                0 => Some(offset),
                1 => f.get_position().checked_add_signed(offset as i64),
                2 => f.get_length().checked_add_signed(offset as i64),
                _ => None,
            }
            .ok_or_else(|| FsError::new(5, "Invalid file position"))?;
            f.seek(position);
            if f.get_error() != GError::OK {
                return Err(error(f.get_error()));
            }
            Ok(f.get_position())
        })
    }
    // FileAccess::flush does not promise OS-level fsync durability.
    fn sync_all(&mut self) -> Result<(), FsError> {
        Err(unsupported())
    }
    fn close(self: Box<Self>) -> Result<(), FsError> {
        HANDLES.with(|h| {
            let mut file = h
                .borrow_mut()
                .remove(&self.id)
                .expect("file belongs to this worker");
            file.close();
        });
        Ok(())
    }
}
/// Only future compilation snapshots use the newly configured capability.
pub fn configure(enabled: bool) -> Result<(), joky::Error> {
    let next = if enabled {
        Some(FileService::new(
            Arc::new(EngineFiles),
            FileLimits {
                workers: 1,
                ..Default::default()
            },
        )?)
    } else {
        None
    };
    if let Some(previous) = SERVICE.with(|s| s.replace(next)) {
        RETIRED.with(|r| r.borrow_mut().push(previous));
    }
    Ok(())
}
pub fn current() -> Option<FileService> {
    SERVICE.with(|s| s.borrow().clone())
}
/// Engine teardown is an explicit drain boundary, outside task polling. Godot's
/// object API must remain alive until all worker-owned FileAccess objects close.
pub fn shutdown() {
    if let Some(service) = SERVICE.with(|s| s.take()) {
        RETIRED.with(|r| r.borrow_mut().push(service));
    }
    RETIRED.with(|r| {
        let services = r.take();
        for s in &services {
            s.shutdown();
        }
        while services.iter().any(|s| !s.is_quiescent()) {
            std::thread::yield_now();
        }
    });
}
