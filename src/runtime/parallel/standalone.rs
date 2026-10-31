//! Blocking driver for compiler-authored, statically linked scalar roots.
use super::{pool, ExecutionMode, ParallelExecutor, Storage, TaskBudget};
use crate::{
    runtime::{
        cps::{CpsEntry, CpsFrame},
        error,
    },
    Error, ErrorCode, ErrorPhase,
};
use std::{sync::Arc, task::Poll};
#[repr(C)]
pub struct ExecutionConfigV1 {
    pub version: u32,
    pub size: u32,
    pub mode: u32,
    pub workers: u32,
    pub max_tasks: usize,
}
#[repr(C)]
pub struct TaskMainV1 {
    pub version: u32,
    pub size: u32,
    pub entry: Option<CpsEntry>,
    pub frame_slots: usize,
    pub result_kind: u32,
    pub reserved: u32,
}
fn invalid() -> Error {
    Error::new_with_code(
        ErrorPhase::Abi,
        ErrorCode::AbiError,
        "Invalid task execution configuration or compiler entry",
    )
}
impl ExecutionConfigV1 {
    pub fn mode(&self) -> Result<ExecutionMode, Error> {
        if self.version != 1
            || self.size as usize != std::mem::size_of::<Self>()
            || self.max_tasks == 0
        {
            return Err(invalid());
        }
        match (self.mode, self.workers) {
            (0, 0) => Ok(ExecutionMode::Local),
            (1, workers) => ExecutionMode::Parallel {
                workers: workers as usize,
            }
            .validate(),
            _ => Err(invalid()),
        }
    }
}
struct MainJob {
    frame: Box<CpsFrame>,
    entry: CpsEntry,
}
impl pool::Job for MainJob {
    fn poll(&mut self) -> Poll<Result<pool::Output, Error>> {
        match error::checked(|| unsafe { (self.entry)(&mut *self.frame) }) {
            Ok(0) => Poll::Ready(Ok(Box::new(self.frame.result))),
            Ok(1) => {
                pool::wait_plan(self.frame.wait_plan());
                Poll::Pending
            }
            Ok(_) => Poll::Ready(Err(invalid())),
            Err(error) => Poll::Ready(Err(error)),
        }
    }
}
/// # Safety
/// Pointers are valid for this call; the entry and full call graph match this
/// compiler-authored descriptor and runtime ABI. Executable code remains live.
#[no_mangle]
pub unsafe extern "C" fn __joky_task_main_v1(
    config: *const ExecutionConfigV1,
    descriptor: *const TaskMainV1,
    output: *mut u64,
) -> i32 {
    let run = || -> Result<u64, Error> {
        if pool::active() || output.is_null() {
            return Err(invalid());
        }
        let config = config.as_ref().ok_or_else(invalid)?;
        let descriptor = descriptor.as_ref().ok_or_else(invalid)?;
        let mode = config.mode()?;
        if descriptor.version != 1
            || descriptor.size as usize != std::mem::size_of::<TaskMainV1>()
            || descriptor.reserved != 0
            || descriptor.result_kind > 1
        {
            return Err(invalid());
        }
        let entry = descriptor.entry.ok_or_else(invalid)?;
        match mode {
            ExecutionMode::Local => {
                let mut frame = CpsFrame::try_new(descriptor.frame_slots)?;
                error::checked(|| crate::runtime::fs::joky_fs_block_on_native(entry, &mut frame))?
                    .eq(&0)
                    .then_some(frame.result)
                    .ok_or_else(invalid)
            }
            ExecutionMode::Parallel { workers } => {
                crate::runtime::dns::initialize()?;
                let pool = ParallelExecutor::new(workers, config.max_tasks)?;
                let slots = descriptor.frame_slots;
                let root = Storage::new(vec![TaskBudget::new(crate::runtime::arena::MAX_BYTES)])?;
                let task = pool.submit(
                    root,
                    Arc::new(super::control::Files(Some(
                        crate::runtime::fs::FileService::standalone()?,
                    ))),
                    Box::new(move || {
                        Ok(Box::new(MainJob {
                            frame: Box::new(CpsFrame::try_new(slots)?),
                            entry,
                        }))
                    }),
                )?;
                task.wait()?;
                let result = task.take().ok_or_else(invalid)??;
                result.downcast::<u64>().map(|v| *v).map_err(|_| invalid())
            }
        }
    };
    match run() {
        Ok(value) => {
            output.write(value);
            0
        }
        Err(e) => {
            error::record_error(e);
            -1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn task_pool_standalone_rejects_bad_config_before_entry() {
        unsafe extern "C" fn unreachable_entry(_: *mut CpsFrame) -> i32 {
            panic!("invalid config executed")
        }
        let mut config = ExecutionConfigV1 {
            version: 1,
            size: std::mem::size_of::<ExecutionConfigV1>() as u32,
            mode: 1,
            workers: 1,
            max_tasks: 8,
        };
        let mut descriptor = TaskMainV1 {
            version: 1,
            size: std::mem::size_of::<TaskMainV1>() as u32,
            entry: Some(unreachable_entry),
            frame_slots: 0,
            result_kind: 0,
            reserved: 0,
        };
        let mut value = 99;
        for case in 0..5 {
            match case {
                0 => config.version = 2,
                1 => {
                    config.version = 1;
                    config.workers = 0;
                }
                2 => {
                    config.workers = 1;
                    descriptor.version = 2;
                }
                3 => {
                    descriptor.version = 1;
                    descriptor.result_kind = 2;
                }
                _ => {
                    descriptor.result_kind = 0;
                    descriptor.reserved = 1;
                }
            }
            assert!(error::checked(|| unsafe {
                __joky_task_main_v1(&config, &descriptor, &mut value)
            })
            .is_err());
            assert_eq!(value, 99);
        }
    }
}
