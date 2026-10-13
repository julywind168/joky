//! HR5-A: one Script-owned scalar domain, staged code and an independent new root.
use super::*;
use crate::core::state_domain::{migration_eligible, scalar_record};
use std::any::Any;

pub(crate) struct PreparedMigration {
    registry: Weak<RefCell<Domains>>,
    target: DomainHandle,
    reads: reads::ReadSet,
    original: LoadedModule,
    module: LoadedModule,
    next_version: u64,
    ty: Type,
    root: Root,
    schema: u64,
    _charge: reads::Charge,
}
fn selected(domains: &Domains, owners: &[Rc<dyn Any>]) -> Result<DomainHandle, Error> {
    if domains.shutting_down {
        return Err(invalid("Migration executor is closed"));
    }
    let mut selected = None;
    for (&sequence, domain) in &domains.entries {
        if domain.closing
            || !owners.iter().any(|owner| {
                Rc::as_ptr(owner) as *const () as usize == code::module_key(&domain.module)
            })
        {
            continue;
        }
        if selected.is_some() {
            return Err(invalid(
                "Single-domain migration requires exactly one live Script domain",
            ));
        }
        selected = Some(DomainHandle {
            registry: domains.identity,
            sequence,
            writable: true,
        });
    }
    let target = selected.ok_or_else(|| invalid("Migration requires one live Script domain"))?;
    domains.get(target)?;
    Ok(target)
}
pub(crate) fn schema(
    registry: &Rc<RefCell<Domains>>,
    owners: &[Rc<dyn Any>],
) -> Result<u64, Error> {
    let domains = registry.borrow();
    let target = selected(&domains, owners)?;
    Ok(domains.get(target)?.schema)
}
fn old_view(ty: &Type) -> Result<String, Error> {
    if !scalar_record(ty) {
        return Err(invalid("HR5-A migration requires flat scalar record roots"));
    }
    let Type::Record(record) = ty else {
        unreachable!()
    };
    let mut source = String::from("\nstruct PreviousState {\n");
    for field in &record.fields {
        source.push_str(&format!("{}: {},\n", field.name, field.ty));
    }
    source.push_str("}\n");
    Ok(source)
}
fn matches_view(old: &Type, view: &Type) -> bool {
    let (Type::Record(old), Type::Record(view)) = (old, view) else {
        return false;
    };
    view.name == "PreviousState"
        && old.fields.len() == view.fields.len()
        && old
            .fields
            .iter()
            .zip(&view.fields)
            .all(|(a, b)| a.name == b.name && a.ty == b.ty && a.offset == b.offset)
        && Type::Record(old.clone()).storage_layout().ok()
            == Type::Record(view.clone()).storage_layout().ok()
}
pub(crate) fn prepare(
    engine: &VmEngine,
    registry: &Rc<RefCell<Domains>>,
    owners: &[Rc<dyn Any>],
    expected_schema: u64,
    source: &str,
    method: &str,
) -> Result<PreparedMigration, Error> {
    let original = engine
        .current
        .clone()
        .ok_or_else(|| invalid("No Script module loaded"))?;
    let (target, old, reads, charge) = {
        let domains = registry.borrow();
        let target = selected(&domains, owners)?;
        let domain = domains.get(target)?;
        if domain.schema != expected_schema {
            return Err(invalid("Migration schema identity changed"));
        }
        let charge = reads::Charge::new(
            domains.budget.clone(),
            std::mem::size_of::<PreparedMigration>(),
        )?;
        (
            target,
            domain.ty.clone(),
            reads::ReadSet::single(target, domain.revision, domain.code, domain.schema),
            charge,
        )
    };
    // The view is generated from the captured schema, never supplied by the caller.
    // Duplicate declarations are rejected by the normal compiler.
    let archive = old_view(&old)?;
    let ast = crate::parser::parse(&format!("{source}{archive}"))?;
    let (module, next_version) = engine.stage_module(&ast)?;
    let info = module.instance_info(method)?;
    if !info.domain_entry
        || !migration_eligible(&info)
        || !matches_view(
            &old,
            &info.signature.params[0].reference_type().unwrap().target,
        )
    {
        return Err(invalid(
            "Migration requires a proved fn(&PreviousState) -> NewState entry",
        ));
    }
    let (Type::Record(before), Type::Record(after)) = (&old, &info.signature.ret) else {
        unreachable!()
    };
    if before.name != after.name || code::compatible(&old, &info.signature.ret) {
        return Err(invalid(
            "Migration must change the schema of the same nominal state type",
        ));
    }
    let root = {
        let domains = registry.borrow();
        reads
            .validate(&domains)
            .map_err(|_| invalid("Migration changed during preparation"))?;
        let domain = domains.get(target)?;
        // A shared scalar pointer is borrowed only for this synchronous, proved
        // call. All allocations and the returned root belong to the new arena.
        Root::construct(
            &module,
            &info,
            method,
            &[domain.root.as_ref().unwrap().scalar_pointer()],
            engine.interpreter_limits(),
            domains.budget.clone(),
        )?
    };
    Ok(PreparedMigration {
        registry: Rc::downgrade(registry),
        target,
        reads,
        original,
        module,
        next_version,
        ty: info.signature.ret,
        root,
        schema: source::next_token()?,
        _charge: charge,
    })
}
pub(crate) fn publish(
    engine: &mut VmEngine,
    registry: &Rc<RefCell<Domains>>,
    owners: &[Rc<dyn Any>],
    plan: PreparedMigration,
) -> Result<(), Error> {
    if !plan.registry.ptr_eq(&Rc::downgrade(registry))
        || engine
            .current
            .as_ref()
            .is_none_or(|m| !source::same_module(m, &plan.original))
    {
        return Err(invalid(
            "Migration Script binding changed before publication",
        ));
    }
    // Reserve the cleanup list before the final validation/publication interval.
    let timers = timers::schema_dependents(registry, plan.target)?;
    let mut domains = registry.borrow_mut();
    if selected(&domains, owners)? != plan.target {
        return Err(invalid("Migration domain selection changed"));
    }
    plan.reads
        .validate(&domains)
        .map_err(|_| invalid("Migration conflict: state, code or schema changed"))?;
    let domain = domains.entries.get_mut(&plan.target.sequence).unwrap();
    let revision = domain
        .revision
        .checked_add(1)
        .ok_or_else(|| limited("State domain revision exhausted"))?;
    let code = domain
        .code
        .checked_add(1)
        .ok_or_else(|| limited("State domain code version exhausted"))?;
    // Only infallible ownership moves and scalar stores from here to dropping the
    // registry borrow. Old roots and modules are retired after publication.
    let old_root = domain.root.replace(plan.root);
    let old_module = std::mem::replace(&mut domain.module, plan.module.clone());
    let old_type = std::mem::replace(&mut domain.ty, plan.ty);
    domain.revision = revision;
    domain.code = code;
    domain.schema = plan.schema;
    if domain.source_module != 0 {
        domain.source_module = code::module_key(&domain.module);
    }
    let old_entry = engine.install_staged(plan.module, plan.next_version);
    drop(domains);
    timers::invalidate_schema(registry, timers);
    drop((old_root, old_module, old_type, old_entry));
    Ok(())
}

#[cfg(test)]
mod tests;
