//! Code publication for live domains. Roots keep their storage, revision and
//! identity; only the module that runs their short operations changes between
//! turns. Suspended requests keep running the code they started with.
use super::*;
use crate::backends::interp;

/// Same nominal record name and identical field names, privacy, slots and types.
/// Record ids are fresh per load, so equal ids are sufficient but not required.
pub(super) fn compatible(a: &Type, b: &Type) -> bool {
    match (a, b) {
        (Type::Reference(a), Type::Reference(b)) => {
            a.mutable == b.mutable && compatible(&a.target, &b.target)
        }
        (Type::Record(a), Type::Record(b)) => {
            a == b
                || (a.name == b.name
                    && a.closure.is_none()
                    && b.closure.is_none()
                    && a.repr_c == b.repr_c
                    && a.indirect == b.indirect
                    && a.arguments.len() == b.arguments.len()
                    && a.arguments
                        .iter()
                        .zip(&b.arguments)
                        .all(|(a, b)| compatible(a, b))
                    && a.fields.len() == b.fields.len()
                    && a.fields.iter().zip(&b.fields).all(|(a, b)| {
                        a.name == b.name
                            && a.private == b.private
                            && a.offset == b.offset
                            && compatible(&a.ty, &b.ty)
                    }))
        }
        _ => a == b,
    }
}

/// The caller encodes inputs and decodes results with its own signature.
pub(super) fn compatible_entry(caller: &FunctionInfo, current: &FunctionInfo) -> bool {
    caller.suspends == current.suspends
        && caller.signature.params.len() == current.signature.params.len()
        && caller
            .signature
            .params
            .iter()
            .zip(&current.signature.params)
            .all(|(a, b)| compatible(a, b))
        && compatible(&caller.signature.ret, &current.signature.ret)
}

/// A caller may run an older or newer module than the code published for a domain.
pub(super) fn admits(
    caller: &LoadedModule,
    current: &LoadedModule,
    name: &str,
    request: bool,
) -> bool {
    if source::same_module(caller, current) {
        return true;
    }
    let info = |module: &LoadedModule| {
        if request {
            source::request_info(module, name).cloned()
        } else {
            module.instance_info(name).ok()
        }
    };
    matches!((info(caller), info(current)), (Some(a), Some(b)) if compatible_entry(&a, &b))
}

fn module_key(module: &LoadedModule) -> usize {
    match module {
        LoadedModule::Jit(module) => Rc::as_ptr(module) as usize,
        LoadedModule::Interpreter(module) => Rc::as_ptr(module) as usize,
    }
}

fn state_types(module: &LoadedModule) -> HashMap<String, Type> {
    let names: Vec<String> = match module {
        LoadedModule::Jit(module) => module.instance_entries.keys().cloned().collect(),
        LoadedModule::Interpreter(module) => interp::function_names(module),
    };
    let mut types = HashMap::new();
    for name in names {
        let Ok(info) = module.instance_info(&name) else {
            continue;
        };
        if !info.domain_entry {
            continue;
        }
        let ty = if instance::constructor(&info) {
            Some(&info.signature.ret)
        } else {
            instance::receiver(&info)
        };
        if let Some(Type::Record(record)) = ty {
            types
                .entry(record.name.clone())
                .or_insert_with(|| Type::Record(record.clone()));
        }
    }
    types
}

impl VmEngine {
    /// Rebind live domains to the current module when it declares the same state
    /// schema. A changed schema rejects the whole publication before any domain
    /// moves; domains whose type the module does not declare stay on their code.
    /// Returns the number of rebound domains.
    pub fn publish_domain_code(&self) -> Result<usize, Error> {
        let module = self
            .current
            .clone()
            .ok_or_else(|| invalid("No module loaded"))?;
        publish(&self.domains, &module)
    }
}

