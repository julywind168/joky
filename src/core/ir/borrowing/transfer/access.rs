//! Access, escape and suspension checks shared by instruction transfers.
use super::*;

impl Instruction<'_> {
    pub(super) fn record_escape(&self, value: Option<&Reference>, returned: bool) {
        if let (Some(escaped), Some(value)) = (self.facts.escaped, value) {
            let mut escaped = escaped.borrow_mut();
            if returned {
                escaped.returns_interior |= value.interior;
            }
            let roots = if returned {
                &mut escaped.returned
            } else {
                &mut escaped.retained
            };
            roots.extend(value.origins.iter().map(|origin| origin.root));
            roots.extend(&value.arenas);
        }
    }
    fn record_arena_escape(&self, value: &Reference, arenas: &Set) {
        if let Some(escaped) = self.facts.escaped {
            let mut escaped = escaped.borrow_mut();
            for arena in arenas {
                let sources = escaped.arena_retained.entry(*arena).or_default();
                sources.extend(value.origins.iter().map(|origin| origin.root));
                sources.extend(&value.arenas);
            }
        }
    }
    pub(super) fn check_access(
        &self,
        slot: &Slot,
        exclusive: bool,
        state: &State,
        alive: &Set,
        ignore: Option<usize>,
        path: &[String],
    ) -> Result<(), Error> {
        let value = state
            .get(&slot.index)
            .filter(|v| !v.invalid)
            .ok_or_else(|| self.unavailable_value(slot, state))?;
        let projection;
        let value = if path.is_empty() {
            value
        } else {
            projection = value.projected(path, self.facts.local_roots);
            &projection
        };
        let conflict = |alias: &Reference, mutable: bool| {
            if exclusive
                && explicit_arena(&slot.ty)
                && value
                    .origins
                    .iter()
                    .any(|origin| alias.arenas.contains(&origin.root))
            {
                return true;
            }
            if !value.overlaps(alias) {
                return false;
            }
            let ancestor =
                alias.loans.is_subset(&value.ancestors) && alias.loans.is_disjoint(&value.loans);
            !ancestor && (exclusive || mutable && !alias.loans.is_subset(self.facts.reservations))
        };
        for &other in alive {
            // Local storage and unique buffer owners are not competing loans.
            // Access through an owner still checks every live view; lifetime
            // markers keeping the owner live must not freeze its views.
            if other == slot.index
                || Some(other) == ignore
                || self.facts.local_places.contains(&other)
                || self.facts.types[&other].is_owner()
            {
                continue;
            }
            if let Some(alias) = state.get(&other).filter(|v| !v.invalid) {
                if conflict(
                    alias,
                    self.facts.types[&other]
                        .reference_type()
                        .is_some_and(|r| r.mutable),
                ) {
                    let ending = matches!(
                        self.facts.plan.ops.get(self.pc),
                        Some(Op::Buffer(BufferOp::End(_)) | Op::Place(PlaceOp::End(_)))
                    );
                    let message = if ending {
                        "Borrowed value would outlive its owner at the end of this scope"
                    } else {
                        "Reference access conflicts with a live borrow; finish using the borrowed view first"
                    };
                    return Err(type_error(
                        self.span(),
                        ErrorCode::InvalidAssignment,
                        message,
                    ));
                }
            }
        }
        for (alias, mutable) in state
            .retained
            .values()
            .chain(state.regions.values().flat_map(|region| region.values()))
        {
            if conflict(alias, *mutable) {
                return Err(type_error(
                    self.span(),
                    ErrorCode::InvalidAssignment,
                    "Reference access conflicts with a borrow retained by the task arena",
                ));
            }
        }
        for (arena, retained) in &state.arena_retained {
            // A region's internal references move and die with that region.
            if slot.ty.is_explicit_arena()
                && value.origins.iter().any(|origin| origin.root == *arena)
            {
                continue;
            }
            if retained
                .values()
                .any(|(alias, mutable)| conflict(alias, *mutable))
            {
                return Err(type_error(
                    self.span(),
                    ErrorCode::InvalidAssignment,
                    "Reference access conflicts with a borrow retained by an explicit arena",
                ));
            }
        }
        Ok(())
    }
    fn unavailable_value(&self, slot: &Slot, state: &State) -> Error {
        let mut error = type_error(
            self.span(),
            ErrorCode::InvalidAssignment,
            if slot.ty.is_owner() {
                "Use of a moved or uninitialized owning value"
            } else {
                "Use of a moved, sealed or uninitialized exclusive reference"
            },
        );
        if !slot.ty.is_owner() {
            return error;
        }
        let binding = self.facts.plan.diagnostic_bindings.get(&slot.index);
        if let Some((name, declaration, _)) = binding {
            error = error.with_label(*declaration, format!("`{name}` declared here"));
        }
        if let Some(pc) = state
            .get(&slot.index)
            .and_then(|value| value.invalidated_at)
        {
            let moved = self.facts.plan.ops.spans()[pc];
            let name = binding.map_or("This value".to_owned(), |(name, _, _)| format!("`{name}`"));
            let context = if pc >= self.pc {
                "in a previous loop iteration"
            } else {
                "on a path reaching this use"
            };
            error = error.with_label(moved, format!("{name} moved or consumed here {context}"));
            if let Some((name, _, ty)) = binding {
                error = error.with_label(*self.span(), format!("`{name}` used again here"));
                error = error.with_help(format!("If the earlier operation only reads the value, consider `&{name}` and a reference-accepting parameter or binding instead of transferring ownership."));
                if *ty == Type::String {
                    error = error.with_help(format!("If an independent String is needed, use `{name}.clone()` at the earlier move; cloning allocates a new string."));
                }
            }
        }
        error
    }

    pub(super) fn check_storage(&self, slot: &Slot, state: &State) -> Result<(), Error> {
        self.record_escape(state.get(&slot.index), false);
        if self.facts.escaped.is_none()
            && state
                .get(&slot.index)
                .is_some_and(|v| v.has_root(self.facts.argument_roots))
        {
            return Err(type_error(
                self.span(),
                ErrorCode::InvalidAssignment,
                "@noescape function cannot retain an argument borrow",
            ));
        }
        let source = state.get(&slot.index);
        let reason = if slot.ty.contains_buffers() || source.is_some_and(|v| v.storage) {
            Some("Buffer owners and views cannot be retained by the task arena")
        } else if source.is_some_and(|v| v.has_root(self.facts.local_roots)) {
            Some("A local borrow cannot be retained by the task arena")
        } else {
            None
        };
        if let Some(reason) = reason {
            let reason = if source.is_some_and(|v| !v.arenas.is_empty()) {
                "A reference into an explicit arena cannot be retained beyond that arena's lifetime"
            } else {
                reason
            };
            return Err(type_error(
                self.span(),
                ErrorCode::InvalidAssignment,
                reason,
            ));
        }
        Ok(())
    }
    pub(super) fn check_arena_storage(
        &self,
        slot: &Slot,
        arenas: &Set,
        state: &State,
        summarized: bool,
    ) -> Result<(), Error> {
        let value = &state[&slot.index];
        self.record_arena_escape(value, arenas);
        if self.facts.escaped.is_none()
            && !arenas.is_disjoint(self.facts.arena_parameter_roots)
            && value.has_root(self.facts.argument_roots)
        {
            return Err(type_error(
                self.span(),
                ErrorCode::InvalidAssignment,
                "@noescape function cannot retain an argument borrow in an explicit arena",
            ));
        }
        if slot.ty.is_owner()
            || !summarized
                && (slot
                    .ty
                    .reference_type()
                    .is_some_and(|reference| reference.mutable)
                    || explicit_arena(&slot.ty))
        {
            return Err(type_error(
                self.span(),
                ErrorCode::InvalidAssignment,
                "Explicit arena payloads require Copy values and shared data references",
            ));
        }
        // A borrowed arena belongs to the caller. Callee-owned storage cannot
        // cover its lifetime, even when the returned object is never observed.
        if !arenas.is_disjoint(self.facts.arena_parameter_roots)
            && (value.has_root(self.facts.local_roots) || value.has_root(self.facts.owner_roots))
        {
            return Err(type_error(
                self.span(),
                ErrorCode::InvalidAssignment,
                "A local borrow cannot be retained by a caller's explicit arena",
            ));
        }
        Ok(())
    }
    pub(super) fn record_child_escapes(&self, name: &str, args: &[Option<Slot>], state: &State) {
        let callee = &self.facts.functions[name];
        if !callee.extern_abi.has_text() {
            for (index, arg) in call_arguments(callee, name, args) {
                if index.map_or(!callee.noescape, |i| callee.parameter_retained(i)) {
                    for slot in arg.iter().flat_map(refs) {
                        self.record_escape(state.get(&slot.index), false);
                    }
                }
            }
        }
    }
    pub(in super::super) fn check_result(&self, state: &State) -> Result<(), Error> {
        if let Some(result) = &self.facts.plan.result {
            for slot in refs(result) {
                self.record_escape(state.get(&slot.index), true);
                if self.facts.escaped.is_none()
                    && state
                        .get(&slot.index)
                        .is_some_and(|v| v.has_root(self.facts.argument_roots))
                {
                    return Err(type_error(
                        self.span(),
                        ErrorCode::InvalidAssignment,
                        "@noescape function cannot return an argument borrow",
                    ));
                }
                if !slot.ty.is_owner()
                    && state
                        .get(&slot.index)
                        .is_some_and(|value| value.has_root(self.facts.owner_roots))
                {
                    return Err(type_error(
                        self.span(),
                        ErrorCode::InvalidAssignment,
                        "Cannot return a view of a local or by-value buffer owner",
                    ));
                }
                if state
                    .get(&slot.index)
                    .is_some_and(|value| value.has_root(self.facts.local_roots))
                {
                    return Err(type_error(
                        self.span(),
                        ErrorCode::InvalidAssignment,
                        "Cannot return a reference to a local binding",
                    ));
                }
                self.check_access(
                    &slot,
                    exclusive_access(&slot.ty),
                    state,
                    self.live,
                    None,
                    &[],
                )?;
            }
        }
        Ok(())
    }
    pub(super) fn check_suspension(&self, state: &State) -> Result<(), Error> {
        let suspending = match &self.facts.plan.ops[self.pc] {
            Op::Call { name, .. } => self.facts.functions[name].suspends,
            Op::Concurrent { .. } | Op::TaskBranch { .. } | Op::TaskJoin { .. } => true,
            _ => false,
        };
        // Stable explicit regions may cross ordinary CPS calls. Keep concurrency
        // separate, including helpers and unknown suspending call contracts, so
        // an arena-dependent reference cannot be laundered into a child task.
        let concurrent = match &self.facts.plan.ops[self.pc] {
            Op::Call { name, .. } => self.facts.functions[name].uses_concurrency,
            Op::Concurrent { .. } | Op::TaskBranch { .. } | Op::TaskJoin { .. } => true,
            _ => false,
        };
        if concurrent
            && self.live.iter().any(|index| {
                state.get(index).is_some_and(|value| !value.invalid)
                    && (explicit_arena(&self.facts.types[index])
                        || state
                            .get(index)
                            .is_some_and(|value| !value.arenas.is_empty()))
            })
        {
            return Err(type_error(
                        self.span(),
                        ErrorCode::InvalidCall,
                        "Explicit arenas and their references cannot enter concurrency or calls that may enter concurrency",
                    ));
        }
        if suspending
            && self.live.iter().any(|index| {
                self.facts
                    .types
                    .get(index)
                    .is_some_and(Type::contains_dyn_view)
            })
        {
            return Err(type_error(
                self.span(),
                ErrorCode::InvalidCall,
                "A dyn view cannot remain live across suspension or enter concurrency",
            ));
        }
        // Local places live in stable activation storage. Interior views remain
        // pinned by their owner loans, which forbid relocation and early cleanup.
        // Storage eligibility does not grant permission to retain a reference.
        if suspending
            && self.live.iter().any(|index| {
                !self.facts.local_places.contains(index)
                    && !self.facts.types[index].is_suspend_safe_view()
                    && state.get(index).is_some_and(|value| {
                        !value.invalid
                            && value.has_root(self.facts.local_roots)
                            && !value.loans.is_subset(self.facts.reservations)
                    })
            })
        {
            return Err(type_error(
                self.span(),
                ErrorCode::InvalidCall,
                "A local borrow cannot remain live across suspension",
            ));
        }
        if suspending
            && self.live.iter().any(|index| {
                self.facts.types[index].reference_type().is_some()
                    && !self.facts.local_places.contains(index)
                    && !self.facts.types[index].is_suspend_safe_view()
                    && state
                        .get(index)
                        .is_some_and(|value| !value.invalid && value.interior)
            })
        {
            return Err(type_error(
                self.span(),
                ErrorCode::InvalidCall,
                "An interior element or Slice borrow cannot remain live across suspension",
            ));
        }
        if suspending
            && self.live.iter().any(|index| {
                self.facts.types[index].reference_type().is_some()
                    && !self.facts.types[index].is_buffer_view()
                    && !self.facts.types[index].is_suspend_safe_view()
                    && state.get(index).is_some_and(|value| {
                        // Explicit regions also admit Copy graphs with shared
                        // references. Their stored loans last until region
                        // cleanup; they are not relocatable container elements.
                        let region_object = !value.arenas.is_empty()
                            && self.facts.types[index]
                                .reference_type()
                                .is_some_and(|r| r.target.is_copy_storage_value());
                        !value.invalid
                            && value.storage
                            && value.has_root(self.facts.owner_roots)
                            && !region_object
                    })
            })
        {
            return Err(type_error(
                self.span(),
                ErrorCode::InvalidCall,
                "A borrowed owner or element view cannot remain live across suspension",
            ));
        }
        Ok(())
    }
}
