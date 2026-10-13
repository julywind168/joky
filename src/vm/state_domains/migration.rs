//! Single Script-owned domain migration with an independently owned candidate graph.
use super::*;
use crate::core::state_domain::migration_eligible;
mod view;
use std::any::Any;

pub(crate) struct PreparedMigration {
    registry: Weak<RefCell<Domains>>,
    target: DomainHandle,
    reads: reads::ReadSet,
    // Candidate storage is destroyed before either pinned code owner.
    root: Root,
    original: LoadedModule,
    module: LoadedModule,
    next_version: u64,
    ty: Type,
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
    let input = crate::parser::parse(source)?;
    let view = view::View::new(&old, &input.imports)?;
    let ast = crate::parser::parse(&format!(
        "{}\n{source}\n{}",
        view.imports, view.declarations
    ))?;
    let (module, next_version) = engine.stage_module(&ast)?;
    let info = module.instance_info(method)?;
    if !info.domain_entry
        || !migration_eligible(&info)
        || !view::matches(
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
        // A shared graph pointer is borrowed only for this synchronous, proved
        // call. All allocations and the returned root belong to the new arena.
        Root::construct(
            &module,
            &info,
            method,
            &[domain.root.as_ref().unwrap().borrow_pointer(&old)],
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
