//! Non-owning observations. Counts describe holders, not Rc edges or RSS.
use super::*;

#[derive(Default, Debug)]
pub(crate) struct Holds {
    pub domains: usize,
    pub tasks: usize,
    pub queued: usize,
    pub results: usize,
}
impl Domains {
    pub(crate) fn module_holds(&self, key: usize) -> Holds {
        let mut holds = Holds::default();
        let executor = self.executor.upgrade();
        for domain in self.entries.values() {
            holds.domains += usize::from(code::module_key(&domain.module) == key);
            holds.tasks += domain
                .tasks
                .iter()
                .filter(|(id, task)| {
                    task.module == key
                        && (task.group.running() != 0
                            || executor.as_ref().is_some_and(|e| e.contains(**id)))
                })
                .count();
        }
        for request in self.requests.values() {
            let retained = match &request.work {
                Work::Pair(pair) => pair.module_key() == key,
                Work::Commit(Some(candidate)) => candidate.holds_module(key),
                Work::CommitPair(Some(bundle)) => bundle.holds_module(key),
                Work::CommitSet(Some(set)) => set.holds_module(key),
                _ => false,
            } || request
                .set_snapshot
                .as_ref()
                .is_some_and(|s| s.holds_module(key));
            if retained {
                if request.outcome.is_some() {
                    holds.results += 1;
                } else {
                    holds.queued += 1;
                }
            }
        }
        holds
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::VmMode;
    const SOURCE: &str = "import joky/task struct Room { n:Int32 } fn make(n:Int32)->Room {Room{n:n}} fn sum(a:&Room,b:&Room)->Int32 {a.n+b.n} fn wait() @suspends ->Int32 {task.sleep_ms(100000) 1}";

    #[test]
    fn state_domain_module_diagnostics_distinguish_tasks_queues_and_unclaimed_results() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(SOURCE).unwrap();
            let old = vm.current.clone().unwrap();
            let key = code::module_key(&old);
            let a = vm
                .create_domain("make", &[InstanceValue::Int32(1)])
                .unwrap();
            let b = vm
                .create_domain("make", &[InstanceValue::Int32(2)])
                .unwrap();
            let task = vm.spawn_domain_task(a.read_handle(), "wait", &[]).unwrap();
            vm.poll_tasks(1).unwrap();
            let mut request = vm
                .queue_domain_pair([a.read_handle(), b.read_handle()], "sum", &[])
                .unwrap();
            let holds = vm.domains.borrow().module_holds(key);
            assert_eq!(
                (holds.domains, holds.tasks, holds.queued, holds.results),
                (2, 1, 1, 0)
            );
            vm.poll_tasks(1).unwrap();
            vm.load_source(SOURCE).unwrap();
            vm.publish_domain_code().unwrap();
            let holds = vm.domains.borrow().module_holds(key);
            assert_eq!(
                (holds.domains, holds.tasks, holds.queued, holds.results),
                (0, 1, 0, 1)
            );
            request.take_result().unwrap();
            assert_eq!(vm.domains.borrow().module_holds(key).results, 0);
            vm.cancel_domain_task(task);
            assert_eq!(vm.domains.borrow().module_holds(key).tasks, 0);
            a.close();
            b.close();
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }
}
