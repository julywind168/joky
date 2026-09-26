/// 任务调度器（阶段1：单线程占位，后续阶段扩展为多 worker）

/// 任务优先级
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    Normal,
}

/// 单个任务
pub struct Task {
    /// 任务执行函数（无参，自包含）
    pub entry: Box<dyn FnOnce() + Send>,
    pub priority: Priority,
}

/// 阶段1：串行调度器，直接同步执行入队的任务
pub struct Scheduler {
    queue: Vec<Task>,
}

impl Scheduler {
    pub fn new() -> Self {
        Scheduler { queue: Vec::new() }
    }

    /// 入队一个任务
    pub fn spawn(&mut self, task: Task) {
        self.queue.push(task);
    }

    /// 按顺序执行队列中所有任务（阶段1：同步串行）
    pub fn run_all(&mut self) {
        let tasks = std::mem::take(&mut self.queue);
        for task in tasks {
            (task.entry)();
        }
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn 串行执行任务() {
        let mut sched = Scheduler::new();
        let log = Arc::new(Mutex::new(Vec::<i32>::new()));

        for i in 0..3 {
            let log = Arc::clone(&log);
            sched.spawn(Task {
                entry: Box::new(move || log.lock().unwrap().push(i)),
                priority: Priority::Normal,
            });
        }

        sched.run_all();
        assert_eq!(*log.lock().unwrap(), vec![0, 1, 2]);
    }
}
