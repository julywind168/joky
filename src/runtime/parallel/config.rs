//! Execution policy is fixed at construction; Local never starts task workers.
use super::ParallelExecutor;
use crate::{runtime::executor::HostExecutor, Error, ErrorCode, ErrorPhase};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExecutionMode {
    #[default]
    Local,
    Parallel {
        workers: usize,
    },
}
impl ExecutionMode {
    pub fn validate(self) -> Result<Self, Error> {
        if matches!(self, Self::Parallel { workers } if workers == 0 || workers > 256) {
            return Err(Error::new_with_code(
                ErrorPhase::Runtime,
                ErrorCode::InvalidArgument,
                "Parallel workers must be 1..=256",
            ));
        }
        Ok(self)
    }
    pub fn create(self, max_tasks: usize) -> Result<TaskExecutor, Error> {
        self.validate()?;
        if max_tasks == 0 {
            return Err(Error::new_with_code(
                ErrorPhase::Runtime,
                ErrorCode::InvalidArgument,
                "Task capacity must be positive",
            ));
        }
        Ok(match self {
            Self::Local => TaskExecutor::Local(HostExecutor::new(
                std::rc::Rc::new(crate::runtime::executor::SystemClock),
                max_tasks,
            )),
            Self::Parallel { workers } => {
                TaskExecutor::Parallel(ParallelExecutor::new(workers, max_tasks)?)
            }
        })
    }
}
/// Local continuations and transferable prepared tasks retain separate APIs.
pub enum TaskExecutor {
    Local(HostExecutor),
    Parallel(ParallelExecutor),
}
