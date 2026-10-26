//! One Script module, a complete domain read set, and a single publication interval.
use super::*;
const MAX_DOMAINS: usize = 256;

struct Entry {
    target: DomainHandle,
    old: Type,
    ty: Option<Type>,
    // Both candidate and retired roots drop before their code owners.
    root: Option<std::sync::Arc<Root>>,
    retired_module: Option<LoadedModule>,
    step: Option<Step>,
    schema: u64,
}
pub(crate) struct Batch {
    entries: Vec<Entry>,
    registry: Weak<RefCell<Domains>>,
    reads: reads::ReadSet,
    original: LoadedModule,
    module: LoadedModule,
    next_version: u64,
    cursor: usize,
    _charge: reads::Charge,
}
fn owns(domain: &Domain, owners: &[Rc<dyn Any>]) -> bool {
    !domain.closing
        && owners.iter().any(|owner| {
            Rc::as_ptr(owner) as *const () as usize == code::module_key(&domain.module)
        })
}
fn root_name(ty: &Type) -> &str {
    let Type::Record(record) = ty else {
        unreachable!("domain record root")
    };
    &record.name
}
fn view_name(ty: &Type) -> String {
    format!("Previous{}", root_name(ty).rsplit('.').next().unwrap())
}
impl Batch {
    fn validate(
        &self,
        engine: &VmEngine,
        registry: &Rc<RefCell<Domains>>,
        domains: &Domains,
        owners: &[Rc<dyn Any>],
    ) -> Result<(), Error> {
        parallel::ensure_idle(domains)?;
        if !self.registry.ptr_eq(&Rc::downgrade(registry))
            || engine
                .current
                .as_ref()
                .is_none_or(|m| !source::same_module(m, &self.original))
        {
            return Err(invalid("Migration batch Script binding changed"));
        }
        self.reads
            .validate(domains)
            .map_err(|_| invalid("Migration batch conflict: state, code or schema changed"))?;
        let mut count = 0;
        for (&sequence, domain) in &domains.entries {
            if owns(domain, owners) {
                count += 1;
                if !self.entries.iter().any(|e| e.target.sequence == sequence) {
                    return Err(invalid("Migration batch domain selection changed"));
                }
            }
        }
        if count != self.entries.len() {
            return Err(invalid("Migration batch domain selection changed"));
        }
        Ok(())
    }
    fn ready(&self) -> bool {
        self.entries
            .iter()
            .all(|e| e.ty.is_some() && e.step.as_ref().is_none_or(|s| s.done))
    }
}
pub(crate) fn begin(
    engine: &VmEngine,
    registry: &Rc<RefCell<Domains>>,
    owners: &[Rc<dyn Any>],
    source: &str,
) -> Result<Batch, Error> {
    let original = engine
        .current
        .clone()
        .ok_or_else(|| invalid("No Script module loaded"))?;
    let domains = registry.borrow();
    parallel::ensure_idle(&domains)?;
    if domains.shutting_down {
        return Err(invalid("Migration executor is closed"));
    }
    let count = domains.entries.values().filter(|d| owns(d, owners)).count();
    if count == 0 || count > MAX_DOMAINS {
        return Err(invalid(
            "Migration batch requires 1 to 256 live Script domains",
        ));
    }
    let charge = reads::Charge::new(
        domains.budget.clone(),
        std::mem::size_of::<Batch>()
            + count * (std::mem::size_of::<Entry>() + std::mem::size_of::<DomainHandle>()),
    )?;
    let mut targets = Vec::new();
    targets
        .try_reserve_exact(count)
        .map_err(|_| limited("Migration batch allocation failed"))?;
    for (&sequence, domain) in &domains.entries {
        if owns(domain, owners) {
            targets.push(DomainHandle {
                registry: domains.identity,
                sequence,
                writable: true,
            });
        }
    }
    targets.sort_unstable_by_key(|t| t.sequence);
    let reads = reads::ReadSet::capture_many(&domains, &targets)?;
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(count)
        .map_err(|_| limited("Migration batch allocation failed"))?;
    for target in targets {
        let domain = domains.get(target)?;
        entries.push(Entry {
            target,
            old: domain.ty.clone(),
            ty: None,
            root: None,
            retired_module: None,
            step: None,
            schema: domain.schema,
        });
    }
    drop(domains);
    let input = crate::parser::parse(source)?;
    let types: Vec<_> = entries.iter().map(|e| e.old.clone()).collect();
    let view = view::View::many(&types, &input.imports)?;
    let ast = crate::parser::parse(&format!(
        "{}\n{source}\n{}",
        view.imports, view.declarations
    ))?;
    let (module, next_version) = engine.stage_module(&ast)?;
    let new_types = code::state_types(&module);
    // Compatible roots are read dependencies: keep their storage and schema.
    for entry in &mut entries {
        if let Some(ty) = new_types
            .get(root_name(&entry.old))
            .filter(|ty| code::compatible(&entry.old, ty))
        {
            entry.ty = Some(ty.clone());
        }
    }
    Ok(Batch {
        entries,
        registry: Rc::downgrade(registry),
        reads,
        original,
        module,
        next_version,
        cursor: 0,
        _charge: charge,
    })
}
/// Stage every instance of the constructor's nominal return type. Failure consumes
/// the whole batch at the public boundary, including candidates staged earlier.
pub(crate) fn stage(
    engine: &VmEngine,
    registry: &Rc<RefCell<Domains>>,
    owners: &[Rc<dyn Any>],
    batch: &mut Batch,
    start: &str,
    advance: &str,
) -> Result<(), Error> {
    let domains = registry.borrow();
    batch.validate(engine, registry, &domains, owners)?;
    let info = batch.module.instance_info(start)?;
    if !info.domain_entry || !migration_eligible(&info) {
        return Err(invalid("Migration batch requires a proved constructor"));
    }
    let ty = &info.signature.ret;
    let old_view = &info.signature.params[0].reference_type().unwrap().target;
    let step_info = if advance.is_empty() {
        None
    } else {
        Some(batch.module.instance_info(advance)?)
    };
    if let Some(info) = &step_info {
        if !info.domain_entry
            || !migration_step_eligible(info)
            || !code::compatible(
                ty,
                &info.signature.params[0].reference_type().unwrap().target,
            )
            || !code::compatible(
                old_view,
                &info.signature.params[1].reference_type().unwrap().target,
            )
        {
            return Err(invalid("Invalid migration batch step signature"));
        }
    }
    let selected: Vec<_> = batch
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| root_name(&e.old) == root_name(ty))
        .map(|(i, _)| i)
        .collect();
    if selected.is_empty() {
        return Err(invalid(
            "Migration batch has no domain of the constructor's type",
        ));
    }
    // Validate every instance before invoking the first constructor.
    for &i in &selected {
        let entry = &batch.entries[i];
        if entry.ty.is_some() {
            return Err(invalid(
                "Migration batch type is already staged or compatible",
            ));
        }
        if !view::matches_named(&entry.old, old_view, &view_name(&entry.old))
            || code::compatible(&entry.old, ty)
        {
            return Err(invalid("Migration batch old view or new schema mismatch"));
        }
    }
    for i in selected {
        let entry = &mut batch.entries[i];
        let domain = domains.get(entry.target)?;
        entry.root = Some(std::sync::Arc::new(Root::construct(
            &batch.module,
            &info,
            start,
            &[domain.root.as_ref().unwrap().borrow_pointer(&domain.ty)],
            engine.interpreter_limits(),
            domains.budget.clone(),
        )?));
        entry.ty = Some(ty.clone());
        entry.schema = source::next_token()?;
        if step_info.is_some() {
            let charge = reads::Charge::new(domains.budget.clone(), advance.len())?;
            entry.step = Some(Step {
                method: advance.into(),
                done: false,
                _charge: charge,
            });
        }
    }
    Ok(())
}
/// At most one unfinished root callback per host step, with round-robin selection.
pub(crate) fn step(
    engine: &VmEngine,
    registry: &Rc<RefCell<Domains>>,
    owners: &[Rc<dyn Any>],
    batch: &mut Batch,
    budget: u64,
) -> Result<bool, Error> {
    let domains = registry.borrow();
    batch.validate(engine, registry, &domains, owners)?;
    if budget == 0 {
        return Err(invalid("Migration step budget must be positive"));
    }
    if batch.entries.iter().any(|e| e.ty.is_none()) {
        return Err(invalid("Migration batch has unstaged types"));
    }
    for _ in 0..batch.entries.len() {
        let i = batch.cursor;
        batch.cursor = (i + 1) % batch.entries.len();
        let entry = &mut batch.entries[i];
        let Some(step) = entry.step.as_mut().filter(|s| !s.done) else {
            continue;
        };
        let domain = domains.get(entry.target)?;
        let info = batch.module.instance_info(&step.method)?;
        let mut words = [
            0,
            domain.root.as_ref().unwrap().borrow_pointer(&domain.ty),
            budget,
        ];
        let result = entry
            .root
            .as_mut()
            .and_then(std::sync::Arc::get_mut)
            .unwrap()
            .invoke(
                &batch.module,
                entry.ty.as_ref().unwrap(),
                &step.method,
                &info,
                &mut words,
                engine.interpreter_limits(),
            )?;
        let InstanceValue::Bool(done) = result else {
            unreachable!("validated step return")
        };
        step.done = done;
        break;
    }
    Ok(batch.ready())
}
pub(crate) fn publish(
    engine: &mut VmEngine,
    registry: &Rc<RefCell<Domains>>,
    owners: &[Rc<dyn Any>],
    mut batch: Batch,
) -> Result<(), Error> {
    if !batch.ready() {
        return Err(invalid("Migration batch is incomplete"));
    }
    let mut timers = Vec::new();
    timers
        .try_reserve_exact(batch.entries.len())
        .map_err(|_| limited("Migration timer allocation failed"))?;
    for entry in &batch.entries {
        if entry.root.is_some() {
            timers.push(timers::schema_dependents(registry, entry.target)?);
        }
    }
    let mut periodic = Vec::new();
    periodic
        .try_reserve_exact(batch.entries.len())
        .map_err(|_| limited("Periodic migration allocation failed"))?;
    for entry in &batch.entries {
        periodic.push(timers::automatic::prepare(
            registry,
            entry.target,
            &batch.module,
            entry.ty.as_ref().unwrap(),
            entry.root.is_some(),
        )?);
    }
    let mut domains = registry.borrow_mut();
    batch.validate(engine, registry, &domains, owners)?;
    // Check ALL counters before moving any root or binding.
    for entry in &batch.entries {
        let domain = domains.get(entry.target)?;
        domain
            .code
            .checked_add(1)
            .ok_or_else(|| limited("State domain code version exhausted"))?;
        if entry.root.is_some() {
            domain
                .revision
                .checked_add(1)
                .ok_or_else(|| limited("State domain revision exhausted"))?;
        }
    }
    // No allocations, user code, destructors or fallible operations in this interval.
    for (entry, periodic) in batch.entries.iter_mut().zip(&mut periodic) {
        let domain = domains.entries.get_mut(&entry.target.sequence).unwrap();
        if entry.root.is_some() {
            std::mem::swap(&mut domain.root, &mut entry.root);
            domain.revision += 1;
            domain.schema = entry.schema;
        }
        entry.ty = Some(std::mem::replace(&mut domain.ty, entry.ty.take().unwrap()));
        entry.retired_module = Some(std::mem::replace(&mut domain.module, batch.module.clone()));
        domain.code += 1;
        periodic.commit(domain);
        if domain.source_module != 0 {
            domain.source_module = code::module_key(&batch.module);
        }
    }
    let old_entry = engine.install_staged(batch.module.clone(), batch.next_version);
    drop(domains);
    drop(periodic);
    for timers in timers {
        timers::invalidate_schema(registry, timers);
    }
    drop((batch, old_entry));
    Ok(())
}

impl Batch {
    pub(crate) fn holds_module(&self, key: usize) -> bool {
        code::module_key(&self.original) == key || code::module_key(&self.module) == key
    }
}

#[cfg(test)]
#[path = "batch_tests.rs"]
mod tests;
