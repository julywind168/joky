//! A copied diagnostic snapshot never extends a Script module's lifetime.
use super::*;
use crate::backends::{interp::BytecodeModule, jit::CompiledModule};
use crate::vm::state_domains::diagnostics::Holds;

#[derive(Debug)]
struct ModuleReport {
    version: u64,
    current: bool,
    references: usize,
    calls: usize,
    holds: Holds,
    migrations: usize,
}
fn reports(state: &HandleState) -> Result<Vec<ModuleReport>, Error> {
    let engine = state
        .engine
        .try_borrow()
        .map_err(|_| runtime_error(ErrorCode::InvalidState, "VM is executing another operation"))?;
    state.remember(&engine);
    let current = engine.script_code_owner().map(|m| Rc::downgrade(&m));
    let session = state.session.borrow();
    let registry = session.as_ref().and_then(|s| s.domains.upgrade());
    let domains = registry.as_ref().map(|r| r.borrow());
    let migration = state.migration.borrow();
    let mut reports = Vec::new();
    for weak in state.modules.borrow().iter() {
        // Observe the count before the temporary upgrade used to read metadata.
        let references = weak.strong_count();
        let Some(owner) = weak.upgrade() else {
            continue;
        };
        let version = if let Some(module) = owner.downcast_ref::<CompiledModule>() {
            module.version.0
        } else if let Some(module) = owner.downcast_ref::<BytecodeModule>() {
            module.version.0
        } else {
            continue;
        };
        let key = Rc::as_ptr(&owner) as *const () as usize;
        let calls = session.as_ref().map_or(0, |session| {
            session
                .calls
                .iter()
                .filter_map(Weak::upgrade)
                .filter(|call| call.module.ptr_eq(weak))
                .count()
        });
        let migrations = usize::from(match migration.as_ref() {
            Some(Migration::Single(plan)) => plan.holds_module(key),
            Some(Migration::Batch(plan)) => plan.holds_module(key),
            None => false,
        });
        reports.push(ModuleReport {
            version,
            current: current.as_ref().is_some_and(|m| m.ptr_eq(weak)),
            references,
            calls,
            holds: domains
                .as_ref()
                .map_or_else(Holds::default, |d| d.module_holds(key)),
            migrations,
        });
    }
    reports.sort_by_key(|report| report.version);
    Ok(reports)
}

#[export_name = "joky_vm_module_count"]
pub(crate) extern "C" fn module_count(handle: u64) -> i32 {
    operation(-1, || {
        i32::try_from(reports(lookup(handle)?.as_ref())?.len())
            .map_err(|_| runtime_error(ErrorCode::ResourceLimit, "Module count overflow"))
    })
}

#[export_name = "joky_vm_print_modules"]
pub(crate) extern "C" fn print_modules(handle: u64) -> i32 {
    operation(-1, || {
        let reports = reports(lookup(handle)?.as_ref())?;
        let count = i32::try_from(reports.len())
            .map_err(|_| runtime_error(ErrorCode::ResourceLimit, "Module count overflow"))?;
        let mut output = std::io::stderr().lock();
        for report in reports {
            writeln!(output,
                "joky/vm {handle}: module={} current={} references={} script_calls={} domains={} domain_tasks={} queued={} results={} migrations={}",
                report.version, report.current, report.references, report.calls,
                report.holds.domains, report.holds.tasks, report.holds.queued,
                report.holds.results, report.migrations,
            ).map_err(|e| runtime_error(ErrorCode::RuntimeError, e.to_string()))?;
        }
        Ok(count)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::executor::TaskGroup;

    #[test]
    fn script_module_diagnostics_observe_calls_domains_and_retirement_without_pinning() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let create = if mode == VmMode::Jit {
                "create"
            } else {
                "create_interpreter"
            };
            let script = "import joky/domain import joky/task struct Room { n:Int32 } fn make(n:Int32)->Room {Room{n:n}} fn work(n:Int32) @suspends ->Int32 {let room=domain.create(make,n) task.sleep_ms(100000) 42}";
            let escaped = script.replace('{', "{{").replace('}', "}}");
            let mut host = VmEngine::with_mode(mode);
            host.load_source(&format!(
                r#"import joky/vm
fn main() @suspends ->Int32 {{
    let h=vm.{create}()
    let source={escaped:?}
    vm.load_source(h,&source)
    vm.call_async_int32(h,"work",1)
}}"#
            ))
            .unwrap();
            let baseline = crate::runtime::arena::LIVE.with(|n| n.get());
            let executor = HostExecutor::default();
            let group = TaskGroup::new();
            executor
                .spawn(&group, host.prepare_async("main", &[]).unwrap())
                .unwrap();
            for _ in 0..20 {
                executor.poll(1).unwrap();
            }
            let handle = VMS.with(|vms| *vms.borrow().keys().next().unwrap());
            let state = lookup(handle).unwrap();
            let first = reports(&state).unwrap();
            assert_eq!(first.len(), 1);
            assert!(first[0].current);
            assert_eq!((first[0].calls, first[0].holds.domains), (1, 1));
            let old = state.modules.borrow()[0].clone();
            assert_eq!(
                unsafe {
                    load_source(
                        handle,
                        crate::runtime::string::JokyText::new(script).as_ptr(),
                    )
                },
                0
            );
            assert_eq!(module_count(handle), 2);
            let loaded = reports(&state).unwrap();
            assert!(!loaded[0].current);
            assert_eq!((loaded[0].calls, loaded[0].holds.domains), (1, 1));
            {
                let owners = state
                    .modules
                    .borrow()
                    .iter()
                    .filter_map(Weak::upgrade)
                    .collect::<Vec<_>>();
                assert_eq!(
                    state
                        .engine
                        .borrow()
                        .publish_script_code(&executor, &owners)
                        .unwrap(),
                    1
                );
            }
            let published = reports(&state).unwrap();
            assert_eq!((published[0].calls, published[0].holds.domains), (1, 0));
            assert_eq!(published[1].holds.domains, 1);
            let references = old.strong_count();
            let copied = reports(&state).unwrap();
            assert_eq!(copied[0].references, references);
            assert_eq!(old.strong_count(), references);
            executor.cancel_group(&group);
            assert_eq!(module_count(handle), 1);
            assert_eq!(
                old.strong_count(),
                0,
                "retained reports must not pin old code"
            );
            assert_eq!(executor.domains().borrow().usage(), (0, 0, 0, 0));
            assert_eq!(close(handle), 0);
            assert_eq!(module_count(handle), -1);
            drop(state);
            drop(executor);
            assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), baseline);
        }
    }
}
