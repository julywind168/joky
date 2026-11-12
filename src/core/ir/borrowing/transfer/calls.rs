//! Ordinary call effects and returned reference provenance.
use super::*;

impl Instruction<'_> {
    pub(super) fn call(
        &self,
        name: &str,
        args: &[Option<Slot>],
        result: &Option<Slot>,
        state: &mut State,
        alive: &Set,
    ) -> Result<(), Error> {
        let callee = &self.facts.functions[name];
        let loan = self.loan();
        if callee.suspends
            && alive.iter().any(|index| {
                self.facts.types[index].is_buffer_view()
                    && !self.facts.types[index].is_suspend_safe_view()
                    && state.get(index).is_some_and(|value| {
                        !value.invalid && value.has_root(self.facts.owner_roots)
                    })
            })
        {
            return Err(type_error(
                self.span(),
                ErrorCode::InvalidCall,
                "A borrowed text view cannot remain live across suspension",
            ));
        }
        let arguments = call_arguments(callee, name, args);
        let noescape = |index: Option<usize>| {
            index.map_or(callee.noescape, |index| callee.parameter_noescape(index))
        };
        let mut noescape_inputs: Set = arguments
            .iter()
            .copied()
            .filter(|(index, _)| noescape(*index))
            .flat_map(|(_, arg)| arg.iter().flat_map(refs).map(|slot| slot.index))
            .collect();
        // An aliased argument passed to an unpromised parameter still
        // escapes, even when another parameter promises noescape.
        for slot in arguments
            .iter()
            .copied()
            .filter(|(index, _)| !noescape(*index))
            .flat_map(|(_, arg)| arg.iter().flat_map(refs))
        {
            noescape_inputs.remove(&slot.index);
        }
        let retained_inputs: Set = arguments
            .iter()
            .copied()
            .filter(|(index, _)| {
                index.map_or(!callee.noescape, |index| callee.parameter_retained(index))
            })
            .flat_map(|(_, arg)| arg.iter().flat_map(refs).map(|slot| slot.index))
            .collect();
        // Returning a view and saving one are distinct effects. Unknown
        // host/indirect calls remain conservative, including wrappers
        // that never request an arena. Text views have a checked ABI.
        if !callee.extern_abi.has_text() {
            for arg in args.iter().flatten().flat_map(refs) {
                if retained_inputs.contains(&arg.index) {
                    self.record_escape(state.get(&arg.index), false);
                }
            }
        }
        let inputs: Vec<_> = args
            .iter()
            .flatten()
            .flat_map(refs)
            .map(|mut slot| {
                slot.ty = self.facts.types[&slot.index].clone();
                slot
            })
            .collect();
        for arg in &inputs {
            self.check_access(arg, exclusive_access(&arg.ty), state, self.live, None, &[])?;
        }
        for &(source, destination) in &callee.inferred_arena_retained {
            let arenas: Set = arguments
                .iter()
                .filter(|(parameter, _)| *parameter == Some(destination))
                .flat_map(|(_, argument)| argument.iter().flat_map(refs))
                .flat_map(|slot| state[&slot.index].arenas.iter().copied())
                .collect();
            for input in arguments
                .iter()
                .filter(|(parameter, _)| *parameter == Some(source))
                .flat_map(|(_, argument)| argument.iter().flat_map(refs))
            {
                self.check_arena_storage(&input, &arenas, state, true)?;
                let value = state[&input.index].clone();
                for arena in &arenas {
                    retain_arena(state, *arena, (loan, input.index), value.clone());
                }
            }
        }
        // Unknown retention must never turn an explicit-region reference
        // into task-lived storage, including allocation-free host wrappers.
        for arg in &inputs {
            if retained_inputs.contains(&arg.index)
                && !arg.ty.is_owner()
                && !state[&arg.index].arenas.is_empty()
            {
                self.check_storage(arg, state)?;
            }
        }
        // Writable reference-element buffers impose a storage contract
        // even in allocation-free helpers (notably set). This follows
        // the signature, so aliases/generics/wrappers cannot hide it.
        if callee.uses_arena || inputs.iter().any(|arg| writable_buffer(&arg.ty)) {
            for arg in &inputs {
                // Owner/view types and fixed-array references cannot be
                // stored: allocation does not turn their temporary loans
                // into task-long retention. Return dependencies still
                // carry their full origins through indirect calls.
                let symbolic_borrow = arg.ty.reference_type().is_some_and(|r| {
                    r.target.scalar_types().iter().any(|ty| {
                        matches!(ty, Type::Parameter(_) | Type::Projection(_)) && ty.is_owner()
                    })
                });
                if symbolic_borrow {
                    // An unknown pointee may specialize to ordinary Copy
                    // storage, so its borrow can be retained. Keep that
                    // possibility in the effect summary without treating
                    // the symbolic owner lane as concrete buffer storage.
                    if self.facts.escaped.is_none()
                        && retained_inputs.contains(&arg.index)
                        && state[&arg.index].has_root(self.facts.argument_roots)
                    {
                        return Err(type_error(
                            self.span(),
                            ErrorCode::InvalidAssignment,
                            "@noescape function cannot retain an argument borrow",
                        ));
                    }
                    continue;
                }
                if arg.ty.contains_buffers()
                    || arg.ty.contains_place_storage()
                    || !retained_inputs.contains(&arg.index)
                {
                    continue;
                }
                self.check_storage(arg, state)?;
                let value = state[&arg.index].clone();
                retain(
                    &mut state.retained,
                    (loan, arg.index),
                    value,
                    arg.ty.reference_type().unwrap().mutable,
                );
            }
        }
        let contents = union(inputs.iter().filter_map(|arg| {
            if !arg.ty.buffer_elements().is_empty() {
                Some(buffer_contents(state, arg))
            } else if noescape_inputs.contains(&arg.index)
                && arg
                    .ty
                    .reference_type()
                    .is_some_and(|r| !r.mutable && !r.target.contains_references())
            {
                // A shared, nonescaping lookup key cannot become a stored
                // element loan just because another parameter is writable.
                None
            } else {
                Some(state[&arg.index].clone())
            }
        }));
        for arg in inputs.iter().filter(|arg| writable_buffer(&arg.ty)) {
            let identity = state[&arg.index].clone();
            add_contents(state, &identity, &contents);
        }
        for mut dst in result.iter().flat_map(refs) {
            dst.ty = self.facts.types[&dst.index].clone();
            let mutable = exclusive_access(&dst.ty);
            // A proved independent callback lane and its exclusive cursor must
            // retain distinct origins when returned in the same packet.
            let independent_capture = !mutable && callee.cursor_return_independent;
            // A returned borrow does not point into an owning sibling lane.
            // Keep fresh view identities outside owner and stored-content roots.
            let origin = if dst.ty.is_owner() {
                loan
            } else {
                2 * self.facts.plan.slot_count
                    + self.facts.plan.ops.len() * (1 + usize::from(independent_capture))
                    + self.pc
            };
            let mut value = Reference {
                origins: BTreeSet::from([Origin::new(origin)]),
                loans: Set::from([loan]),
                storage: dst.ty.is_owner() || dst.ty.is_buffer_view(),
                // Unknown ordinary host references retain their existing ABI
                // contract. Unknown owner views must remain conservative.
                interior: !dst.ty.is_owner()
                    && callee.returns_interior
                    && (callee.inferred_retained.is_some() || dst.ty.is_buffer_view()),
                ..Default::default()
            };
            for arg in &inputs {
                if noescape_inputs.contains(&arg.index) && !arg.ty.is_owner() {
                    continue;
                }
                if independent_capture && arg.ty.reference_type().is_some_and(|r| r.mutable) {
                    continue;
                }
                let source = if dst.ty.is_owner() {
                    // Owner identity flows only from consumed owners.
                    if !arg.ty.is_owner() {
                        continue;
                    }
                    state[&arg.index].clone()
                } else if dst.ty.is_buffer_view() {
                    // Preserve the existing signature-wide dependency
                    // contract for returned views, including ordinary
                    // reference inputs. Owners still cannot lend a view.
                    if arg.ty.is_owner() {
                        continue;
                    }
                    state[&arg.index].clone()
                } else if reference_buffer(&arg.ty) {
                    // Shared element values can outlive this owner/view;
                    // they never grant exclusive access to the referent.
                    if mutable {
                        continue;
                    }
                    buffer_contents(state, arg)
                } else {
                    state[&arg.index].clone()
                };
                value.arenas.extend(&source.arenas);
                value.storage |= !source.arenas.is_empty();
                // An Arena parameter contributes a lifetime, not object
                // identity: every allocation through it remains disjoint.
                if explicit_arena(&arg.ty) && !explicit_arena(&dst.ty) {
                    continue;
                }
                if !mutable || exclusive_access(&arg.ty) {
                    value.origins.extend(source.origins.iter().cloned());
                    value.storage |= source.storage;
                    value.interior |= source.interior;
                }
                value.ancestors.extend(&source.ancestors);
                value.ancestors.extend(&source.loans);
            }
            if dst.ty.is_owner() && reference_buffer(&dst.ty) {
                add_contents(state, &value, &contents);
            }
            state.insert(dst.index, value);
        }
        for arg in inputs.iter().filter(|arg| arg.ty.is_owner()) {
            if arg.ty.is_explicit_arena() {
                let roots: Vec<_> = state[&arg.index]
                    .origins
                    .iter()
                    .map(|origin| origin.root)
                    .collect();
                for root in roots {
                    state.arena_retained.remove(&root);
                }
            }
            state.get_mut(&arg.index).unwrap().invalidate(self.pc);
        }
        Ok(())
    }
}
