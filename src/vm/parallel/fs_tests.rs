use super::*;
use crate::backends::aot::tests::ScratchDir;
use crate::runtime::{
    fs::{FileHandle, FileService, FileSystem, FsError},
    parallel::{scoped, ParallelExecutor},
};
use crate::vm::{VmEngine, VmMode};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Condvar, Mutex,
    },
    time::{Duration, Instant},
};
const SOURCE: &str = include_str!("../../../tests/fixtures/scoped_borrow_fs.jk");
fn wait(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ready() {
        assert!(Instant::now() < deadline, "file task did not drain");
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn dispatched(task: &ParallelTask, workers: usize) {
    let observed = scoped::observations(task.identity());
    let sender = observed
        .iter()
        .find(|o| o.source)
        .expect("missing file source");
    assert!(observed
        .iter()
        .any(|o| !o.source && (workers == 1 || o.thread != sender.thread)));
}
#[test]
fn scoped_borrow_file_calls_preserve_bytes_authority_and_dispatch() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let work = ScratchDir::new("scoped-files");
        let service = FileService::native(work.path(), Default::default()).unwrap();
        let mut vm = VmEngine::with_mode(mode);
        vm.set_file_service(service.clone()).unwrap();
        vm.load_source(SOURCE).unwrap_or_else(|e| panic!("{e}"));
        for workers in [1, 3] {
            let budget = TaskBudget::new(8 << 20);
            let mut pool = ParallelExecutor::new(workers, 32).unwrap();
            let task = vm
                .prepare_parallel_task("main", vec![], vec![budget.clone()])
                .unwrap()
                .submit(&pool)
                .unwrap();
            wait(|| task.is_finished());
            assert_eq!(
                task.take_result().unwrap().value(),
                &TaskValue::Scalar(InstanceValue::Int32(0))
            );
            dispatched(&task, workers);
            drop(task);
            pool.wait_closed();
            assert_eq!(budget.used(), 0);
            assert_eq!(
                std::fs::read(work.path().join("first.bin")).unwrap(),
                "中\0🙂".as_bytes()
            );
            assert_eq!(
                std::fs::read(work.path().join("second.bin")).unwrap(),
                "second\0文件".as_bytes()
            );
            wait(|| service.usage() == Some((0, 0, 0)));
        }
        drop(vm);
        service.shutdown();
        wait(|| service.is_quiescent());
    }
}
#[derive(Default)]
struct Gate {
    started: AtomicBool,
    released: Mutex<bool>,
    wake: Condvar,
}
struct Backend(Arc<Gate>);
struct File(Arc<Gate>);
impl FileSystem for Backend {
    fn open(&self, _: &str, _: u64) -> Result<Box<dyn FileHandle>, FsError> {
        Ok(Box::new(File(self.0.clone())))
    }
}
impl FileHandle for File {
    fn read(&mut self, bytes: &mut [u8]) -> Result<usize, FsError> {
        self.0.started.store(true, Ordering::Release);
        let (released, timeout) = self
            .0
            .wake
            .wait_timeout_while(
                self.0.released.lock().unwrap(),
                Duration::from_secs(30),
                |released| !*released,
            )
            .unwrap();
        assert!(*released && !timeout.timed_out());
        // This buffer must belong to the file service even after the scoped task drains.
        bytes.fill(42);
        Ok(bytes.len())
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
        Ok(())
    }
}
#[test]
fn scoped_borrow_file_cancel_leaves_running_io_in_service_storage() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for close in [false, true] {
            let gate = Arc::new(Gate::default());
            let service =
                FileService::new(Arc::new(Backend(gate.clone())), Default::default()).unwrap();
            let mut vm = VmEngine::with_mode(mode);
            vm.set_file_service(service.clone()).unwrap();
            vm.load_source(SOURCE).unwrap_or_else(|e| panic!("{e}"));
            let budget = TaskBudget::new(4 << 20);
            let mut pool = ParallelExecutor::new(3, 16).unwrap();
            let task = vm
                .prepare_parallel_task("parked", vec![], vec![budget.clone()])
                .unwrap()
                .submit(&pool)
                .unwrap();
            wait(|| gate.started.load(Ordering::Acquire));
            if close {
                pool.request_close();
            } else {
                task.request_cancel();
            }
            wait(|| task.is_finished());
            assert_eq!(
                task.take_result().err().unwrap().code(),
                ErrorCode::Cancelled
            );
            dispatched(&task, 3);
            drop(task);
            pool.wait_closed();
            assert_eq!(budget.used(), 0);
            assert!(service.usage().is_some_and(|(_, requests, _)| requests > 0));
            *gate.released.lock().unwrap() = true;
            gate.wake.notify_all();
            drop(vm);
            service.shutdown();
            wait(|| service.is_quiescent());
            assert_eq!(service.usage(), Some((0, 0, 0)));
        }
    }
}
