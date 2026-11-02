//! Conservative thread qualification of instantiated types and concrete code.
//! Evidence is independent of execution mode; Local modules remain loadable.
use super::{ModulePlan, Op};
use crate::{Error, ErrorCode, ErrorPhase, Type};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

mod callables;
mod contracts;
pub(crate) mod packet;
pub mod report;
pub(crate) mod scoped;

fn display_name(name: &str) -> String {
    if name.starts_with("__joky_task_") {
        "<branch body>".into()
    } else if let Some((_, template)) = name
        .strip_prefix("__joky_generic_")
        .and_then(|suffix| suffix.split_once('_'))
    {
        format!("{template}<…>")
    } else {
        name.into()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Qualification {
    pub transfer: bool,
    pub share: bool,
    /// Usable by a checked call on the owning worker, including local borrows.
    pub worker_local: bool,
}

/// This is a type/storage property, not a proof that a particular pointer or
/// callable is transferable. Packet admission must additionally prove lifetime,
/// code ownership, destructor effects and exclusive/shared access.
fn qualify_with(ty: &Type, resolve: &impl Fn(&Type) -> Result<Type, Error>) -> Qualification {
    fn check(
        root: &Type,
        shared: bool,
        worker_local: bool,
        resolve: &impl Fn(&Type) -> Result<Type, Error>,
    ) -> bool {
        let mut pending = vec![(root.clone(), shared)];
        let mut seen = HashSet::new();
        let mut retained = Vec::new();
        while let Some((ty, share)) = pending.pop() {
            let Ok(ty) = resolve(&ty) else {
                return false;
            };
            let key = match &ty {
                Type::Record(r) => Some((0, std::sync::Arc::as_ptr(r) as usize, share)),
                Type::Enum(e) => Some((1, std::sync::Arc::as_ptr(e) as usize, share)),
                Type::Tuple(t) => Some((2, std::sync::Arc::as_ptr(t) as usize, share)),
                Type::FixedArray(a) => Some((3, std::sync::Arc::as_ptr(a) as usize, share)),
                _ => None,
            };
            if key.is_some_and(|key| !seen.insert(key)) {
                continue;
            }
            if key.is_some() {
                retained.push(ty.clone());
            }
            if seen.len() > 65_536 {
                return false;
            }
            match ty {
                Type::Reference(r) => {
                    if !worker_local && share && r.mutable {
                        return false;
                    }
                    // Across threads, moving a shared reference still requires
                    // Share. Worker-local calls use ordinary checked loans.
                    pending.push((r.target.clone(), !worker_local && !r.mutable));
                }
                Type::Buffer(element)
                | Type::BufferElement(element)
                | Type::BufferSlice(element) => pending.push(((*element).clone(), share)),
                // These descriptors borrow worker-owned storage and mutate local
                // cursor position. This grants neither Transfer nor Share.
                Type::BufferIterator(element)
                | Type::BufferPairIterator(element)
                | Type::BufferEntry(element)
                    if worker_local =>
                {
                    pending.push(((*element).clone(), false));
                }
                Type::FixedArray(a) => pending.push((a.element.clone(), share)),
                Type::Tuple(t) => pending.extend(t.elements.iter().cloned().map(|ty| (ty, share))),
                Type::Record(r)
                    if r.closure.as_ref().is_none_or(|c| c.storable)
                        && (worker_local || !share || r.closure.is_none()) =>
                {
                    pending.extend(
                        r.arguments
                            .iter()
                            .filter_map(|a| a.as_type())
                            .cloned()
                            .map(|ty| (ty, share)),
                    );
                    pending.extend(r.fields.iter().map(|f| (f.ty.clone(), share)));
                }
                Type::Enum(e) => {
                    pending.extend(
                        e.arguments
                            .iter()
                            .filter_map(|a| a.as_type())
                            .cloned()
                            .map(|ty| (ty, share)),
                    );
                    pending.extend(
                        e.variants
                            .iter()
                            .filter_map(|v| v.payload.clone())
                            .map(|ty| (ty, share)),
                    );
                }
                Type::Resource(ref name)
                    if worker_local
                        && matches!(name.as_ref(), "arena.Arena" | "shutdown.SignalsOwner") => {}
                Type::Resource(ref name)
                    if !share
                        && (crate::runtime::tcp::transferable_type(name)
                            || crate::runtime::parallel::control::resource_type(name)
                            || crate::runtime::buffer::crypto::transferable_type(name)
                            || crate::runtime::shutdown::transferable_type(name)) => {}
                Type::Named(ref name)
                    if matches!(name.as_str(), "domain.Handle" | "admission.Token") => {}
                Type::String | Type::Function(_) => {}
                Type::DynTrait(_) if worker_local || !share => {}
                ref scalar if crate::core::instance::scalar(scalar) => {}
                // Cursor aliases mutate shared position. Raw, erased, unresolved
                // and thread-bound capabilities never acquire implicit evidence.
                _ => return false,
            }
        }
        true
    }
    Qualification {
        transfer: check(ty, false, false, resolve),
        share: check(ty, true, false, resolve),
        worker_local: check(ty, false, true, resolve),
    }
}
#[cfg(test)]
fn qualify(ty: &Type) -> Qualification {
    qualify_with(ty, &|ty| Ok(ty.clone()))
}

#[derive(Clone, Default)]
pub(crate) struct Effects {
    rejected: HashMap<String, Error>,
    known: HashSet<String>,
    packets: HashSet<String>,
}
impl Effects {
    pub(crate) fn require_packet(&self, entry: &str) -> Result<(), Error> {
        self.require(entry)?;
        if !self.packets.contains(entry) {
            return Err(Error::new_with_code(
                ErrorPhase::Abi,
                ErrorCode::InvalidCall,
                format!("Parallel entry `{entry}` has unsupported packet types"),
            ));
        }
        Ok(())
    }
    pub(crate) fn require(&self, entry: &str) -> Result<(), Error> {
        if let Some(error) = self.rejected.get(entry) {
            return Err(error.clone());
        }
        if !self.known.contains(entry) {
            return Err(Error::new_with_code(
                ErrorPhase::Abi,
                ErrorCode::InvalidCall,
                format!("Unknown parallel entry `{entry}`"),
            ));
        }
        Ok(())
    }
}

/// Only binding admission may supply `hosts`; legacy extern registrations never
/// populate it. Indirect signatures are not executable targets or certificates.
pub(crate) fn analyse(
    plan: &ModulePlan,
    prepared: &crate::core::prepared::PreparedModule,
    hosts: &HashSet<String>,
) -> Effects {
    let nominals = crate::core::types::nominal_types(prepared.types(), prepared.ast());
    let resolve = |ty: &Type| match &nominals {
        Ok(nominals)
            if matches!(ty, Type::Record(r) if r.indirect)
                || matches!(ty, Type::Enum(e) if e.indirect) =>
        {
            crate::core::types::reference_target(ty, nominals, prepared.types())
        }
        Ok(_) => Ok(ty.clone()),
        Err(error) => Err(error.clone()),
    };
    // Function values can only originate at compiler-authored FunctionRef sites.
    // Keep a closed set of targets per calling signature in this frozen module;
    // an indirect edge depends on every possible target, including cleanup effects.
    // Host boundaries below cannot introduce new callable values.
    let targets = callables::Targets::new(plan);
    let dyn_tables: Vec<_> = plan
        .bodies
        .values()
        .flat_map(|b| &b.ops)
        .filter_map(|op| {
            if let Op::DynTable { table, .. } = op {
                Some(table)
            } else {
                None
            }
        })
        .collect();
    let mut reasons = BTreeMap::<String, String>::new();
    let mut edges = BTreeMap::<String, BTreeSet<String>>::new();
    let mut packets = HashSet::new();
    let mut names: Vec<_> = plan.functions.keys().cloned().collect();
    names.sort();
    for name in &names {
        let info = &plan.functions[name];
        let signature = &info.signature;
        if signature
            .params
            .iter()
            .chain(std::iter::once(&signature.ret))
            .any(|ty| {
                // Ordinary calls stay on their worker, including borrowed
                // cursors and views. Packet admission below remains separate.
                !qualify_with(ty, &resolve).worker_local
            })
        {
            reasons.insert(
                name.clone(),
                "input or result has Local storage or an unqualified callable".into(),
            );
        }
        let Some(body) = plan.bodies.get(name) else {
            use crate::core::types::walk::{TypeEdges, TypeWalk};
            let callable = signature
                .params
                .iter()
                .chain(std::iter::once(&signature.ret))
                .any(|ty| {
                    TypeWalk::new(ty, TypeEdges::Fields)
                        .any(|ty| matches!(ty, Type::Function(_)) || ty.is_closure_value())
                });
            let builtin = contracts::builtin(name, signature, info.suspends).is_some();
            if callable || (!hosts.contains(name) && !builtin) {
                reasons.insert(
                    name.clone(),
                    "host binding has no explicit ParallelSafe contract".into(),
                );
            }
            continue;
        };
        let dependencies = edges.entry(name.clone()).or_default();
        // A transferable environment keeps its exact compiler-created target,
        // even when the receiving helper only stores/drops it without calling it.
        // Fn/FnMut/FnOnce specialization uses direct calls, not forged pointers.
        for ty in signature
            .params
            .iter()
            .chain(std::iter::once(&signature.ret))
        {
            for ty in crate::core::types::walk::TypeWalk::new(
                ty,
                crate::core::types::walk::TypeEdges::Fields,
            ) {
                if let Some(closure) = ty.closure_info() {
                    dependencies.insert(closure.entry.clone());
                }
                if let Some(interface) = ty.dyn_interface() {
                    let candidates: Vec<_> = dyn_tables
                        .iter()
                        .filter(|t| t.interface == *interface)
                        .collect();
                    if candidates.is_empty()
                        || candidates
                            .iter()
                            .any(|t| !qualify_with(&t.self_type, &resolve).worker_local)
                    {
                        reasons.insert(
                            name.clone(),
                            "dynamic target lacks immutable thread evidence".into(),
                        );
                    }
                    for table in candidates {
                        dependencies.extend(table.methods.iter().cloned());
                    }
                }
            }
        }
        if signature
            .params
            .iter()
            .chain(std::iter::once(&signature.ret))
            .all(|ty| {
                !ty.contains_dyn()
                    && packet::classify(ty, &resolve, &|ty| body.has_custom_drop(ty)).is_some()
            })
        {
            packets.insert(name.clone());
        }
        for op in &body.ops {
            dependencies.extend(op.dependencies().cloned());
            let branch_supported = |name: &String| {
                plan.functions.get(name).is_some_and(|f| {
                    let signature = scoped::signature(plan, name);
                    let owned = |ty: &Type| {
                        packet::classify(ty, &resolve, &|_| false)
                            .is_some_and(packet::Kind::implemented)
                    };
                    signature.params.iter().all(|ty| {
                        // Cursor references and thread-bound capabilities
                        // retain their existing dynamic-capture rejection.
                        owned(ty)
                            || (ty.reference_type().is_some()
                                && (qualify_with(ty, &resolve).transfer
                                    || crate::runtime::parallel::scoped::Shape::of(ty).is_some()))
                    }) && owned(&f.signature.ret)
                })
            };
            let mut indirects = Vec::new();
            match op {
                Op::Call {
                    indirect: Some(slot),
                    ..
                } => indirects.push(slot),
                Op::Concurrent { branches, .. } => {
                    indirects.extend(branches.iter().filter_map(|b| b.indirect.as_ref()));
                }
                _ => {}
            }
            for slot in indirects {
                let Type::Function(function) = &slot.ty else {
                    reasons.entry(name.clone()).or_insert_with(|| {
                        format!(
                            "indirect target `{}` lacks immutable thread evidence",
                            slot.ty
                        )
                    });
                    continue;
                };
                let mut found = false;
                for target in targets.matching(function) {
                    found = true;
                    dependencies.insert(target.to_owned());
                }
                if !found {
                    reasons.entry(name.clone()).or_insert_with(|| {
                        format!(
                            "indirect target `{}` lacks immutable thread evidence",
                            slot.ty
                        )
                    });
                }
            }
            let local = match op {
                Op::DynMethod { dst, .. }
                    if matches!(&dst.ty, Type::Function(f) if f.suspends) =>
                {
                    Some("suspending dyn dispatch is not supported in Parallel")
                }
                Op::Concurrent { branches, .. }
                    if branches.iter().any(|branch| {
                        let signature = match branch.indirect.as_ref().map(|slot| &slot.ty) {
                            Some(Type::Function(f)) => &f.signature,
                            Some(_) => return true,
                            None => &plan.functions[&branch.name].signature,
                        };
                        let owned = |ty| {
                            packet::classify(ty, &resolve, &|_| false)
                                .is_some_and(packet::Kind::implemented)
                        };
                        !signature
                            .params
                            .iter()
                            .all(|ty| ty.reference_type().is_some() || owned(ty))
                            || !owned(&signature.ret)
                    }) =>
                {
                    Some("parallel group requires supported owned packets or worker-local borrows")
                }
                Op::TaskBranch { entry, .. } if !branch_supported(entry) => {
                    Some("parallel branch captures require supported owned packets or worker-local borrows")
                }
                Op::DynTable { table, .. }
                    if !qualify_with(&table.self_type, &resolve).worker_local =>
                {
                    Some("dynamic target has Local concrete storage")
                }
                Op::FunctionRef(slot, _) if !matches!(slot.ty, Type::Function(_)) => {
                    Some("raw function address lacks immutable thread evidence")
                }
                Op::DomainModule(_) | Op::OutboxEmit { .. } => {
                    Some("state domain operation is Local")
                }
                _ => None,
            };
            if let Some(reason) = local {
                reasons.entry(name.clone()).or_insert_with(|| reason.into());
            }
        }
    }
    // Monotone rejection reaches a fixed point even for mutually recursive SCCs.
    // Unknown callees cannot acquire evidence from a traversal's visiting set.
    loop {
        let mut changed = false;
        for (name, dependencies) in &edges {
            if reasons.contains_key(name) {
                continue;
            }
            if let Some(dependency) = dependencies.iter().find(|dependency| {
                reasons.contains_key(*dependency) || !plan.functions.contains_key(*dependency)
            }) {
                reasons.insert(
                    name.clone(),
                    format!(
                        "callee or cleanup `{dependency}` requires Local execution: {}",
                        reasons
                            .get(dependency)
                            .map_or("unknown target", String::as_str)
                    ),
                );
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    Effects {
        packets,
        known: names.into_iter().collect(),
        rejected: reasons
            .into_iter()
            .map(|(name, reason)| {
                let error = Error::new_with_code(
                    ErrorPhase::Abi,
                    ErrorCode::InvalidCall,
                    format!("Parallel entry `{name}` rejected: {reason}"),
                )
                .with_span(plan.functions[&name].span);
                (name, error)
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parallel_map_protocols_follow_concrete_generic_call_edges() {
        let source = r#"
            import joky/map.{type Map}
            import joky/set.{type Set}
            import joky/eq.{type Eq, type BorrowEq}
            import joky/hash.{type Hash, type BorrowHash}
            extern fn local(value: Int32) -> UInt64
            struct BadHash { id: Int32 }
            struct BadEq { id: Int32 }
            impl BorrowEq for BadHash {
                fn eq_ref(&self, other: &Self) -> Bool { self.id == other.id }
            }
            impl BorrowHash for BadHash {
                fn hash_ref(&self) -> UInt64 { local(self.id) }
            }
            impl BorrowEq for BadEq {
                fn eq_ref(&self, other: &Self) -> Bool { local(self.id) == local(other.id) }
            }
            impl BorrowHash for BadEq {
                fn hash_ref(&self) -> UInt64 { 7 }
            }
            struct ValueHash { id: Int32 }
            struct ValueEq { id: Int32 }
            impl BorrowEq for ValueHash {
                fn eq_ref(&self, other: &Self) -> Bool { self.id == other.id }
            }
            impl Hash for ValueHash { fn hash(self) -> UInt64 { local(self.id) } }
            impl Eq for ValueEq { fn eq(self, other: Self) -> Bool { local(self.id) == local(other.id) } }
            impl BorrowHash for ValueEq { fn hash_ref(&self) -> UInt64 { 7 } }
            fn hash_value_map(value: Map<ValueHash, Int32>) @suspends -> Bool {
                let key = ValueHash { id: 1 }
                value.contains_key_by_ref(&key)
            }
            fn eq_value_set(value: Set<ValueEq>) @suspends -> Bool {
                let key = ValueEq { id: 1 }
                value.contains(&key)
            }
            fn carry(value: Map<BadHash, Int32>) @suspends -> Map<BadHash, Int32> { value }
            fn hash_map(value: Map<BadHash, Int32>) @suspends -> Bool {
                let key = BadHash { id: 1 }
                value.contains_key_by_ref(&key)
            }
            fn eq_set(value: Set<BadEq>) @suspends -> Bool {
                let key = BadEq { id: 1 }
                value.contains(&key)
            }
            fn query<K: BorrowHash + BorrowEq, V>(value: &Map<K, V>, key: &K) @noescape(key) -> Bool {
                value.contains_key_by_ref(key)
            }
            fn borrowed_hash(value: &Map<BadHash, Int32>) @suspends -> Bool {
                let key = BadHash { id: 1 }
                query(value, &key)
            }
            fn borrowed_eq(value: &Set<BadEq>) @suspends -> Bool {
                let key = BadEq { id: 1 }
                value.contains(&key)
            }
            fn borrowed_branches() @suspends -> Bool {
                let value = Map.new::<BadHash, Int32>()
                let (a, b) = join { | borrowed_hash(&value) | borrowed_hash(&value) }
                a
            }
            fn child(value: Map<BadHash, Int32>) @suspends -> Bool { hash_map(value) }
            fn branches(value: Map<BadHash, Int32>) @suspends -> Bool {
                let (a, b) = join { | child(value) | child(Map.new::<BadHash, Int32>()) }
                a
            }
        "#;
        let types = crate::TypeRegistry::default().with_prelude(false);
        let ast = types
            .resolve_with_modules(&crate::parser::parse(source).unwrap(), &Default::default())
            .unwrap();
        let prepared =
            crate::core::prepared::PreparedModule::new(&ast, &types, &Default::default()).unwrap();
        let plan = prepared.plan(&Default::default()).unwrap();
        for name in ["borrowed_hash", "borrowed_eq"] {
            let signature = scoped::signature(&plan, name);
            assert!(crate::runtime::parallel::scoped::Shape::of(&signature.params[0]).is_some());
        }
        let effects = analyse(&plan, &prepared, &HashSet::new());
        effects.require("carry").unwrap();
        effects.require_packet("carry").unwrap();
        for name in [
            "hash_map",
            "eq_set",
            "hash_value_map",
            "eq_value_set",
            "branches",
            "borrowed_hash",
            "borrowed_eq",
            "borrowed_branches",
        ] {
            assert!(effects.require(name).is_err(), "{name}");
        }
        let admitted = analyse(&plan, &prepared, &HashSet::from(["local".into()]));
        for name in [
            "hash_map",
            "eq_set",
            "hash_value_map",
            "eq_value_set",
            "branches",
            "borrowed_hash",
            "borrowed_eq",
            "borrowed_branches",
        ] {
            admitted.require(name).unwrap();
        }
    }

    #[test]
    fn parallel_indirect_evidence_includes_every_frozen_target() {
        let source = r#"
            extern fn local(value: Int32) -> Int32
            fn safe(value: Int32) -> Int32 { value + 1 }
            fn bad(value: Int32) -> Int32 { local(value) }
            fn choose() -> fn(Int32) -> Int32 { bad }
            fn apply(f: fn(Int32) -> Int32) -> Int32 { f(41) }
            fn root() @suspends -> Int32 { apply(safe) }
        "#;
        let types = crate::TypeRegistry::default().with_prelude(false);
        let ast = types
            .resolve_with_modules(&crate::parser::parse(source).unwrap(), &Default::default())
            .unwrap();
        let prepared =
            crate::core::prepared::PreparedModule::new(&ast, &types, &Default::default()).unwrap();
        let plan = prepared.plan(&Default::default()).unwrap();
        let local = analyse(&plan, &prepared, &HashSet::new());
        local.require("safe").unwrap();
        // There is no direct edge from apply to bad. The immutable target set
        // must still include bad, even though this particular root passes safe.
        assert!(local.require("apply").is_err());
        assert!(local.require("root").is_err());
        let qualified = analyse(&plan, &prepared, &HashSet::from(["local".into()]));
        qualified.require_packet("root").unwrap();
    }

    #[test]
    fn parallel_qualification_distinguishes_storage_and_call_effects() {
        assert_eq!(
            qualify(&Type::Int32),
            Qualification {
                transfer: true,
                share: true,
                worker_local: true,
            }
        );
        let arena = Type::Resource("arena.Arena".into());
        for ty in [arena.clone(), Type::reference(arena, false)] {
            assert_eq!(
                qualify(&ty),
                Qualification {
                    transfer: false,
                    share: false,
                    worker_local: true
                }
            );
            assert!(packet::classify(&ty, &|ty| Ok(ty.clone()), &|_| false).is_none());
        }
        assert!(!qualify(&Type::Ptr).transfer);
        assert!(!qualify(&Type::Resource("HostResource".into())).share);
        for name in [
            "shutdown.ControllerOwner",
            "crypto.Sha256Owner",
            "crypto.HmacSha256Owner",
            "tcp.StreamOwner",
            "tcp.ListenerOwner",
            "tcp.ReaderOwner",
            "tcp.WriterOwner",
            "vm.Owner",
            "fs.FileOwner",
            "domain.DirectoryOwner",
            "outbox.QueueOwner",
            "admission.PermitOwner",
        ] {
            let resource = Type::Resource(name.into());
            assert!(qualify(&resource).transfer);
            assert!(crate::runtime::parallel::packet::Shape::of(&resource).is_ok());
            assert!(!qualify(&resource).share);
            assert!(!qualify(&Type::reference(resource, false)).transfer);
        }
        for name in ["HostResource", "domain.StateOwner", "shutdown.SignalsOwner"] {
            assert!(!qualify(&Type::Resource(name.into())).transfer);
        }
        assert!(qualify(&Type::Resource("shutdown.SignalsOwner".into())).worker_local);
        assert!(!qualify(&Type::Resource("shutdown.SignalsOwner".into())).share);
        for cursor in [
            Type::BufferIterator(std::sync::Arc::new(Type::Int32)),
            Type::BufferPairIterator(std::sync::Arc::new(Type::tuple(vec![
                Type::Int32,
                Type::Int32,
            ]))),
            Type::BufferEntry(std::sync::Arc::new(Type::tuple(vec![
                Type::Int32,
                Type::Int32,
            ]))),
        ] {
            let evidence = qualify(&cursor);
            assert!(evidence.worker_local);
            assert!(!evidence.transfer && !evidence.share);
            for mutable in [false, true] {
                let reference = Type::reference(cursor.clone(), mutable);
                assert!(qualify(&reference).worker_local);
                assert!(!qualify(&reference).transfer);
                assert!(packet::classify(&reference, &|ty| Ok(ty.clone()), &|_| false).is_none());
            }
        }
        assert!(!qualify(&Type::BufferIterator(std::sync::Arc::new(Type::Ptr))).worker_local);
        assert!(qualify(&Type::reference(Type::Int32, false)).share);
        assert!(!qualify(&Type::reference(Type::Int32, true)).share);
        assert!(!qualify(&Type::reference(Type::reference(Type::Int32, true), false)).transfer);
        let source = r#"
            import joky/drop.{type Drop}
            extern fn local(value: Int32) -> Int32
            struct Box<T> { value: T }
            fn scalar_box(value: Box<Int32>) -> Int32 { value.value }
            fn opaque_box(value: Box<Ptr>) { }
            struct Tracked { value: Int32, text: String }
            impl Drop for Tracked { fn drop(&mut self) { local(self.value); } }
            fn destructor() @suspends { let value = Tracked { value: 1, text: "owned" }; }
            fn inspect(value: &Tracked) @suspends -> Int32 { value.value }
            fn destructor_borrow() @suspends -> Int32 {
                let value = Tracked { value: 1, text: "owned" }
                let (a, b) = join { | inspect(&value) | inspect(&value) }; a + b
            }
            fn even(n: Int32) -> Int32 { if n == 0 { 42 } else { odd(n - 1) } }
            fn odd(n: Int32) -> Int32 { even(n - 1) }
            fn bad_a(n: Int32) -> Int32 { if n == 0 { local(n) } else { bad_b(n - 1) } }
            fn bad_b(n: Int32) -> Int32 { bad_a(n) }
            fn children() @suspends -> Int32 { let (a, b) = join { | first() | second() }; a + b }
            fn first() @suspends -> Int32 { even(2) }
            fn second() @suspends -> Int32 { bad_b(2) }
            fn indirect(f: fn(Int32) -> Int32) -> Int32 { f(2) }
        "#;
        let types = crate::TypeRegistry::default().with_prelude(false);
        let ast = types
            .resolve_with_modules(&crate::parser::parse(source).unwrap(), &Default::default())
            .unwrap();
        let prepared =
            crate::core::prepared::PreparedModule::new(&ast, &types, &Default::default()).unwrap();
        let plan = prepared.plan(&Default::default()).unwrap();
        let effects = analyse(&plan, &prepared, &HashSet::new());
        for name in ["even", "odd", "first", "scalar_box"] {
            effects.require(name).unwrap();
        }
        for name in [
            "local",
            "bad_a",
            "bad_b",
            "second",
            "children",
            "indirect",
            "opaque_box",
            "destructor",
            "destructor_borrow",
        ] {
            assert!(effects.require(name).is_err(), "{name}");
        }
        let effects = analyse(&plan, &prepared, &HashSet::from(["local".into()]));
        effects.require("children").unwrap();
        effects.require("destructor_borrow").unwrap();
        assert!(effects.require("indirect").is_err());
    }
}