fn publish(registry: &Rc<RefCell<Domains>>, module: &LoadedModule) -> Result<usize, Error> {
    let types = state_types(module);
    let mut domains = registry.borrow_mut();
    if domains.shutting_down {
        return Err(invalid("Domain VM is shutting down"));
    }
    let mut moves = Vec::new();
    for (&id, domain) in &domains.entries {
        if domain.closing || domain.failure.is_some() || source::same_module(&domain.module, module)
        {
            continue;
        }
        let Type::Record(record) = &domain.ty else {
            continue;
        };
        let Some(ty) = types.get(&record.name) else {
            continue;
        };
        if !compatible(&domain.ty, ty) {
            return Err(Error::new_with_code(
                ErrorPhase::Runtime,
                ErrorCode::TypeMismatch,
                format!(
                    "State type `{}` changed its schema; publication requires a migration",
                    record.name
                ),
            ));
        }
        moves
            .try_reserve(1)
            .map_err(|_| limited("Domain publication allocation failed"))?;
        moves.push((id, ty.clone()));
    }
    let mut retired = Vec::new();
    retired
        .try_reserve(moves.len())
        .map_err(|_| limited("Domain publication allocation failed"))?;
    // Validation is complete: only moves and scalar stores remain.
    let key = module_key(module);
    for (id, ty) in &moves {
        let domain = domains.entries.get_mut(id).unwrap();
        retired.push(std::mem::replace(&mut domain.module, module.clone()));
        domain.ty = ty.clone();
        domain.code = domain.code.wrapping_add(1);
        if domain.source_module != 0 {
            domain.source_module = key;
        }
    }
    drop(domains);
    // Old code is released outside the registry borrow, once nothing else holds it.
    drop(retired);
    Ok(moves.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::VmMode;

    const ROOM: &str = r#"
import joky/domain
import joky/task

struct Room { ticks: Int32, players: Int32 }
fn open(players: Int32) -> Room { Room { ticks: 0, players: players } }
fn tick(room: &mut Room, step: Int32) @noescape -> Int32 {
    room.ticks = room.ticks + step
    room.ticks
}
fn ticks(room: &Room) @noescape -> Int32 { room.ticks }
fn version() -> Int32 { 1 }
fn session(step: Int32) @suspends -> Int32 {
    let code = version()
    task.yield_now()
    domain.write(tick, step)
    code
}
"#;

    fn upgraded() -> String {
        ROOM.replace("room.ticks + step", "room.ticks + step * 10")
            .replace("fn version() -> Int32 { 1 }", "fn version() -> Int32 { 2 }")
    }

    fn alive(vm: &VmEngine) -> Box<dyn Fn() -> bool> {
        match vm.current.as_ref().unwrap() {
            LoadedModule::Jit(module) => {
                let weak = Rc::downgrade(module);
                Box::new(move || weak.strong_count() > 0)
            }
            LoadedModule::Interpreter(module) => {
                let weak = Rc::downgrade(module);
                Box::new(move || weak.strong_count() > 0)
            }
        }
    }

    fn value(request: &mut DomainRequest) -> InstanceValue {
        match request.take_result() {
            Some(DomainOutcome::Value(value)) => value,
            other => panic!("unexpected domain outcome: {other:?}"),
        }
    }

    fn call(
        vm: &mut VmEngine,
        handle: DomainHandle,
        method: &str,
        args: &[InstanceValue],
    ) -> InstanceValue {
        let mut request = vm.queue_domain_call(handle, method, args).unwrap();
        vm.poll_tasks(1).unwrap();
        value(&mut request)
    }

    fn finish(vm: &mut VmEngine, task: TaskId) -> u64 {
        for _ in 0..10 {
            for done in vm.poll_tasks(4).unwrap().completed {
                if done.id == task {
                    return *done.result.as_ref().unwrap();
                }
            }
        }
        panic!("domain task did not finish");
    }

    #[test]
    fn state_domain_code_publication_rebinds_operations_between_turns() {
        use InstanceValue::Int32;
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let before = crate::runtime::arena::LIVE.with(|n| n.get());
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(ROOM).unwrap();
            let v1 = alive(&vm);
            let owner = vm.create_domain("open", &[Int32(4)]).unwrap();
            let write = owner.write_handle();
            assert_eq!(call(&mut vm, write, "tick", &[Int32(1)]), Int32(1));

            // A v1 flow suspends, and a v1 turn is queued, before the update.
            let session = vm.spawn_domain_task(write, "session", &[Int32(1)]).unwrap();
            assert!(vm.poll_tasks(1).unwrap().completed.is_empty());
            let mut queued = vm.queue_domain_call(write, "tick", &[Int32(1)]).unwrap();

            vm.load_source(&upgraded()).unwrap();
            // Loading alone binds new domains only.
            let fresh = vm.create_domain("open", &[Int32(1)]).unwrap();
            assert_eq!(vm.publish_domain_code().unwrap(), 1);
            assert_eq!(vm.publish_domain_code().unwrap(), 0);

            // Every later turn runs v2; the suspended flow keeps its own v1 code.
            assert_eq!(finish(&mut vm, session), 1);
            assert_eq!(value(&mut queued), Int32(11));
            assert_eq!(call(&mut vm, write, "ticks", &[]), Int32(21));
            assert_eq!(vm.domain_revision(owner.read_handle()).unwrap(), 3);
            let next = vm.spawn_domain_task(write, "session", &[Int32(1)]).unwrap();
            assert_eq!(finish(&mut vm, next), 2);
            assert_eq!(call(&mut vm, write, "ticks", &[]), Int32(31));
            assert!(!v1(), "v1 code must retire after its last flow");

            // A changed schema rejects publication without moving any domain.
            let v2 = alive(&vm);
            vm.load_source(
                &upgraded()
                    .replace("players: Int32 }", "players: Int32, bonus: Int32 }")
                    .replace("players: players }", "players: players, bonus: 0 }"),
            )
            .unwrap();
            let error = vm.publish_domain_code().unwrap_err();
            assert_eq!(error.code(), ErrorCode::TypeMismatch);
            assert_eq!(call(&mut vm, write, "tick", &[Int32(1)]), Int32(41));
            assert!(v2());

            drop(owner);
            drop(fresh);
            vm.poll_tasks(10).unwrap();
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
            drop(vm);
            assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), before);
        }
    }

    #[test]
    fn state_domain_code_publication_rejects_changed_operation_signatures() {
        use InstanceValue::Int32;
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(ROOM).unwrap();
            let owner = vm.create_domain("open", &[Int32(4)]).unwrap();
            let write = owner.write_handle();
            let session = vm.spawn_domain_task(write, "session", &[Int32(1)]).unwrap();
            assert!(vm.poll_tasks(1).unwrap().completed.is_empty());
            let mut queued = vm.queue_domain_call(write, "tick", &[Int32(1)]).unwrap();

            // Same state schema, but `tick` now takes a Bool step.
            vm.load_source(
                &ROOM
                    .replace("step: Int32) @noescape", "step: Bool) @noescape")
                    .replace("room.ticks + step", "room.ticks + if step { 1 } else { 0 }")
                    .replace("domain.write(tick, step)", "domain.write(tick, step > 0)"),
            )
            .unwrap();
            assert_eq!(vm.publish_domain_code().unwrap(), 1);

            // Old encodings never reach new code; the state itself stays healthy.
            let mut outcome = None;
            for _ in 0..10 {
                for done in vm.poll_tasks(4).unwrap().completed {
                    if done.id == session {
                        outcome = Some(done.result.is_err());
                    }
                }
            }
            assert_eq!(outcome, Some(true));
            assert!(matches!(
                queued.take_result(),
                Some(DomainOutcome::Failed(error)) if error.code() == ErrorCode::TypeMismatch
            ));
            assert_eq!(call(&mut vm, write, "ticks", &[]), Int32(0));
            assert_eq!(vm.domain_revision(owner.read_handle()).unwrap(), 0);
        }
    }
}
