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
        (Type::Buffer(a), Type::Buffer(b)) => compatible(a, b),
        (Type::Tuple(a), Type::Tuple(b)) => {
            a.elements.len() == b.elements.len()
                && a.elements
                    .iter()
                    .zip(&b.elements)
                    .all(|(a, b)| compatible(a, b))
        }
        (Type::Enum(a), Type::Enum(b)) => {
            a == b
                || (a.name == b.name
                    && a.indirect == b.indirect
                    && a.arguments.len() == b.arguments.len()
                    && a.arguments
                        .iter()
                        .zip(&b.arguments)
                        .all(|(a, b)| compatible(a, b))
                    && a.variants.len() == b.variants.len()
                    && a.variants.iter().zip(&b.variants).all(|(a, b)| {
                        a.name == b.name
                            && a.offset == b.offset
                            && match (&a.payload, &b.payload) {
                                (None, None) => true,
                                (Some(a), Some(b)) => compatible(a, b),
                                _ => false,
                            }
                    }))
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

pub(super) fn module_key(module: &LoadedModule) -> usize {
    match module {
        LoadedModule::Jit(module) => Rc::as_ptr(module) as usize,
        LoadedModule::Interpreter(module) => Rc::as_ptr(module) as usize,
    }
}

pub(super) fn state_types(module: &LoadedModule) -> HashMap<String, Type> {
    let names: Vec<String> = match module {
        LoadedModule::Jit(module) => module.instance_entries.keys().cloned().collect(),
        LoadedModule::Interpreter(module) => interp::instance_names(module),
    };
    let mut types = HashMap::new();
    for name in names {
        let Ok(info) = module.instance_info(&name) else {
            continue;
        };
        if !info.domain_entry {
            continue;
        }
        let ty = if instance::constructor(&info)
            || crate::core::state_domain::constructor_eligible(&info)
        {
            Some(&info.signature.ret)
        } else {
            instance::receiver(&info).or_else(|| {
                crate::core::state_domain::value_eligible(&info)
                    .then(|| &info.signature.params[0].reference_type().unwrap().target)
            })
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

impl VmEngine {
    pub(crate) fn publish_script_code(
        &self,
        executor: &crate::runtime::executor::HostExecutor,
        owners: &[Rc<dyn std::any::Any>],
    ) -> Result<usize, Error> {
        let module = self
            .current
            .as_ref()
            .ok_or_else(|| invalid("No module loaded"))?;
        publish_matching(&executor.domains(), module, |bound| {
            owners
                .iter()
                .any(|owner| Rc::as_ptr(owner) as *const () as usize == module_key(bound))
        })
    }
}

fn publish(registry: &Rc<RefCell<Domains>>, module: &LoadedModule) -> Result<usize, Error> {
    publish_matching(registry, module, |_| true)
}

fn publish_matching(
    registry: &Rc<RefCell<Domains>>,
    module: &LoadedModule,
    owns: impl Fn(&LoadedModule) -> bool,
) -> Result<usize, Error> {
    let types = state_types(module);
    let mut domains = registry.borrow_mut();
    if domains.shutting_down {
        return Err(invalid("Domain VM is shutting down"));
    }
    let mut moves = Vec::new();
    for (&id, domain) in &domains.entries {
        if !owns(&domain.module)
            || domain.closing
            || domain.failure.is_some()
            || source::same_module(&domain.module, module)
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
        let code = domain
            .code
            .checked_add(1)
            .ok_or_else(|| limited("State domain code version exhausted"))?;
        moves
            .try_reserve(1)
            .map_err(|_| limited("Domain publication allocation failed"))?;
        moves.push((id, ty.clone(), code));
    }
    let mut retired = Vec::new();
    retired
        .try_reserve(moves.len())
        .map_err(|_| limited("Domain publication allocation failed"))?;
    // Validation is complete: only moves and scalar stores remain.
    let key = module_key(module);
    for (id, ty, code) in &moves {
        let domain = domains.entries.get_mut(id).unwrap();
        retired.push(std::mem::replace(&mut domain.module, module.clone()));
        domain.ty = ty.clone();
        domain.code = *code;
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
    domain.write(tick, step).unwrap()
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
                    .replace(
                        "domain.write(tick, step).unwrap()",
                        "domain.write(tick, step > 0).unwrap()",
                    ),
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

    const ACCESS: &str = r#"
struct State { n: Int32 }
struct Rules { n: Int32 }
fn make(n: Int32) -> State { State { n: n } }
fn rules(n: Int32) -> Rules { Rules { n: n } }
fn read(s: &State) -> Int32 { s.n }
fn allowed(s: &Rules) -> Int32 { s.n }
fn pair(a: &mut State, b: &Rules, n: Int32) -> Int32 { a.n = a.n + n + b.n a.n }
fn many(a: &mut State, b: &mut State, c: &mut State, d: &Rules, n: Int32) -> Int32 {
    a.n = a.n + n + d.n b.n = b.n + n c.n = c.n + n a.n
}
"#;
    const RULES: &str = r#"
struct Rules { n: Int32 }
fn rules(n: Int32) -> Rules { Rules { n: n } }
fn allowed(s: &Rules) -> Int32 { s.n + 10 }
"#;

    #[test]
    fn state_domain_code_publication_rebinds_queued_access_from_first_domain() {
        use InstanceValue::Int32;
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            for count in [2, 4] {
                for update in ["none", "all", "readonly", "roundtrip"] {
                    let mut vm = VmEngine::with_mode(mode);
                    vm.load_source(ACCESS).unwrap();
                    let original = vm.current.clone().unwrap();
                    let mut owners = Vec::new();
                    for i in 0..count {
                        owners.push(
                            vm.create_domain(
                                if i + 1 == count { "rules" } else { "make" },
                                &[Int32(1)],
                            )
                            .unwrap(),
                        );
                    }
                    let handles: Vec<_> = owners
                        .iter()
                        .enumerate()
                        .map(|(i, o)| {
                            if i + 1 == count {
                                o.read_handle()
                            } else {
                                o.write_handle()
                            }
                        })
                        .collect();
                    let mut queued = vm
                        .queue_domain_access(
                            &handles,
                            if count == 2 { "pair" } else { "many" },
                            &[Int32(2)],
                        )
                        .unwrap();
                    let upgraded = ACCESS.replace("a.n + n", "a.n + n * 10");
                    vm.load_source(if update == "readonly" {
                        RULES
                    } else {
                        &upgraded
                    })
                    .unwrap();
                    if update != "none" {
                        assert_eq!(
                            vm.publish_domain_code().unwrap(),
                            if update == "readonly" { 1 } else { count }
                        );
                        if update == "roundtrip" {
                            // Same module identity again must not hide an intervening publication.
                            publish(&vm.domains, &original).unwrap();
                        }
                    }
                    vm.poll_tasks(100).unwrap();
                    let first = if update == "all" { 22 } else { 4 };
                    assert_eq!(value(&mut queued), Int32(first));
                    for (i, owner) in owners.iter().enumerate() {
                        let writable = i + 1 < count;
                        let expected = if writable {
                            if i == 0 {
                                first
                            } else {
                                3
                            }
                        } else {
                            1
                        };
                        assert_eq!(
                            vm.domain_revision(owner.read_handle()).unwrap(),
                            u64::from(writable)
                        );
                        if writable {
                            assert_eq!(
                                call(&mut vm, owner.read_handle(), "read", &[]),
                                Int32(expected)
                            );
                        }
                    }
                    drop(owners);
                    assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
                }
            }
        }
    }

    #[test]
    fn state_domain_code_publication_rejects_incompatible_access_abi_before_writes() {
        use InstanceValue::Int32;
        let entry =
            "fn pair(a: &mut State, b: &Rules, n: Int32) -> Int32 { a.n = a.n + n + b.n a.n }";
        let replacements = [
            "fn renamed(a: &mut State, b: &Rules, n: Int32) -> Int32 { a.n = 999 a.n }",
            "fn pair(a: &mut State, b: &Rules, n: Bool) -> Int32 { a.n = 999 a.n }",
            "fn pair(a: &mut State, b: &Rules, n: Int32) -> Bool { a.n = 999 true }",
            "fn pair(a: &State, b: &Rules, n: Int32) -> Int32 { a.n }",
            "fn pair(a: &mut State, b: &mut Rules, n: Int32) -> Int32 { a.n = 999 b.n = 999 a.n }",
            "fn pair(a: &mut State, b: &Rules, n: Int32, extra: Int32) -> Int32 { a.n = 999 a.n }",
            "fn pair(a: &mut State, b: &Rules, n: Int32) -> Int32 { a.n = 999 println(n) a.n }",
        ];
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            for replacement in replacements {
                let mut vm = VmEngine::with_mode(mode);
                vm.load_source(ACCESS).unwrap();
                let caller = vm.current.clone().unwrap();
                let a = vm.create_domain("make", &[Int32(1)]).unwrap();
                let b = vm.create_domain("rules", &[Int32(1)]).unwrap();
                let handles = [a.write_handle(), b.read_handle()];
                let mut queued = vm.queue_domain_pair(handles, "pair", &[Int32(2)]).unwrap();
                vm.load_source(&ACCESS.replace(entry, replacement)).unwrap();
                assert_eq!(vm.publish_domain_code().unwrap(), 2);
                // Both a previously admitted operation and a newly submitted old
                // caller must fail before exposing any writable root address.
                assert!(
                    matches!(pairs::submit(&vm.domains, &handles, caller, "pair", &[Int32(2)], Default::default()), Err(e) if e.code() == ErrorCode::TypeMismatch)
                );
                vm.poll_tasks(100).unwrap();
                assert!(
                    matches!(queued.take_result(), Some(DomainOutcome::Failed(e)) if e.code() == ErrorCode::TypeMismatch)
                );
                assert_eq!(call(&mut vm, a.read_handle(), "read", &[]), Int32(1));
                assert_eq!(call(&mut vm, b.read_handle(), "allowed", &[]), Int32(1));
                for owner in [&a, &b] {
                    assert_eq!(vm.domain_revision(owner.read_handle()).unwrap(), 0);
                }
            }
        }
    }

    #[test]
    fn state_domain_code_publication_rejects_migrated_readonly_dependency_before_writes() {
        use InstanceValue::Int32;
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            for count in [2, 4] {
                let mut vm = VmEngine::with_mode(mode);
                vm.load_source(ACCESS).unwrap();
                let mut states = Vec::new();
                for _ in 1..count {
                    states.push(vm.create_domain("make", &[Int32(1)]).unwrap());
                }
                // Give the read-only dependency independent code ownership so it
                // can migrate without changing the operation's first domain.
                vm.load_source(RULES).unwrap();
                let rules = vm.create_domain("rules", &[Int32(3)]).unwrap();
                let owners = [vm.script_code_owner().unwrap()];
                let mut handles: Vec<_> = states.iter().map(DomainOwner::write_handle).collect();
                handles.push(rules.read_handle());
                let method = if count == 2 { "pair" } else { "many" };
                let mut queued = vm
                    .queue_domain_access(&handles, method, &[Int32(2)])
                    .unwrap();
                let registry = vm.domains.clone();
                let schema = migration::schema(&registry, &owners).unwrap();
                let plan = migration::prepare(&vm, &registry, &owners, schema,
                    "struct Rules { enabled:Bool, n:Int32 } fn upgrade(old:&PreviousState)->Rules {Rules{enabled:true,n:old.n+5}} fn allowed(s:&Rules)->Int32{s.n}",
                    "upgrade").unwrap();
                migration::publish(&mut vm, &registry, &owners, plan).unwrap();
                vm.poll_tasks(100).unwrap();
                assert!(
                    matches!(queued.take_result(), Some(DomainOutcome::Failed(e)) if e.code() == ErrorCode::TypeMismatch)
                );
                assert!(
                    matches!(vm.queue_domain_access(&handles, method, &[Int32(2)]), Err(e) if e.code() == ErrorCode::TypeMismatch)
                );
                for state in &states {
                    assert_eq!(call(&mut vm, state.read_handle(), "read", &[]), Int32(1));
                    assert_eq!(vm.domain_revision(state.read_handle()).unwrap(), 0);
                }
                assert_eq!(call(&mut vm, rules.read_handle(), "allowed", &[]), Int32(8));
                assert_eq!(vm.domain_revision(rules.read_handle()).unwrap(), 1);
            }
        }
    }

    #[test]
    fn state_domain_code_publication_access_snapshot_captures_rebound_versions() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            let old = format!("{ACCESS} fn sum(a:&State,b:&Rules)->Int32 {{a.n+b.n}}");
            vm.load_source(&old).unwrap();
            let a = vm
                .create_domain("make", &[InstanceValue::Int32(1)])
                .unwrap();
            let b = vm
                .create_domain("rules", &[InstanceValue::Int32(2)])
                .unwrap();
            let mut request = vm
                .queue_domain_pair([a.read_handle(), b.read_handle()], "sum", &[])
                .unwrap();
            vm.load_source(&old.replace("a.n+b.n", "a.n+b.n+10"))
                .unwrap();
            vm.publish_domain_code().unwrap();
            vm.poll_tasks(100).unwrap();
            let (result, _, reads) = request.take_snapshot_result().unwrap();
            assert!(matches!(
                result,
                DomainOutcome::Value(InstanceValue::Int32(13))
            ));
            let reads = reads.unwrap();
            assert_eq!(reads.validate(&vm.domains.borrow()), Ok(()));
            vm.load_source(RULES).unwrap();
            vm.publish_domain_code().unwrap();
            assert_eq!(
                reads.validate(&vm.domains.borrow()),
                Err(commits::Status::Conflict)
            );
        }
    }

    #[test]
    fn state_domain_code_publication_preserves_snapshot_execution_credentials() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(ACCESS).unwrap();
            let owner = vm
                .create_domain("make", &[InstanceValue::Int32(7)])
                .unwrap();
            let mut snapshot = vm
                .queue_domain_call(owner.read_handle(), "read", &[])
                .unwrap();
            vm.poll_tasks(1).unwrap();
            vm.load_source(ACCESS).unwrap();
            vm.publish_domain_code().unwrap();
            let (outcome, revision, reads) = snapshot.take_snapshot_result().unwrap();
            assert!(matches!(
                outcome,
                DomainOutcome::Value(InstanceValue::Int32(7))
            ));
            assert_eq!(revision, 0);
            assert_eq!(
                reads.unwrap().validate(&vm.domains.borrow()),
                Err(commits::Status::Conflict)
            );
            assert_eq!(vm.domain_revision(owner.read_handle()).unwrap(), 0);
        }
    }

    #[test]
    fn state_domain_code_publication_exhaustion_rejects_whole_batch() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(ACCESS).unwrap();
            let original = vm.current.clone().unwrap();
            let a = vm
                .create_domain("make", &[InstanceValue::Int32(1)])
                .unwrap();
            let b = vm
                .create_domain("rules", &[InstanceValue::Int32(1)])
                .unwrap();
            vm.domains
                .borrow_mut()
                .entries
                .get_mut(&b.handle.sequence)
                .unwrap()
                .code = u64::MAX;
            vm.load_source(ACCESS).unwrap();
            assert_eq!(
                vm.publish_domain_code().unwrap_err().code(),
                ErrorCode::ResourceLimit
            );
            for owner in [&a, &b] {
                let domains = vm.domains.borrow();
                let domain = domains.get(owner.read_handle()).unwrap();
                assert!(source::same_module(&domain.module, &original));
                assert_eq!(domain.revision, 0);
            }
            assert_eq!(vm.domains.borrow().get(a.read_handle()).unwrap().code, 0);
            assert_eq!(
                call(&mut vm, a.read_handle(), "read", &[]),
                InstanceValue::Int32(1)
            );
        }
    }

    #[test]
    fn state_domain_code_publication_invalidates_readonly_candidate_dependencies() {
        use crate::runtime::executor::Clock;
        use std::time::{Duration, Instant};
        struct TestClock(Cell<Instant>);
        impl Clock for TestClock {
            fn now(&self) -> Instant {
                self.0.get()
            }
        }
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            for many in [false, true] {
                for (update, stage) in [
                    (false, "candidate"),
                    (true, "snapshot"),
                    (true, "candidate"),
                    (true, "queued"),
                ] {
                    let before = crate::runtime::arena::LIVE.with(|n| n.get());
                    let clock = Rc::new(TestClock(Cell::new(Instant::now())));
                    let mut vm = VmEngine::with_mode(mode);
                    vm.configure_tasks(clock.clone(), 100).unwrap();
                    let prepare = if many {
                        r#"
    var set = domain.access_set()
    set.include(a.read_handle()) set.include(b.read_handle())
    set.include(c.read_handle()) set.include(r.read_handle())
    let view = domain.snapshot_set(set)
    let n = domain.read_snapshot(&view, r.read_handle(), allowed)
    let first = domain.candidate_set_from(view, a.handle(), make, n + 10)
    let second = domain.candidate_set_add(first, b.handle(), make, n + 20)
    let candidate = domain.candidate_set_add(second, c.handle(), make, n + 30)
"#
                    } else {
                        r#"
    let view = domain.snapshot_pair(a.read_handle(), r.read_handle(), project)
    let n = view.value()
    let candidate = domain.candidate_from(view, a.handle(), make, n + 10)
"#
                    };
                    let prepare = if stage == "snapshot" {
                        prepare.replace("    let n =", "    task.sleep_ms(1000)\n    let n =")
                    } else {
                        prepare.to_owned()
                    };
                    let pause = if stage == "candidate" {
                        "task.sleep_ms(1000)"
                    } else {
                        ""
                    };
                    let commit = if many {
                        "domain.commit_set(candidate)"
                    } else {
                        "domain.commit_to(a.handle(), candidate)"
                    };
                    vm.load_source(&format!(
                        r#"
import joky/domain
import joky/task
{ACCESS}
fn project(a: &State, r: &Rules) -> Int32 {{ r.n }}
fn probe(n: Int32) @suspends -> Int32 {{
    let a = domain.create(make, 1)
    let b = domain.create(make, 2)
    let c = domain.create(make, 3)
    let r = domain.create(rules, 1)
    {prepare}
    {pause}
    var receipt = {commit}
    let status = match receipt.wait() {{ Ok(_) => 1, Err(Conflict) => 2, _ => 9 }}
    let x = domain.read_from(a.read_handle(), read).unwrap()
    let y = domain.read_from(b.read_handle(), read).unwrap()
    let z = domain.read_from(c.read_handle(), read).unwrap()
    status * 10000 + x * 100 + y * 10 + z
}}
"#
                    ))
                    .unwrap();
                    let host = vm
                        .create_domain("make", &[InstanceValue::Int32(0)])
                        .unwrap();
                    let task = vm
                        .spawn_domain_task(host.write_handle(), "probe", &[InstanceValue::Int32(0)])
                        .unwrap();
                    if stage == "queued" {
                        let mut queued = false;
                        for _ in 0..100 {
                            assert!(vm.poll_tasks(1).unwrap().completed.is_empty());
                            queued = vm.domains.borrow().requests.values().any(|r| {
                                r.outcome.is_none()
                                    && matches!(r.work, Work::Commit(_) | Work::CommitSet(_))
                            });
                            if queued {
                                break;
                            }
                        }
                        assert!(queued, "commit must be queued before publication");
                    } else {
                        for _ in 0..10 {
                            assert!(vm.poll_tasks(100).unwrap().completed.is_empty());
                        }
                    }
                    if update {
                        vm.load_source(RULES).unwrap();
                        // Only the read dependency moves; none of the three write roots do.
                        assert_eq!(vm.publish_domain_code().unwrap(), 1);
                    }
                    clock.0.set(clock.0.get() + Duration::from_millis(1001));
                    assert_eq!(
                        finish(&mut vm, task),
                        if update {
                            20123
                        } else if many {
                            11341
                        } else {
                            11123
                        }
                    );
                    drop(host);
                    assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
                    drop(vm);
                    assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), before);
                }
            }
        }
    }

    #[test]
    fn script_scalar_bridge_publication_excludes_other_module_families() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut host = VmEngine::with_mode(mode);
            let mut script = VmEngine::with_mode(mode);
            let mut other = VmEngine::with_mode(mode);
            host.load_source(ROOM).unwrap();
            script.load_source(ROOM).unwrap();
            other.load_source(ROOM).unwrap();
            let native = host
                .create_domain("open", &[InstanceValue::Int32(0)])
                .unwrap();
            let create = |vm: &VmEngine| {
                super::super::create(
                    &host.domains,
                    vm.current.clone().unwrap(),
                    "open",
                    &[InstanceValue::Int32(0)],
                    Default::default(),
                )
                .unwrap()
            };
            let target = create(&script);
            let unrelated = create(&other);
            let old = script.script_code_owner().unwrap();
            script.load_source(&upgraded()).unwrap();
            assert_eq!(
                script.publish_script_code(&host.executor, &[old]).unwrap(),
                1
            );
            for (owner, expected) in [(&target, 10), (&unrelated, 1), (&native, 1)] {
                assert_eq!(
                    call(
                        &mut host,
                        owner.write_handle(),
                        "tick",
                        &[InstanceValue::Int32(1)]
                    ),
                    InstanceValue::Int32(expected)
                );
            }
        }
    }
}
