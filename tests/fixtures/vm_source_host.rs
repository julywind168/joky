use joky::runtime::fs::{FileHandle, FileLimits, FileService, FileSystem, FsError};
use std::sync::{Arc, Mutex};
const SOURCE: &str = r#"
import joky/domain
import joky/task
import joky/fs
#state struct Room { value:Int32 }
fn make(value:Int32)->Room {Room{value:value}}
impl Room {fn read(&self)->Int32 {self.value+1}}
fn answer()->Int32 {1}
fn files(_:Int32) @suspends ->Bool {
    match fs.open("old.jk",fs.OpenOptions.new()) {
        Err(e)=>match e.kind {fs.ErrorKind.Unsupported=>true,_=>false}
        Ok(f)=>{f.close().unwrap() false}
    }
}
fn hold(directory:domain.DirectoryHandle) @suspends {
    let room=domain.create(make,40)
    directory.insert(1,room.handle()).unwrap()
    task.sleep_ms(100000)
}
fn probe(directory:domain.DirectoryHandle) @suspends ->Int32 {
    match domain.lookup::<Room>(directory,1) {Some(h)=>when "read" (h) {h.value+answer()}.unwrap(),None=>-1}
}
"#;
#[derive(Default)]
struct Backend {
    closed: Arc<Mutex<Vec<String>>>,
}
struct File {
    path: String,
    bytes: Vec<u8>,
    offset: usize,
    closed: Arc<Mutex<Vec<String>>>,
}
impl FileSystem for Backend {
    fn open(&self, path: &str, flags: u64) -> Result<Box<dyn FileHandle>, FsError> {
        assert_eq!(flags, 1);
        let bytes = match path {
            "old.jk" | "close_error.jk" | "read_error.jk" => SOURCE.as_bytes().to_vec(),
            "new.jk" => SOURCE
                .replace("self.value+1", "self.value+2")
                .replace("{1}", "{2}")
                .into_bytes(),
            "schema.jk" => SOURCE
                .replace(
                    "struct Room { value:Int32 }",
                    "struct Room { value:Int32, extra:Int32 }",
                )
                .replace("Room{value:value}", "Room{value:value,extra:0}")
                .replace("{1}", "{99}")
                .into_bytes(),
            "syntax.jk" => b"fn broken(".to_vec(),
            "imports.jk" => b"import app/missing\nfn answer()->Int32{3}".to_vec(),
            "utf8.jk" => vec![0xff],
            "empty.jk" => vec![],
            _ => return Err(FsError::new(3, "Source file not found")),
        };
        Ok(Box::new(File {
            path: path.into(),
            bytes,
            offset: 0,
            closed: self.closed.clone(),
        }))
    }
}
impl FileHandle for File {
    fn read(&mut self, bytes: &mut [u8]) -> Result<usize, FsError> {
        if self.path == "read_error.jk" {
            return Err(FsError::new(10, "Injected read error"));
        }
        // Exercise complete reads across short host transfers.
        let n = bytes.len().min(17).min(self.bytes.len() - self.offset);
        bytes[..n].copy_from_slice(&self.bytes[self.offset..self.offset + n]);
        self.offset += n;
        Ok(n)
    }
    fn write(&mut self, _: &[u8]) -> Result<usize, FsError> {
        unreachable!()
    }
    fn seek(&mut self, _: u32, _: u64) -> Result<u64, FsError> {
        unreachable!()
    }
    fn sync_all(&mut self) -> Result<(), FsError> {
        unreachable!()
    }
    fn close(self: Box<Self>) -> Result<(), FsError> {
        self.closed.lock().unwrap().push(self.path.clone());
        if self.path == "close_error.jk" {
            Err(FsError::new(10, "Injected close error"))
        } else {
            Ok(())
        }
    }
}
pub fn suite(run: impl FnOnce(FileService)) {
    let closed = Arc::new(Mutex::new(Vec::new()));
    let service = FileService::new(
        Arc::new(Backend {
            closed: closed.clone(),
        }),
        FileLimits::default(),
    )
    .unwrap();
    run(service);
    let closed = closed.lock().unwrap();
    for name in [
        "old.jk",
        "new.jk",
        "schema.jk",
        "syntax.jk",
        "imports.jk",
        "utf8.jk",
        "close_error.jk",
        "read_error.jk",
        "empty.jk",
    ] {
        assert!(
            closed.iter().any(|n| n == name),
            "Missing close: {name}: {closed:?}"
        );
    }
}
